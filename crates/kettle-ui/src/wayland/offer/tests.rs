use super::*;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const UTF8: &str = "text/plain;charset=utf-8";
const ALL: PasteWant = PasteWant {
    files: true,
    images: true,
};
const FILES_OFF: PasteWant = PasteWant {
    files: false,
    images: true,
};
const TEXT_MAX: usize = 1024;

/// What the loopback source does when asked for a type.
#[derive(Clone)]
enum Answer {
    /// Write the bytes and close.
    Bytes(Vec<u8>),
    /// Write the bytes, then keep the pipe open and say no more.
    Stall(Vec<u8>),
    /// Write this many bytes and close.
    Fill(usize),
    /// Write without end, as fast as the reader takes it.
    Endless,
}

/// A selection offer whose source is a thread in this process: no compositor
/// and no real clipboard. It records every type it is asked for.
#[derive(Clone, Default)]
struct Loopback(Arc<Source>);

#[derive(Default)]
struct Source {
    answers: Vec<(&'static str, Answer)>,
    asked: Mutex<Vec<&'static str>>,
    stalled: Mutex<Vec<UnixStream>>,
    discarded: AtomicBool,
}

impl Loopback {
    fn new(answers: &[(&'static str, Answer)]) -> Self {
        Self(Arc::new(Source {
            answers: answers.to_vec(),
            ..Source::default()
        }))
    }

    fn kinds(&self) -> Kinds {
        let mut kinds = Kinds::default();
        for (mime, _) in &self.0.answers {
            kinds.note(mime);
        }
        kinds
    }

    fn held(&self) -> Held<Self> {
        Held::Offer(self.clone(), self.kinds())
    }

    fn asked(&self) -> Vec<&'static str> {
        self.0.asked.lock().unwrap().clone()
    }
}

impl Offered for Loopback {
    fn receive(&self, mime: &'static str, fd: BorrowedFd<'_>) {
        self.0.asked.lock().unwrap().push(mime);
        let mut pipe = UnixStream::from(fd.try_clone_to_owned().unwrap());
        let answer = self
            .0
            .answers
            .iter()
            .find(|(offered, _)| *offered == mime)
            .map(|(_, answer)| answer.clone());
        match answer {
            Some(Answer::Bytes(bytes)) => {
                std::thread::spawn(move || {
                    let _ = pipe.write_all(&bytes);
                });
            }
            Some(Answer::Stall(bytes)) => {
                pipe.write_all(&bytes).unwrap();
                self.0.stalled.lock().unwrap().push(pipe);
            }
            Some(Answer::Fill(mut left)) => {
                std::thread::spawn(move || {
                    let chunk = [0x42u8; 64 * 1024];
                    while left > 0 && pipe.write_all(&chunk[..left.min(chunk.len())]).is_ok() {
                        left -= left.min(chunk.len());
                    }
                });
            }
            Some(Answer::Endless) => {
                std::thread::spawn(move || {
                    let chunk = [0x55u8; 64 * 1024];
                    while pipe.write_all(&chunk).is_ok() {}
                });
            }
            // A type the source never offered: it closes the pipe unwritten.
            None => {}
        }
    }

