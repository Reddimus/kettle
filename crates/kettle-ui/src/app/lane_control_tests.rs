use super::{
    LaneRenderVerdict, MAX_SOURCE_COLUMN, earliest, gallery_step, lane_announced,
    lane_control_name, lane_previews_item, lane_render_verdict, lane_source, next_hover,
    preview_canvas, preview_failure_notice, scrolled_source, source_scroll_by,
};
use std::sync::Arc;

fn spec(source: kettle_media::Source) -> crate::media::JobSpec {
    crate::media::JobSpec::from_job(kettle_media::Job {
        kind: kettle_media::JobKind::Auto,
        source,
        theme: kettle_media::Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: kettle_media::Canvas::Theme,
        target: kettle_media::Target {
            width: 8,
            height: 8,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: Vec::new(),
    })
    .unwrap()
}

/// A file this platform names, from a Unix path: on Windows the same
/// path on drive C.
fn file(path: &[u8]) -> kettle_media::Source {
    #[cfg(unix)]
    let bytes = path.to_vec();
    #[cfg(windows)]
    let bytes = format!(
        "C:{}",
        std::str::from_utf8(path).unwrap().replace('/', "\\")
    )
    .encode_utf16()
    .flat_map(u16::to_le_bytes)
    .collect();
    kettle_media::Source::Path {
        path: kettle_media::NativePath::new(bytes).unwrap(),
        authorization: kettle_media::Authorization::ExternalAttested(
            kettle_media::ExternalAttested { dev: 1, ino: 2 },
        ),
    }
}

fn with_source(id: u64, source: crate::media::ItemSource) -> crate::media::ShelfItem {
    crate::media::ShelfItem::new(
        id,
        None,
        "plan".into(),
        crate::media::Provenance::User,
        kettle_media::MediaKind::Mermaid,
        Vec::new(),
        kettle_core::ImageData::new(1, 1, vec![0, 0, 0, 255]).unwrap(),
        source,
    )
}

const PAGES: [&str; 2] = ["flowchart LR\n  A --> B\n", "pie\n  \"a\" : 1\n"];

fn gallery_item(id: u64, source: kettle_media::Source) -> crate::media::ShelfItem {
    let gallery = crate::media::Gallery::new(PAGES.map(String::from).to_vec()).unwrap();
    let page = crate::media::ItemSource::page(&spec(source), Arc::new(gallery), 0).unwrap();
    with_source(id, page)
}

fn item(id: u64, kind: kettle_media::MediaKind, svg: &[u8]) -> crate::media::ShelfItem {
    crate::media::ShelfItem::new(
        id,
        None,
        "plot".into(),
        crate::media::Provenance::User,
        kind,
        Vec::new(),
        kettle_core::ImageData::new(1, 1, vec![0, 0, 0, 255]).unwrap(),
        crate::media::ItemSource::sample(svg),
    )
}

fn video(seconds: u64) -> kettle_media::VideoInfo {
    kettle_media::VideoInfo {
        duration_ms: seconds * 1000,
        width: 640,
        height: 360,
        rotation: 0,
        codec: kettle_media::VideoCodec::H264,
        fps_milli: Some(30_000),
        has_audio: true,
        container: Some(kettle_media::video::VideoContainer::IsoBmff),
    }
}

#[test]
fn preview_menu_actions_keep_the_item_generation_and_view_mode() {
    use super::PreviewMenuTarget;
    use kettle_render::MediaLaneMode;

    let mut image = item(7, kettle_media::MediaKind::Svg, b"<svg/>");
    let mut panel = crate::window_state::PreviewPanel::new(image.id);
    let target = PreviewMenuTarget {
        pane: 3,
        item: image.id,
        generation: image.generation,
        mode: MediaLaneMode::Rendered,
    };
    assert!(target.matches(&image, &panel));
    image.generation += 1;
    assert!(
        !target.matches(&image, &panel),
        "replacement must not retarget a menu"
    );
    image.generation -= 1;
    panel.mode = MediaLaneMode::Source;
    assert!(
        !target.matches(&image, &panel),
        "a different view must not change what Copy means"
    );
    panel.mode = MediaLaneMode::Rendered;
    panel.item += 1;
    assert!(
        !target.matches(&image, &panel),
        "another preview cannot receive the action"
    );
}

#[test]
fn preview_menu_copy_names_describe_the_clipboard_content() {
    use super::{ContextMenuClick, ContextMenuItem, PreviewMenuAction, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneMode as M, MediaLanePlay as P};

    for (kind, expected) in [
        ("image", "Copy image"),
        ("document", "Copy page as image"),
        ("video", "Copy frame"),
        ("source", "Copy source"),
    ] {
        let mut lane = super::tests::test_lane_panel();
        lane.rect = (0.0, 0.0, 1200.0, 800.0);
        lane.pin_label = Some("Pin".into());
        lane.document = kind == "document";
        if kind == "document" {
            lane.page = Some((1, 2));
        }
        if kind == "video" {
            lane.play = Some(P::Play);
            lane.scrubber = Some(kettle_render::MediaLaneScrubber {
                seekable: false,
                position_ms: 0,
                duration_ms: 1000,
                label: "0:00 / 0:01".into(),
            });
        }
        if kind == "source" {
            lane.mode = M::Source;
            lane.source = Some(kettle_render::MediaLaneSource {
                rows: vec!["<svg/>".into()],
                first_row: 0,
                total_rows: 1,
            });
        }
        let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
        let target = PreviewMenuTarget {
            pane: 7,
            item: 9,
            generation: 4,
            mode: lane.mode,
        };
        let rows = super::menus::preview_menu_items(
            target,
            &lane,
            geometry.full,
            None,
            Translator::new(Language::En),
        );
        let copies: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                ContextMenuItem::Preview {
                    label,
                    action: PreviewMenuAction::Copy(mode),
                    ..
                } => Some((*label, *mode)),
                _ => None,
            })
            .collect();
        assert_eq!(copies, [(expected, lane.mode)], "{kind}");
        for (index, row) in rows.iter().enumerate() {
            if matches!(row, ContextMenuItem::Separator) {
                continue;
            }
            assert!(super::item_is_dispatchable(row));
            assert!(
                matches!(super::item_to_click(row, index), Some(ContextMenuClick::Preview { target: bound, .. }) if bound == target)
            );
        }
    }
}

#[test]
fn preview_menu_keeps_supported_actions_when_the_toolbar_overflows() {
    use super::{ContextMenuItem, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneMode as M, MediaLanePlay as P, MediaLaneSound as S};

    for kind in ["image", "document", "video"] {
        let mut lane = super::tests::test_lane_panel();
        lane.rect = (0.0, 0.0, 1200.0, 800.0);
        lane.reload = true;
        lane.open_outside = true;
        lane.page = Some((2, 3));
        if kind == "image" {
            lane.source = Some(kettle_render::MediaLaneSource {
                rows: vec!["<svg/>".into()],
                first_row: 0,
                total_rows: 1,
            });
        } else if kind == "document" {
            lane.document = true;
        } else {
            lane.page = None;
            lane.pin_label = Some("Pin".into());
            lane.play = Some(P::Pause);
            lane.sound = Some(S::Heard);
        }
        let target = PreviewMenuTarget {
            pane: 7,
            item: 9,
            generation: 4,
            mode: M::Rendered,
        };
        let rows = |lane: &kettle_render::MediaLanePanel,
                    geometry: &kettle_render::MediaLaneGeometry| {
            super::menus::preview_menu_items(
                target,
                lane,
                geometry.full,
                None,
                Translator::new(Language::En),
            )
            .into_iter()
            .filter_map(|row| match row {
                ContextMenuItem::Preview { label, action, .. } => Some((label, action)),
                _ => None,
            })
            .collect::<Vec<_>>()
        };
        let wide = kettle_render::media_lane_geometry(&lane, (10.0, 16.0), (10.0, 16.0)).unwrap();
        let expected = rows(&lane, &wide);
        lane.rect.2 = 160.0;
        let narrow = kettle_render::media_lane_geometry(&lane, (10.0, 16.0), (10.0, 16.0)).unwrap();
        assert!(narrow.full, "{kind} still has visible content");
        assert!(narrow.copy.is_none(), "{kind} toolbar has overflowed");
        assert_eq!(rows(&lane, &narrow), expected, "{kind}");
    }
}

