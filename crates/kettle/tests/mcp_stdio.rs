//! Spawn the real `kettle mcp` stdio server and speak JSON-RPC over pipes.
//!
//! Unit tests cover the handler functions, and `kettle mcp --self-test` covers
//! the in-process path. This test pins the agent-facing process boundary Claude
//! Code / Codex use: newline-delimited JSON-RPC on stdin/stdout plus a real
//! `tools/call` round trip.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn kettle() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kettle"))
}

fn run_mcp_stdio(messages: &[Value]) -> (i32, String, String) {
    run_mcp_stdio_with(&["mcp"], messages)
}

fn run_mcp_stdio_with(args: &[&str], messages: &[Value]) -> (i32, String, String) {
    let mut child = kettle()
        .args(args)
        // A display server outside Kettle must not find the Kettle this test
        // may itself run in.
        .env_remove("KETTLE_PID")
        .env_remove("KETTLE_PANE_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kettle mcp");

    {
        let mut stdin = child.stdin.take().expect("mcp stdin");
        for msg in messages {
            writeln!(stdin, "{msg}").expect("write mcp message");
        }
    } // EOF tells the server to shut down after processing queued requests.

    let mut out = String::new();
    let mut err = String::new();
    child
        .stdout
        .take()
        .expect("mcp stdout")
        .read_to_string(&mut out)
        .expect("read mcp stdout");
    child
        .stderr
        .take()
        .expect("mcp stderr")
        .read_to_string(&mut err)
        .expect("read mcp stderr");
    let status = child.wait().expect("wait mcp");
    (status.code().unwrap_or(-1), out, err)
}

fn parse_responses(out: &str) -> Vec<Value> {
    out.lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSON line {line:?}: {e}"))
        })
        .collect()
}

#[test]
fn codex_artifact_inbox_is_removed_after_stdio_shutdown() {
    let (code, out, err) = run_mcp_stdio_with(
        &["mcp", "--display", "--codex-card-hook"],
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"kettle_agent_hook","arguments":{"event":"UserPromptSubmit"}}}),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let responses = parse_responses(&out);
    let response = responses.iter().find(|reply| reply["id"] == 2).unwrap();
    let hook: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let context = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let directory = context
        .split_once("private artifact inbox ")
        .unwrap()
        .1
        .split_once(" (maximum")
        .unwrap()
        .0;
    assert!(
        !std::path::Path::new(directory).exists(),
        "MCP shutdown leaked {directory}"
    );
}

