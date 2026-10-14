//! Deliver a private PNG to a verified Loupe bus connection, never its alias.

use std::collections::HashMap;
use std::path::Path;
use std::process::Child;
use std::time::Duration;

use futures_lite::future;
use kettle_ctl::agent_launch::HeldExecutable;
use kettle_ctl::process::{self, ProcessIdentity};
use zbus::{Connection, Message, connection::Builder, names::OwnedUniqueName};

use super::{LOUPE, OpenFailure, Viewer};

const NAME: &str = "org.gnome.Loupe";
const OBJECT: &str = "/org/gnome/Loupe";
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REPLY: usize = 4096;

struct Receiver {
    name: OwnedUniqueName,
    identity: ProcessIdentity,
}

#[derive(Default)]
struct Started(Option<Child>);

impl Drop for Started {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn open(file: &Path) -> Result<Option<Child>, OpenFailure> {
    if !super::trusted_program(Path::new(LOUPE)) {
        return Err(OpenFailure::Viewer);
    }
    let held = HeldExecutable::open(Path::new(LOUPE)).map_err(|_| OpenFailure::Viewer)?;
    let mut started = Started::default();
    future::block_on(future::or(
        async {
            let connection = Builder::session()
                .map_err(|_| OpenFailure::Viewer)?
                .max_queued(1)
                .method_timeout(CALL_TIMEOUT)
                .build()
                .await
                .map_err(|_| OpenFailure::Viewer)?;
            let mut owner = owner(&connection).await.map_err(|_| OpenFailure::Viewer)?;
            if owner.is_none() {
                // No file reaches a launcher that could forward it to an alias.
                started.0 = Some(
                    Viewer::Loupe
                        .command(file)
                        .spawn()
                        .map_err(|_| OpenFailure::Viewer)?,
                );
                while owner.is_none() {
                    owner = self::owner(&connection)
                        .await
                        .map_err(|_| OpenFailure::Viewer)?;
                    if owner.is_some() {
                        break;
                    }
                    if started.0.as_mut().is_some_and(|child| {
                        child.try_wait().map_or(true, |status| status.is_some())
                    }) {
                        return Err(OpenFailure::Viewer);
                    }
                    async_io::Timer::after(Duration::from_millis(20)).await;
                }
            }
            let Some(owner) = owner else {
                return Err(OpenFailure::Viewer);
            };
            let receiver = receiver(&connection, owner, &held)
                .await
                .map_err(|_| OpenFailure::Viewer)?;
            deliver(&connection, &receiver, &held, file)
                .await
                .map_err(|_| OpenFailure::Refused)?;
            Ok(started.0.take())
        },
        async {
            async_io::Timer::after(OPEN_TIMEOUT).await;
            Err(OpenFailure::Viewer)
        },
    ))
}

#[cfg(test)]
mod tests;

fn bounded(reply: Message) -> zbus::Result<Message> {
    if reply.body().len() > MAX_REPLY {
        return Err(zbus::Error::Failure("image viewer reply too large".into()));
    }
    Ok(reply)
}

async fn bus_query(connection: &Connection, method: &str, name: &str) -> zbus::Result<Message> {
    bounded(
        connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                method,
                &name,
            )
            .await?,
    )
}

async fn owner(connection: &Connection) -> zbus::Result<Option<OwnedUniqueName>> {
    match bus_query(connection, "GetNameOwner", NAME).await {
        Ok(reply) => reply.body().deserialize().map(Some),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn receiver(
    connection: &Connection,
    name: OwnedUniqueName,
    held: &HeldExecutable,
) -> zbus::Result<Receiver> {
    let uid: u32 = bus_query(connection, "GetConnectionUnixUser", name.as_str())
        .await?
        .body()
        .deserialize()?;
    let pid: u32 = bus_query(connection, "GetConnectionUnixProcessID", name.as_str())
        .await?
        .body()
        .deserialize()?;
    let identity = process::identity(pid)
        .map_err(|_| zbus::Error::Failure("image viewer process unavailable".into()))?;
    // SAFETY: geteuid has no preconditions.
    if uid != unsafe { libc::geteuid() } || !held.matches(identity) {
        return Err(zbus::Error::Failure("image viewer identity changed".into()));
    }
    Ok(Receiver { name, identity })
}

async fn deliver(
    connection: &Connection,
    receiver: &Receiver,
    held: &HeldExecutable,
    file: &Path,
) -> zbus::Result<()> {
    if !held.matches(receiver.identity) {
        return Err(zbus::Error::Failure("image viewer identity changed".into()));
    }
    let uri = url::Url::from_file_path(file)
        .map_err(|_| zbus::Error::Failure("invalid image copy path".into()))?;
    if uri.as_str().len() > 16384 {
        return Err(zbus::Error::Failure("image copy path too long".into()));
    }
    let platform: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    let reply = connection
        .call_method(
            Some(receiver.name.as_str()),
            OBJECT,
            Some("org.freedesktop.Application"),
            "Open",
            &(vec![uri.as_str()], platform),
        )
        .await?;
    bounded(reply)?.body().deserialize::<()>()
}