#[test]
fn preview_menu_uses_available_controls_and_does_not_offer_empty_copy() {
    use super::{ContextMenuItem, PreviewMenuAction as A, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneHit as H, MediaLaneMode as M, MediaLaneSource};

    let mut lane = super::tests::test_lane_panel();
    lane.rect = (0.0, 0.0, 1200.0, 800.0);
    lane.pin_label = Some("Pin".into());
    lane.source = Some(MediaLaneSource {
        rows: vec!["<svg/>".into()],
        first_row: 0,
        total_rows: 1,
    });
    let target = PreviewMenuTarget {
        pane: 7,
        item: 9,
        generation: 4,
        mode: M::Rendered,
    };
    let actions = |lane: &kettle_render::MediaLanePanel| {
        let geometry = kettle_render::media_lane_geometry(lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
        super::menus::preview_menu_items(
            target,
            lane,
            geometry.full,
            None,
            Translator::new(Language::En),
        )
        .into_iter()
        .filter_map(|row| match row {
            ContextMenuItem::Preview { action, .. } => Some(action),
            _ => None,
        })
        .collect::<Vec<_>>()
    };
    let available = actions(&lane);
    assert!(available.contains(&A::Copy(M::Rendered)));
    assert!(
        available.contains(&A::Copy(M::Source)),
        "source can be copied without switching views"
    );
    assert!(available.contains(&A::Control(H::ZoomIn)));
    assert!(available.contains(&A::Control(H::Pin)));
    assert!(
        !available.contains(&A::Control(H::Play)),
        "images do not offer video controls"
    );
    lane.image = None;
    lane.source = None;
    lane.copy = false;
    let unavailable = actions(&lane);
    assert!(
        !unavailable
            .iter()
            .any(|action| matches!(action, A::Copy(_)))
    );
    assert!(!unavailable.contains(&A::Control(H::ZoomIn)));
    assert!(unavailable.contains(&A::Control(H::Close)));
}

#[test]
fn preview_menu_pages_stop_at_document_boundaries() {
    use super::{ContextMenuItem, PreviewMenuAction as A, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneHit as H, MediaLaneMode as M};
    let mut lane = super::tests::test_lane_panel();
    lane.rect = (0.0, 0.0, 1200.0, 800.0);
    lane.document = true;
    let target = PreviewMenuTarget {
        pane: 7,
        item: 9,
        generation: 4,
        mode: M::Rendered,
    };
    for (index, previous, next) in [(1, false, true), (2, true, false)] {
        lane.page = Some((index, 2));
        let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
        let actions: Vec<_> = super::menus::preview_menu_items(
            target,
            &lane,
            geometry.full,
            None,
            Translator::new(Language::En),
        )
        .into_iter()
        .filter_map(|row| match row {
            ContextMenuItem::Preview { action, .. } => Some(action),
            _ => None,
        })
        .collect();
        assert_eq!(actions.contains(&A::Control(H::PreviousPage)), previous);
        assert_eq!(actions.contains(&A::Control(H::NextPage)), next);
    }
}

#[test]
fn preview_menu_playback_rows_capture_the_named_command() {
    use super::{ContextMenuItem, PreviewMenuAction as A, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneMode as M, MediaLanePlay as P, MediaLaneSound as S};
    let mut lane = super::tests::test_lane_panel();
    lane.rect = (0.0, 0.0, 1200.0, 800.0);
    lane.pin_label = Some("Pin".into());
    lane.play = Some(P::Pause);
    lane.sound = Some(S::Heard);
    lane.scrubber = Some(kettle_render::MediaLaneScrubber {
        seekable: true,
        position_ms: 100,
        duration_ms: 1000,
        label: "0:00 / 0:01".into(),
    });
    let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
    let target = PreviewMenuTarget {
        pane: 7,
        item: 9,
        generation: 4,
        mode: M::Rendered,
    };
    let actions: Vec<_> = super::menus::preview_menu_items(
        target,
        &lane,
        geometry.full,
        None,
        Translator::new(Language::En),
    )
    .into_iter()
    .filter_map(|row| match row {
        ContextMenuItem::Preview { action, .. } => Some(action),
        _ => None,
    })
    .collect();
    assert!(actions.contains(&A::Play(P::Pause)));
    assert!(actions.contains(&A::Sound(S::Heard)));
}

#[test]
fn preview_menu_expansion_rows_set_the_named_state() {
    use super::{ContextMenuItem, PreviewMenuAction as A, PreviewMenuTarget};
    use kettle_i18n::{Language, Translator};
    use kettle_render::{MediaLaneHit as H, MediaLaneMode as M};
    let mut lane = super::tests::test_lane_panel();
    lane.rect = (0.0, 0.0, 1200.0, 800.0);
    let target = PreviewMenuTarget {
        pane: 7,
        item: 9,
        generation: 4,
        mode: M::Rendered,
    };
    for collapsed in [false, true] {
        lane.collapsed = collapsed;
        let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
        let rows = super::menus::preview_menu_items(
            target,
            &lane,
            geometry.full,
            None,
            Translator::new(Language::En),
        );
        let label = if collapsed {
            "Expand preview"
        } else {
            "Collapse preview"
        };
        assert!(rows.iter().any(|row| matches!(row,
            ContextMenuItem::Preview { label: shown, action: A::Expanded(value), .. }
                if *shown == label && *value == collapsed)));
        assert!(!rows.iter().any(|row| matches!(
            row,
            ContextMenuItem::Preview {
                action: A::Control(H::Toggle),
                ..
            }
        )));
    }
}

#[test]
fn copying_a_paused_video_uses_the_displayed_frame() {
    use kettle_media::session::{SessionFrame, SessionStatus};

    struct Paused(std::cell::RefCell<Option<SessionFrame>>);

    impl crate::media::PlaybackSession for Paused {
        fn status(&self) -> SessionStatus {
            SessionStatus::Opening
        }
        fn take_frame(&self) -> Option<SessionFrame> {
            self.0.borrow_mut().take()
        }
        fn recycle(&self, _rgba: Vec<u8>) {}
        fn play(&self) {}
        fn pause(&self) {}
        fn keep(&self) {}
        fn audio(&self) -> Option<kettle_media::AudioStatus> {
            None
        }
        fn set_muted(&self, _muted: bool) {}
    }

    let digest = kettle_media::content_digest(b"clip", None).unwrap();
    let mut clip = with_source(
        1,
        crate::media::ItemSource::new(spec(file(b"/clip.mp4")), digest, false, None),
    );
    clip.kind = kettle_media::MediaKind::Video.into();
    clip.video = Some(video(9));
    let panel = crate::window_state::PreviewPanel::new(1);
    let session = Paused(std::cell::RefCell::new(Some(SessionFrame {
        seq: 1,
        pts_ms: 4000,
        width: 1,
        height: 1,
        rgba: vec![9, 8, 7, 255],
    })));
    let mut playback = crate::media::LanePlayback::new(
        clip.id,
        clip.generation,
        9000,
        Box::new(session),
        4000,
        false,
        std::time::Instant::now(),
    );
    assert!(playback.poll(&kettle_core::GraphicsBudget::default()));
    let frame = super::lane_frame(&clip, &panel, None, Some(&playback));
    let copied = super::lane_copy_image(&clip, frame).unwrap();
    assert_eq!(copied.rgba.as_slice(), &[9, 8, 7, 255]);
    assert_eq!(
        copied.allocation_key(),
        playback.frame().unwrap().allocation_key()
    );

    let loading = crate::media::LanePreview::loading(
        clip.id,
        clip.generation,
        7,
        kettle_media::client::RenderControl::default(),
        std::time::Instant::now(),
        crate::media::PreviewStart::User,
    );
    let frame = super::lane_frame(&clip, &panel, Some(&loading), Some(&playback));
    assert!(frame.is_none(), "a loading silent preview shows the poster");
    clip.generation += 1;
    let frame = super::lane_frame(&clip, &panel, None, Some(&playback));
    let copied = super::lane_copy_image(&clip, frame).unwrap();
    assert_eq!(
        copied.allocation_key(),
        clip.image().unwrap().allocation_key()
    );
}

#[test]
fn copying_a_sheet_frame_copies_only_its_pixels() {
    let clip = item(1, kettle_media::MediaKind::Video, b"clip");
    let sheet = kettle_core::ImageData::new(
        2,
        2,
        vec![1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255],
    )
    .unwrap();
    let copied = super::lane_copy_image(
        &clip,
        Some(kettle_render::MediaLaneFrame {
            sheet,
            source: (1, 1, 1, 1),
            size: (640, 360),
        }),
    )
    .unwrap();
    assert_eq!((copied.width, copied.height), (1, 1));
    assert_eq!(copied.rgba.as_slice(), &[4, 4, 4, 255]);
}

/// A lane offers a silent preview only for a video with a length, shown
/// rendered, whose source it can read again: not for an image, a video
/// without length, or one whose bytes went back to the account.
#[test]
fn only_a_video_lane_offers_a_preview() {
    let digest = kettle_media::content_digest(b"clip", None).unwrap();
    let mut clip = with_source(
        1,
        crate::media::ItemSource::new(spec(file(b"/clip.mp4")), digest, false, None),
    );
    clip.kind = kettle_media::MediaKind::Video.into();
    clip.video = Some(video(9));
    let panel = crate::window_state::PreviewPanel::new(1);
    assert!(lane_previews_item(&clip, &panel).is_some());
    let mut still = clip.clone();
    still.video = Some(video(0));
    assert!(lane_previews_item(&still, &panel).is_none());
    let mut image = clip.clone();
    image.kind = kettle_media::MediaKind::Raster.into();
    assert!(lane_previews_item(&image, &panel).is_none());
    let mut unknown = clip.clone();
    unknown.video = None;
    assert!(lane_previews_item(&unknown, &panel).is_none());
    let mut released = clip;
    released.source.spec.input = crate::media::SourceInput::Released;
    assert!(lane_previews_item(&released, &panel).is_none());
}

/// A failed preview says which decoder to install, that there is no
/// sandbox, that the file changed, that it took too long, or that it
/// could not be made.
#[test]
fn a_failed_preview_says_why() {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    for (failure, notice) in [
        (F::BackendUnavailable, N::PreviewNeedsDecoder),
        (F::CodecUnavailable, N::PreviewNeedsDecoder),
        (F::UnsupportedContainer, N::PreviewNeedsDecoder),
        (F::SandboxUnavailable, N::PreviewNeedsSandbox),
        (F::Changed, N::Changed),
        (F::RenderTimeout, N::PreviewTimedOut),
        (F::RenderResource, N::PreviewFailed),
        (F::WorkerUnavailable, N::PreviewFailed),
        (F::Busy, N::PreviewFailed),
    ] {
        assert_eq!(preview_failure_notice(failure), notice, "{failure:?}");
    }
    let now = std::time::Instant::now();
    let later = now + std::time::Duration::from_secs(1);
    assert_eq!(earliest(Some(later), Some(now)), Some(now));
    assert_eq!(earliest(None, Some(now)), Some(now));
    assert_eq!(earliest(Some(later), None), Some(later));
    assert_eq!(earliest(None, None), None);
}

/// A screen reader hears that a preview is being made, else the lane's
/// notice, and nothing while frames play.
#[test]
fn a_lane_announces_its_notice_or_a_preview_being_made() {
    use crate::window_state::LaneNotice;
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let now = std::time::Instant::now();
    let loading = crate::media::LanePreview::loading(
        1,
        0,
        7,
        kettle_media::client::RenderControl::default(),
        now,
        crate::media::PreviewStart::User,
    );
    assert_eq!(
        lane_announced(Some(&loading), Some(LaneNotice::Changed), &tr).as_deref(),
        Some("Making a silent preview…")
    );
    assert_eq!(
        lane_announced(None, Some(LaneNotice::PreviewTimedOut), &tr).as_deref(),
        Some("The preview took too long")
    );
    assert_eq!(lane_announced(None, None, &tr), None);
}

/// A rest on a lane's picture starts when the pointer arrives or moves,
/// keeps its time while the pointer stays put or once a loop started
/// from it, and ends when the pointer leaves the picture.
#[test]
fn a_hover_rest_is_a_pointer_that_stays_put() {
    let now = std::time::Instant::now();
    let later = now + std::time::Duration::from_millis(300);
    let arrived = next_hover(None, Some(4), Some((10.0, 10.0)), now).unwrap();
    assert_eq!((arrived.pane, arrived.since, arrived.used), (4, now, false));
    let still = next_hover(Some(arrived), Some(4), Some((10.0, 10.0)), later).unwrap();
    assert_eq!(still.since, now);
    let moved = next_hover(Some(arrived), Some(4), Some((11.0, 10.0)), later).unwrap();
    assert_eq!(moved.since, later, "moving starts the rest again");
    let used = crate::window_state::LaneHover {
        used: true,
        ..arrived
    };
    let after = next_hover(Some(used), Some(4), Some((50.0, 10.0)), later).unwrap();
    assert!(
        after.used && after.since == now,
        "a loop started is not started again"
    );
    let other = next_hover(Some(used), Some(5), Some((50.0, 10.0)), later).unwrap();
    assert!(!other.used && other.pane == 5);
    assert_eq!(
        next_hover(Some(used), None, Some((50.0, 10.0)), later),
        None
    );
}

/// A preview's frames are the lane's: they are routed before any other
/// lane render, never become the item's pixels or its cards' poster,
/// start only on the user's gesture and not for a changed file, end
/// with their lane, and tick with the event loop.
#[test]
fn the_silent_preview_is_wired() {
    let source = kettle_test_support::production_source(&super::app_source());
    let code = kettle_test_support::code_only(&source);
    let flat = code.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = |name: &str| {
        flat.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split(" fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_owned()
    };
    assert!(body("finish_lane_render").contains(
        "if let Some(ticket) = render.preview { return self.finish_lane_preview(ws, render, ticket, spec, result); } if let Some(ticket) = render.tile {"
    ));
    let finish = body("finish_lane_preview");
    for kept in ["item.pixels", "set_poster", "item.size", "item.video"] {
        assert!(!finish.contains(kept), "a preview touched {kept}");
    }
    assert!(finish.contains(
        "preview.waits_on() == Some(ticket) && preview.serves(render.item, render.generation)"
    ));
    assert!(
        finish.contains("lane_render_verdict(pane.media_shelf.items(), &render, &rendered.digest)")
    );
    let start = body("start_lane_preview");
    assert!(
        start.split_once(") {").is_some_and(|(_, rest)| rest
            .trim_start()
            .starts_with("if self.ctl_driving || self.lua_driving { return; }")),
        "a preview reads the file only on the user's own gesture"
    );
    assert!(start.contains("item.source.changed"));
    assert!(start.contains(".limit_reply(limit)"));
    assert!(start.contains("crate::media::Sender::LanePreview(pane)"));
    assert!(finish.contains(
        ".is_some_and(|preview| preview.overdue(now)) { return fail(self, ws, LaneNotice::PreviewTimedOut); }"
    ));
    assert!(body("close_preview").contains("self.end_lane_preview(ws, pane);"));
    assert!(flat.contains(
        "self.lua_driving = true; self.handle_action(ws, action, event_loop); self.lua_driving = false;"
    ));
    let tick = body("tick_lane_previews");
    assert!(
        !tick.contains("start_lane_preview"),
        "a hover never makes frames"
    );
    assert!(tick.contains("preview.replay(now, crate::media::PreviewStart::Hover)"));
    assert!(body("hovered_lane_picture").contains(
        "if window_is_render_hidden(ws) || self.pointer_modal_open(ws) || ws.context_menu.is_some()"
    ));
    assert!(body("prune_previews").contains("self.end_lane_preview(ws, pane);"));
    assert!(body("hovered_lane_picture").contains("let pointer = ws.native_pointer?;"));
    let wait = body("about_to_wait_inner");
    assert!(wait.contains("let lane_preview_wait = self.tick_lane_previews(ws, now);"));
    assert!(wait.contains(
        "if let Some(next) = lane_preview_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }"
    ));
    assert!(body("lane_controls").contains("(15, geometry.play, Hit::Play),"));
    assert_eq!(lane_control_name(kettle_render::MediaLaneHit::Play), "play");
}

/// A video that cannot play says which decoder to install, that there is
/// no sandbox, that the file changed, that it took too long, or that it
/// stopped; never a preview's words.
#[test]
fn a_failed_playback_says_why() {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    for (failure, notice) in [
        (F::BackendUnavailable, N::VideoNeedsDecoder),
        (F::CodecUnavailable, N::VideoNeedsDecoder),
        (F::UnsupportedContainer, N::VideoNeedsDecoder),
        (F::SandboxUnavailable, N::VideoNeedsSandbox),
        (F::Changed, N::Changed),
        (F::RenderTimeout, N::VideoTimedOut),
        (F::RenderResource, N::VideoFailed),
        (F::WorkerUnavailable, N::VideoFailed),
        (F::RenderParse, N::VideoFailed),
    ] {
        assert_eq!(
            super::playback_failure_notice(failure),
            notice,
            "{failure:?}"
        );
    }
}

/// A gallery page being drawn says so, not that the canvas changed.
#[test]
fn a_gallery_page_being_drawn_is_not_a_new_canvas() {
    use crate::window_state::LaneNotice as N;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        assert_ne!(
            super::lane_notice_text(N::RenderingPage, &tr),
            super::lane_notice_text(N::Rendering, &tr),
            "{language:?}"
        );
    }
}

/// Only MP4 and QuickTime, or a container not known yet, may go to
/// Apple's decoder on macOS, and so ask for sound; elsewhere every
/// container asks, its decoder saying it plays none.
#[test]
fn only_apples_containers_ask_for_sound_on_macos() {
    use kettle_media::video::VideoContainer as C;
    for container in [Some(C::IsoBmff), Some(C::QuickTime), None] {
        assert!(super::apple_reads(container), "{container:?}");
    }
    for container in [C::WebM, C::Matroska, C::Avi] {
        assert_eq!(
            super::apple_reads(Some(container)),
            !cfg!(target_os = "macos"),
            "{container:?}"
        );
    }
}

/// A video playing without sound says why, each reason in its own
/// words, in English and Spanish; sound that can play never says it is
/// silent for a reason.
#[test]
fn a_silent_playback_says_why() {
    use kettle_i18n::Text as T;
    use kettle_media::AudioStatus as A;
    for (why, text) in [
        (A::NoTrack, T::MediaLaneNoticeVideoNoSoundTrack),
        (A::DecoderSilent, T::MediaLaneNoticeVideoSilentDecoder),
        (A::Unsupported, T::MediaLaneNoticeVideoSoundUnsupported),
        (A::Unavailable, T::MediaLaneNoticeVideoSoundUnavailable),
        (A::Off, T::MediaLaneNoticeVideoPlaying),
        (A::Ready, T::MediaLaneNoticeVideoPlaying),
    ] {
        assert_eq!(super::silence_text(why), text, "{why:?}");
    }
    // ctl names each its own way.
    let names: std::collections::HashSet<_> = [
        crate::media::PlaybackSound::Heard,
        crate::media::PlaybackSound::Muted,
    ]
    .into_iter()
    .chain(
        [
            A::NoTrack,
            A::DecoderSilent,
            A::Unsupported,
            A::Unavailable,
            A::Off,
        ]
        .map(crate::media::PlaybackSound::Silent),
    )
    .map(super::playback_sound_name)
    .collect();
    assert_eq!(names.len(), 7);
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let said: std::collections::HashSet<_> = [
            A::NoTrack,
            A::DecoderSilent,
            A::Unsupported,
            A::Unavailable,
            A::Off,
        ]
        .into_iter()
        .map(|why| tr.text(super::silence_text(why)))
        .collect();
        assert_eq!(said.len(), 5, "{language:?}");
    }
}

