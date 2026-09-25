//! The mini language's function skeleton parsed with **nom** instead of
//! hand-rolled scanning — the `increparse-nom` showcase.
//!
//! Compare with `examples/mini_lang.rs` in the core crate: the pass is the
//! same shape, but the scanning is nom combinators emitting *slice-relative*
//! ranges; `increparse_nom::nom_pass` rebases them to absolute spans so the
//! incremental tree can reuse regions across edits.
//!
//! Note the mixed schedule: round 0 is a nom pass, rounds 1–2 are
//! hand-rolled. Passes are independent — mix freely.
//!
//! Run with `cargo run -p increparse-nom --example mini_lang_nom`.

use std::error::Error;
use std::ops::Range;

use increparse::{
    CancelToken, Engine, NodeId, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Session, Span,
    Status,
};
use increparse_nom::{nom_pass, NomChildren};
use nom::branch::alt;
use nom::bytes::complete::{tag, take, take_till, take_while1};
use nom::character::complete::{anychar, multispace0};
use nom::combinator::{map, opt, recognize};
use nom::multi::many0;
use nom::IResult;
use nom::Parser;
use nom_locate::LocatedSpan;

type Located<'a> = LocatedSpan<&'a str>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LangCtx {
    File,
    Function { name: String, params: Vec<String> },
    Return { function: String },
}

fn ws(i: Located) -> IResult<Located, Located> {
    recognize(multispace0).parse(i)
}

fn ident(i: Located) -> IResult<Located, Located> {
    let (i, s) = take_while1(|c: char| c.is_ascii_alphanumeric() || c == '_').parse(i)?;
    if s.fragment()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        Ok((i, s))
    } else {
        Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Verify,
        )))
    }
}

/// Recognizes a balanced `{ ... }` block. Combinators don't count nesting,
/// so this is a tiny custom scanner wearing a nom signature.
fn braced(i: Located) -> IResult<Located, Range<usize>> {
    let base = i.location_offset();
    let bytes = i.fragment().as_bytes();
    if bytes.first() != Some(&b'{') {
        return Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::Tag,
        )));
    }
    let mut depth = 0usize;
    let mut len = None;
    for (k, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    len = Some(k + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(len) = len else {
        return Err(nom::Err::Error(nom::error::Error::new(
            i,
            nom::error::ErrorKind::TakeWhile1,
        )));
    };
    let (i, _) = take(len).parse(i)?;
    Ok((i, base..base + len))
}

fn param_list(i: Located) -> IResult<Located, Vec<String>> {
    let (i, text) = take_till(|c| c == ')' || c == '\n')(i)?;
    let mut params = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if ident(LocatedSpan::new(part)).is_err() {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        params.push(part.to_string());
    }
    Ok((i, params))
}

/// One `def name(params) { ... }`, as a relative range plus context.
fn function_def(i: Located) -> IResult<Located, (Range<usize>, LangCtx)> {
    let start = i.location_offset();
    let (i, _) = tag("def").parse(i)?;
    let (i, name_loc) = recognize((ws, ident, multispace0)).parse(i)?;
    let name = name_loc.fragment().trim().to_string();
    let (i, _) = tag("(").parse(i)?;
    let (i, params) = param_list.parse(i)?;
    let (i, _) = tag(")").parse(i)?;
    let (i, _) = recognize((multispace0, opt(tag(";")))).parse(i)?;
    let (i, body) = braced.parse(i)?;
    let end = body.end;
    Ok((
        i,
        (
            start..end,
            LangCtx::Function {
                name: name.trim_end().to_string(),
                params,
            },
        ),
    ))
}

/// The file: skip garbage byte by byte, collect well-formed functions.
/// Malformed definitions (like `def noise( {`) cost one byte of resync, and
/// everything after them still parses.
fn functions_pass_fn(i: Located) -> IResult<Located, NomChildren<LangCtx>> {
    let (i, defs) = many0(alt((map(function_def, Some), map(anychar, |_| None)))).parse(i)?;
    Ok((i, defs.into_iter().flatten().collect()))
}

struct FunctionsPass;

impl Pass for FunctionsPass {
    type Ctx = LangCtx;

    fn parse(&self, source: &str, span: Span, ctx: &LangCtx) -> Outcome<LangCtx> {
        if !matches!(ctx, LangCtx::File) {
            return Outcome::Failed;
        }
        nom_pass(functions_pass_fn).parse(source, span, ctx)
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

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
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
        "three well-formed functions captured via nom"
    );
    assert!(
        session
            .tree()
            .nodes()
            .any(|id| session.tree().status(id) == Status::Failed),
        "bad's empty return failed"
    );

    // Incremental: append a function — only the new chain is re-parsed.
    let mut source = source.to_string();
    let appended = "\ndef ten() { return 10; }\n";
    session.edit(increparse::Edit::insert(source.len(), appended.len()));
    source.push_str(appended);
    let report = session.run(&engine, &source, &SerialExecutor, &CancelToken::new());
    println!("\nafter append: {report:?}");
    assert_eq!(report.nodes_processed, 3, "root + `ten` + its return only");

    Ok(())
}
