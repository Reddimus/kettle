//! `kettle ctl` — a thin control-plane client.
//!
//! Discovers a running kettle's control server (via the kettle-ctl registry, or
//! `--pid`), issues one method call, and prints the result — or, for
//! `kettle ctl events`, streams the event feed as NDJSON. A scripting + manual
//! debugging front-end for the same surface `kettle mcp` exposes to agents.

use kettle_ctl::Client;
use serde_json::Value;

use crate::CtlArgs;

/// Run `kettle ctl …`; returns the process exit code (0 ok, 1 on error).
pub fn run_ctl(args: CtlArgs) -> i32 {
    let mut client = match Client::discover(args.pid) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kettle ctl: {e}");
            return 1;
        }
    };

    if args.method == "events" {
        return stream_events(&mut client, args.pane);
    }

    let params = match build_params(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("kettle ctl: {e}");
            return 1;
        }
    };

    match client.call(&args.method, params.clone()) {
        Ok(result) => {
            if args.raw {
                println!("{result}");
            } else {
                print!("{}", pretty(&args.method, &params, &result));
            }
            0
        }
        Err(e) => {
            eprintln!("kettle ctl: {e}");
            1
        }
    }
}

/// Subscribe, then print each event line as it arrives until EOF / Ctrl+C.
fn stream_events(client: &mut Client, pane: Option<u64>) -> i32 {
    let mut params = serde_json::Map::new();
    if let Some(p) = pane {
        params.insert("pane".into(), p.into());
    }
    if let Err(e) = client.call("subscribe", Value::Object(params)) {
        eprintln!("kettle ctl: subscribe failed: {e}");
        return 1;
    }
    loop {
        match client.next_event() {
            Ok(Some(ev)) => {
                // E3 (audit v2.32.0): ping keepalives are now consumed inside
                // `Client::next_event` (the single forward-compat seam), so no
                // per-consumer skip is needed here.
                // Filter by pane when requested.
                if let Some(want) = pane
                    && ev.pane.is_some()
                    && ev.pane != Some(want)
                {
                    continue;
                }
                match serde_json::to_string(&ev) {
                    Ok(line) => println!("{line}"),
                    Err(_) => continue,
                }
            }
            Ok(None) => return 0,
            Err(e) => {
                eprintln!("kettle ctl: {e}");
                return 1;
            }
        }
    }
}

/// Merge `--pane` / `--text` / `--json` into the request params object.
fn build_params(args: &CtlArgs) -> Result<Value, String> {
    let mut map = if let Some(json) = &args.json {
        match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(m)) => m,
            Ok(_) => return Err("--json must be a JSON object".into()),
            Err(e) => return Err(format!("--json is not valid JSON: {e}")),
        }
    } else {
        serde_json::Map::new()
    };
    if let Some(p) = args.pane {
        map.insert("pane".into(), p.into());
    }
    if let Some(text) = &args.text {
        // A few methods take a named string field; everything else takes `text`.
        let key = match args.method.as_str() {
            "run_command" => "command",
            "perform_action" => "action",
            _ => "text",
        };
        map.insert(key.into(), Value::String(text.clone()));
    }
    // v2.20.0: `--regex` → `wait_for`'s regex param.
    if let Some(re) = &args.regex {
        map.insert("regex".into(), Value::String(re.clone()));
    }
    // `--keys "escape,ctrl+c"` → the `send_keys` / `dispatch_ui_key` token array.
    if let Some(keys) = &args.keys {
        let arr: Vec<Value> = keys
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(|k| Value::String(k.to_string()))
            .collect();
        if arr.is_empty() {
            return Err("--keys is empty".into());
        }
        map.insert("keys".into(), Value::Array(arr));
    }
    Ok(Value::Object(map))
}