/// Native Codex terminates its MCP process group before closing the transport.
/// Keep stdin open, including an unfinished request, to exercise that exit path.
#[cfg(unix)]
#[test]
fn codex_artifact_inbox_is_removed_on_termination_signals() {
    use std::io::BufRead as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::{Duration, Instant};

    struct Peer(std::process::Child);
    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let temporary = tempfile::tempdir().unwrap();
        let mut peer = Peer(
            kettle()
                .args(["mcp", "--display", "--codex-card-hook"])
                .env_remove("KETTLE_PID")
                .env_remove("KETTLE_PANE_ID")
                .env("TMPDIR", temporary.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut input = peer.0.stdin.take().unwrap();
        let output = peer.0.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"signal-test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"kettle_agent_hook","arguments":{"event":"UserPromptSubmit"}}}),
        ] {
            writeln!(input, "{message}").unwrap();
        }
        let context = loop {
            let line = receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == 2 {
                let hook: Value = serde_json::from_str(
                    response["result"]["content"][0]["text"].as_str().unwrap(),
                )
                .unwrap();
                break hook["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap()
                    .to_owned();
            }
        };
        let directory = std::path::Path::new(
            context
                .split_once("private artifact inbox ")
                .unwrap()
                .1
                .split_once(" (maximum")
                .unwrap()
                .0,
        );
        std::fs::write(directory.join("finished.svg"), "<svg/>").unwrap();
        let outside = temporary.path().join("durable.svg");
        std::fs::write(&outside, "durable original").unwrap();
        std::os::unix::fs::symlink(&outside, directory.join("alias.svg")).unwrap();
        write!(input, "{{\"unfinished\":").unwrap();
        input.flush().unwrap();
        // SAFETY: the owned, unreaped child still has this PID. No process
        // group or unrelated process receives the signal.
        assert_eq!(unsafe { libc::kill(peer.0.id() as libc::pid_t, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = peer.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "MCP ignored signal {signal}");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.signal(), Some(signal));
        assert!(!directory.exists(), "signal {signal} leaked {directory:?}");
        assert_eq!(
            std::fs::read_to_string(outside).unwrap(),
            "durable original"
        );
    }
}

/// `kettle mcp --display`, over real stdio in both eras, offers exactly
/// `kettle_show` and refuses every other tool before running it.
#[test]
fn mcp_stdio_display_mode_offers_only_kettle_show() {
    let (code, out, err) = run_mcp_stdio_with(
        &["mcp", "--display"],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "kettle_run", "arguments": {"command": ["echo", "no"]}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "kettle_show", "arguments": {"path": "relative.png"}}}),
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    let responses = parse_responses(&out);
    let by_id = |id: u64| {
        responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap_or_else(|| panic!("no response {id}: {out}"))
    };
    assert!(
        by_id(1)["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("kettle_show")
    );
    let tools: Vec<_> = by_id(2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["kettle_show"]);
    assert_eq!(by_id(3)["error"]["code"], -32602);
    assert_eq!(by_id(4)["result"]["isError"], true);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "kettle-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let modern = run_mcp_stdio_with(
        &["mcp", "--display"],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": meta}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": meta}}),
        ],
    );
    let responses = parse_responses(&modern.1);
    let discover = &responses[0]["result"];
    assert_eq!(
        (discover["cacheScope"].as_str(), discover["ttlMs"].as_u64()),
        (Some("private"), Some(0))
    );
    let tools = &responses[1]["result"]["tools"];
    assert_eq!(tools.as_array().map(Vec::len), Some(1));
    assert_eq!(tools[0]["name"], "kettle_show");
}

#[test]
fn mcp_stdio_discovery_without_metadata_preserves_legacy_lifecycle() {
    for args in [&["mcp"][..], &["mcp", "--display"][..]] {
        let messages = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "server/discover"}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 6, "method": "server/discover"}),
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
        ];
        let (code, out, err) = run_mcp_stdio_with(args, &messages);
        assert_eq!(code, 0, "{err}");
        let responses = parse_responses(&out);
        assert_eq!(responses.len(), 7, "{out}");
        let by_id = |id: u64| {
            responses
                .iter()
                .find(|response| response["id"] == id)
                .unwrap_or_else(|| panic!("missing response {id}: {out}"))
        };
        for id in [1, 4, 6] {
            assert_eq!(
                by_id(id)["result"]["supportedVersions"],
                json!(["2026-07-28", "2025-11-25", "2025-06-18"])
            );
            assert_eq!(by_id(id)["result"]["resultType"], "complete");
        }
        assert_eq!(by_id(2)["error"]["code"], -32002);
        assert_eq!(by_id(3)["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(by_id(5)["error"]["code"], -32002);
        assert!(by_id(7)["result"]["tools"].is_array());
    }
}

#[test]
fn mcp_stdio_initialize_list_and_kettle_run() {
    #[cfg(windows)]
    let command = json!(["cmd", "/c", "echo", "mcp-stdio-marker-42"]);
    #[cfg(unix)]
    let command = json!(["echo", "mcp-stdio-marker-42"]);

    let messages = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "kettle_run",
                "arguments": {
                    "command": command,
                    "timeout_s": 5,
                    "strip_ansi": true
                }
            }
        }),
    ];

    let (code, out, err) = run_mcp_stdio(&messages);
    assert_eq!(code, 0, "mcp process failed; stderr: {err}");
    let responses = parse_responses(&out);
    assert_eq!(responses.len(), 3, "stdout was: {out:?}");

    let init = responses
        .iter()
        .find(|r| r["id"] == 1)
        .expect("init response");
    assert_eq!(init["result"]["serverInfo"]["name"], "kettle");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let tools = responses
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tools response");
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"kettle_run"));
    assert!(names.contains(&"kettle_send_keys"));
    assert!(names.contains(&"kettle_dispatch_ui_key"));
    assert!(names.contains(&"kettle_wait_for"));

    let call = responses
        .iter()
        .find(|r| r["id"] == 3)
        .expect("call response");
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    if text.contains("cannot start PTY") || text.contains("PTY") {
        eprintln!("skipping kettle_run assertion: no PTY available");
        return;
    }
    assert_eq!(
        call["result"].get("isError").and_then(Value::as_bool),
        Some(false),
        "kettle_run should not be an MCP error: {text}"
    );
    assert!(text.contains("exit code: 0"), "tool text was: {text:?}");
    assert!(
        text.contains("mcp-stdio-marker-42"),
        "tool text was: {text:?}"
    );
}

