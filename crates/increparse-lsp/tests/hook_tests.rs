//! Tests for the interactive hooks (describe_fn / hover_fn / definition_fn
//! / completion_fn) served over a `Connection::memory()` pair.

use std::time::Duration;

use increparse::prelude::*;
use increparse_lsp::prelude::*;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{CompletionItem, CompletionResponse, Diagnostic, Hover, Location};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctx {
    File,
    Word { name: &'static str },
}

/// Round 0: splits the file at byte 4 into two named regions.
struct Split;

impl Pass for Split {
    type Ctx = Ctx;

    fn parse(&self, _source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        if !matches!(ctx, Ctx::File) {
            return Outcome::Failed;
        }
        let split = (span.start + 4).min(span.end);
        Outcome::Expand(vec![
            (
                Span::new(span.start, split, span.rev),
                Ctx::Word { name: "left" },
            ),
            (
                Span::new(split, span.end, span.rev),
                Ctx::Word { name: "right" },
            ),
        ])
    }
}

/// Round 1: settles regions, and records which byte was last hovered /
/// defined / completed.
struct Settle {
    seen: Arc<AtomicUsize>,
}

impl Pass for Settle {
    type Ctx = Ctx;

    fn parse(&self, _source: &str, span: Span, _ctx: &Ctx) -> Outcome<Ctx> {
        self.seen.fetch_add(span.len(), Ordering::SeqCst);
        Outcome::Done
    }
}

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn language() -> SimpleLanguage<Ctx> {
    SimpleLanguage::new(
        Engine::with((
            Split,
            Settle {
                seen: Arc::new(AtomicUsize::new(0)),
            },
        )),
        Ctx::File,
    )
    .describe_fn(|ctx| match ctx {
        Ctx::File => None,
        Ctx::Word { name } => Some(format!("the {name} region")),
    })
    .definition_fn(|doc, offset| {
        // Go-to-definition: jump to the start of whichever region the
        // cursor is in (toy behaviour, but exercises the plumbing).
        let tree = doc.session().tree();
        let id = tree.node_at(offset)?;
        Some(vec![doc.location(tree.span(id))])
    })
    .completion_fn(|_doc, _offset| {
        Some(CompletionResponse::Array(vec![CompletionItem {
            label: "left".into(),
            ..Default::default()
        }]))
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

#[test]
fn interactive_hooks_serve_end_to_end() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (server, client) = Connection::memory();
    let server_thread =
        std::thread::spawn(move || serve_on::<Ctx, _>(server, language(), Documents::new()));

    client.sender.send(request(
        1,
        "initialize",
        serde_json::json!({"capabilities": {}}),
    ))?;
    for _ in 0..10 {
        match client.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Response(Response {
                id: got,
                response_result,
            })) if got == RequestId::from(1) => {
                let caps = response_result.expect("ok")["capabilities"].clone();
                assert_eq!(caps["hoverProvider"], true);
                assert_eq!(caps["definitionProvider"], true);
                assert!(caps["completionProvider"].is_object());
                break;
            }
            _ => continue,
        }
    }
    client
        .sender
        .send(notification("initialized", serde_json::json!({})))?;

    client.sender.send(notification(
        "textDocument/didOpen",
        serde_json::json!({
            "textDocument": {"uri": "file:///h.mini", "languageId": "mini", "version": 0,
                             "text": "abcdef!ghij"},
        }),
    ))?;

    // Hover at byte 6 (inside the right region): describe_fn's sentence,
    // with the node's range attached.
    client.sender.send(request(
        2,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": "file:///h.mini"},
            "position": {"line": 0, "character": 6},
        }),
    ))?;
    for _ in 0..10 {
        match client.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Response(Response {
                id: got,
                response_result,
            })) if got == RequestId::from(2) => {
                let hover: Hover = serde_json::from_value(response_result.expect("hover"))?;
                match hover.contents {
                    lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(text)) => {
                        assert_eq!(text, "the right region");
                    }
                    other => panic!("unexpected hover contents: {other:?}"),
                }
                assert!(hover.range.is_some());
                break;
            }
            _ => continue,
        }
    }

    // Definition at byte 6: one location in this document.
    client.sender.send(request(
        3,
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": "file:///h.mini"},
            "position": {"line": 0, "character": 6},
        }),
    ))?;
    for _ in 0..10 {
        match client.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Response(Response {
                id: got,
                response_result,
            })) if got == RequestId::from(3) => {
                let locations: Vec<Location> =
                    serde_json::from_value(response_result.expect("locations"))?;
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].uri.as_str(), "file:///h.mini");
                break;
            }
            _ => continue,
        }
    }

    // Completion: the hook's array comes back.
    client.sender.send(request(
        4,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": {"uri": "file:///h.mini"},
            "position": {"line": 0, "character": 6},
        }),
    ))?;
    for _ in 0..10 {
        match client.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Response(Response {
                id: got,
                response_result,
            })) if got == RequestId::from(4) => {
                let response: CompletionResponse =
                    serde_json::from_value(response_result.expect("completions"))?;
                match response {
                    CompletionResponse::Array(items) => {
                        assert_eq!(items[0].label, "left");
                    }
                    other => panic!("unexpected completion response: {other:?}"),
                }
                break;
            }
            _ => continue,
        }
    }

    client
        .sender
        .send(request(5, "shutdown", serde_json::Value::Null))?;
    client
        .sender
        .send(notification("exit", serde_json::Value::Null))?;
    server_thread.join().unwrap()?;
    Ok(())
}

#[test]
fn describe_fn_and_hover_fn_are_shape_agnostic() {
    // A language of *sections and keys* — no functions anywhere. The hooks
    // only ever see contexts and byte offsets.
    let config = SimpleLanguage::new(
        Engine::with((pass_fn(|_s: &str, _p, _c: &Ctx| Outcome::Done),)),
        Ctx::File,
    )
    .diagnostic_fn(|_doc, node| {
        Some(Diagnostic {
            message: format!("status: {:?}", node.status),
            ..Diagnostic::default()
        })
    })
    .describe_fn(|ctx| match ctx {
        Ctx::File => None,
        Ctx::Word { name } => Some(format!("a {name} section of the config")),
    })
    .definition_fn(|doc, offset| {
        let tree = doc.session().tree();
        tree.node_at(offset)
            .map(|id| vec![doc.location(tree.span(id))])
    })
    .completion_fn(|_doc, _offset| None)
    .label_fn(|ctx| match ctx {
        Ctx::File => None,
        Ctx::Word { name } => Some(NodeLabel::new(*name).kind(lsp_types::SymbolKind::STRUCT)),
    });

    assert!(config.supports_symbols());
    assert!(config.supports_hover());
    assert!(config.supports_definition());
    assert!(config.supports_completion());
}
