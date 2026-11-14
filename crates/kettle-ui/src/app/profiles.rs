//! Profile discovery consumers and isolated process launches.

use super::*;
use kettle_i18n::{Text as T, Translator};

impl App {
    pub(super) fn profiles_ready(&mut self) {
        if !self.profile_inventory.complete() {
            return;
        }
        self.sync_macos_menu(None);
    }
}

pub(super) fn open_profile_palette(ws: &mut WindowState, profiles: Option<&[String]>) {
    ws.palette_profiles = profiles.map(<[String]>::to_vec);
    ws.palette_input = Some((Default::default(), 0));
}

pub(super) fn palette_profile_status(
    profiles: Option<&[String]>,
    tr: &Translator,
) -> Option<&'static str> {
    profiles.is_none().then(|| tr.text(T::MenuProfilesPending))
}

#[derive(Debug, PartialEq)]
pub(super) enum ProfileCycle {
    Unavailable,
    Empty,
    Next(String),
}

pub(super) fn profile_cycle(
    snapshot: Option<&[String]>,
    current: Option<&str>,
    forward: bool,
) -> ProfileCycle {
    match snapshot {
        None => ProfileCycle::Unavailable,
        Some([]) => ProfileCycle::Empty,
        Some(names) => ProfileCycle::Next(pick_next_profile(current, names, forward)),
    }
}

pub(super) fn append_profile_choices(
    items: &mut Vec<ContextMenuItem>,
    profiles: Option<&[String]>,
    tr: &Translator,
) {
    if profiles.is_some_and(<[String]>::is_empty) {
        return;
    }
    items.push(ContextMenuItem::Separator);
    items.push(ContextMenuItem::Submenu {
        label: tr.text(T::MenuProfile).to_string(),
        items: match profiles {
            Some(profiles) => profile_choices(profiles),
            None => vec![profile_pending_item(tr)],
        },
    });
}

fn profile_choices(profiles: &[String]) -> Vec<ContextMenuItem> {
    profiles
        .iter()
        .map(|name| ContextMenuItem::ProfileChoice {
            label: name.clone(),
            profile: name.clone(),
        })
        .collect()
}

fn profile_pending_item(tr: &Translator) -> ContextMenuItem {
    ContextMenuItem::info(tr.text(T::MenuProfilesPending))
}

pub(super) fn profile_launch_items(
    profiles: Option<&[String]>,
    tr: &Translator,
) -> Vec<ContextMenuItem> {
    if profiles.is_none() {
        return vec![profile_pending_item(tr)];
    }
    profile_launch_entries(profiles, tr)
        .into_iter()
        .map(|(label, action)| ContextMenuItem::DynamicItem {
            label: label.shown(tr).to_string(),
            action,
            enabled: true,
            toggle: None,
        })
        .collect()
}

/// Shared by all windows in one App; never wait on the event loop for admission.
#[derive(Default)]
pub(super) struct ProfileLaunchGate(std::sync::Arc<std::sync::atomic::AtomicBool>);

struct ProfileLaunchPermit(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ProfileLaunchGate {
    fn try_acquire(&self) -> std::io::Result<ProfileLaunchPermit> {
        use std::sync::atomic::Ordering;
        self.0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| ProfileLaunchPermit(self.0.clone()))
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::WouldBlock, "profile validation busy")
            })
    }
}

impl Drop for ProfileLaunchPermit {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub(super) fn launch_profile(gate: &ProfileLaunchGate, name: String, tr: Translator) {
    let notify = move |error| {
        log::warn!("profile launch failed: {error}");
        fire_notify(
            tr.text(T::NotifyProfileLaunchFailed),
            tr.text(T::NotifyProfileLaunchFailedBody),
        );
    };
    if let Err(error) = spawn_profile_launch(
        gate,
        name,
        Config::try_list_profiles,
        |name| {
            let exe = std::env::current_exe().unwrap_or_else(|_| "kettle".into());
            std::process::Command::new(exe)
                .arg(format!("--profile={name}"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        },
        |mut child| child.wait().map(|_| ()),
        notify,
    ) {
        log::warn!("profile launch not admitted: {error}");
        fire_notify(
            tr.text(T::NotifyProfileLaunchFailed),
            tr.text(if error.kind() == std::io::ErrorKind::WouldBlock {
                T::NotifyProfileLaunchBusy
            } else {
                T::NotifyProfileLaunchFailedBody
            }),
        );
    }
}

fn spawn_profile_launch<C: Send + 'static>(
    gate: &ProfileLaunchGate,
    name: String,
    enumerate: impl FnOnce() -> std::io::Result<Vec<String>> + Send + 'static,
    spawn: impl FnOnce(&str) -> std::io::Result<C> + Send + 'static,
    reap: impl FnOnce(C) -> std::io::Result<()> + Send + 'static,
    failure: impl FnOnce(std::io::Error) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let permit = gate.try_acquire()?;
    std::thread::Builder::new()
        .name("kettle-profile".into())
        .spawn(move || {
            // Only a successful current scan can authorize this exact name.
            let result = enumerate().and_then(|names| {
                if names.contains(&name) {
                    spawn(&name)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "profile is not listed",
                    ))
                }
            });
            // Retain every started child before releasing admission. Reaping
            // stays on this already-started worker, independent of validation.
            drop(permit);
            if let Err(error) = result.and_then(reap) {
                failure(error);
            }
        })
}

#[cfg(test)]
mod tests;