/// A video plays in its lane only on the user's own gesture and not for
/// a changed file, with one decoder a lane and at most two sessions at
/// once; its session is kept while the event loop runs and paused while
/// the lane is hidden; a drag along its bar opens it on release, and one
/// cancelled leaves it; and it ends with its lane, item and pane. The
/// play button plays the video; the palette's preview_play stays the
/// silent preview.
#[test]
fn video_playback_is_wired() {
    let source = kettle_test_support::production_source(&super::app_source());
    let code = kettle_test_support::code_only(&source);
    let flat = code.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = |name: &str| {
        flat.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split(" fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_owned()
    };
    let open = body("open_lane_playback");
    assert!(
        open.split_once(") {").is_some_and(|(_, rest)| rest
            .trim_start()
            .starts_with("use crate::window_state::LaneNotice; if self.ctl_driving || self.lua_driving { return; }")),
        "a video opens only on the user's own gesture"
    );
    assert!(open.contains("item.source.changed"));
    // Resuming a session reads the file as opening one does: a control
    // client or Lua script may only pause, and a changed file is
    // refused, both before the session is told.
    let toggle = body("toggle_lane_play");
    let told = toggle.find("playback.toggle(now)").unwrap();
    let driving = toggle
        .find("if !pauses && (self.ctl_driving || self.lua_driving) { return; }")
        .unwrap();
    let changed = toggle
        .find("if !pauses && changed { if let Some(panel) = ws.preview_panels.get_mut(&pane) { panel.notice = Some(crate::window_state::LaneNotice::Changed); } return self.lanes_changed(ws, false); }")
        .unwrap();
    assert!(driving < told && changed < told);
    assert!(
        toggle.contains(
            "let changed = preview_item(ws, pane).is_some_and(|item| item.source.changed);"
        )
    );
    // A session that fails Changed marks its own item changed.
    assert!(body("poll_lane_playbacks").contains(
        "if failure == kettle_media::FailureCode::Changed && let Some(playback) = ws.lane_playbacks.get(&pane) && let Some(item) = ws .mux .panes .get_mut(&pane) .and_then(|pane| pane.media_shelf.get_mut(playback.item)) .filter(|item| item.generation == playback.generation) { item.source.changed = true; }"
    ));
    let ended = open.find("self.end_lane_preview(ws, pane);").unwrap();
    let roomed = open.find("self.make_room_for_playback(ws);").unwrap();
    let played = open.find("client.play(&open, wake)").unwrap();
    assert!(ended < roomed && roomed < played);
    assert!(
        body("start_lane_preview").contains(
            "if let Some(playback) = ws.lane_playbacks.get_mut(&pane) { playback.park(); }"
        )
    );
    assert!(
        body("make_room_for_playback").contains("if live < crate::media::MAX_PLAYING { return; }")
    );
    let tick = body("tick_lane_playbacks");
    assert!(tick.contains("playback.set_hidden(!visible.contains(pane)); playback.keep();"));
    assert!(tick.contains("if scrubbing != Some(*pane) { playback.cancel_scrub(); }"));
    assert!(tick.contains("live.then_some(LANE_PLAYBACK_KEEP)"));
    let wait = body("about_to_wait_inner");
    assert!(wait.contains("let lane_playback_wait = self.tick_lane_playbacks(ws, now);"));
    assert!(wait.contains(
        "if let Some(next) = lane_playback_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }"
    ));
    assert!(
        body("end_lane_drag").contains("if drag.scrub { self.seek_lane_playback(ws, drag.pane); }")
    );
    assert!(body("close_preview").contains("ws.lane_playbacks.remove(&pane);"));
    assert!(
        body("prune_previews").contains("ws.lane_playbacks.retain(|pane, _| live.contains(pane));")
    );
    assert!(body("act_on_preview").contains("Hit::Play => self.toggle_lane_play(ws, pane),"));
    assert!(flat.contains("Action::VideoPlay => Some(Hit::Play),"));
    // Sound: a control client or a Lua script may mute, never unmute,
    // and is refused before the playback is told; a new session asks
    // for the sound as the user left it.
    let sound = body("toggle_lane_sound");
    let refused = sound
        .find("if playback.muted() && (self.ctl_driving || self.lua_driving) { return; }")
        .unwrap();
    assert!(refused < sound.find("playback.toggle_mute()").unwrap());
    assert!(body("act_on_preview").contains("Hit::Sound => self.toggle_lane_sound(ws, pane),"));
    assert!(flat.contains("Action::VideoMute => Some(Hit::Sound),"));
    assert!(flat.contains("K::Mute => self.toggle_lane_sound(ws, pane),"));
    // A session Apple's decoder turned down opens again, silent; one
    // opened silent says only an external decoder plays it.
    let poll = body("poll_lane_playbacks");
    assert!(poll.contains(
        "if let Some((at_ms, play)) = playback.take_reopen() { reopen.push((pane, at_ms, play)); }"
    ));
    assert!(poll.contains(
        "for (pane, at_ms, play) in reopen { self.open_lane_playback(ws, pane, at_ms, play); }"
    ));
    assert!(open.contains(
        "if open.audio == kettle_media::AudioRequest::Off && let Some(playback) = ws.lane_playbacks.get_mut(&pane) { playback.play_silently(); }"
    ));
    assert!(body("lane_play_open").contains("audio: if apple_reads(info.container) {"));
    // ui_geometry's playback carries it (keys are blanked as strings).
    assert!(flat.contains(": panel.frame.is_some(), : sound, });"));
    assert!(flat.contains(
        "let sound = ws .lane_playbacks .get(&pane) .and_then(crate::media::LanePlayback::sound) .map(playback_sound_name);"
    ));
    assert!(body("lane_play_open").contains(
        "ws.lane_playbacks .get(&pane) .filter(|playback| playback.serves(item.id, item.generation)) .map_or( kettle_media::AudioRequest::On, crate::media::LanePlayback::audio_request, )"
    ));
    assert!(flat.contains(
        "Action::PreviewPlay => { if let Some(pane) = ws.mux.active_focus() && ws.preview_panels.contains_key(&pane) { self.toggle_lane_preview(ws, pane); } }"
    ));
    assert!(body("tick_lane_previews").contains("&& !ws.lane_playbacks.contains_key(&hover.pane)"));
    assert_eq!(
        lane_control_name(kettle_render::MediaLaneHit::Scrub),
        "scrubber"
    );
}

/// The wheel up scrolls a source back and the wheel down on. A swipe
/// whose content moves right shows the columns to its left, as winit
/// counts it; the wheel with Shift held moves sideways, as does the
/// swipe macOS makes of it. No count a control client sends overflows.
#[test]
fn wheel_steps_scroll_a_source_the_way_its_content_moves() {
    let steps = |lines, cols| crate::input::WheelSteps {
        notches: 0,
        lines,
        cols,
    };
    assert_eq!(source_scroll_by(steps(3, 0), false), (-3, 0), "wheel up");
    assert_eq!(source_scroll_by(steps(-2, 0), false), (2, 0), "wheel down");
    assert_eq!(
        source_scroll_by(steps(0, 1), false),
        (0, -1),
        "content right"
    );
    assert_eq!(
        source_scroll_by(steps(0, -1), false),
        (0, 1),
        "content left"
    );
    assert_eq!(source_scroll_by(steps(-1, 0), true), (0, 1), "Shift, down");
    assert_eq!(
        source_scroll_by(steps(0, -1), true),
        (0, 1),
        "macOS's Shift"
    );
    assert_eq!(
        source_scroll_by(steps(i32::MIN, i32::MIN), true),
        (0, i32::MAX)
    );
    // A 60-pixel swipe right from column ten shows column nine.
    let (rows, columns) = source_scroll_by(steps(0, 1), false);
    assert_eq!(scrolled_source((0, 10), rows, columns, 5), (0, 9));
}

/// A source replaced under its key by a shorter one shows from its last
/// row rather than past its end; within it, from the row scrolled to.
#[test]
fn a_lane_shows_its_source_from_a_row_it_has() {
    let source = crate::media::ItemSource::sample(b"<svg/>");
    let replaced = lane_source(&source, (80, 0), 4, 10);
    assert_eq!(replaced.first_row, 0);
    assert_eq!(replaced.rows, ["<svg/>"]);
    assert_eq!(replaced.total_rows, 1);
    let source = crate::media::ItemSource::sample(b"a\nb\nc\n");
    let within = lane_source(&source, (1, 0), 1, 10);
    assert_eq!((within.first_row, within.rows), (1, vec!["b".to_owned()]));
}

/// Leftover wheel motion goes when its gesture ends, wherever it was
/// banked; motion over the terminal is not banked for a lane, nor is a
/// closed lane's kept.
#[test]
fn a_lanes_wheel_motion_goes_with_its_gesture() {
    let src = super::production_source();
    let wheel = src
        .split("WindowEvent::MouseWheel { delta, phase, .. } =>")
        .nth(1)
        .expect("the wheel event");
    let ended = wheel
        .find("winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled")
        .expect("a gesture's end");
    let lane = wheel
        .find("if self.lane_takes_wheel(ws) {")
        .expect("the lane's wheel");
    assert!(
        wheel[ended..lane]
            .contains("ws.wheel.reset();\n                    ws.lane_wheel.reset();")
    );
    assert!(wheel[lane..].contains(
        "ws.lane_wheel.reset();\n                let steps = ws.wheel.feed(&delta, self.cfg.scroll_multiplier);"
    ));
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert!(
        body("ctl_mouse_wheel_delta")
            .contains("ws.lane_wheel.reset();\n        let steps = ws.wheel.feed(")
    );
    assert!(body("close_preview").contains("ws.lane_wheel.reset();"));
}

/// A source scrolls row by row to its last row and no further, and from
/// its first column; nothing scrolls past its start.
#[test]
fn a_source_scrolls_within_its_rows_and_from_its_first_column() {
    assert_eq!(scrolled_source((0, 0), 3, 0, 10), (3, 0));
    assert_eq!(scrolled_source((3, 0), -5, 0, 10), (0, 0));
    assert_eq!(scrolled_source((0, 0), 100, 0, 10), (9, 0));
    assert_eq!(scrolled_source((5, 0), 0, 0, 0), (0, 0), "an empty source");
    assert_eq!(scrolled_source((0, 2), 0, -9, 10), (0, 0));
    assert_eq!(scrolled_source((0, 2), 0, 4, 10), (0, 6));
    assert_eq!(
        scrolled_source((0, MAX_SOURCE_COLUMN), 0, 7, 10),
        (0, MAX_SOURCE_COLUMN)
    );
}

/// A raster's and an SVG's canvas is the user's choice over its kind's
/// own; a diagram's is the one its pixels were rendered for, whatever
/// the user chose, until a render for that choice comes back.
#[test]
fn a_canvas_is_the_users_except_a_diagrams_until_it_renders() {
    use kettle_media::MediaKind;
    use kettle_render::MediaCanvas as C;
    let mut panel = crate::window_state::PreviewPanel::new(1);
    let canvas = |kind, panel: &crate::window_state::PreviewPanel| {
        preview_canvas(&item(1, kind, b"<svg/>"), panel)
    };
    assert_eq!(canvas(MediaKind::Svg, &panel), C::White);
    assert_eq!(canvas(MediaKind::Raster, &panel), C::Checker);
    assert_eq!(canvas(MediaKind::Mermaid, &panel), C::Theme);
    panel.canvas = Some(C::White);
    assert_eq!(canvas(MediaKind::Raster, &panel), C::White);
    assert_eq!(
        canvas(MediaKind::Mermaid, &panel),
        C::Theme,
        "its pixels were rendered for the pane's background"
    );
    let mut rendered = item(1, MediaKind::Mermaid, b"flowchart LR");
    rendered.source.spec.canvas = kettle_media::Canvas::White;
    assert_eq!(preview_canvas(&rendered, &panel), C::White);
}

/// A lane's render replaces its item's pixels only for the item asked
/// about, unreplaced, read from the source it came from.
#[test]
fn a_lane_render_counts_only_for_the_same_item_and_source() {
    let shelf = [item(4, kettle_media::MediaKind::Mermaid, b"flowchart LR")];
    let render = crate::media::LaneRender {
        window: 1,
        pane: 2,
        item: 4,
        generation: 0,
        preview: None,
        tile: None,
        page: None,
    };
    let same = shelf[0].source.digest.clone();
    let other = kettle_media::content_digest(b"flowchart TD", None).unwrap();
    assert_eq!(
        lane_render_verdict(&shelf, &render, &same),
        LaneRenderVerdict::Same
    );
    // Bytes kept cannot change: another digest is of bytes gone by.
    assert_eq!(
        lane_render_verdict(&shelf, &render, &other),
        LaneRenderVerdict::Gone
    );
    // A file read again can.
    let read = crate::media::ItemSource::new(
        spec(file(b"/tmp/flow.mmd")),
        same.clone(),
        true,
        Some("flowchart LR".into()),
    );
    let from_file = [with_source(4, read)];
    assert_eq!(
        lane_render_verdict(&from_file, &render, &other),
        LaneRenderVerdict::Changed
    );
    assert_eq!(
        lane_render_verdict(&from_file, &render, &same),
        LaneRenderVerdict::Same
    );
    let replaced = crate::media::LaneRender {
        generation: 1,
        ..render
    };
    assert_eq!(
        lane_render_verdict(&shelf, &replaced, &same),
        LaneRenderVerdict::Gone
    );
    assert_eq!(
        lane_render_verdict(&[], &render, &same),
        LaneRenderVerdict::Gone
    );
}

/// A gallery page's render counts for that page's bytes, never as a
/// changed file, whatever the gallery was read from; a render of the
/// page shown before paging, or of a page the gallery lacks, is gone.
#[test]
fn a_gallery_page_render_counts_for_its_own_bytes() {
    let digest = |text: &str| kettle_media::content_digest(text.as_bytes(), None).unwrap();
    for source in [
        file(b"/tmp/plan.md"),
        kettle_media::Source::Bytes(PAGES[0].into()),
    ] {
        let mut shelf = [gallery_item(4, source)];
        let render = |page: Option<u32>| crate::media::LaneRender {
            window: 1,
            pane: 2,
            item: 4,
            generation: 0,
            preview: None,
            tile: None,
            page: page.map(|page| crate::media::PageAsk { page, ticket: 1 }),
        };
        assert_eq!(
            lane_render_verdict(&shelf, &render(Some(1)), &digest(PAGES[1])),
            LaneRenderVerdict::Same
        );
        for (asked, read) in [(Some(1), PAGES[0]), (Some(2), PAGES[1]), (None, PAGES[1])] {
            assert_eq!(
                lane_render_verdict(&shelf, &render(asked), &digest(read)),
                LaneRenderVerdict::Gone,
                "{asked:?}"
            );
        }
        assert_eq!(
            lane_render_verdict(&shelf, &render(None), &digest(PAGES[0])),
            LaneRenderVerdict::Same
        );
        assert!(shelf[0].source.select(1));
        assert_eq!(
            lane_render_verdict(&shelf, &render(None), &digest(PAGES[0])),
            LaneRenderVerdict::Gone,
            "a canvas render of the page shown before"
        );
    }
}

/// A video's detail line gives its own size, not its poster's, its
/// codec, length and sound; anything else keeps kind and size.
#[test]
fn a_videos_detail_says_what_it_is() {
    let tr = kettle_i18n::Translator::default();
    let mut item = crate::media::ShelfItem::new(
        1,
        None,
        "clip.webm".into(),
        crate::media::Provenance::Verified,
        kettle_media::MediaKind::Video,
        Vec::new(),
        kettle_core::ImageData::new(400, 225, vec![0; 400 * 225 * 4]).unwrap(),
        crate::media::ItemSource::sample(b"<svg/>"),
    );
    assert_eq!(
        super::media_item_detail(tr, "Video", &item),
        "Video · 400×225"
    );
    let mut info = kettle_media::VideoInfo {
        duration_ms: 12_400,
        width: 1280,
        height: 720,
        rotation: 0,
        codec: kettle_media::VideoCodec::Vp8,
        fps_milli: Some(25_000),
        has_audio: false,
        container: Some(kettle_media::video::VideoContainer::WebM),
    };
    item.video = Some(info);
    assert_eq!(
        super::media_item_detail(tr, "Video", &item),
        "Video · 1280×720 · VP8 · 0:12 · no audio"
    );
    info.codec = kettle_media::VideoCodec::Unknown;
    info.has_audio = true;
    item.video = Some(info);
    assert_eq!(
        super::media_item_detail(tr, "Video", &item),
        "Video · 1280×720 · 0:12 · with audio"
    );
}

/// A video no decoder here reads says which one the user can install;
/// other failures keep their notices.
#[test]
fn a_missing_decoder_says_what_to_install() {
    use kettle_i18n::Text as T;
    use kettle_media::FailureCode as F;
    for failure in [
        F::UnsupportedContainer,
        F::CodecUnavailable,
        F::BackendUnavailable,
    ] {
        assert_eq!(
            super::preview_failure_body(failure),
            T::NotifyBodyPreviewNeedsDecoder
        );
    }
    assert_eq!(
        super::preview_failure_body(F::SandboxUnavailable),
        T::NotifyBodyPreviewNeedsSandbox,
        "no decoder to install fixes a missing sandbox"
    );
    assert_eq!(
        super::preview_failure_body(F::RenderParse),
        T::NotifyBodyPreviewUnsupported
    );
    assert_eq!(
        super::preview_failure_body(F::WorkerUnavailable),
        T::NotifyBodyPreviewUnavailable
    );
    let tr = kettle_i18n::Translator::default();
    assert!(
        tr.text(T::NotifyBodyPreviewNeedsDecoder)
            .contains("brew install ffmpeg")
    );
}

/// A video's metadata, from its poster's render, goes onto its shelf
/// item and into the Show result, and a re-render keeps it.
#[test]
fn videos_keep_what_they_are() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = |name: &str| {
        flat.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split(" fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_owned()
    };
    let finish = body("finish_show");
    for needle in [
        "let video = rendered.video.as_ref().map(|video| video.info);",
        "item.layout = Some(layout); item.video = video;",
        ".with_pages(page);",
        "self.publish_show(ws, (origin, route), item, (inline, inline_refused), page);",
    ] {
        assert!(finish.contains(needle), "finish_show: {needle}");
    }
    let publish = body("publish_show");
    for needle in [
        "let (kind, video, (width, height)) = (item.kind, item.video, item.size);",
        "result.video = video.map(kettle_ctl::show::ShowVideo::from);",
        "(published.id, generation), (kind, video.as_ref(), page), &image,",
        "result.page = page.map(kettle_ctl::show::ShowPage::from);",
    ] {
        assert!(publish.contains(needle), "publish_show: {needle}");
    }
    let card = body("register_card");
    assert!(card.contains("duration_ms: video.map(|video| video.duration_ms),"));
    assert!(card.contains("video.map_or((image.width, image.height), |video| { (video.width, video.height) }), video.map(|video| video.duration_ms),"));
    let lane = body("finish_lane_render");
    for needle in [
        "let video = rendered.video.as_ref().map(|video| video.info);",
        "if video.is_some() { item.video = video; }",
    ] {
        assert!(lane.contains(needle), "finish_lane_render: {needle}");
    }
}

/// A Markdown gallery arrives as its page rendered, a Mermaid item that
/// keeps every page; a page asked for renders from those bytes, is taken
/// only while its lane still waits on it, becomes the item's source then,
/// and starts fitted from the top; one that fails or is not wanted lets
/// the lane's wait go.
#[test]
fn galleries_are_wired() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = |name: &str| {
        flat.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split(" fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_owned()
    };
    let finish = body("finish_show");
    for needle in [
        "let (item_kind, source) = if let Some(gallery) = gallery { let Some(page) = crate::media::ItemSource::page(&spec, gallery, 0) else { return notify_preview_failure(tr, origin.refuse(FailureCode::OverBudget)); }; (kettle_media::MediaKind::Mermaid, page) } else if kind == kettle_media::MediaKind::Markdown {",
        ".zip(crate::media::Gallery::new(rendered.fence_sources))",
        "crate::media::ItemSource::page(&spec, Arc::new(gallery), index)",
        "return notify_preview_failure(tr, origin.refuse(FailureCode::OverBudget));",
        "(kettle_media::MediaKind::Mermaid, page)",
        "(kettle_media::MediaKind::Markdown, page)",
        "provenance, item_kind, warnings.clone(),",
    ] {
        assert!(finish.contains(needle), "finish_show: {needle}");
    }
    let page = body("page_preview");
    for needle in [
        "let Some(page) = gallery_step(shown, count, asked, step).filter(|&page| asked != Some(page)) else {",
        "let spec = item.source.page_spec(page).map(|mut spec| { if let Some(canvas) = canvas { spec.canvas = canvas; } spec });",
        "if page == shown { panel.page = None; panel.notice = None;",
        "self.page_tickets += 1; let ask = crate::media::PageAsk { page, ticket: self.page_tickets, }; panel.page = Some((generation, ask));",
        "page: Some(ask), }; let notice = if document { crate::window_state::LaneNotice::RenderingDocumentPage } else { crate::window_state::LaneNotice::RenderingPage }; self.render_for_lane(ws, render, spec, notice);",
    ] {
        assert!(page.contains(needle), "page_preview: {needle}");
    }
    let lane = body("finish_lane_render");
    for needle in [
        "if !self.lane_wants(ws, &render) { return; }",
        "app.forget_lane_page(ws, &render); app.tell_lane(",
        "Err(RenderError::Cancelled) => return self.forget_lane_page(ws, &render),",
        "LaneRenderVerdict::Gone => return self.forget_lane_page(ws, &render),",
        "if let Some(ask) = render.page && !item.source.select(ask.page) { return; }",
        "item.warnings = warnings;",
        "if render.page.is_some() && page_waited_on(panel, render.generation) == render.page { panel.page = None; } if paged {",
    ] {
        assert!(lane.contains(needle), "finish_lane_render: {needle}");
    }
    assert!(flat.contains(
        "if !self.lane_wants(ws, &render) { continue; } self.forget_lane_page(ws, &render); self.tell_lane("
    ));
    assert!(body("preview_lane_panel").contains(
        "let asked = page_waited_on(panel, item.generation).map_or(shown, |ask| ask.page); ( usize::try_from(asked).map_or(usize::MAX, |asked| asked.saturating_add(1)), count, )"
    ));
    let again = body("render_preview_again");
    for needle in [
        ".and_then(|panel| page_waited_on(panel, generation)) .and_then(|ask| item.source.page_spec(ask.page).map(|spec| (ask.page, spec)));",
        "spec = page_spec; self.page_tickets += 1;",
        "panel.page = Some((generation, ask));",
        "tile: None, page, }; self.render_for_lane(ws, render, spec, crate::window_state::LaneNotice::Rendering);",
    ] {
        assert!(again.contains(needle), "render_preview_again: {needle}");
    }
    assert!(
        body("lane_wants")
            .contains(".is_some_and(|panel| lane_takes_page(panel, render, ask, shown))")
    );
    assert!(body("forget_lane_page").contains(
        "panel.item == render.item && page_waited_on(panel, render.generation) == Some(ask)"
    ));
    assert!(body("act_on_preview").contains(
        "Hit::PreviousPage => self.page_preview(ws, pane, crate::media::PageStep::By(-1)), Hit::NextPage => self.page_preview(ws, pane, crate::media::PageStep::By(1)),"
    ));
    assert!(
        body("preview_focus_key").contains("K::Page(step) => self.page_preview(ws, pane, step),")
    );
}

/// A lane takes a page render for the page it waits on, at the
/// generation it was asked at, or for the page it shows, drawn again;
/// never for another item, generation or page.
#[test]
fn a_lane_takes_the_page_it_waits_on_or_shows() {
    let ask = |page, ticket| crate::media::PageAsk { page, ticket };
    let mut panel = crate::window_state::PreviewPanel::new(4);
    let render = |ask| crate::media::LaneRender {
        window: 1,
        pane: 2,
        item: 4,
        generation: 7,
        preview: None,
        tile: None,
        page: Some(ask),
    };
    let takes = |panel: &crate::window_state::PreviewPanel, asked| {
        super::lane_takes_page(panel, &render(asked), asked, Some(0))
    };
    assert!(!takes(&panel, ask(1, 1)));
    assert!(takes(&panel, ask(0, 1)), "the page shown, drawn again");
    panel.page = Some((7, ask(1, 2)));
    assert_eq!(super::page_waited_on(&panel, 7), Some(ask(1, 2)));
    assert_eq!(super::page_waited_on(&panel, 8), None, "another generation");
    assert!(takes(&panel, ask(1, 2)));
    // The same page asked again since: the older request is not taken,
    // so its failure or late pixels cannot stand for the newer one.
    assert!(!takes(&panel, ask(1, 1)));
    assert!(!takes(&panel, ask(2, 2)));
    panel.page = Some((6, ask(1, 2)));
    assert!(!takes(&panel, ask(1, 2)), "asked of another generation");
    let other = crate::window_state::PreviewPanel::new(5);
    assert!(
        !super::lane_takes_page(&other, &render(ask(0, 1)), ask(0, 1), Some(0)),
        "another item"
    );
}

/// A step pages from the page asked for, else the one shown, and stops
/// at either end.
#[test]
fn a_gallery_steps_without_wrapping() {
    use crate::media::PageStep::By;
    assert_eq!(gallery_step(0, 3, None, By(1)), Some(1));
    assert_eq!(
        gallery_step(0, 3, Some(1), By(1)),
        Some(2),
        "from the asked"
    );
    assert_eq!(gallery_step(0, 3, Some(2), By(1)), None, "past the last");
    assert_eq!(gallery_step(0, 3, None, By(-1)), None, "before the first");
    assert_eq!(gallery_step(2, 3, Some(1), By(-1)), Some(0));
    assert_eq!(
        gallery_step(1, 3, Some(2), By(-1)),
        Some(1),
        "back to the shown"
    );
    assert_eq!(gallery_step(0, 1, None, By(1)), None);
    assert_eq!(
        gallery_step(255, 300, None, By(1)),
        Some(256),
        "a document's pages pass 255"
    );
    assert_eq!(gallery_step(299, 300, None, By(1)), None);
    assert_eq!(gallery_step(0, 3, None, By(i32::MAX)), None);
}

/// Home and End name a gallery's first and last pages, wherever it stands
/// and whatever is on its way.
#[test]
fn home_and_end_name_the_first_and_last_pages() {
    use crate::media::PageStep::{First, Last};
    assert_eq!(gallery_step(5, 300, None, First), Some(0));
    assert_eq!(gallery_step(5, 300, Some(7), Last), Some(299));
    assert_eq!(gallery_step(0, 1, None, Last), Some(0), "one page");
    assert_eq!(gallery_step(0, 0, None, First), None, "no pages");
    assert_eq!(gallery_step(0, 0, None, Last), None, "no pages");
}

/// A PDF's page of `pages` read from `source`, whose bytes have `digest`.
fn document_item(
    id: u64,
    source: kettle_media::Source,
    pages: u32,
    digest: &kettle_media::Digest,
) -> crate::media::ShelfItem {
    let source = crate::media::ItemSource::new(spec(source), digest.clone(), false, None)
        .with_pages(Some(kettle_media::PageOf {
            index: 0,
            count: pages,
        }));
    let mut item = with_source(id, source);
    item.kind = kettle_media::MediaKind::Pdf.into();
    item
}

/// A document pages by reading its source again: every page has the
/// document's digest, so another one on a page read again is a file that
/// changed, not a page gone by; a page past the last is none, and a reload
/// reads the file again at the page shown.
#[test]
fn a_documents_pages_read_its_file_again() {
    let digest = kettle_media::content_digest(b"%PDF-1.7 document", None).unwrap();
    let other = kettle_media::content_digest(b"%PDF-1.7 rewritten", None).unwrap();
    let render = |page: Option<u32>| crate::media::LaneRender {
        window: 1,
        pane: 2,
        item: 4,
        generation: 0,
        preview: None,
        tile: None,
        page: page.map(|page| crate::media::PageAsk { page, ticket: 1 }),
    };
    let mut shelf = [document_item(4, file(b"/tmp/report.pdf"), 300, &digest)];
    assert!(shelf[0].source.is_document());
    assert_eq!(
        lane_render_verdict(&shelf, &render(Some(299)), &digest),
        LaneRenderVerdict::Same
    );
    for asked in [Some(299), None] {
        assert_eq!(
            lane_render_verdict(&shelf, &render(asked), &other),
            LaneRenderVerdict::Changed,
            "{asked:?}"
        );
    }
    let spec = shelf[0].source.page_spec(299).unwrap();
    assert_eq!(spec.kind, kettle_media::JobKind::PdfPage { index: 299 });
    assert!(
        matches!(spec.input, crate::media::SourceInput::Path { .. }),
        "the file again"
    );
    assert!(shelf[0].source.page_spec(300).is_none());
    assert!(!shelf[0].source.select(300));
    assert!(shelf[0].source.select(299));
    assert_eq!(shelf[0].source.shown_page(), Some((299, 300)));
    let panel = crate::window_state::PreviewPanel::new(4);
    assert_eq!(
        super::reload_of(&shelf[0], &panel).unwrap().1.page,
        Some(crate::media::ReadPage::Document(299))
    );
    // Bytes a request carried cannot change: another digest is a render
    // gone by.
    let carried = [document_item(
        4,
        kettle_media::Source::Bytes(b"%PDF-1.7 document".to_vec()),
        2,
        &digest,
    )];
    assert_eq!(
        lane_render_verdict(&carried, &render(Some(1)), &other),
        LaneRenderVerdict::Gone
    );
    // Carried bytes given back have no page left to render.
    let mut released = carried;
    assert!(released[0].source.page_spec(1).is_some());
    released[0].source.release();
    assert!(released[0].source.page_spec(1).is_none());
}

/// A gallery read from a file reloads that file at the page shown; one
/// copied together has no file to read.
/// A Markdown file's gallery is a Markdown item, as its card and the
/// `show` reply say, and each page still draws as the Mermaid diagram it is:
/// on the canvas its render chose, and sharpened when zoomed.
#[test]
fn a_markdown_gallery_is_markdown_and_draws_as_a_diagram() {
    let mut item = gallery_item(6, file(b"/tmp/plan.md"));
    item.kind = kettle_media::MediaKind::Markdown.into();
    item.source.spec.canvas = kettle_media::Canvas::White;
    let panel = crate::window_state::PreviewPanel::new(6);
    assert_eq!(
        crate::app::chosen_canvas(&item, &panel),
        kettle_render::MediaCanvas::White
    );
    assert!(crate::media::draws_as_diagram(item.kind));
    assert_eq!(item.kind.as_str(), "markdown");
    let held = kettle_media::Crop {
        x: 0,
        y: 0,
        width: 1024,
        height: 512,
    };
    let layout = kettle_media::RenderLayout {
        source_width: 200.0,
        source_height: 100.0,
        image_in_target: held,
        result_in_target: held,
    };
    assert!(
        crate::media::plan_crop(
            item.kind.media().unwrap(),
            &layout,
            (1024, 512),
            (8192.0, 4096.0),
            (0.25, 0.25, 0.75, 0.75),
            1.0,
        )
        .is_some()
    );
}

#[test]
fn a_gallery_reloads_its_file_at_the_page_shown() {
    let panel = crate::window_state::PreviewPanel::new(4);
    let mut read = gallery_item(4, file(b"/tmp/plan.md"));
    assert!(super::preview_reloads(&read));
    let (native, reload) = super::reload_of(&read, &panel).unwrap();
    #[cfg(unix)]
    let named = std::path::Path::new("/tmp/plan.md");
    #[cfg(windows)]
    let named = std::path::Path::new("C:\\tmp\\plan.md");
    assert_eq!(native, named);
    assert_eq!(reload.page, Some(crate::media::ReadPage::Gallery(0)));
    assert!(read.source.select(1));
    assert_eq!(
        super::reload_of(&read, &panel).unwrap().1.page,
        Some(crate::media::ReadPage::Gallery(1))
    );
    let copied = gallery_item(5, kettle_media::Source::Bytes(PAGES[0].into()));
    assert!(!super::preview_reloads(&copied));
    assert!(super::reload_of(&copied, &panel).is_none());
}

/// Each lane control has its own name over ctl and its own node id.
#[test]
fn lane_controls_have_distinct_names_and_node_ids() {
    use kettle_render::MediaLaneHit as Hit;
    let hits = [
        Hit::Previous,
        Hit::Next,
        Hit::OpenOutside,
        Hit::Toggle,
        Hit::Close,
        Hit::Mode,
        Hit::Canvas,
        Hit::Copy,
        Hit::Reload,
        Hit::ZoomOut,
        Hit::ZoomIn,
        Hit::Fit,
        Hit::PreviousPage,
        Hit::NextPage,
        Hit::Play,
        Hit::Sound,
        Hit::Theater,
        Hit::Fullscreen,
        Hit::Scrub,
        Hit::Resize,
        Hit::Inside,
    ];
    let names: std::collections::HashSet<_> =
        hits.iter().map(|hit| lane_control_name(*hit)).collect();
    assert_eq!(names.len(), hits.len());
    // The lane, each control, and the bar after them.
    use super::{LANE_CONTROLS, LANE_SCRUBBER_PART, lane_controls};
    let parts = 0..=20;
    let ids: std::collections::HashSet<_> = parts
        .clone()
        .map(|part| super::accessibility_lane_id(7, part).expect("in range"))
        .collect();
    assert_eq!(ids.len(), LANE_CONTROLS + 3);
    let controls: std::collections::HashSet<_> = lane_controls(
        &kettle_render::media_lane_geometry(
            &super::tests::test_lane_panel(),
            (10.0, 20.0),
            (10.0, 20.0),
        )
        .unwrap(),
    )
    .iter()
    .map(|(part, _, _)| *part)
    .collect();
    assert_eq!(controls, (1..=16).chain([19, 20]).collect());
    assert!(!controls.contains(&LANE_SCRUBBER_PART));
    for part in parts {
        let id = super::accessibility_lane_id(7, part).unwrap();
        assert_eq!(super::accessibility_lane_part(id), Some((7, part)));
    }
}

/// Over a rendered item the wheel pans by a touchpad's pixels as they
/// come and by a wheel's lines a notch, in cells and by the multiplier,
/// sideways with Shift; with the zoom modifier it zooms a step a notch,
/// or a notch's worth of pixels, never more than ten steps an event. A
/// pinch zooms by its delta, bounded.
#[test]
fn the_wheel_and_a_pinch_pan_or_zoom_a_rendered_item() {
    use super::{LaneWheel, lane_wheel, lane_wheel_steps, pinch_zoom};
    use winit::event::MouseScrollDelta::{LineDelta, PixelDelta};
    let pixels = |x, y| PixelDelta(winit::dpi::PhysicalPosition::new(x, y));
    let assert_zoom = |wheel, expected: f64| {
        let LaneWheel::Zoom(actual) = wheel else {
            panic!("expected wheel zoom, got {wheel:?}");
        };
        assert!(
            (actual - expected).abs() <= expected.abs() * 1e-12,
            "zoom factor {actual} differs from {expected}"
        );
    };
    let cell = (8.0, 16.0);
    assert_eq!(
        lane_wheel(&pixels(3.0, -7.5), false, false, cell, 1.0),
        LaneWheel::Pan((3.0, -7.5)),
        "a touchpad's pixels"
    );
    assert_eq!(
        lane_wheel(&LineDelta(0.0, -1.0), false, false, cell, 1.0),
        LaneWheel::Pan((0.0, -48.0)),
        "three rows a notch"
    );
    assert_eq!(
        lane_wheel(&LineDelta(0.0, 1.0), false, false, cell, 2.0),
        LaneWheel::Pan((0.0, 96.0)),
        "by the multiplier"
    );
    assert_eq!(
        lane_wheel(&LineDelta(0.0, 1.0), false, true, cell, 1.0),
        LaneWheel::Pan((24.0, 0.0)),
        "sideways with Shift"
    );
    assert_eq!(
        lane_wheel(&LineDelta(0.0, 1.0), true, false, cell, 1.0),
        LaneWheel::Zoom(kettle_render::MEDIA_ZOOM_STEP)
    );
    assert_zoom(
        lane_wheel(&LineDelta(0.0, -2.0), true, true, cell, 1.0),
        kettle_render::MEDIA_ZOOM_STEP.powi(-2),
    );
    assert_eq!(
        lane_wheel(&pixels(0.0, 60.0), true, false, cell, 1.0),
        LaneWheel::Zoom(kettle_render::MEDIA_ZOOM_STEP),
        "a notch's worth of pixels"
    );
    assert_zoom(
        lane_wheel(&LineDelta(0.0, 1e6), true, false, cell, 1.0),
        kettle_render::MEDIA_ZOOM_STEP.powi(10),
    );
    // A control client's whole steps do as a wheel's lines would.
    let steps = |notches, lines, cols| crate::input::WheelSteps {
        notches,
        lines,
        cols,
    };
    assert_eq!(
        lane_wheel_steps(steps(1, 3, 0), true, false, cell),
        LaneWheel::Zoom(kettle_render::MEDIA_ZOOM_STEP),
        "the zoom modifier zooms"
    );
    assert_zoom(
        lane_wheel_steps(steps(100, 300, 0), true, true, cell),
        kettle_render::MEDIA_ZOOM_STEP.powi(10),
    );
    assert_eq!(
        lane_wheel_steps(steps(-1, -3, 0), false, false, cell),
        LaneWheel::Pan((0.0, -48.0))
    );
    assert_eq!(
        lane_wheel_steps(steps(1, 3, 0), false, true, cell),
        LaneWheel::Pan((24.0, 0.0)),
        "sideways with Shift"
    );
    assert_eq!(
        lane_wheel_steps(steps(0, 0, -2), false, false, cell),
        LaneWheel::Pan((-48.0, 0.0)),
        "three columns a sideways step"
    );
    assert_eq!(pinch_zoom(0.1), 1.1);
    assert_eq!(pinch_zoom(-0.9), 0.5);
    assert_eq!(pinch_zoom(50.0), 2.0);
    assert_eq!(pinch_zoom(f64::NAN), 1.0);
    // Three logical pixels make a drag, at any scale.
    assert!(!super::past_drag_slop((10.0, 10.0), (11.0, 12.0), 1.0));
    assert!(super::past_drag_slop((10.0, 10.0), (13.0, 10.0), 1.0));
    assert!(!super::past_drag_slop((10.0, 10.0), (14.0, 10.0), 2.0));
    assert!(super::past_drag_slop((10.0, 10.0), (10.0, 4.0), 2.0));
    for (action, hit) in [
        (
            kettle_config::Action::PreviewZoomIn,
            kettle_render::MediaLaneHit::ZoomIn,
        ),
        (
            kettle_config::Action::PreviewZoomOut,
            kettle_render::MediaLaneHit::ZoomOut,
        ),
        (
            kettle_config::Action::PreviewFit,
            kettle_render::MediaLaneHit::Fit,
        ),
    ] {
        assert_eq!(super::lane_action_hit(&action), Some(hit));
    }
}

/// The wheel over a lane goes to `wheel_over_lane` natively and from a
/// control client; a pinch over one zooms it. A primary press on a
/// rendered item's content arms a drag, whose motion pans it and reaches
/// nothing else until its release, the lane's on both paths; the drag
/// goes when the window loses focus.
#[test]
fn lane_gestures_are_wired() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "if self.lane_takes_wheel(ws) { ws.wheel.reset(); self.wheel_over_lane(ws, &delta); return; }",
        "if self.lane_takes_wheel(ws) { ws.wheel.reset(); self.wheel_over_lane( ws, &winit::event::MouseScrollDelta::LineDelta(0.0, notches as f32), ); return true; }",
        "if self.lane_takes_wheel(ws) { self.wheel_steps_over_lane(ws, steps); return true; }",
        "WindowEvent::PinchGesture { delta, .. } => { ws.resume_native_pointer(); if self.lane_takes_wheel(ws) { self.pinch_over_lane(ws, delta); } }",
        "ws.selection_autoscroll_edge = 0; // A press the lane took owns the pointer's motion until its // release: it resizes the lane or pans its item, and nothing // else sees it. if self.resize_preview_lane(ws) || self.drag_preview_lane(ws) { self.sync_cursor_icon(ws); return; }",
        "ws.selection_autoscroll_edge = 0; if self.resize_preview_lane(ws) || self.drag_preview_lane(ws) { return; }",
        "// The release of a press a lane took is the lane's. if bcode == 0 && ws.lane_resize.is_some() { self.end_lane_resize(ws); self.sync_cursor_icon(ws); return; } if bcode == 0 && let Some(drag) = ws.lane_drag.take() { self.end_lane_drag(ws, drag); self.sync_cursor_icon(ws); return; } // The release of a press a card took is the card's too.",
        "ws.held_buttons.end_own(bcode); if self.confirm_mouse_release(ws, bcode) { return true; } if bcode == 0 && ws.lane_resize.is_some() { self.end_lane_resize(ws); return true; } if bcode == 0 && let Some(drag) = ws.lane_drag.take() { self.end_lane_drag(ws, drag); return true; }",
        "Self::hide_cursor_layer(ws); // Its release may never come here. ws.lane_drag = None; self.end_lane_resize(ws);",
        // Any press ends a drag whose release went elsewhere, natively
        // and from a control client, as does a menu opening; a menu or
        // dialog open since takes the pointer from it.
        "the card's gesture is over. self.end_lane_resize(ws); ws.lane_drag = None; self.leave_preview_focus_for_press(ws); if bcode == 0 { ws.card_press = None; }",
        "// receives it. Focus leaves any card. self.end_lane_resize(ws); ws.lane_drag = None; self.leave_preview_focus_for_press(ws); if bcode == 0 { ws.card_press = None; }",
        "ws.pane_drag = None; ws.lane_drag = None; self.end_lane_resize(ws); ws.reuse_pane_snapshots_once = false;",
        // A press on the edge drags it; the PTYs follow at most every
        // interval, and once more when it ends or when a paused drag's
        // interval is up.
        "if hit == Some(kettle_render::MediaLaneHit::Resize) { ws.lane_resize = Some(crate::window_state::LaneResize {",
        "if ws.mux.drag_lane_edge(resize.pane, area, style, pointer) {",
        "if ws.lane_resize.take().is_some_and(|resize| resize.pending) { self.resize_all(ws); }",
        "let lane_resize_wait = self.flush_lane_resize(ws, now);",
        // A menu or dialog opened from the keyboard ends a drag at
        // once, and so does a side button's press.
        "if (ws.lane_drag.is_some() || ws.lane_resize.is_some()) && (ws.context_menu.is_some() || self.non_search_modal_open(ws)) { ws.lane_drag = None; self.end_lane_resize(ws); }",
        "if let Some(sgr) = extra_mouse_sgr(button) { // Any press ends a lane's drag or edge drag, as below. self.end_lane_resize(ws); ws.lane_drag = None;",
        "if let Some(next) = lane_resize_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }",
        "if ws.context_menu.is_some() || self.non_search_modal_open(ws) { ws.lane_drag = None; return false; }",
        "if hit == Some(kettle_render::MediaLaneHit::Inside) && let Some(geometry) = geometry && geometry.fit.is_some() && rect_contains(geometry.content, x, y)",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
}

