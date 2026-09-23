//! Tests for the `SimpleLanguage` builder, driven over a
//! `Connection::memory()` pair.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use incraparse::prelude::*;
use incraparse_lsp::{serve_on, Documents, NodeLabel, SimpleLanguage};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::Diagnostic;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctx {
    File,
    Region { name: &'static str },
}

/// Round 0: splits the file at byte 4 into two named regions.
struct Split;

impl Pass for Split {
    type Ctx = Ctx;

    fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        if !matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        let split = (span.start + 4).min(span.end);
        let _head = source[split..].chars().next().unwrap_or('?');
        Outcome::Expand(vec![
            (
                Span::new(span.start, split, span.rev),
                Ctx::Region { name: "left" },
            ),
            (
                Span::new(split, span.end, span.rev),
                Ctx::Region { name: "right" },
            ),
        ])
    }
}

/// Round 1: fails regions containing `!`.
struct NoBang;

impl Pass for NoBang {
    type Ctx = Ctx;

    fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        if matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        if source[span.to_range()].contains('!') {
            Outcome::Failed
        } else {
            Outcome::Done
        }
    }
}

fn language(calls: Arc<AtomicUsize>) -> SimpleLanguage<Ctx> {
    SimpleLanguage::new(Engine::with((Split, NoBang)), Ctx::File)
        .diagnostic_fn(move |_doc, _node| {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(Diagnostic {
                message: "bang".into(),
                ..Diagnostic::default()
            })
        })
        .label_fn(|ctx| match ctx {
            Ctx::File => None,
            Ctx::Region { name } => Some(NodeLabel::new(*name)),
        })
}

fn request(id: i64, method: &str, params: serde_json::Value) -> Message {
    Message::Request(Request {
        id: RequestId::from(id as i32),
        method: method.into(),
        params,
    })
}

fn notification(method: &str, params: serde_json::Value) -> Message {
    Message::Notification(Notification::new(method.into(), params))
}

fn next_response(connection: &Connection, id: i64) -> serde_json::Value {
    for _ in 0..10 {
        match connection.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Response(Response {
                id: got,
                response_result,
            })) => {
                if got == RequestId::from(id as i32) {
                    return response_result.expect("response should be Ok");
                }
            }
            Ok(_) => continue,
            Err(err) => panic!("no server message: {err:?}"),
        }
    }
    panic!("no response with id {id}");
}

fn next_diagnostics(connection: &Connection, version: i64) -> Vec<Diagnostic> {
    for _ in 0..10 {
        match connection.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Notification(n)) => {
                if n.method == "textDocument/publishDiagnostics"
                    && n.params["version"].as_i64() == Some(version)
                {
                    let diags: Vec<Diagnostic> =
                        serde_json::from_value(n.params["diagnostics"].clone()).unwrap();
                    return diags;
                }
            }
            Ok(_) => continue,
            Err(err) => panic!("no diagnostics: {err:?}"),
        }
    }
    panic!("no diagnostics notification");
}

#[test]
fn simple_language_serves_end_to_end() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    let server_thread =
        std::thread::spawn(move || serve_on::<Ctx, _>(server, language(calls2), Documents::new()));

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    let caps = next_response(&client, 1)["capabilities"].clone();
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["documentSymbolProvider"], true);
    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    // Open with a bang in the right region: one diagnostic.
    client.sender.send(notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {"uri": "file:///s.mini", "languageId": "mini", "version": 0,
                             "text": "abcdef!ghij"},
        }),
    ))?;
    let diags = next_diagnostics(&client, 0);
    assert_eq!(diags.len(), 1);

    // Outline: both regions, named by label_fn.
    client.sender.send(request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": "file:///s.mini"}}),
    ))?;
    let symbols = next_response(&client, 2);
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["left", "right"]);

    // Fix the bang: diagnostics clear; the label_fn hook was not consulted
    // again (labels come from the tree).
    client.sender.send(notification(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": "file:///s.mini", "version": 1},
            "contentChanges": [{
                "range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 7}},
                "text": "X",
            }],
        }),
    ))?;
    let diags = next_diagnostics(&client, 1);
    assert_eq!(diags.len(), 0);

    client
        .sender
        .send(request(3, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;

    // The diagnostic hook ran once: one failing node in the open publish,
    // and none after the fix.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn simple_language_without_hooks_advertises_no_symbols(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        serve_on::<Ctx, _>(
            server,
            SimpleLanguage::new(Engine::with((NoBang,)), Ctx::File),
            Documents::new(),
        )
    });

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    let caps = next_response(&client, 1)["capabilities"].clone();
    assert_eq!(caps["documentSymbolProvider"], false);
    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    client
        .sender
        .send(request(2, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;
    Ok(())
}