/// A compact human-readable rendering of common method results.
fn pretty(method: &str, request_params: &Value, result: &Value) -> String {
    match method {
        "list_panes" => {
            let mut out = String::new();
            if let Some(panes) = result.get("panes").and_then(|p| p.as_array()) {
                for p in panes {
                    let focus = if p["focused"].as_bool() == Some(true) {
                        "*"
                    } else {
                        " "
                    };
                    out.push_str(&format!(
                        "{focus} pane {:<4} tab {:<2} {:>3}x{:<3} {}  {}\n",
                        p["id"],
                        p["tab"],
                        p["cols"],
                        p["rows"],
                        p["cwd"].as_str().unwrap_or("?"),
                        p["title"].as_str().unwrap_or(""),
                    ));
                }
            }
            out.push_str(&page_notice(request_params, result));
            out
        }
        "list_tabs" => {
            let mut out = String::new();
            if let Some(tabs) = result.get("tabs").and_then(|t| t.as_array()) {
                for t in tabs {
                    let active = if t["active"].as_bool() == Some(true) {
                        "*"
                    } else {
                        " "
                    };
                    out.push_str(&format!(
                        "{active} tab {:<2} {}\n",
                        t["index"],
                        t["title"].as_str().unwrap_or(""),
                    ));
                }
            }
            out.push_str(&page_notice(request_params, result));
            out
        }
        "read_screen" => {
            let mut out = result
                .get("text")
                .and_then(|t| t.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{result}\n"));
            out.push_str(&page_notice(request_params, result));
            out
        }
        "screenshot" => {
            let path = result.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let scope = if result.get("full_window").and_then(|v| v.as_bool()) == Some(true) {
                format!("window {}", result["window"])
            } else {
                format!("pane {}", result["pane"])
            };
            format!("saved screenshot of {scope} to {path}\n")
        }
        "run_command" => {
            let code = result.get("exit_code");
            let timed = result.get("timed_out").and_then(|t| t.as_bool()) == Some(true);
            let out = result.get("output").and_then(|o| o.as_str()).unwrap_or("");
            let truncated = if result.get("output_truncated").and_then(Value::as_bool) == Some(true)
            {
                "\n[output truncated]"
            } else {
                ""
            };
            if timed {
                format!("{out}{truncated}\n[timed out — no exit code]\n")
            } else {
                format!(
                    "{out}{truncated}\n[exit {}]\n",
                    code.unwrap_or(&Value::Null)
                )
            }
        }
        // v2.20.0 (agent plane).
        "send_keys" => format!(
            "sent {} keys ({} bytes) to pane {}\n",
            result["keys"], result["bytes"], result["pane"]
        ),
        "dispatch_ui_key" => format!(
            "dispatched {} keys to the {} modal in window {} (open: {})\n",
            result["keys"], result["modal"], result["window"], result["open"]
        ),
        "send_mouse" => format!(
            "sent mouse {} at [{}, {}] to window {} (handled: {})\n",
            result["event"],
            result["cursor"][0],
            result["cursor"][1],
            result["window"],
            result["handled"]
        ),
        "perform_action" => format!(
            "performed action {} on window {}\n",
            result["action"], result["window"]
        ),
        "dispatch_keybind" => {
            if result.get("dispatched").and_then(|v| v.as_bool()) == Some(true) {
                format!(
                    "dispatched keybind {} ({}) on window {}\n",
                    result["trigger"], result["action"], result["window"]
                )
            } else if result.get("modal_blocked").and_then(|v| v.as_bool()) == Some(true) {
                format!(
                    "keybind dispatch blocked by modal on window {}\n",
                    result["window"]
                )
            } else {
                format!("no keybind matched on window {}\n", result["window"])
            }
        }
        "ui_geometry" | "read_cells" => {
            serde_json::to_string_pretty(result).unwrap_or_else(|_| format!("{result}\n")) + "\n"
        }
        "wait_for" => {
            if result.get("matched").and_then(|m| m.as_bool()) == Some(true) {
                format!("matched after {} ms\n", result["elapsed_ms"])
            } else {
                format!("timed out after {} ms (no match)\n", result["elapsed_ms"])
            }
        }
        _ => format!("{result}\n"),
    }
}

fn page_notice(request_params: &Value, result: &Value) -> String {
    if result.get("truncated").and_then(Value::as_bool) != Some(true) {
        return String::new();
    }
    let cursor = result
        .get("next_cursor")
        .and_then(Value::as_str)
        .unwrap_or("");
    let snapshot = result.get("snapshot").and_then(Value::as_str).unwrap_or("");
    let mut continuation = request_params.as_object().cloned().unwrap_or_default();
    continuation.insert("cursor".into(), Value::String(cursor.to_string()));
    continuation.insert("snapshot".into(), Value::String(snapshot.to_string()));
    let json = Value::Object(continuation);
    format!("\n[more: repeat with --json '{json}']\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_pretty_output_explains_continuation() {
        let result = serde_json::json!({
            "panes": [],
            "truncated": true,
            "next_cursor": "10",
            "snapshot": "abc",
        });
        let output = pretty("list_panes", &serde_json::json!({}), &result);
        assert!(output.contains(r#""cursor":"10""#));
        assert!(output.contains(r#""snapshot":"abc""#));
    }

    #[test]
    fn read_screen_continuation_keeps_original_parameters() {
        let result = serde_json::json!({
            "text": "page one",
            "truncated": true,
            "next_cursor": "1024",
            "snapshot": "stable",
        });
        let params = serde_json::json!({"pane": 7, "scrollback_lines": 4000, "limit": 1024});
        let output = pretty("read_screen", &params, &result);
        assert!(output.contains(r#""pane":7"#));
        assert!(output.contains(r#""scrollback_lines":4000"#));
        assert!(output.contains(r#""limit":1024"#));
        assert!(output.contains(r#""cursor":"1024""#));
        assert!(output.contains(r#""snapshot":"stable""#));
    }

    #[test]
    fn run_command_pretty_output_discloses_truncation() {
        let result = serde_json::json!({
            "output": "surviving tail",
            "output_truncated": true,
            "exit_code": 1,
            "timed_out": false,
        });
        let output = pretty("run_command", &serde_json::json!({}), &result);
        assert!(output.contains("[output truncated]"));
        assert!(output.contains("[exit 1]"));
    }
}