/// A zoomed lane asks for sharper pixels when its view settles: any view
/// change or lane change wants them, the event loop asks once due and
/// waits for the next, as a sender of their own; a lane's ask reads no
/// file for a control client's view, a changed file or released bytes;
/// they come back to the lane only for its last ticket, never fail
/// aloud, and go with their item, lane or pane.
#[test]
fn lane_crops_are_wired() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "panel.view = view; self.want_lane_crop(ws, pane);",
        ".filter(|(_, panel)| !panel.view.is_fit()) .map(|(pane, _)| *pane) .collect(); let control = self.ctl_driving; let now = std::time::Instant::now(); for pane in zoomed { ws.lane_crops.entry(pane).or_default().again(now, control); }",
        // A finished render on another canvas asks again for a zoomed
        // lane, as the old canvas's pixels no longer serve.
        "} else if !panel.view.is_fit() { // Sharper pixels for the old canvas no longer serve: a zoomed // lane asks again. window .lane_crops .entry(render.pane) .or_default() .again(std::time::Instant::now(), control); }",
        // Another page of a gallery starts fitted, with no sharper pixels
        // of the last.
        "if paged { // Another page starts fitted, its source from the top, with // no sharper pixels of the last. panel.view = kettle_render::MediaViewport::FIT; panel.scroll = (0, 0); window.lane_crops.remove(&render.pane);",
        // A file found changed stays so with its item, not its lane.
        "// still wants these pixels. if verdict == LaneRenderVerdict::Changed { self.mark_preview_changed(ws, &render); } let Some(window) = self.window_by_seq(ws, render.window) else {",
        "// The window, its font or its splits changed what a zoomed lane // shows. self.lanes_moved(ws);",
        // A change found withdraws what still waits to read the file, and
        // a canvas change reads it no more either.
        "// What the lane asked for it and is still waiting reads nothing. self.media .withdraw_lane_renders(render.pane, render.item, render.generation);",
        "// A file found changed is read again only by a reload. if reads_a_file && item.source.changed {",
        "Err(RenderError::Failure(FailureCode::Changed)) => { self.mark_preview_changed(ws, &render); return tell(self, ws, LaneNotice::Changed); }",
        "LaneRenderVerdict::Changed => { self.mark_preview_changed(ws, &render); return tell(self, ws, LaneNotice::Changed); }",
        "// before is no longer wanted. if let Some(crop) = ws.lane_crops.get_mut(&pane) { crop.asked = None; } let Some(geometry) = self.preview_lane_geometry_of(ws, pane) else {",
        "let lane_crop_wait = self.ask_due_lane_crops(ws, now);",
        "if let Some(next) = lane_crop_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }",
        "if (reads_file && (changed || control)) || matches!(spec.input, crate::media::SourceInput::Released)",
        ".admit(crate::media::Sender::LaneTile(pane), deadline, push);",
        "if let Some(ticket) = render.tile { return self.finish_lane_crop(ws, render, ticket, spec, result); } // A gallery page the lane no longer waits on is dropped unsaid. if !self.lane_wants(ws, &render) { return; } let control = self.ctl_driving; let tell =",
        ".filter(|crop| crop.asked == Some(ticket)) else { return; }; crop.asked = None;",
        "if let Some(ticket) = render.tile { if let Some(crop) = self",
        "ws.lane_wheel.reset(); ws.lane_crops.remove(&pane);",
        "_ => { ws.lane_crops.remove(&pane); crate::window_state::PreviewPanel::new(item) }",
        "ws.lane_crops.retain(|pane, _| live.contains(pane));",
        "tile.serves(item.id, item.generation, item.source.spec.canvas)",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
}