/// A modern client sends no `initialize`. A handshake-only server answers
/// every call it makes with `-32002 server is not initialized`, which is the
/// compatibility matrix's "Modern client + Legacy server: Fails" in the one
/// form a user would actually see.
#[test]
fn mcp_stdio_serves_a_modern_client_that_never_handshakes() {
    #[cfg(windows)]
    let command = json!(["cmd", "/c", "echo", "modern-marker-7"]);
    #[cfg(unix)]
    let command = json!(["echo", "modern-marker-7"]);

    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "kettle-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover",
               "params": {"_meta": meta}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
               "params": {"_meta": meta}}),
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "_meta": meta,
                "name": "kettle_run",
                "arguments": {"command": command, "timeout_s": 5, "strip_ansi": true}
            }
        }),
    ];
    let (code, stdout, stderr) = run_mcp_stdio(&messages);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 3, "{stdout}");

    let discover = &responses[0]["result"];
    assert_eq!(discover["resultType"], "complete", "{stdout}");
    assert!(
        discover["supportedVersions"]
            .as_array()
            .expect("supportedVersions")
            .contains(&json!("2026-07-28")),
        "discover must advertise the modern revision: {stdout}"
    );
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "kettle",
        "{stdout}"
    );

    assert!(
        !responses[1]["result"]["tools"]
            .as_array()
            .expect("tools")
            .is_empty(),
        "a modern tools/list must work without a handshake: {stdout}"
    );
    // 2026-07-28 requires every list result to say how long it may be
    // reused; Claude Code rejects a tool list without these and shows the
    // server with no tools.
    let list = &responses[1]["result"];
    assert!(list["ttlMs"].is_u64(), "{stdout}");
    assert!(
        matches!(list["cacheScope"].as_str(), Some("public" | "private")),
        "{stdout}"
    );
    for response in &responses {
        assert!(
            response.get("error").is_none(),
            "no modern request may be refused for want of an initialize: {stdout}"
        );
    }
    assert!(
        serde_json::to_string(&responses[2])
            .unwrap()
            .contains("modern-marker-7"),
        "the tool ran and its output came back: {stdout}"
    );
}

/// The two eras negotiate differently, and conflating them breaks one of them.
///
/// A MODERN request declaring a version kettle does not speak is refused with
/// `UnsupportedProtocolVersion` (-32022) naming what it does speak, so the
/// client can retry.
///
/// A LEGACY `initialize` is not. 2025-11-25 says the server "MUST respond with
/// another protocol version it supports", and the client disconnects if it
/// cannot speak that. Returning -32022 there turns a conforming handshake into
/// a hard failure.
#[test]
fn mcp_stdio_negotiates_an_unknown_version_per_era() {
    let messages = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize",
            "params": {
                "protocolVersion": "1900-01-01",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
    ];
    let (_code, stdout, stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 2, "stdout={stdout}\nstderr={stderr}");

    let modern = &responses[0];
    assert_eq!(modern["error"]["code"], -32022, "{stdout}");
    let supported = modern["error"]["data"]["supported"]
        .as_array()
        .unwrap_or_else(|| panic!("supported list: {stdout}"));
    assert!(supported.contains(&json!("2026-07-28")), "{stdout}");
    assert!(supported.contains(&json!("2025-11-25")), "{stdout}");
    assert_eq!(
        modern["error"]["data"]["requested"], "1900-01-01",
        "{stdout}"
    );

    let legacy = &responses[1];
    assert!(
        legacy.get("error").is_none(),
        "a legacy initialize must succeed with a supported version, not error: {stdout}"
    );
    assert_eq!(
        legacy["result"]["protocolVersion"], "2025-11-25",
        "and that version must be one kettle actually speaks: {stdout}"
    );
}