    fn discard(self) {
        self.0.discarded.store(true, Ordering::Release);
    }
}

struct Sent(Arc<AtomicBool>);

impl Owned for Sent {
    fn discard(self) {
        self.0.store(true, Ordering::Release);
    }
}

fn within(time: Duration) -> Instant {
    Instant::now() + time
}

fn run<T>(deadline: Instant, read: impl FnOnce(&Transfer<'_>) -> T) -> T {
    read(&Transfer {
        flush: &|| Ok(()),
        deadline,
    })
}

fn paste_from(offer: &Loopback, want: PasteWant) -> PasteContent {
    run(within(Duration::from_secs(10)), |transfer| {
        paste(offer.held(), want, TEXT_MAX, transfer)
    })
}

/// What a source offers, what a paste of it gives and the types it reads.
type Case = (
    &'static str,
    Vec<(&'static str, Answer)>,
    &'static str,
    &'static [&'static str],
);

fn error_name(error: &arboard::Error) -> &'static str {
    match error {
        arboard::Error::ContentNotAvailable => "ContentNotAvailable",
        arboard::Error::Timeout => "Timeout",
        arboard::Error::LimitExceeded => "LimitExceeded",
        _ => "another error",
    }
}

/// A paste's outcome, for comparing.
fn outcome(content: PasteContent) -> String {
    match content {
        PasteContent::Files(paths) => format!("files {paths:?}"),
        PasteContent::Image(PasteImage::Png(png)) => format!("png {png:?}"),
        PasteContent::Image(PasteImage::Rgba(_)) => "pixels".into(),
        PasteContent::Text(Ok(text)) => format!("text {text:?}"),
        PasteContent::Text(Err(error)) => format!("no text: {}", error_name(&error)),
        PasteContent::FilesOff => "files off".into(),
        PasteContent::Refused(refusal) => format!("refused {refusal:?}"),
    }
}

fn bytes(text: &str) -> Answer {
    Answer::Bytes(text.as_bytes().to_vec())
}

#[test]
fn kinds_flag_what_a_paste_reads_and_rank_text_utf8_first() {
    let mut kinds = Kinds::default();
    assert!(!kinds.any());
    for mime in ["application/x-other", "image/jpeg", "text/html", ""] {
        kinds.note(mime);
    }
    assert_eq!(kinds, Kinds::default(), "types Kettle does not read");
    kinds.note("STRING");
    assert_eq!(kinds.text, Some("STRING"));
    kinds.note("text/plain");
    assert_eq!(kinds.text, Some("text/plain"));
    kinds.note(UTF8);
    kinds.note("UTF8_STRING");
    assert_eq!(kinds.text, Some(UTF8));
    kinds.note(URI_LIST);
    kinds.note(GNOME_COPIED_FILES);
    kinds.note(PNG);
    assert_eq!(
        kinds,
        Kinds {
            text: Some(UTF8),
            uri_list: true,
            gnome_copied_files: true,
            png: true,
        }
    );
    assert_eq!(
        kinds.text_only(),
        Kinds {
            text: Some(UTF8),
            ..Kinds::default()
        }
    );
    for only in [URI_LIST, GNOME_COPIED_FILES, PNG] {
        let mut kinds = Kinds::default();
        kinds.note(only);
        assert!(kinds.any(), "{only} alone is a selection worth keeping");
    }
}

#[test]
fn a_paste_negotiates_from_the_offer_and_reads_each_type_once() {
    let png = Answer::Bytes(vec![7, 8, 9]);
    let local = bytes("file:///tmp/a%20b.png\r\nfile:///tmp/c\r\n");
    let gnome = bytes("copy\nfile:///tmp/a%20b.png\nfile:///tmp/c");
    let paths = "files [\"/tmp/a b.png\", \"/tmp/c\"]";
    let cases: &[Case] = &[
        (
            "text and an image: the text",
            vec![(UTF8, bytes("héllo")), (PNG, png.clone())],
            "text \"héllo\"",
            &[UTF8],
        ),
        (
            "an image alone: the image",
            vec![(PNG, png.clone())],
            "png [7, 8, 9]",
            &[PNG],
        ),
        (
            "local files with the text copy a file manager adds: the files",
            vec![
                (URI_LIST, local.clone()),
                (UTF8, bytes("/tmp/a b.png\n/tmp/c")),
            ],
            paths,
            &[URI_LIST],
        ),
        (
            "local files and an image of the first: the files",
            vec![(URI_LIST, local.clone()), (PNG, png.clone())],
            paths,
            &[URI_LIST],
        ),
        (
            "only gnome-copied-files: the files",
            vec![
                (GNOME_COPIED_FILES, gnome.clone()),
                ("text/plain", bytes("x")),
            ],
            paths,
            &[GNOME_COPIED_FILES],
        ),
        (
            "both file types: the URI list is the one read",
            vec![
                (GNOME_COPIED_FILES, bytes("copy\nfile:///other")),
                (URI_LIST, local.clone()),
            ],
            paths,
            &[URI_LIST],
        ),
        (
            "a link: its text",
            vec![
                (URI_LIST, bytes("https://example.invalid/a\r\n")),
                (UTF8, bytes("https://example.invalid/a")),
            ],
            "text \"https://example.invalid/a\"",
            &[URI_LIST, UTF8],
        ),
        (
            "a remote file: its text",
            vec![
                (URI_LIST, bytes("sftp://host.invalid/srv/a\r\n")),
                ("STRING", bytes("sftp://host.invalid/srv/a")),
            ],
            "text \"sftp://host.invalid/srv/a\"",
            &[URI_LIST, "STRING"],
        ),
        (
            "a remote file and an image, no text: the image",
            vec![
                (URI_LIST, bytes("smb://server.invalid/a\r\n")),
                (PNG, png.clone()),
            ],
            "png [7, 8, 9]",
            &[URI_LIST, PNG],
        ),
        (
            "an empty file list: the text",
            vec![(URI_LIST, bytes("# none\r\n")), (UTF8, bytes("t"))],
            "text \"t\"",
            &[URI_LIST, UTF8],
        ),
        (
            "a malformed file list: refused, and its text is not read",
            vec![
                (URI_LIST, bytes("file:///tmp/a%GG\r\n")),
                (UTF8, bytes("/tmp/a%GG")),
            ],
            "refused Files",
            &[URI_LIST],
        ),
        (
            "a gnome list without its verb line: refused",
            vec![
                (GNOME_COPIED_FILES, bytes("file:///tmp/a")),
                (UTF8, bytes("t")),
            ],
            "refused Files",
            &[GNOME_COPIED_FILES],
        ),
        (
            "an image that arrives empty: refused",
            vec![(PNG, Answer::Bytes(Vec::new()))],
            "refused Image",
            &[PNG],
        ),
        (
            "text that is not UTF-8 is replaced, not refused",
            vec![("TEXT", Answer::Bytes(vec![b'a', 0xff, b'b']))],
            "text \"a\u{fffd}b\"",
            &["TEXT"],
        ),
    ];
    for (name, answers, expected, asked) in cases {
        let offer = Loopback::new(answers);
        assert_eq!(outcome(paste_from(&offer, ALL)), *expected, "{name}");
        assert_eq!(offer.asked(), *asked, "{name}: types read");
    }

    let image_only = Loopback::new(&[(PNG, png)]);
    let no_images = PasteWant {
        files: true,
        images: false,
    };
    assert_eq!(
        outcome(paste_from(&image_only, no_images)),
        "no text: ContentNotAvailable",
        "paste-images = off"
    );
    assert!(
        image_only.asked().is_empty(),
        "the image is not read at all"
    );

    let empty = run(within(Duration::from_secs(1)), |transfer| {
        paste(Held::<Loopback>::Empty, ALL, TEXT_MAX, transfer)
    });
    assert_eq!(outcome(empty), "no text: ContentNotAvailable");
}

#[test]
fn paste_files_off_is_not_bypassed_by_the_text_copy_of_the_paths() {
    let local = bytes("file:///home/u/secret%20plan.txt\r\n");
    let text = bytes("/home/u/secret plan.txt");
    for (name, answers, asked) in [
        (
            "GTK: a URI list and the raw paths as text",
            vec![(URI_LIST, local.clone()), (UTF8, text.clone())],
            URI_LIST,
        ),
        (
            "Nautilus: gnome-copied-files and text",
            vec![
                (
                    GNOME_COPIED_FILES,
                    bytes("cut\nfile:///home/u/secret%20plan.txt"),
                ),
                ("text/plain", text.clone()),
            ],
            GNOME_COPIED_FILES,
        ),
        (
            "a local file among remote ones",
            vec![
                (
                    URI_LIST,
                    bytes("sftp://host.invalid/a\r\nfile:///home/u/b\r\n"),
                ),
                (UTF8, text.clone()),
            ],
            URI_LIST,
        ),
        (
            "a copied image file with its picture",
            vec![(URI_LIST, local.clone()), (PNG, Answer::Bytes(vec![1]))],
            URI_LIST,
        ),
    ] {
        let offer = Loopback::new(&answers);
        assert_eq!(
            outcome(paste_from(&offer, FILES_OFF)),
            "files off",
            "{name}"
        );
        assert_eq!(
            offer.asked(),
            [asked],
            "{name}: neither text nor image is read"
        );
    }

    // A link is not a copied file: its text pastes whatever `paste-files` is.
    let link = Loopback::new(&[
        (URI_LIST, bytes("https://example.invalid/a\r\n")),
        (UTF8, bytes("https://example.invalid/a")),
    ]);
    assert_eq!(
        outcome(paste_from(&link, FILES_OFF)),
        "text \"https://example.invalid/a\""
    );
    // Ordinary text is untouched by the policy.
    let plain = Loopback::new(&[(UTF8, bytes("/home/u/typed by hand"))]);
    assert_eq!(
        outcome(paste_from(&plain, FILES_OFF)),
        "text \"/home/u/typed by hand\""
    );
}

#[test]
fn paste_files_off_says_nothing_about_a_list_it_would_not_have_pasted() {
    let text = bytes("/home/u/secret plan.txt");
    let many = "file:///tmp/a\r\n".repeat(33);
    for (name, list) in [
        ("a bad path", bytes("file:///a/../b\r\n")),
        (
            "a bad escape",
            bytes("file:///home/u/secret%GGplan.txt\r\n"),
        ),
        ("more than 32 files", bytes(&many)),
        ("over 1 MiB", Answer::Fill(uri_list::MAX_BYTES + 1)),
        ("a list that never ends", Answer::Endless),
    ] {
        // With file paste on the user is told why nothing came.
        let on = Loopback::new(&[(URI_LIST, list.clone()), (UTF8, text.clone())]);
        assert_eq!(outcome(paste_from(&on, ALL)), "refused Files", "{name}");
        // With it off there is no notice to act on: nothing is pasted, not
        // even the text copy of the paths, and nothing is said.
        let off = Loopback::new(&[(URI_LIST, list), (UTF8, text.clone())]);
        assert_eq!(outcome(paste_from(&off, FILES_OFF)), "files off", "{name}");
        assert_eq!(off.asked(), [URI_LIST], "{name}: the text is not read");
    }
    // A list that stalls is a transfer that timed out, whatever the policy.
    let stalled = Loopback::new(&[(URI_LIST, Answer::Stall(Vec::new())), (UTF8, text)]);
    let content = run(within(Duration::from_millis(80)), |transfer| {
        paste(stalled.held(), FILES_OFF, TEXT_MAX, transfer)
    });
    assert_eq!(outcome(content), "no text: Timeout");
}

#[test]
fn each_read_stops_at_its_cap_even_when_the_source_never_ends() {
    // The deadline is far off: it is the cap that ends each of these.
    let started = Instant::now();
    let image = Loopback::new(&[(PNG, Answer::Endless)]);
    assert_eq!(outcome(paste_from(&image, ALL)), "refused Image");
    let files = Loopback::new(&[(URI_LIST, Answer::Endless), (UTF8, bytes("t"))]);
    assert_eq!(outcome(paste_from(&files, ALL)), "refused Files");
    assert_eq!(files.asked(), [URI_LIST]);
    let text = Loopback::new(&[(UTF8, Answer::Endless)]);
    assert_eq!(outcome(paste_from(&text, ALL)), "no text: LimitExceeded");
    assert!(started.elapsed() < Duration::from_secs(10));

    // Exactly at a cap is taken; one byte more is refused.
    let at = |len: usize| {
        let offer = Loopback::new(&[(UTF8, Answer::Bytes(vec![b'a'; len]))]);
        paste_from(&offer, ALL)
    };
    assert!(matches!(at(TEXT_MAX), PasteContent::Text(Ok(text)) if text.len() == TEXT_MAX));
    assert_eq!(outcome(at(TEXT_MAX + 1)), "no text: LimitExceeded");
    let list = |len: usize| {
        let mut body = b"file:///tmp/a\n#".to_vec();
        body.resize(len, b'#');
        paste_from(&Loopback::new(&[(URI_LIST, Answer::Bytes(body))]), ALL)
    };
    assert_eq!(outcome(list(uri_list::MAX_BYTES)), "files [\"/tmp/a\"]");
    assert_eq!(outcome(list(uri_list::MAX_BYTES + 1)), "refused Files");
}

#[test]
fn an_image_is_read_up_to_64_mib_and_refused_one_byte_over() {
    assert_eq!(crate::paste_image::PNG_SOURCE_MAX, 64 << 20);
    let sized = |len: usize| {
        let offer = Loopback::new(&[(PNG, Answer::Fill(len))]);
        run(within(Duration::from_secs(60)), |transfer| {
            paste(offer.held(), ALL, TEXT_MAX, transfer)
        })
    };
    let at_the_cap = sized(crate::paste_image::PNG_SOURCE_MAX);
    assert!(
        matches!(&at_the_cap, PasteContent::Image(PasteImage::Png(png))
            if png.len() == crate::paste_image::PNG_SOURCE_MAX),
        "an image at the cap is taken"
    );
    drop(at_the_cap);
    assert!(matches!(
        sized(crate::paste_image::PNG_SOURCE_MAX + 1),
        PasteContent::Refused(PasteRefusal::Image)
    ));
}

#[test]
fn a_stalled_source_costs_the_deadline_and_no_more() {
    for (name, answers) in [
        (
            "a file list never written",
            vec![(URI_LIST, Answer::Stall(Vec::new())), (UTF8, bytes("t"))],
        ),
        (
            "a file list cut off",
            vec![(URI_LIST, Answer::Stall(b"file:///tmp/a".to_vec()))],
        ),
        (
            "text never written",
            vec![(UTF8, Answer::Stall(Vec::new()))],
        ),
        (
            "text cut off",
            vec![(UTF8, Answer::Stall(b"half a comm".to_vec()))],
        ),
        (
            "an image never written",
            vec![(PNG, Answer::Stall(Vec::new()))],
        ),
        (
            "an image cut off",
            vec![(PNG, Answer::Stall(b"\x89PNG".to_vec()))],
        ),
    ] {
        let offer = Loopback::new(&answers);
        let started = Instant::now();
        let content = run(within(Duration::from_millis(80)), |transfer| {
            paste(offer.held(), ALL, TEXT_MAX, transfer)
        });
        let waited = started.elapsed();
        assert_eq!(outcome(content), "no text: Timeout", "{name}");
        assert!(
            waited >= Duration::from_millis(80),
            "{name}: waited {waited:?}"
        );
        assert!(waited < Duration::from_secs(2), "{name}: waited {waited:?}");
        assert_eq!(
            offer.asked().len(),
            1,
            "{name}: nothing else is tried after a timeout"
        );
    }

    // One deadline covers every read of a paste: a list that takes most of
    // it leaves the text only what remains.
    let slow = Loopback::new(&[
        (URI_LIST, bytes("https://example.invalid/\r\n")),
        (UTF8, Answer::Stall(Vec::new())),
    ]);
    let started = Instant::now();
    let content = run(within(Duration::from_millis(120)), |transfer| {
        paste(slow.held(), ALL, TEXT_MAX, transfer)
    });
    assert_eq!(outcome(content), "no text: Timeout");
    assert!(started.elapsed() < Duration::from_secs(2));

    // A deadline already past reads nothing.
    let late = Loopback::new(&[(UTF8, bytes("t"))]);
    let content = run(Instant::now(), |transfer| {
        paste(late.held(), ALL, TEXT_MAX, transfer)
    });
    assert_eq!(outcome(content), "no text: Timeout");
}

#[test]
fn a_flush_that_fails_reads_nothing() {
    let offer = Loopback::new(&[(UTF8, bytes("t"))]);
    let content = paste(
        offer.held(),
        ALL,
        TEXT_MAX,
        &Transfer {
            flush: &|| Err(Error::new(ErrorKind::NotConnected, "no Wayland bridge")),
            deadline: within(Duration::from_secs(1)),
        },
    );
    assert_eq!(outcome(content), "no text: ContentNotAvailable");
}

#[test]
fn the_files_a_preview_names_are_local_ones_only() {
    let files = |answers: &[(&'static str, Answer)]| {
        let offer = Loopback::new(answers);
        run(within(Duration::from_secs(10)), |transfer| {
            local_files(offer.held(), transfer)
        })
    };
    assert_eq!(
        files(&[(URI_LIST, bytes("file:///tmp/a%20b\r\n"))]),
        Some(vec![PathBuf::from("/tmp/a b")])
    );
    assert_eq!(
        files(&[(GNOME_COPIED_FILES, bytes("copy\nfile:///tmp/c"))]),
        Some(vec![PathBuf::from("/tmp/c")])
    );
    assert_eq!(
        files(&[(URI_LIST, bytes("https://example.invalid/\r\n"))]),
        None
    );
    assert_eq!(files(&[(URI_LIST, bytes("file:///a%GG\r\n"))]), None);
    let text = Loopback::new(&[(UTF8, bytes("/tmp/a"))]);
    let named = run(within(Duration::from_secs(1)), |transfer| {
        local_files(text.held(), transfer)
    });
    assert_eq!(named, None);
    assert!(
        text.asked().is_empty(),
        "text is not read for a file preview"
    );
}

/// PRIMARY and CLIPBOARD are two channels of the same shape.
type Selection = Channel<Loopback, Sent>;

fn text_of(channel: &Selection, max: usize) -> Result<String> {
    run(within(Duration::from_secs(10)), |transfer| {
        load_text(channel.held(), max, transfer)
    })
}

#[test]
fn a_primary_selection_is_read_from_its_offer_as_text_only() {
    let mut primary = Selection::default();
    assert_eq!(
        text_of(&primary, 64).unwrap_err().kind(),
        ErrorKind::NotFound
    );

    // Whatever else a source lists on PRIMARY, only its text is kept.
    let offer = Loopback::new(&[
        ("STRING", bytes("legacy")),
        (UTF8, bytes("selected text")),
        (URI_LIST, bytes("file:///tmp/a\r\n")),
        (PNG, Answer::Bytes(vec![1])),
    ]);
    primary.set_offer(Some((offer.clone(), offer.kinds().text_only())));
    assert_eq!(text_of(&primary, 64).unwrap(), "selected text");
    assert_eq!(offer.asked(), [UTF8]);
    assert_eq!(
        text_of(&primary, 4).unwrap_err().kind(),
        ErrorKind::FileTooLarge
    );
    let content = run(within(Duration::from_secs(10)), |transfer| {
        paste(primary.held(), ALL, 64, transfer)
    });
    assert_eq!(outcome(content), "text \"selected text\"");
    assert!(!offer.asked().contains(&URI_LIST) && !offer.asked().contains(&PNG));

    // A new selection replaces the offer and lets the old one go.
    let next = Loopback::new(&[("text/plain", bytes("next"))]);
    primary.set_offer(Some((next.clone(), next.kinds().text_only())));
    assert!(offer.0.discarded.load(Ordering::Acquire));
    assert_eq!(text_of(&primary, 64).unwrap(), "next");
    primary.set_offer(None);
    assert!(next.0.discarded.load(Ordering::Acquire));
    assert_eq!(
        text_of(&primary, 64).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn a_selection_this_client_wrote_is_answered_from_memory_until_it_is_replaced() {
    let mut primary = Selection::default();
    let mut clipboard = Selection::default();
    let offer = Loopback::new(&[(UTF8, bytes("from another client"))]);
    primary.set_offer(Some((offer.clone(), offer.kinds())));

    let gone: Vec<_> = (0..6).map(|_| Arc::new(AtomicBool::new(false))).collect();
    primary.own(1, Sent(gone[1].clone()), Arc::from("selected in Kettle"));
    assert_eq!(text_of(&primary, 64).unwrap(), "selected in Kettle");
    assert!(
        offer.asked().is_empty(),
        "no read of the pipe for our own text"
    );
    assert_eq!(
        text_of(&primary, 4).unwrap_err().kind(),
        ErrorKind::FileTooLarge
    );
    assert_eq!(primary.text_for(1).as_deref(), Some("selected in Kettle"));
    assert_eq!(primary.text_for(2), None);
    assert!(
        matches!(clipboard.held(), Held::Empty),
        "writing PRIMARY leaves CLIPBOARD alone"
    );
    clipboard.own(9, Sent(gone[0].clone()), Arc::from("copied"));
    assert_eq!(text_of(&clipboard, 64).unwrap(), "copied");
    assert_eq!(text_of(&primary, 64).unwrap(), "selected in Kettle");

    // Another client takes the selection: the compositor cancels our source
    // and its offer is what a paste reads again.
    primary.cancelled(1);
    assert!(gone[1].load(Ordering::Acquire));
    assert_eq!(text_of(&primary, 64).unwrap(), "from another client");
    primary.cancelled(1);

    // Sources wait for late `send` requests, a bounded few of them.
    for id in 2..=5 {
        primary.own(
            id,
            Sent(gone[id as usize].clone()),
            Arc::from(format!("text {id}")),
        );
    }
    assert_eq!(text_of(&primary, 64).unwrap(), "text 5");
    assert_eq!(primary.text_for(2).as_deref(), Some("text 2"));
    primary.own(6, Sent(gone[0].clone()), Arc::from("text 6"));
    assert!(
        gone[2].load(Ordering::Acquire),
        "the oldest source is let go"
    );
    assert_eq!(primary.text_for(2), None);
    // Cancelling an older source keeps the newest one's ownership.
    primary.cancelled(3);
    assert_eq!(text_of(&primary, 64).unwrap(), "text 6");

    primary.close();
    assert!(gone[4].load(Ordering::Acquire) && gone[5].load(Ordering::Acquire));
    assert!(offer.0.discarded.load(Ordering::Acquire));
    assert!(matches!(primary.held(), Held::Empty));
}

#[test]
fn bounded_reads_stop_at_the_cap_the_deadline_and_eof() {
    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all("héllo".as_bytes()).unwrap();
    drop(writer);
    let deadline = within(Duration::from_secs(1));
    assert_eq!(
        read_bounded(&mut reader, 6, deadline).unwrap(),
        "héllo".as_bytes()
    );

    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"0123456789").unwrap();
    drop(writer);
    let error = read_bounded(&mut reader, 4, deadline).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::FileTooLarge);

    let (mut reader, _writer) = UnixStream::pair().unwrap();
    assert_eq!(
        read_bounded(&mut reader, 4, within(Duration::from_millis(50)))
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}