#[test]
fn viewer_retains_keyboard_ownership_until_it_closes() {
    let source = super::production_source();
    let release = source
        .split("fn exit_preview_focus(")
        .nth(1)
        .unwrap()
        .split("\n    fn ")
        .next()
        .unwrap();
    let flat = release.split_whitespace().collect::<Vec<_>>().join(" ");
    let modal = flat
        .find("if ws.preview_overlay.is_some() { return; }")
        .expect("a background viewer must still own the keyboard");
    assert!(modal < flat.find("ws.preview_focus.take()").unwrap());
    let close = source
        .split("fn restore_overlay_origin(")
        .nth(1)
        .unwrap()
        .split("\n    fn ")
        .next()
        .unwrap();
    assert!(
        close.find("ws.preview_overlay = None;").unwrap()
            < close.find("self.exit_preview_focus(ws);").unwrap()
    );
}

#[test]
fn viewer_drag_is_not_cancelled_by_its_own_modal() {
    let source = super::production_source();
    let tick = source.split("fn about_to_wait_inner(").nth(1).unwrap();
    let flat = tick.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("if (ws.lane_drag.is_some() || ws.lane_resize.is_some()) && (ws.context_menu.is_some() || self.non_search_modal_open(ws))"));
}

#[test]
fn viewer_pin_node_cannot_be_reused_as_a_lane_collapse() {
    let mut lane = super::tests::test_lane_panel();
    lane.pin_label = Some("Pin".into());
    let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
    let pin = super::lane_controls(&geometry)
        .into_iter()
        .find(|(_, _, hit)| *hit == kettle_render::MediaLaneHit::Pin)
        .unwrap()
        .0;
    lane.pin_label = None;
    let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
    assert!(
        !super::lane_controls(&geometry)
            .iter()
            .any(|(part, _, _)| *part == pin)
    );
}

