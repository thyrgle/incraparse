//! The mini language's function skeleton parsed with **chumsky** — the
//! `incraparse-chumsky` showcase.
//!
//! The pass emits *slice-relative* [`SimpleSpan`]s; `incraparse_chumsky::chumsky_pass`
//! rebases them to absolute spans so the incremental tree can reuse regions
//! across edits.
//!
//! chumsky notes baked into this example:
//!
//! * parsers are built in a factory fn tied to the input lifetime,
//! * `Parser::parse` demands whole-input consumption, so the file scanner
//!   ends with `.then_ignore(any().repeated())` and the round-1 pass
//!   consumes the entire region,
//! * malformed definitions are skipped by a one-byte fallback
//!   (`func.or(any())` inside `repeated`) — error resilience without
//!   leaving the combinator.
//!
//! Run with `cargo run -p incraparse-chumsky --example mini_lang_chumsky`.

use std::error::Error;

use chumsky::prelude::*;
use incraparse::{
    CancelToken, Engine, NodeId, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Session, Span,
};
use incraparse_chumsky::{chumsky_pass, ChumChildren};

#[derive(Clone, Debug, PartialEq, Eq)]
enum LangCtx {
    File,
    Function { name: String, params: Vec<String> },
    Return { function: String },
}

/// Round 0: every `def name(params) { ... }` in the file.
///
/// MiniLang bodies contain no nested braces, so a body is "everything up to
/// the next `}`"; malformed definitions are skipped by `recover_with`.
fn functions_parser<'a>(
) -> impl Parser<'a, &'a str, ChumChildren<LangCtx>, extra::Err<Rich<'a, char>>> {
    let ident = text::ident().map(|s: &str| s.to_string());
    let params = ident
        .padded()
        .separated_by(just(',').padded())
        .allow_trailing()
        .collect::<Vec<String>>()
        .delimited_by(just('('), just(')'))
        .padded();
    // No nested braces in MiniLang bodies.
    let body = none_of('}').repeated().to_slice();

    just("def")
        .ignore_then(ident.padded())
        .then(params)
        .then(just('{').ignore_then(body).then_ignore(just('}')))
        .map_with(
            |((name, params), _body): ((String, Vec<String>), &str), e| {
                let span: SimpleSpan = e.span();
                vec![(span, LangCtx::Function { name, params })]
            },
        )
        .padded()
        .or(any().to(Vec::new()))
        .repeated()
        .collect::<Vec<Vec<(SimpleSpan, LangCtx)>>>()
        .map(|defs| {
            defs.into_iter()
                .flatten()
                .collect::<Vec<(SimpleSpan, LangCtx)>>()
        })
        .then_ignore(any().repeated())
}

struct FunctionsPass;

impl Pass for FunctionsPass {
    type Ctx = LangCtx;

    fn parse(&self, source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
        if !matches!(ctx, LangCtx::File) {
            return Outcome::Failed;
        }
        chumsky_pass(|slice: &str| functions_parser().parse(slice)).parse(source, span, ctx)
    }
}

/// Round 1: expand `return …;` statements (hand-rolled — passes mix freely).
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
            while i < span.end && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
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

/// Round 2: the checker — an empty `return;` fails and stays as a leaf.
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
            Outcome::Failed
        } else {
            Outcome::Done
        }
    }
}

fn function_ids(tree: &ParseTree<LangCtx>) -> Vec<(String, NodeId)> {
    tree.nodes()
        .filter_map(|id| match tree.ctx(id) {
            LangCtx::Function { name, .. } => Some((name.clone(), id)),
            _ => None,
        })
        .collect()
}

fn dump(tree: &ParseTree<LangCtx>, source: &str, id: NodeId, depth: usize) {
    let indent = "  ".repeat(depth);
    let text = tree.text(source, id).replace('\n', "\\n");
    let text = if text.len() > 40 {
        format!("{}...", &text[..40])
    } else {
        text
    };
    println!(
        "{indent}{} {:?} {} {:?} {:?}",
        depth,
        tree.status(id),
        tree.span(id),
        tree.ctx(id),
        text
    );
    for child in tree.children(id) {
        dump(tree, source, *child, depth + 1);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let source = r#"
def add(a, b) { return a + b; }
def bad(x) { return; }
def noise( { return broken;
def zero() { return 0; }
"#;

    let mut schedule = Schedule::new();
    schedule.push(FunctionsPass);
    schedule.push(BodyPass);
    schedule.push(ReturnPass);
    let engine = Engine::new(schedule);

    let mut session: Session<LangCtx> =
        Session::new(0, Span::new(0, source.len(), 0), LangCtx::File);
    let report = session.run(&engine, source, &SerialExecutor, &CancelToken::new());
    println!("report: {report:?}\n");
    dump(session.tree(), source, session.tree().root(), 0);

    assert!(
        function_ids(session.tree())
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            == ["add", "bad", "zero"],
        "three well-formed functions captured via chumsky"
    );

    // Incremental: append a function — only the new chain is re-parsed.
    let mut source = source.to_string();
    let appended = "\ndef ten() { return 10; }\n";
    session.edit(incraparse::Edit::insert(source.len(), appended.len()));
    source.push_str(appended);
    let report = session.run(&engine, &source, &SerialExecutor, &CancelToken::new());
    println!("\nafter append: {report:?}");
    assert_eq!(report.nodes_processed, 3, "root + `ten` + its return only");

    Ok(())
}
