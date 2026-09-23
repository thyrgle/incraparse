//! End-to-end smoke test: spawns the `incraparse-lua-server` binary with the
//! checked-in `minilang.lua` fixture and drives a real LSP session over
//! stdio — initialize, didOpen with a broken function, incremental fix,
//! documentSymbol, shutdown.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const SOURCE: &str = "def add(a, b) { return a + b; }\ndef bad(x) { return; }";

fn spawn_server() -> (Child, ChildStdin, Receiver<String>) {
    let path = env!("CARGO_BIN_EXE_incraparse-lua-server");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/minilang.lua");
    let mut child = Command::new(path)
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn incraparse-lua-server");

    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut content_length: Option<usize> = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    content_length = value.parse().ok();
                }
            }
            let Some(len) = content_length else { return };
            let mut body = vec![0u8; len];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            if tx
                .send(String::from_utf8_lossy(&body).into_owned())
                .is_err()
            {
                return;
            }
        }
    });

    (child, stdin, rx)
}

fn send(stdin: &mut ChildStdin, value: &serde_json::Value) {
    let body = value.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

fn next_message(rx: &Receiver<String>) -> serde_json::Value {
    loop {
        let text = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(text) => text,
            Err(err) => panic!("no server message arrived: {err:?}"),
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        if value.get("id").is_some() || value.get("method").is_some() {
            return value;
        }
    }
}

fn notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn request(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn wait_for_diagnostics(rx: &Receiver<String>, version: i64) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for diagnostics"
        );
        let msg = next_message(rx);
        if msg["method"] == "textDocument/publishDiagnostics"
            && msg["params"]["version"].as_i64() == Some(version)
        {
            return msg["params"]["diagnostics"].clone();
        }
    }
}

#[test]
fn lua_defined_server_smoke() {
    let (mut child, mut stdin, rx) = spawn_server();

    send(
        &mut stdin,
        &request(1, "initialize", serde_json::json!({"capabilities": {}})),
    );
    let initialized = next_message(&rx);
    assert_eq!(
        initialized["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    assert_eq!(
        initialized["result"]["capabilities"]["documentSymbolProvider"],
        true
    );
    send(
        &mut stdin,
        &notification("initialized", serde_json::json!({})),
    );

    // didOpen with a broken `bad` function -> one diagnostic from the Lua
    // diagnostic hook.
    send(
        &mut stdin,
        &notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": "file:///test.mini",
                    "languageId": "mini",
                    "version": 0,
                    "text": SOURCE,
                }
            }),
        ),
    );
    let diags = wait_for_diagnostics(&rx, 0);
    assert_eq!(diags.as_array().unwrap().len(), 1, "bad's return must fail");
    assert!(diags[0]["message"].to_string().contains("bad"));

    // Incremental didChange: `return;` -> `return x;` on line 1.
    send(
        &mut stdin,
        &notification(
            "textDocument/didChange",
            serde_json::json!({
                "textDocument": { "uri": "file:///test.mini", "version": 1 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 1, "character": 13 },
                        "end": { "line": 1, "character": 20 },
                    },
                    "text": "return x;",
                }],
            }),
        ),
    );
    let diags = wait_for_diagnostics(&rx, 1);
    assert_eq!(diags.as_array().unwrap().len(), 0, "the fix heals the file");

    // documentSymbol: both functions visible with Lua-provided details.
    send(
        &mut stdin,
        &request(
            2,
            "textDocument/documentSymbol",
            serde_json::json!({"textDocument": {"uri": "file:///test.mini"}}),
        ),
    );
    let response = next_message(&rx);
    assert_eq!(response["id"], 2);
    let symbols = response["result"].as_array().expect("symbol array");
    let names: Vec<&str> = symbols
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["add", "bad"]);
    assert_eq!(symbols[0]["detail"], "(a, b)");

    // Shutdown handshake.
    send(&mut stdin, &request(3, "shutdown", serde_json::Value::Null));
    let shutdown = next_message(&rx);
    assert_eq!(shutdown["id"], 3);
    send(&mut stdin, &notification("exit", serde_json::Value::Null));

    drop(stdin);
    let status = child.wait().expect("wait");
    assert!(status.success(), "server exited with {status}");
}