#[test]
fn viewer_keyboard_controls_include_the_video_seek_slider() {
    let mut lane = super::tests::test_lane_panel();
    lane.scrubber = Some(kettle_render::MediaLaneScrubber {
        seekable: true,
        position_ms: 0,
        duration_ms: 1000,
        label: "0:00".into(),
    });
    let geometry = kettle_render::media_lane_geometry(&lane, (8.0, 16.0), (8.0, 16.0)).unwrap();
    let controls = super::lane_interactive_controls(&lane, &geometry);
    assert!(
        controls
            .iter()
            .any(|(part, _, hit)| *part == super::LANE_SCRUBBER_PART
                && *hit == kettle_render::MediaLaneHit::Scrub)
    );
}

/// A lane given the keyboard owns every key ahead of the application's
/// shortcuts and the terminal, natively and through `dispatch_ui_key`;
/// a control client's keys and text are refused before anything is
/// written; an input method composes nothing meanwhile; and it gives
/// the keyboard back on Esc, a press elsewhere, focus loss, or whenever
/// its lane can no longer use it.
#[test]
fn a_lane_holding_the_keyboard_is_wired_as_a_modal() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview = flat
        .find("if ws.preview_focus.is_some() { self.preview_focus_key(ws, &event.logical_key, event.repeat); remember_closing_key(ws, key_modal, event.physical_key); return; }")
        .expect("the key path");
    let keybinds = flat
        .find("if let Some((trigger, act)) = resolve_keybind_action(")
        .expect("keybinds");
    assert!(preview < keybinds, "before the application's shortcuts");
    for needle in [
        "} else if ws.preview_focus.is_some() { KeyModal::Preview } else { return None; })",
        "KeyModal::Preview => TextModal::Preview,",
        "TextModal::Preview => self.preview_focus_key(ws, key, false),",
        "if self.preview_holds_keyboard(ws, pane) { return Response::err(req.id, ec::BUSY, PREVIEW_HOLDS_KEYBOARD); } let Some(p) = Self::ctl_pane_ref(ws, &self.windows, pane) else {",
        "if ws.preview_focus.is_some() && matches!( ime, winit::event::Ime::Preedit(..) | winit::event::Ime::Commit(_) ) { return; }",
        "window.set_ime_allowed(ws.preview_focus.is_none());",
        "// The keyboard goes back to the terminal. self.exit_preview_focus(ws);",
        "// A lane that can no longer use the keyboard gives it back. self.check_preview_focus(ws);",
        // The pointer, the wheel and drops still work: a lane holding
        // the keyboard is no pointer modal, and a drop gives it back.
        "fn any_modal_open(&self, ws: &WindowState) -> bool { ws.search.open || self.pointer_modal_open(ws) }",
        "// goes back to the terminal first. self.exit_preview_focus(ws);",
        // Option counts on macOS, and any modal opening takes the
        // keyboard, a confirmation too.
        "let action = crate::media::preview_key(key, held_modifiers(ws));",
        "// if it closes again before the event loop looks. self.exit_preview_focus(ws);",
        "// And the keyboard, from a preview lane holding it. self.exit_preview_focus(ws);",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
    assert_eq!(
        flat.matches("if self.preview_holds_keyboard(ws, pane) {")
            .count(),
        2,
        "send_keys and send_text"
    );
    assert_eq!(
        flat.matches("self.leave_preview_focus_for_press(ws);")
            .count(),
        3,
        "native, side button and control presses"
    );
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    let focus = body("focus_preview");
    assert!(focus.contains(".is_some_and(|geometry| geometry.full)"));
    assert!(focus.contains("end_modal_composition(ws);"));
    assert!(focus.contains("window.set_ime_allowed(false);"));
    let check = body("check_preview_focus");
    for condition in [
        "top_modal(ws) == Some(KeyModal::Preview)",
        "ws.mux.active_focus() == Some(focus.pane)",
        ".is_some_and(|panel| panel.item == focus.item)",
        ".is_some_and(|geometry| geometry.full);",
    ] {
        assert!(check.contains(condition), "{condition}");
    }
}

