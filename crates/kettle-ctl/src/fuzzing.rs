//! The entry point for fuzzing what the control server and the activation
//! endpoint read from a peer, and the records other processes leave in the
//! registry and presence directories. [`ctl`] takes any bytes and must
//! return without panicking; what it parses must also read back as itself.
//! The fuzz target (feature `fuzzing`) and the in-process sweep below call
//! the same function.

use serde_json::Value;

use crate::activation::ActivationRequest;
use crate::discovery::RegistryEntry;
use crate::presence::PresenceEntry;
use crate::protocol::{self, MAX_PAGE_ITEMS, PageRequest};
use crate::show::ShowRequest;

/// Parse `data` past its first byte as what that byte picks: a request
/// stream, paging params, `show` params, an activation request, or a
/// registry or presence record.
pub fn ctl(data: &[u8]) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    match pick % 5 {
        0 => request_stream(bytes, usize::from(pick / 5) + 1),
        1 => {
            if let Ok(params) = serde_json::from_slice::<Value>(bytes) {
                page(&params);
            }
        }
        2 => {
            if let Ok(params) = serde_json::from_slice::<Value>(bytes) {
                show(params);
            }
        }
        3 => {
            if let Ok(request) = serde_json::from_slice::<ActivationRequest>(bytes) {
                activation(&request);
            }
        }
        _ => {
            if let Ok(entry) = serde_json::from_slice::<PresenceEntry>(bytes) {
                let _ = entry.is_valid();
            }
            if let Ok(entry) = serde_json::from_slice::<RegistryEntry>(bytes) {
                let written = serde_json::to_vec(&entry).expect("a registry entry serializes");
                assert_eq!(
                    serde_json::from_slice::<RegistryEntry>(&written).ok(),
                    Some(entry)
                );
            }
        }
    }
}

/// Read `bytes` as a peer's request stream arriving `piece` bytes at a
/// time: each line is found where a whole read finds it, and a request
/// that parses writes back as one that parses the same.
fn request_stream(bytes: &[u8], piece: usize) {
    let mut whole = 0;
    let expected = protocol::find_newline(bytes, &mut whole);
    let mut buffer = Vec::new();
    let mut scanned = 0;
    let mut found = None;
    for part in bytes.chunks(piece) {
        buffer.extend_from_slice(part);
        found = protocol::find_newline(&buffer, &mut scanned);
        if found.is_some() {
            break;
        }
        assert_eq!(scanned, buffer.len(), "a scan stopped short of the end");
    }
    assert_eq!(found, expected, "reading in pieces found another newline");
    for line in bytes.split(|&byte| byte == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Ok(request) = protocol::parse_request_line(line) else {
            continue;
        };
        let written = serde_json::to_string(&request).expect("a request serializes");
        let again = protocol::parse_request_line(&written).expect("a written request parses");
        assert_eq!(
            (
                again.v,
                again.id,
                &again.method,
                &again.params,
                again.caller
            ),
            (
                request.v,
                request.id,
                &request.method,
                &request.params,
                request.caller
            )
        );
    }
}

/// Paging params that parse page within the most items a page may hold.
fn page(params: &Value) {
    let Ok(request) = PageRequest::from_params(params) else {
        return;
    };
    assert!((1..=MAX_PAGE_ITEMS).contains(&request.limit));
    assert!(request.offset == 0 || request.snapshot.is_some());
    for total in [
        0,
        1,
        request.limit,
        request.offset,
        request
            .offset
            .saturating_add(request.limit)
            .saturating_add(1),
        usize::MAX,
    ] {
        let (start, end, next, truncated) = request.bounds(total);
        assert!(start <= end && end <= total && end - start <= request.limit);
        assert_eq!(truncated, end < total);
        assert_eq!(next, truncated.then(|| end.to_string()));
    }
}

/// A `show` request that parses writes back as params that parse to it.
fn show(params: Value) {
    let Ok(request) = ShowRequest::parse(params) else {
        return;
    };
    let written = request
        .clone()
        .into_params()
        .expect("a parsed request writes back");
    assert_eq!(ShowRequest::parse(written).as_ref(), Ok(&request));
}

/// A valid activation request writes back as the same valid request.
fn activation(request: &ActivationRequest) {
    if !request.is_valid() {
        return;
    }
    let written = serde_json::to_vec(request).expect("a request serializes");
    let again: ActivationRequest =
        serde_json::from_slice(&written).expect("a written request parses");
    assert!(again.is_valid());
    assert_eq!(&again, request);
}

#[cfg(test)]
mod tests {
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    #[test]
    fn mutated_peer_input_parses_back_as_itself() {
        let inputs: Vec<(u8, &[u8])> = vec![
            (0, br#"{"v":1,"id":7,"method":"get_state"}"#),
            (5, b"{\"v\":1,\"id\":1,\"method\":\"list_panes\",\"params\":{\"limit\":5}}\n{\"v\":2,\"id\":2,\"method\":\"x\"}\npartial"),
            (10, br#"{"v":1,"id":3,"method":"send_text","params":{"pane":2,"text":"ls\n"},"caller":{"pid":4201,"start_token":"18446744073709551615","pane_hint":7}}"#),
            (1, br#"{"cursor":"10","limit":3,"snapshot":"abc"}"#),
            (1, br#"{"limit":99999}"#),
            (2, br#"{"svg":"<svg xmlns='http://www.w3.org/2000/svg'/>","title":"A \u0007 title","key":"k","pane":3}"#),
            (2, br#"{"mermaid":"flowchart LR; A-->B","inline":"below"}"#),
            (2, br#"{"image_b64":"iVBORw0KGgo=","title":"png"}"#),
            (2, br#"{"path":"/tmp/notes.md","dev":1,"ino":2,"markdown_index":3}"#),
            (3, br#"{"v":1,"action":"open_window","cwd":"/home/me","identity":{"recording_key":"ab:cd-1","record_raw_input":true},"launch_id":"f00d-1"}"#),
            (3, br#"{"v":1,"action":"open_window","identity":{}}"#),
            (4, br##"{"v":1,"pid":42,"win":3,"rgb":"#a1b2c3","auto":true,"started_unix":5,"start_token":9}"##),
            (4, br#"{"v":1,"kind":"gui","pid":42,"endpoint":"/run/kettle.sock","version":"5.1.0","started_unix":1}"#),
        ];
        let seeds: Vec<Vec<u8>> = inputs
            .into_iter()
            .map(|(pick, bytes)| {
                let mut seed = vec![pick];
                seed.extend_from_slice(bytes);
                seed
            })
            .collect();
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("ctl", &seeds);
        for case in cases(&seeds, 800, 0x5eed_0301) {
            ctl(&case);
        }
    }

    /// The checks are live: a request stream read in pieces must find the
    /// newline a whole read does, and a page must hold what it says.
    #[test]
    fn the_checks_see_what_they_check() {
        let mut scanned = 0;
        assert_eq!(protocol::find_newline(b"ab\ncd", &mut scanned), Some(2));
        request_stream(b"ab\ncd\n", 1);
        let request = PageRequest::from_params(&serde_json::json!({"limit": 3})).unwrap();
        assert_eq!(request.bounds(10), (0, 3, Some("3".into()), true));
    }
}