/// Both fields the specification marks required are required. Filling in a
/// default for a missing one would let a server answer a request it cannot
/// actually characterize.
#[test]
fn mcp_stdio_rejects_a_modern_request_missing_required_meta() {
    let messages = [json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}
    })];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses[0]["error"]["code"], -32602, "{stdout}");
}

/// The legacy era still works, unchanged: "Legacy client + Dual-era server:
/// Works." A legacy result must NOT grow `resultType`, which a legacy client
/// has no reason to expect.
#[test]
fn mcp_stdio_still_serves_a_legacy_client_unchanged() {
    let messages = [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "kettle-test", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(
        responses[0]["result"]["protocolVersion"], "2025-11-25",
        "{stdout}"
    );
    for field in ["resultType", "ttlMs", "cacheScope"] {
        assert!(
            responses[1]["result"].get(field).is_none(),
            "a legacy result must stay legacy-shaped: {stdout}"
        );
    }
    assert!(
        !responses[1]["result"]["tools"]
            .as_array()
            .expect("tools")
            .is_empty(),
        "{stdout}"
    );
}

/// A modern client with a small bug — the version sent as a number, or `_meta`
/// sent as something other than an object — must be told what is wrong. Falling
/// through to the legacy path would answer "server is not initialized", which
/// points at a handshake the client is right not to be sending.
#[test]
fn mcp_stdio_names_a_malformed_modern_envelope_instead_of_blaming_the_handshake() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
               "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": 20260728}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
               "params": {"_meta": "2026-07-28"}}),
    ];
    let (_code, stdout, _stderr) = run_mcp_stdio(&messages);
    let responses = parse_responses(&stdout);
    assert_eq!(responses.len(), 2, "{stdout}");
    for response in &responses {
        assert_eq!(response["error"]["code"], -32602, "{stdout}");
        let message = response["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("_meta"),
            "the error must point at the envelope, not the handshake: {stdout}"
        );
    }
}