/// A copied or selected diagram is the user's own pull of the source
/// taken back from the agent's UI: refused for a control client before
/// the clipboard or a selection is read, rendered from bytes as
/// Mermaid, and offered in the menu, the clipboard's only on
/// Shift+right-click and neither probed for a menu a control client
/// opens. A path pull shares the same admission.
/// "Copy agent setup commands" copies the function `kettle agent-setup
/// --print` prints, for the configured shell or `SHELL`, from this
/// Kettle's own path, and says why when it cannot.
#[test]
fn copy_agent_setup_is_wired() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("Action::CopyAgentSetup => self.copy_agent_setup(),"));
    let body = flat
        .split("fn copy_agent_setup(&mut self) {")
        .nth(1)
        .and_then(|rest| rest.split(" fn ").next())
        .expect("copy_agent_setup");
    for needle in [
        ".cfg .env .iter() .rev() .find(|(name, _)| name == \"SHELL\")",
        "Some(kettle_core::default_pane_shell(configured).into())",
        "crate::codex_shell::codex_function(&kettle, shell)",
        "Err(crate::codex_shell::FunctionError::Translocated) => { return not_copied(T::NotifyBodyAgentSetupTranslocated); }",
        "&tr.notify_body_agent_setup_copied(shell.name()),",
        "if !copied { return not_copied(T::NotifyBodyAgentSetupClipboard); }",
    ] {
        assert!(body.contains(needle), "{needle}");
    }
}

