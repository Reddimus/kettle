//! Simulated native harness for the Linux card smoke; makes no model calls.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn main() {
    let target = std::env::var("FIXTURE_AGENT").unwrap();
    if std::env::args().any(|arg| arg == "--version") {
        let default = if target == "codex" {
            "codex-cli 0.163.0"
        } else {
            "2.1.303 (Claude Code)"
        };
        println!("{}", std::env::var("FIXTURE_AGENT_VERSION").unwrap_or_else(|_| default.into()));
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("FIXTURE_CASE").unwrap());
    let mut server = Server(
        Command::new(std::env::var_os("FIXTURE_KETTLE").unwrap())
            .args([
                "mcp",
                "--display",
                if target == "codex" {
                    "--codex-card-hook"
                } else {
                    "--claude-card-hook"
                },
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    std::fs::write(
        root.join("pids"),
        format!("{} {}\n", std::process::id(), server.0.id()),
    )
    .unwrap();
    let mut input = server.0.stdin.take().unwrap();
    let mut output = BufReader::new(server.0.stdout.take().unwrap());
    let mut ask = |line: &str, reply: bool| {
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
        if !reply {
            return String::new();
        }
        let mut reply = String::new();
        assert!(output.read_line(&mut reply).unwrap() > 0);
        reply
    };
    let initialize = ask(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"kettle-card-fixture","version":"1"}}}"#,
        true,
    );
    std::fs::write(root.join("initialize.json"), initialize).unwrap();
    ask(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
        false,
    );
    let model = ask(&std::env::var("FIXTURE_SHOW_REQUEST").unwrap(), true);
    std::fs::write(root.join("model.json"), &model).unwrap();
    let hook = ask(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"kettle_card","arguments":{"tool_use_id":"fixture-1"}}}"#,
        true,
    );
    std::fs::write(root.join("hook.json"), &hook).unwrap();
    let parser = r#"import json,sys
reply=json.load(sys.stdin)
message=json.loads(reply['result']['content'][0]['text']).get('systemMessage','')
if message:
 print('\u21b3 Hook \u00b7 ' if sys.argv[1]=='codex' else '  \u23bf \u00a0PostToolUse:mcp__plugin_kettle_kettle__kettle_show says:',end='')
 for line in message.split('\n')[1:]: print('\n'+('    ' if sys.argv[1]=='codex' else '     ')+line,end='')
 print()
"#;
    let mut formatter = Command::new("/usr/bin/python3")
        .args(["-c", parser, &target])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    formatter
        .stdin
        .take()
        .unwrap()
        .write_all(hook.as_bytes())
        .unwrap();
    let formatted = formatter.wait_with_output().unwrap();
    assert!(formatted.status.success());
    std::io::stdout().write_all(&formatted.stdout).unwrap();
    std::io::stdout().flush().unwrap();
    std::fs::write(root.join("ready"), "ready\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline && !root.join("quit").exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
}