/// A Kettle with agent previews on, in this test process: a control server
/// registered where a `kettle mcp` started from here looks, which answers
/// every `show` as shown and sends each request's params back to the test.
#[cfg(unix)]
struct FakeKettle {
    runtime: tempfile::TempDir,
    shows: std::sync::mpsc::Receiver<Value>,
    agy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl FakeKettle {
    fn start() -> Self {
        use std::io::BufRead as _;
        let runtime = tempfile::Builder::new()
            .prefix("kmcp")
            .permissions({
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::Permissions::from_mode(0o700)
            })
            .tempdir_in("/tmp")
            .expect("runtime dir");
        let registry = runtime.path().join("kettle").join("ctl");
        let pid = std::process::id();
        let endpoint = kettle_ctl::discovery::default_endpoint(&registry, pid);
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&registry)
                .expect("registry");
        }
        let listener = kettle_ctl::transport::CtlListener::bind(&endpoint).expect("bind");
        let entry =
            kettle_ctl::discovery::RegistryEntry::registering("gui", pid, endpoint, "5.0.0", 1);
        kettle_ctl::discovery::register(&registry, &entry).expect("register");
        let (sent, shows) = std::sync::mpsc::channel();
        let agy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let policy = agy.clone();
        std::thread::spawn(move || {
            while let Ok(stream) = listener.accept() {
                let sent = sent.clone();
                let policy = policy.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone");
                    for line in std::io::BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let request: Value = serde_json::from_str(&line).expect("request");
                        let reply = if request["method"] == "show" {
                            let _ = sent.send(request["params"].clone());
                            json!({"v": 1, "id": request["id"], "ok": true, "result": {
                                "pane": 1, "verified": true, "window": 1, "item": 2,
                                "kind": "mermaid", "width": 64, "height": 48, "warnings": []}})
                        } else if request["method"] == "agent_display_status" {
                            json!({"v":1,"id":request["id"],"ok":true,"result":{
                                "agy":policy.load(std::sync::atomic::Ordering::Acquire)}})
                        } else {
                            json!({"v": 1, "id": request["id"], "ok": false,
                                "error": {"code": "unknown_method", "message": "m"}})
                        };
                        if writeln!(writer, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            runtime,
            shows,
            agy,
        }
    }

    fn spawn(&self, args: &[&str]) -> std::process::Child {
        kettle()
            .args(args)
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.runtime.path().join("config"))
            .env_remove("KETTLE_PID")
            .env_remove("KETTLE_PANE_ID")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn kettle mcp")
    }

    /// Run `kettle mcp` with `args` here, where it finds this Kettle.
    fn mcp(&self, args: &[&str], messages: &[Value]) -> Vec<Value> {
        let mut child = self.spawn(args);
        {
            let mut stdin = child.stdin.take().expect("stdin");
            for message in messages {
                writeln!(stdin, "{message}").expect("write");
            }
        }
        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("stdout")
            .read_to_string(&mut out)
            .expect("read");
        child.wait().expect("wait");
        parse_responses(&out)
    }
}

#[cfg(unix)]
#[test]
fn agy_guidance_is_portable_and_page_keys_use_the_validated_page() {
    let kettle = FakeKettle::start();
    kettle.agy.store(true, std::sync::atomic::Ordering::Release);
    let config = kettle.runtime.path().join("config/kettle");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config"),
        "agent-display = false\nagent-display-agy = false\n",
    )
    .unwrap();
    let mut child = kettle.spawn(&["agent-setup", "--agy-hook", "guidance"]);
    writeln!(child.stdin.take().unwrap(), "{{}}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let guidance: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        guidance["injectSteps"][0]["ephemeralMessage"]
            .as_str()
            .unwrap()
            .contains("kettle_show")
    );
    let file = kettle.runtime.path().join("pages.pdf");
    std::fs::write(&file, "%PDF-1.4\n").unwrap();
    let mut keys = Vec::new();
    for page in [json!(2), json!(2.0), json!(3)] {
        let replies = kettle.mcp(&["mcp", "--display", "--agy-display"], &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"kettle_show","arguments":{"path":file,"page":page}}}),
        ]);
        let reply = replies.iter().find(|reply| reply["id"] == 2).unwrap();
        assert_eq!(reply["result"]["structuredContent"]["status"], "sent");
        keys.push(
            kettle
                .shows
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()["key"]
                .clone(),
        );
    }
    assert_eq!(keys[0], keys[1], "2 and 2.0 must update the same page");
    assert_ne!(keys[0], keys[2], "distinct pages must have distinct keys");
    kettle
        .agy
        .store(false, std::sync::atomic::Ordering::Release);
    let mut child = kettle.spawn(&["agent-setup", "--agy-hook", "guidance"]);
    writeln!(child.stdin.take().unwrap(), "{{}}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({})
    );
    // Disabled integration wins before parameter validation or file access.
    for arguments in [json!({"page": 0}), json!({"path": "/missing/preview.png"})] {
        let replies = kettle.mcp(&["mcp", "--display", "--agy-display"], &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"kettle_show","arguments":arguments}}),
        ]);
        let reply = replies.iter().find(|reply| reply["id"] == 2).unwrap();
        assert_eq!(
            reply["result"]["structuredContent"]["code"],
            "display_disabled"
        );
    }
}

