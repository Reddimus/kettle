pub use arboard::{Clear, Get, Set};

#[allow(dead_code, unexpected_cfgs)]
#[rustfmt::skip]
#[path = "../../../vendor/arboard-3.6.1/src/common.rs"]
mod common;
pub use common::Error;

#[rustfmt::skip]
#[path = "../../../vendor/arboard-3.6.1/src/platform/x11_incr.rs"]
mod x11_incr;

#[cfg(target_os = "linux")]
#[test]
fn default_paste_x11_owner_cannot_renew_media_incr_forever() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    use x11rb::{
        connection::Connection,
        protocol::{Event, xproto::*},
        wrapper::ConnectionExt as _,
    };
    if std::env::var_os("KETTLE_X11_CLIPBOARD_TEST").as_deref() != Some("1".as_ref()) {
        eprintln!("SKIP: requires KETTLE_X11_CLIPBOARD_TEST=1 on a private X11 display");
        return;
    }
    let (owner, screen) = x11rb::connect(None).unwrap();
    let win = owner.generate_id().unwrap();
    owner
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            owner.setup().roots[screen].root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap();
    let atom = |name: &[u8]| {
        owner
            .intern_atom(false, name)
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let selection = atom(b"CLIPBOARD");
    let incr = atom(b"INCR");
    let png = atom(b"image/png");
    let uri = atom(b"text/uri-list");
    owner
        .set_selection_owner(win, selection, x11rb::CURRENT_TIME)
        .unwrap();
    owner.sync().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let serving = std::thread::spawn(move || {
        let mut transfer = None;
        let mut next_chunk = Instant::now();
        while !stopped.load(Ordering::Relaxed) {
            match owner.poll_for_event().unwrap() {
                Some(Event::SelectionRequest(request)) => {
                    assert!([png, uri].contains(&request.target));
                    owner
                        .change_window_attributes(
                            request.requestor,
                            &ChangeWindowAttributesAux::new()
                                .event_mask(EventMask::PROPERTY_CHANGE),
                        )
                        .unwrap();
                    owner
                        .change_property32(
                            PropMode::REPLACE,
                            request.requestor,
                            request.property,
                            incr,
                            &[1],
                        )
                        .unwrap();
                    let reply = SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: request.time,
                        requestor: request.requestor,
                        selection: request.selection,
                        target: request.target,
                        property: request.property,
                    };
                    owner
                        .send_event(false, request.requestor, EventMask::NO_EVENT, reply)
                        .unwrap();
                    transfer = Some((request.requestor, request.property, request.target, false));
                    owner.flush().unwrap();
                }
                Some(Event::PropertyNotify(event)) if event.state == Property::DELETE => {
                    if let Some((window, property, _, ready)) = &mut transfer
                        && event.window == *window
                        && event.atom == *property
                    {
                        *ready = true;
                    }
                }
                _ => {}
            }
            if Instant::now() >= next_chunk {
                if let Some((window, property, target, ready)) = &mut transfer
                    && *ready
                {
                    // Correct type, nonempty, no final empty property. Small chunks renew
                    // the old per-chunk timeout without ever exhausting a byte ceiling.
                    if owner
                        .change_property8(PropMode::REPLACE, *window, *property, *target, b"x")
                        .unwrap()
                        .check()
                        .is_err()
                    {
                        transfer = None;
                    } else {
                        *ready = false;
                    }
                    owner.flush().unwrap();
                }
                next_chunk = Instant::now() + Duration::from_millis(100);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    // These are the acquisitions invoked before text by App::paste_clipboard.
    for image in [false, true] {
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut clipboard = arboard::Clipboard::new().unwrap();
            let result = if image {
                clipboard.get_image().map(|_| ())
            } else {
                clipboard.get().file_list().map(|_| ())
            };
            tx.send(result).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        if !matches!(result, Ok(Err(arboard::Error::Timeout))) {
            stop.store(true, Ordering::Relaxed);
            serving.join().unwrap();
            panic!("paste acquisition failed to stop waiting: {result:?}");
        }
        reader.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
}
