//! A small but complete language server for the mini language, built on the
//! framework-agnostic `increparse-lsp` adapter and the sync `lsp-server`
//! scaffold.
//!
//! Demonstrates the whole story:
//!
//! * `didOpen`/`didChange` -> [`Document::apply_changes`] -> incremental
//!   re-parse (only edited regions re-run their passes),
//! * failing regions -> publishable diagnostics,
//! * `textDocument/documentSymbol` -> function symbols read straight off the
//!   parse tree, available even while some regions are still broken.
//!
//! Try it with any LSP client, e.g. VS Code + a launch config pointing at
//! `cargo run -p increparse-lsp --example mini_lang_server`, on a file like:
//!
//! ```text
//! def add(a, b) { return a + b; }
//! def bad(x) { return; }
//! ```

use std::collections::HashMap;
use std::error::Error;

use increparse::{CancelToken, Engine, Outcome, Pass, Schedule, SerialExecutor, Span};
use increparse_lsp::{diagnostics, DiagnosticsOptions, Document, PositionEncoding};
use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbol, DocumentSymbolParams, OneOf, PublishDiagnosticsParams,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum LangCtx {
    File,
    Function { name: String, params: Vec<String> },
    Return { function: String },
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn read_ident(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i >= bytes.len() || !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
        return None;
    }
    let mut end = i + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    Some((i, end))
}

fn match_brace(bytes: &[u8], open: usize, limit: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().take(limit).skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_params(bytes: &[u8], start: usize, end: usize) -> Option<Vec<String>> {
    let text = std::str::from_utf8(&bytes[start..end]).ok()?;
    let mut params = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let ident = read_ident(part.as_bytes(), 0)?;
        if ident.1 != part.len() {
            return None;
        }
        params.push(part.to_string());
    }
    Some(params)
}

struct FunctionsPass;

impl Pass for FunctionsPass {
    type Ctx = LangCtx;

    fn parse(&self, source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
        if !matches!(ctx, LangCtx::File) {
            return Outcome::Failed;
        }
        let bytes = source.as_bytes();
        let mut children = Vec::new();
        let mut i = span.start;
        while i < span.end {
            i = skip_ws(bytes, i);
            let Some((kw_start, kw_end)) = read_ident(bytes, i) else {
                i += 1;
                continue;
            };
            if &source[kw_start..kw_end] != "def" {
                i = kw_end;
                continue;
            }
            let name_pos = skip_ws(bytes, kw_end);
            let Some((name_start, name_end)) = read_ident(bytes, name_pos) else {
                i = kw_end;
                continue;
            };
            let open_paren = skip_ws(bytes, name_end);
            if open_paren >= span.end || bytes[open_paren] != b'(' {
                i = kw_end;
                continue;
            }
            let line_end = (open_paren..span.end)
                .find(|&j| bytes[j] == b'\n')
                .unwrap_or(span.end);
            let Some(close_paren) = (open_paren + 1..line_end).find(|&j| bytes[j] == b')') else {
                i = line_end + 1;
                continue;
            };
            let Some(params) = parse_params(bytes, open_paren + 1, close_paren) else {
                i = close_paren + 1;
                continue;
            };
            let open_brace = skip_ws(bytes, close_paren + 1);
            if open_brace >= span.end || bytes[open_brace] != b'{' {
                i = close_paren + 1;
                continue;
            }
            let Some(close_brace) = match_brace(bytes, open_brace, span.end) else {
                break;
            };
            children.push((
                Span::new(i, close_brace + 1, span.rev),
                LangCtx::Function {
                    name: source[name_start..name_end].to_string(),
                    params,
                },
            ));
            i = close_brace + 1;
        }
        Outcome::Expand(children)
    }
}

struct BodyPass;

impl Pass for BodyPass {
    type Ctx = LangCtx;

    fn parse(&self, source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
        let LangCtx::Function { name, .. } = ctx else {
            return Outcome::Failed;
        };
        let bytes = source.as_bytes();
        let mut children = Vec::new();
        let mut i = span.start;
        while i < span.end {
            i = skip_ws(bytes, i);
            if source[i..].starts_with("return") {
                let Some(semi) = (i..span.end).find(|&j| bytes[j] == b';') else {
                    break;
                };
                children.push((
                    Span::new(i, semi + 1, span.rev),
                    LangCtx::Return {
                        function: name.clone(),
                    },
                ));
                i = semi + 1;
            } else {
                i += 1;
            }
        }
        Outcome::Expand(children)
    }
}

struct ReturnPass;

impl Pass for ReturnPass {
    type Ctx = LangCtx;

    fn parse(&self, source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
        let LangCtx::Return { function: _ } = ctx else {
            return Outcome::Failed;
        };
        let text = source[span.to_range()].trim();
        let expr = text
            .strip_prefix("return")
            .and_then(|rest| rest.strip_suffix(';'))
            .map(str::trim)
            .unwrap_or("");
        if expr.is_empty() {
            return Outcome::Failed;
        }
        Outcome::Done
    }
}

