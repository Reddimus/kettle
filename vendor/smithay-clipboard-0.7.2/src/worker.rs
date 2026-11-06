use std::io::{Error, Result};
use std::sync::mpsc::Sender;

use sctk::reexports::calloop::channel::Channel;
use sctk::reexports::calloop::{channel, EventLoop};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::Connection;

use crate::state::{SelectionTarget, State};

/// Spawn a clipboard worker, which dispatches its own `EventQueue` and handles
/// clipboard requests.
pub fn spawn(
    name: String,
    display: Connection,
    rx_chan: Channel<Command>,
    worker_replier: Sender<Result<String>>,
    ready: Sender<Result<()>>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            worker_impl(display, rx_chan, worker_replier, ready);
        })
        .ok()
}

/// Clipboard worker thread command.
#[derive(Eq, PartialEq)]
pub enum Command {
    /// Store data to a clipboard.
    Store(String),
    /// Store data to a primary selection.
    StorePrimary(String),
    /// An acknowledged selection write that expires before execution.
    StoreBounded(bool, String, std::time::Instant),
    /// Load data from a clipboard.
    Load,
    /// Load primary selection.
    LoadPrimary,
    /// A selection read with an absolute acquisition budget.
    LoadBounded(bool, usize, std::time::Instant),
    /// Shutdown the worker.
    Exit,
}

/// Handle clipboard requests.
fn worker_impl(
    connection: Connection,
    rx_chan: Channel<Command>,
    reply_tx: Sender<Result<String>>,
    ready: Sender<Result<()>>,
) {
    let (globals, event_queue) = match registry_queue_init(&connection) {
        Ok(data) => data,
        Err(_) => return,
    };

    let mut event_loop = match EventLoop::<State>::try_new() {
        Ok(event_loop) => event_loop,
        Err(_) => return,
    };
    let loop_handle = event_loop.handle();

    let mut state = match State::new(&globals, &event_queue.handle(), loop_handle.clone(), reply_tx)
    {
        Some(state) => state,
        None => return,
    };

    if loop_handle
        .insert_source(rx_chan, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                match event {
                    Command::StorePrimary(contents) => {
                        state.store_selection(SelectionTarget::Primary, contents);
                    },
                    Command::Store(contents) => {
                        state.store_selection(SelectionTarget::Clipboard, contents);
                    },
                    Command::StoreBounded(primary, contents, deadline) => {
                        let target = if primary {
                            SelectionTarget::Primary
                        } else {
                            SelectionTarget::Clipboard
                        };
                        let result = if std::time::Instant::now() >= deadline {
                            Err(std::io::ErrorKind::TimedOut.into())
                        } else {
                            state
                                .store_selection(target, contents)
                                .map(|()| String::new())
                                .ok_or_else(|| Error::other("selection write unavailable"))
                        };
                        let _ = state.reply_tx.send(result);
                    },
                    Command::Load if state.data_device_manager_state.is_some() => {
                        if let Err(err) = state.load_selection(SelectionTarget::Clipboard, None) {
                            let _ = state.reply_tx.send(Err(err));
                        }
                    },
                    Command::LoadPrimary if state.data_device_manager_state.is_some() => {
                        if let Err(err) = state.load_selection(SelectionTarget::Primary, None) {
                            let _ = state.reply_tx.send(Err(err));
                        }
                    },
                    Command::Load | Command::LoadPrimary => {
                        let _ = state
                            .reply_tx
                            .send(Err(Error::other("requested selection is not supported")));
                    },
                    Command::LoadBounded(primary, max, deadline) => {
                        let target = if primary {
                            SelectionTarget::Primary
                        } else {
                            SelectionTarget::Clipboard
                        };
                        if let Err(err) = state.load_selection(target, Some((max, deadline))) {
                            let _ = state.reply_tx.send(Err(err));
                        }
                    },
                    Command::Exit => state.exit = true,
                }
            }
        })
        .is_err()
    {
        return;
    }

    if WaylandSource::new(connection, event_queue).insert(loop_handle).is_err() {
        return;
    }

    let _ = ready.send(Ok(()));
    loop {
        if event_loop.dispatch(None, &mut state).is_err() || state.exit {
            break;
        }
    }
}
