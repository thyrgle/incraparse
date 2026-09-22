//! In-process tests for the `serve()` skeleton, driven over a
//! `Connection::memory()` pair — a full LSP session without stdio.

use std::error::Error;
use std::time::Duration;

use incraparse::{Engine, Outcome, Pass, Schedule, Span};
use incraparse_lsp::{serve_on, Documents, Language};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{Diagnostic, DocumentSymbol, Position};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctx {
    File,
    Left,
    Right { head: char },
}

struct SplitAtFour;

impl Pass for SplitAtFour {
    type Ctx = Ctx;

    fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        if !matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        let split = (span.start + 4).min(span.end);
        let head = source[split..].chars().next().unwrap_or('?');
        Outcome::Expand(vec![
            (Span::new(span.start, split, span.rev), Ctx::Left),
            (Span::new(split, span.end, span.rev), Ctx::Right { head }),
        ])
    }
}

/// Fails any region containing `!`.
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

struct TestLang<const SYMBOLS: bool> {
    engine: Engine<Ctx>,
}

impl<const SYMBOLS: bool> TestLang<SYMBOLS> {
    fn new() -> Self {
        let mut schedule = Schedule::new();
        schedule.push(SplitAtFour);
        schedule.push(NoBang);
        Self {
            engine: Engine::new(schedule),
        }
    }
}

impl<const SYMBOLS: bool> Language<Ctx> for TestLang<SYMBOLS> {
    const SUPPORTS_SYMBOLS: bool = SYMBOLS;

    fn engine(&self) -> &Engine<Ctx> {
        &self.engine
    }

    fn root_ctx(&self) -> Ctx {
        Ctx::File
    }

    fn diagnostic(
        &self,
        doc: &incraparse_lsp::Document<Ctx>,
        node: incraparse_lsp::FailedNode<'_, Ctx>,
    ) -> Option<Diagnostic> {
        let text = &doc.text()[node.span.to_range()];
        assert!(text.contains('!'), "fixture only fails on '!'");
        Some(Diagnostic {
            message: format!("bang in {text:?}"),
            ..Diagnostic::default()
        })
    }

    fn symbols(&self, doc: &incraparse_lsp::Document<Ctx>) -> Vec<DocumentSymbol> {
        let tree = doc.session().tree();
        tree.nodes()
            .map(|id| {
                let name = match tree.ctx(id) {
                    Ctx::Left => "left".to_string(),
                    Ctx::Right { head } => format!("right:{head}"),
                    Ctx::File => "file".to_string(),
                };
                DocumentSymbol {
                    name,
                    detail: None,
                    kind: lsp_types::SymbolKind::FUNCTION,
                    range: doc.range(tree.span(id)),
                    selection_range: doc.range(tree.span(id)),
                    children: None,
                    tags: None,
                    #[allow(deprecated)]
                    deprecated: None,
                }
            })
            .collect()
    }
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

fn next_message(connection: &Connection) -> Message {
    match connection.receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(msg) => msg,
        Err(err) => panic!("no server message arrived: {err:?}"),
    }
}

fn next_response(connection: &Connection, id: i64) -> serde_json::Value {
    for _ in 0..10 {
        match next_message(connection) {
            Message::Response(Response {
                id: got,
                response_result,
            }) => {
                if got == RequestId::from(id as i32) {
                    return response_result.expect("response should be Ok");
                }
            }
            _ => continue,
        }
    }
    panic!("no response with id {id}");
}

fn next_diagnostics(connection: &Connection) -> (Option<i32>, Vec<Diagnostic>) {
    for _ in 0..10 {
        if let Message::Notification(n) = next_message(connection) {
            if n.method == "textDocument/publishDiagnostics" {
                let params: lsp_types::PublishDiagnosticsParams =
                    serde_json::from_value(n.params).unwrap();
                return (params.version, params.diagnostics);
            }
        }
    }
    panic!("no publishDiagnostics notification");
}

const URI: &str = "file:///mem.mini";

fn did_open(text: &str) -> Message {
    notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {
                "uri": URI, "languageId": "minilang", "version": 0, "text": text
            }
        }),
    )
}

fn did_change(version: i64, start: u32, end: u32, text: &str) -> Message {
    notification(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": URI, "version": version },
            "contentChanges": [{
                "range": {
                    "start": { "line": 0, "character": start },
                    "end": { "line": 0, "character": end },
                },
                "text": text,
            }],
        }),
    )
}