/// Real MCP process and control transport; the control server is a fixture,
/// so this proves bounded delivery/deduplication, not native card rendering.
#[cfg(unix)]
#[test]
fn codex_fallback_waits_for_stop_and_preserves_primary_delivery() {
    use std::io::BufRead as _;
    use std::time::{Duration, Instant};
    struct Peer(std::process::Child);
    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let kettle = FakeKettle::start();
    let mut peer = Peer(kettle.spawn(&["mcp", "--display", "--codex-card-hook"]));
    let mut input = peer.0.stdin.take().unwrap();
    let output = peer.0.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            let reply: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if sender.send(reply).is_err() {
                break;
            }
        }
    });
    let mut ask = |message: Value| {
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("MCP reply")
    };
    let initialized = ask(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
    );
    assert!(initialized.get("error").is_none());
    // A notification has no response; send it together with the first call.
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let mut ask = |id: u64, name: &str, args: Value, meta: Value| {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":args,"_meta":meta}})
        )
        .unwrap();
        input.flush().unwrap();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("MCP tool reply")
    };
    let guidance = ask(
        2,
        "kettle_agent_hook",
        json!({"event":"UserPromptSubmit"}),
        json!({}),
    );
    let hook: Value =
        serde_json::from_str(guidance["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let context = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let directory = context
        .split_once("private artifact inbox ")
        .unwrap()
        .1
        .split_once(" (maximum")
        .unwrap()
        .0;
    let path = std::path::Path::new(directory).join("a.svg");
    let second = std::path::Path::new(directory).join("b.svg");
    for path in [&path, &second] {
        std::fs::write(path, "<svg/>").unwrap();
    }
    let post = ask(
        3,
        "kettle_agent_hook",
        json!({"event":"PostToolUse"}),
        json!({}),
    );
    assert_eq!(post["result"]["content"][0]["text"], "{}");
    assert!(kettle.shows.try_recv().is_err());
    ask(4, "kettle_agent_hook", json!({"event":"Stop"}), json!({}));
    let first = kettle.shows.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first["path"], path.to_str().unwrap());
    assert!(
        kettle.shows.try_recv().is_err(),
        "more than one artifact per hook"
    );
    ask(5, "kettle_agent_hook", json!({"event":"Stop"}), json!({}));
    assert_eq!(
        kettle.shows.recv_timeout(Duration::from_secs(5)).unwrap()["path"],
        second.to_str().unwrap()
    );
    ask(6, "kettle_agent_hook", json!({"event":"Stop"}), json!({}));
    assert!(
        kettle.shows.try_recv().is_err(),
        "unchanged artifact retried"
    );
    std::fs::write(&path, "<svg updated/>").unwrap();
    let primary = ask(
        7,
        "kettle_show",
        json!({"path":path}),
        json!({"callId":"primary-1"}),
    );
    assert_eq!(primary["result"]["structuredContent"]["status"], "sent");
    let sent = kettle.shows.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        sent["key"], first["key"],
        "primary and fallback must update the same item"
    );
    ask(8, "kettle_agent_hook", json!({"event":"Stop"}), json!({}));
    assert!(
        kettle.shows.try_recv().is_err(),
        "primary delivery was duplicated"
    );
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = peer.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "MCP did not close after EOF");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !std::path::Path::new(directory).exists(),
        "session artifacts leaked"
    );
}

/// `kettle_show` completes through the real `kettle mcp`, in both modes and
/// both protocol eras, from Mermaid source and from a file: the Kettle it
/// runs in gets the source as sent, or the file's path and identity, and
/// the model gets one plain line saying it has not seen the media.
#[cfg(unix)]
#[test]
fn kettle_show_completes_in_both_modes_and_eras() {
    let kettle = FakeKettle::start();
    let directory = tempfile::tempdir().expect("dir");
    let file = directory.path().join("flow.mmd");
    std::fs::write(&file, "graph LR\n  A --> B\n").expect("file");
    let file = file.to_str().expect("utf-8").to_owned();
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "kettle-test", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    for args in [&["mcp", "--display"][..], &["mcp"][..]] {
        for (source, check) in [
            (json!({"mermaid": "graph LR\n  A --> B"}), "mermaid"),
            (json!({"path": file}), "path"),
        ] {
            let legacy = [
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"name": "kettle_show", "arguments": source}}),
            ];
            let modern = [json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"_meta": meta, "name": "kettle_show", "arguments": source}})];
            for (era, messages) in [("legacy", &legacy[..]), ("modern", &modern[..])] {
                let responses = kettle.mcp(args, messages);
                let call = responses
                    .iter()
                    .find(|response| response["id"] == 2)
                    .unwrap_or_else(|| panic!("{args:?} {era}: {responses:?}"));
                let result = &call["result"];
                assert_ne!(result["isError"], true, "{args:?} {era} {check}: {result}");
                let text = result["content"][0]["text"].as_str().unwrap();
                assert!(text.contains("You have not seen its contents"), "{text}");
                assert_eq!(result["structuredContent"]["status"], "sent");
                assert_eq!(result["structuredContent"]["model_has_seen"], false);
                let params = kettle
                    .shows
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("the show arrived");
                match check {
                    "mermaid" => assert_eq!(params["mermaid"], "graph LR\n  A --> B"),
                    _ => {
                        assert_eq!(params["path"], file);
                        assert!(params["dev"].is_u64() && params["ino"].is_u64());
                    }
                }
            }
        }
    }
}
