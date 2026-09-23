//! A small but complete language server for the mini language.
//!
//! Everything protocol-shaped (initialize, document bookkeeping, change
//! translation, diagnostics publishing, symbol dispatch, shutdown) is owned
//! by [`incraparse_lsp::serve`]; this file is only MiniLang's *content*:
//!
//! * three passes — plain closures, no structs,
//! * how a failing region becomes a diagnostic,
//! * how context labels become outline symbols.
//!
//! The equivalent fully hand-written loop lives in
//! `examples/manual_server.rs`. Try this server with any LSP client, e.g.
//! VS Code + a launch config pointing at
//! `cargo run -p incraparse-lsp --example mini_lang_server`, on a file like:
//!
//! ```text
//! def add(a, b) { return a + b; }
//! def bad(x) { return; }
//! ```

use std::error::Error;

use incraparse::prelude::*;
use incraparse_lsp::prelude::*;
use lsp_types::{Diagnostic, DocumentSymbol};

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

/// Round 0: the file -> one region per `def name(params) { ... }`.
fn functions_pass(source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
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

/// Round 1: function bodies -> `return ...;` regions, with the function's
/// name threaded into each child's context.
fn body_pass(source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
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

/// Round 2: the checker — an empty `return;` fails and stays as a leaf.
fn return_pass(source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
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
        Outcome::Failed
    } else {
        Outcome::Done
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let engine = Engine::with((
        pass_fn(functions_pass),
        pass_fn(body_pass),
        pass_fn(return_pass),
    ));

    let language = SimpleLanguage::new(engine, LangCtx::File)
        .diagnostic_fn(|_doc, node| match node.ctx {
            LangCtx::Return { function } => Some(Diagnostic {
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                message: format!("`{function}`: this `return` could not be parsed"),
                source: Some("minilang".into()),
                ..Diagnostic::default()
            }),
            _ => None,
        })
        .label_fn(|ctx| match ctx {
            LangCtx::Function { name, params } => {
                Some(NodeLabel::new(name.clone()).detail(format!("({})", params.join(", "))))
            }
            _ => None,
        })
        .symbols_fn(|doc| {
            // Full control when labels aren't enough: same result here, but
            // read straight off the tree.
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
        });

    incraparse_lsp::serve(language)
}