fn make_engine() -> Engine<LangCtx> {
    let mut schedule = Schedule::new();
    schedule.push(FunctionsPass);
    schedule.push(BodyPass);
    schedule.push(ReturnPass);
    Engine::new(schedule)
}

type Doc = Document<LangCtx>;

fn publish_diagnostics(
    connection: &Connection,
    doc: &Doc,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let diags = diagnostics(doc, DiagnosticsOptions::default(), |node| match node.ctx {
        LangCtx::Return { function } => Some(lsp_types::Diagnostic {
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            message: format!("`{function}`: this `return` could not be parsed"),
            source: Some("increparse-mini-lang".into()),
            ..lsp_types::Diagnostic::default()
        }),
        _ => None,
    });

    let params = PublishDiagnosticsParams {
        uri: doc.uri().clone(),
        diagnostics: diags,
        version: Some(doc.version()),
    };
    connection
        .sender
        .send(Message::Notification(lsp_server::Notification::new(
            "textDocument/publishDiagnostics".into(),
            params,
        )))?;
    Ok(())
}

fn document_symbols(doc: &Doc) -> Vec<DocumentSymbol> {
    let tree = doc.session().tree();
    let mut symbols = Vec::new();
    for id in tree.nodes() {
        if let LangCtx::Function { name, params } = tree.ctx(id) {
            let range = doc.range(tree.span(id));
            #[allow(deprecated)]
            symbols.push(DocumentSymbol {
                name: name.clone(),
                detail: Some(format!("({})", params.join(", "))),
                kind: lsp_types::SymbolKind::FUNCTION,
                range,
                selection_range: range,
                children: None,
                tags: None,
                deprecated: None,
            });
        }
    }
    symbols
}

#[allow(clippy::mutable_key_type)]
fn main_loop(
    connection: Connection,
    engine: Engine<LangCtx>,
    documents: HashMap<lsp_types::Uri, Doc>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut documents = documents;
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    break;
                }
                match req.method.as_str() {
                    "textDocument/documentSymbol" => {
                        let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
                        let symbols = documents
                            .get(&params.text_document.uri)
                            .map(document_symbols)
                            .unwrap_or_default();
                        connection
                            .sender
                            .send(Message::Response(lsp_server::Response::new_ok(
                                req.id, symbols,
                            )))?;
                    }
                    _ => {
                        connection.sender.send(Message::Response(
                            lsp_server::Response::new_err(
                                req.id,
                                lsp_server::ErrorCode::MethodNotFound as i32,
                                "method not supported".into(),
                            ),
                        ))?;
                    }
                }
            }
            Message::Notification(notification) => {
                let Notification { method, params, .. } = notification;
                match method.as_str() {
                    "textDocument/didOpen" => {
                        let params: DidOpenTextDocumentParams = serde_json::from_value(params)?;
                        let item = params.text_document;
                        let mut doc = Doc::open(
                            item.uri.clone(),
                            item.version,
                            item.language_id.clone(),
                            item.text,
                            PositionEncoding::Utf16,
                            LangCtx::File,
                        );
                        doc.apply_changes(
                            &engine,
                            item.version,
                            &[],
                            &SerialExecutor,
                            &CancelToken::new(),
                        );
                        publish_diagnostics(&connection, &doc)?;
                        documents.insert(item.uri, doc);
                    }
                    "textDocument/didChange" => {
                        let params: DidChangeTextDocumentParams = serde_json::from_value(params)?;
                        let uri = params.text_document.uri.clone();
                        if let Some(doc) = documents.get_mut(&uri) {
                            doc.apply_changes(
                                &engine,
                                params.text_document.version,
                                &params.content_changes,
                                &SerialExecutor,
                                &CancelToken::new(),
                            );
                            publish_diagnostics(&connection, doc)?;
                        }
                    }
                    "textDocument/didClose" => {
                        let params: DidCloseTextDocumentParams = serde_json::from_value(params)?;
                        let uri = params.text_document.uri;
                        documents.remove(&uri);
                        connection.sender.send(Message::Notification(
                            lsp_server::Notification::new(
                                "textDocument/publishDiagnostics".into(),
                                PublishDiagnosticsParams {
                                    uri,
                                    diagnostics: Vec::new(),
                                    version: None,
                                },
                            ),
                        ))?;
                    }
                    _ => {}
                }
            }
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = lsp_types::ServerCapabilities {
        position_encoding: Some(PositionEncoding::Utf16.capability()),
        text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Kind(
            lsp_types::TextDocumentSyncKind::INCREMENTAL,
        )),
        document_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    let _initialization_params = connection.initialize(serde_json::to_value(capabilities)?)?;

    // `main_loop` takes the `Connection` by value: dropping it closes the
    // writer channel, which is what lets `io_threads.join()` return after
    // the `exit` notification.
    main_loop(connection, make_engine(), HashMap::new())?;
    io_threads.join()?;
    Ok(())
}