#[test]
fn full_lifecycle_over_memory_connection() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        serve_on::<Ctx, TestLang<true>>(server, TestLang::new(), Documents::new())
    });

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    let caps = next_response(&client, 1)["capabilities"].clone();
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["documentSymbolProvider"], true);
    assert_eq!(caps["textDocumentSync"], 2);

    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    // Open a broken file: the right region contains '!'.
    client.sender.send(did_open("abcdef!gij"))?;
    let (version, diags) = next_diagnostics(&client);
    assert_eq!(version, Some(0));
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].range.start,
        Position {
            line: 0,
            character: 4
        }
    );
    assert!(diags[0].message.contains("bang"));

    // Symbols list both regions.
    client.sender.send(request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": URI}}),
    ))?;
    let symbols = next_response(&client, 2);
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["file", "left", "right:e"]);

    // Fix the bang: diagnostics clear.
    client.sender.send(did_change(1, 6, 7, "X"))?;
    let (version, diags) = next_diagnostics(&client);
    assert_eq!(version, Some(1));
    assert_eq!(diags.len(), 0);

    // Unknown requests get MethodNotFound.
    client
        .sender
        .send(request(3, "textDocument/hover", serde_json::json!({})))?;
    for _ in 0..10 {
        if let Message::Response(Response {
            id: got,
            response_result: Err(err),
        }) = next_message(&client)
        {
            if got == RequestId::from(3) {
                assert_eq!(err.code, lsp_server::ErrorCode::MethodNotFound as i32);
                break;
            }
        }
    }

    // Shutdown handshake ends the loop.
    client
        .sender
        .send(request(4, "shutdown", serde_json::Value::Null))?;
    assert_eq!(next_response(&client, 4), serde_json::Value::Null);
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;

    server_thread.join().unwrap()?;
    Ok(())
}

#[test]
fn symbols_off_answers_method_not_found() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        serve_on::<Ctx, TestLang<false>>(server, TestLang::new(), Documents::new())
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
    client.sender.send(did_open("abcd!fgh"))?;
    let (_, diags) = next_diagnostics(&client);
    assert_eq!(diags.len(), 1);

    client.sender.send(request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": URI}}),
    ))?;
    for _ in 0..10 {
        if let Message::Response(Response {
            id: got,
            response_result: Err(err),
        }) = next_message(&client)
        {
            if got == RequestId::from(2) {
                assert_eq!(err.code, lsp_server::ErrorCode::MethodNotFound as i32);
                break;
            }
        }
    }

    client
        .sender
        .send(request(3, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;
    Ok(())
}

#[test]
fn close_clears_diagnostics_and_forgets_document() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        serve_on::<Ctx, TestLang<true>>(server, TestLang::new(), Documents::new())
    });

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    let _ = next_response(&client, 1);
    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    client.sender.send(did_open("abcd!fgh"))?;
    let _ = next_diagnostics(&client);

    client.sender.send(notification(
        "textDocument/didClose",
        serde_json::json!({"textDocument": {"uri": URI}}),
    ))?;
    let (version, diags) = next_diagnostics(&client);
    assert_eq!(version, None, "close publishes an unversioned empty set");
    assert_eq!(diags.len(), 0);

    // A symbol request for the closed document answers empty, not error.
    client.sender.send(request(
        2,
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": URI}}),
    ))?;
    assert_eq!(next_response(&client, 2), serde_json::json!([]));

    client
        .sender
        .send(request(3, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;
    Ok(())
}

#[test]
fn change_events_for_unknown_document_are_ignored() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        serve_on::<Ctx, TestLang<true>>(server, TestLang::new(), Documents::new())
    });

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    let _ = next_response(&client, 1);
    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    client.sender.send(notification(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": { "uri": "file:///ghost.mini", "version": 3 },
            "contentChanges": [{
                "range": { "start": {"line":0,"character":0}, "end": {"line":0,"character":0} },
                "text": "x",
            }],
        }),
    ))?;

    // Still alive: a fresh open works fine afterwards.
    client.sender.send(did_open("abcd!fgh"))?;
    let (_, diags) = next_diagnostics(&client);
    assert_eq!(diags.len(), 1);

    client
        .sender
        .send(request(2, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;
    Ok(())
}