#[test]
fn copied_diagrams_are_wired() {
    let src = super::production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    let preview = body("preview_copied_diagram");
    let refused = preview.find("if self.ctl_driving {").expect("refused");
    let read = preview
        .find("self.copied_diagram()")
        .expect("the clipboard read");
    let selected = preview
        .find("self.selected_diagram(ws, pane)")
        .expect("the selection read");
    assert!(refused < read && refused < selected);
    assert!(preview.contains("kettle_media::JobKind::Mermaid,"));
    assert!(preview.contains("kettle_media::Source::Bytes(first.into_bytes()),"));
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview_flat = preview.split_whitespace().collect::<Vec<_>>().join(" ");
    // Several are one gallery, refused as over budget without room;
    // more than a gallery holds are refused by name.
    for needle in [
        "E::TooMany => { return fire_notify( tr.text(T::NotifyTitlePreviewNothing), &tr.notify_body_preview_diagram_too_many( kettle_core::MAX_COPIED_DIAGRAMS as u64, ), ); }",
        "let gallery = match count { 1 => None, _ => match crate::media::Gallery::new(sources) {",
        "return notify_preview_failure(tr, Some(kettle_media::FailureCode::OverBudget));",
        "kettle_media::Canvas::Theme, gallery, );",
    ] {
        assert!(preview_flat.contains(needle), "{needle}");
    }
    assert!(body("admit_user_pull_of").contains(
        "inline: None,\n                inline_refused: None,\n                gallery,"
    ));
    for needle in [
        "Action::RenderClipboardAsDiagram => self.preview_copied_diagram(ws, false),",
        "Action::RenderSelectionAsDiagram => self.preview_copied_diagram(ws, true),",
        "action: kettle_config::Action::RenderSelectionAsDiagram,",
        "let reads = !self.ctl_driving;",
        "if reads && in_focused_pane && let Some(pane) = selection_pane && offers_diagram(&self.selected_diagram(ws, pane))",
        "if reads && in_focused_pane && offer_copied && let Some(pane) = ws.mux.active_focus() { let file = self.copied_preview(ws, pane).is_some(); let diagram = offers_diagram(&self.copied_diagram());",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
    assert!(body("pull_preview").contains("self.admit_user_pull("));
    assert_eq!(
        src.matches("GuiActionWitness::from_explicit_gui_action()")
            .count(),
        1,
        "a path pull is still the one witness site"
    );
}

/// A lane's render on another canvas shows on the item's inline cards
/// too: their posters move to the new pixels before the old go.
#[test]
fn a_rerendered_items_cards_show_its_new_pixels() {
    let src = super::production_source();
    let flat = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let finish = flat
        .split("fn finish_lane_render(")
        .nth(1)
        .and_then(|rest| rest.split(" fn ").next())
        .expect("finish_lane_render");
    let moved = finish
        .find("for nonce in cards { pane.inline_cards.set_poster(nonce, Some(&image)); }")
        .expect("posters move");
    let replaced = finish
        .find("item.pixels = crate::media::ItemPixels::Ready(image);")
        .expect("pixels replaced");
    assert!(moved < replaced);
    assert!(finish.contains(".cards_of(render.pane, (render.item, render.generation));"));
}

/// Copying copies what the lane shows: the source in source mode, else
/// the image; a diagram's canvas is rendered again, never for a file
/// while a control client's request is handled; and a lane's render is
/// taken before anything is published.
#[test]
fn lane_content_actions_are_wired() {
    let src = super::production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    let wrapper = body("copy_preview");
    assert!(wrapper.contains("lane_mode(item, panel)"));
    assert!(wrapper.contains("self.copy_preview_in_mode(ws, pane, mode);"));
    let copy = body("copy_preview_in_mode");
    assert!(copy.contains("kettle_render::MediaLaneMode::Source => item"));
    assert!(copy.contains(".map(|text| crate::media::CopyContent::Text(Arc::clone(text)))"));
    assert!(copy.contains(".map(crate::media::CopyContent::Image)"));
    let again = body("render_preview_again");
    let refused = again
        .find("if self.ctl_driving && reads_a_file")
        .expect("no file read for a control client");
    let admitted = again
        .find("self.render_for_lane(ws, render, spec, crate::window_state::LaneNotice::Rendering);")
        .expect("queued as the lane");
    assert!(refused < admitted);
    // A document's page reads its file again: never on a control client's
    // word, nor once the file is found changed.
    let page = body("page_preview");
    let refused = page
        .find("if reads_a_file && (changed || self.ctl_driving)")
        .expect("no file read for a control client or a changed file");
    let admitted = page
        .find("self.render_for_lane(ws, render, spec, notice);")
        .expect("queued as the lane");
    assert!(refused < admitted);
    assert!(
        body("render_for_lane")
            .contains(".admit(crate::media::Sender::Lane(pane), deadline, push);")
    );
    let finish = body("finish_show");
    let lane = finish
        .find("return self.finish_lane_render(ws, render, spec, result);")
        .expect("a lane's render is its own");
    let publish = finish.find("self.publish_show(").expect("publishing");
    assert!(lane < publish);
    assert!(body("publish_show").contains("media_shelf.publish("));
    let canvas = body("next_preview_canvas");
    assert!(canvas.contains(
        "if diagram {\n            self.render_preview_again(ws, pane, media_canvas(next));"
    ));
}

/// Only an item read from a file reloads; bytes a request carried, kept
/// or released, have no file to read.
#[test]
fn only_a_file_item_reloads() {
    let mut item = item(1, kettle_media::MediaKind::Svg, b"<svg/>");
    assert!(!super::preview_reloads(&item), "carried bytes");
    item.source.release();
    assert!(!super::preview_reloads(&item), "released bytes");
    item.source.spec.input = file_input();
    assert!(super::preview_reloads(&item));
}

/// A file this platform names, as an item's source.
fn file_input() -> crate::media::SourceInput {
    #[cfg(unix)]
    let bytes = b"/tmp/plot.svg".to_vec();
    #[cfg(windows)]
    let bytes = "C:\\plot.svg"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    crate::media::SourceInput::Path {
        path: kettle_media::NativePath::new(bytes).unwrap(),
        authorization: kettle_media::Authorization::ExternalAttested(
            kettle_media::ExternalAttested { dev: 1, ino: 2 },
        ),
    }
}

/// A reload reads an item's file again under its key and title, on the
/// canvas the user chose for its lane, so a file that now holds a
/// diagram is rendered for the background the lane showed. Bytes a
/// request carried have no file to read.
#[test]
fn a_reload_keeps_the_canvas_the_lane_shows() {
    use kettle_media::Canvas;
    use kettle_render::MediaCanvas as C;
    let mut svg = item(1, kettle_media::MediaKind::Svg, b"<svg/>");
    let mut panel = crate::window_state::PreviewPanel::new(1);
    assert!(super::reload_of(&svg, &panel).is_none(), "carried bytes");
    svg.source.spec.input = file_input();
    let crate::media::SourceInput::Path { path, .. } = &svg.source.spec.input else {
        unreachable!()
    };
    let (native, reload) = super::reload_of(&svg, &panel).expect("a file");
    assert_eq!(Some(native), super::native_path_buf(path));
    assert_eq!(reload.key, crate::media::ShelfKey::Path(path.clone()));
    assert_eq!(reload.title, "plot");
    assert_eq!(reload.canvas, Canvas::White, "an SVG shows on white");
    panel.canvas = Some(C::Checker);
    let (_, reload) = super::reload_of(&svg, &panel).expect("a file");
    assert_eq!(reload.canvas, Canvas::Checker, "the user's choice");
    let mut diagram = svg.clone();
    diagram.kind = kettle_media::MediaKind::Mermaid.into();
    diagram.source.spec.canvas = Canvas::White;
    let mut shown = crate::window_state::PreviewPanel::new(1);
    let (_, reload) = super::reload_of(&diagram, &shown).expect("a file");
    assert_eq!(reload.canvas, Canvas::White, "the diagram's own");
    // Chosen while a render for it has yet to come back.
    shown.canvas = Some(C::Checker);
    let (_, reload) = super::reload_of(&diagram, &shown).expect("a file");
    assert_eq!(reload.canvas, Canvas::Checker, "the user's choice");
}

/// A native path is the path it names on this platform.
#[cfg(unix)]
#[test]
fn a_native_path_names_its_path() {
    let native = kettle_media::NativePath::new(b"/tmp/my plot\xff.png".to_vec()).unwrap();
    assert_eq!(
        super::native_path_buf(&native),
        Some(std::path::PathBuf::from(
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"/tmp/my plot\xff.png")
        ))
    );
}

/// Reload reads an item's file again as the user's own pull, under the
/// item's key, title and canvas, only for an item read from a file, and
/// what was said about the item goes once it shows again.
#[test]
fn reload_is_a_pull_of_the_items_file_under_its_key() {
    let src = super::production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    let reload = body("reload_preview");
    assert!(reload.contains(".and_then(|(item, panel)| reload_of(item, panel))"));
    assert!(reload.contains("self.pull_preview(ws, pane, &native, Some(reload));"));
    let pull = body("pull_preview");
    let refused = pull
        .find("if self.ctl_driving {")
        .expect("no read for a control client");
    let kept = pull
        .find("Some(item) => (item.key, item.title, item.canvas, item.page),")
        .expect("a reload keeps its item's key, title, canvas and page");
    assert!(refused < kept);
    // A reload reads a gallery or a PDF at the page shown.
    assert!(pull.contains("crate::media::ReadPage::job_kind"));
    assert!(
        body("show_media_item")
            .contains("notice: None,\n                page: None,\n                ..*panel")
    );
    assert_eq!(
        super::lane_action_hit(&kettle_config::Action::PreviewReload),
        Some(kettle_render::MediaLaneHit::Reload)
    );
}

/// A lane shows source only while its item has a source: one replaced by
/// a raster under the same key shows rendered, and copies the image.
#[test]
fn a_lane_without_a_source_shows_and_copies_rendered() {
    use kettle_render::MediaLaneMode as Mode;
    let mut panel = crate::window_state::PreviewPanel::new(1);
    panel.mode = Mode::Source;
    let svg = item(1, kettle_media::MediaKind::Svg, b"<svg/>");
    assert_eq!(super::lane_mode(&svg, &panel), Mode::Source);
    let mut raster = svg.clone();
    raster.kind = kettle_media::MediaKind::Raster.into();
    raster.source.release();
    assert_eq!(super::lane_mode(&raster, &panel), Mode::Rendered);
    let src = super::production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert!(body("copy_preview").contains("lane_mode(item, panel)"));
    assert!(body("copy_preview").contains("self.copy_preview_in_mode(ws, pane, mode);"));
    assert!(body("copy_preview_in_mode").contains("let content = match mode {"));
    let scroll = body("scroll_preview_lane");
    assert!(scroll.contains("lane_mode(item, panel) == kettle_render::MediaLaneMode::Source"));
    assert!(scroll.contains("source_scroll_by(steps, ws.mods.shift_key());"));
    // A menu or dialog over a lane takes the wheel.
    let gate = body("lane_takes_wheel");
    assert!(gate.contains("ws.context_menu.is_none()"));
    assert!(gate.contains("!self.non_search_modal_open(ws)"));
}

/// A file shown as itself: its lane says what it is and how large, and why
/// no picture is drawn; it draws on the theme's canvas, and offers no copy.
#[test]
fn a_file_lane_says_what_the_file_is() {
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let facts = crate::media::FileFacts {
        bytes: 38_200_000,
        identity: kettle_media::PathIdentity {
            dev: 1,
            ino: 2,
            size: 38_200_000,
            mtime_seconds: 3,
            mtime_nanos: 4,
        },
        why: crate::media::FileWhy::TooLarge,
    };
    let item = crate::media::ShelfItem::file(
        3,
        None,
        "scan.tiff".into(),
        crate::media::Provenance::User,
        facts,
        crate::media::ItemSource::undrawn(spec(file(b"/tmp/scan.tiff")), facts.identity),
    );
    assert_eq!(super::media_item_detail(tr, "File", &item), "File · 38 MB");
    let panel = crate::window_state::PreviewPanel::new(3);
    assert_eq!(
        preview_canvas(&item, &panel),
        kettle_render::MediaCanvas::Theme
    );
    assert!(item.image().is_none());
    assert_eq!(item.source.reload_file().map(|(_, page)| page), Some(None));
}

/// A render that fails as a file Kettle does not draw puts the file on the
/// shelf, read from its path and checked against what was asked for; any
/// other failure, or a failure of bytes, stays the failure it was. Both
/// kinds of item go through one door to the shelf, the cards and the reply.
#[test]
fn an_undrawable_file_goes_on_the_shelf_as_itself() {
    let source = kettle_test_support::production_source(&super::app_source());
    let code = kettle_test_support::code_only(&source);
    let flat = code.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = |name: &str| {
        flat.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split(" fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_owned()
    };
    let finish = body("finish_show");
    let steps = [
        "Err(RenderError::Failure(failure)) => { let why = crate::media::FileWhy::of_job(&spec.kind, failure); let facts = why.and_then(|why| match &spec.input { crate::media::SourceInput::Path { path, authorization, } => super::native_path_buf(path) .and_then(|path| crate::media::file_facts(&path, authorization, why)), _ => None, });",
        "let Some(facts) = facts else { return notify_preview_failure(tr, origin.refuse(failure)); };",
        "let item = crate::media::ShelfItem::file( self.media.next_item(), key, title, provenance, facts, crate::media::ItemSource::undrawn(spec, facts.identity), );",
        "return self.publish_show( ws, (origin, route), item, (inline, inline_refused), None, );",
        "self.publish_show(ws, (origin, route), item, (inline, inline_refused), page);",
    ];
    let at: Vec<_> = steps
        .iter()
        .map(|step| finish.find(step).unwrap_or_else(|| panic!("{step}")))
        .collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
    let publish = body("publish_show");
    for needle in [
        ".map(|item| (item.generation, item.card_image()));",
        "crate::media::ItemKind::File(facts) => kettle_ctl::show::ShowResult::file( target, published.id, facts.bytes, facts.why.as_str(), ),",
    ] {
        assert!(publish.contains(needle), "publish_show: {needle}");
    }
}
