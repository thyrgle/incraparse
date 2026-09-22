//! Parses a tiny language of function definitions in three passes:
//!
//! ```text
//! def add(a, b) { return a + b; }
//! ```
//!
//! * Round 0 (`FunctionsPass`): scans the file for `def name(params) { ... }`
//!   skeletons; each match becomes a child region carrying the function's
//!   name and params as context. Malformed regions are skipped.
//! * Round 1 (`BodyPass`): scans each function body for `return expr;`
//!   statements, expanding them into child regions.
//! * Round 2 (`ReturnPass`): validates each return statement's expression is
//!   non-empty and accepts it.
//!
//! Run with `cargo run --example mini_lang`.

use incraparse::{
    CancelToken, Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Span, Status,
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
        let LangCtx::Return { function } = ctx else {
            return Outcome::Failed;
        };
        let text = source[span.to_range()].trim();
        let expr = text
            .strip_prefix("return")
            .and_then(|rest| rest.strip_suffix(';'))
            .map(str::trim)
            .unwrap_or("");
        if expr.is_empty() {
            eprintln!("  !! empty return in `{function}` at {span}");
            return Outcome::Failed;
        }
        Outcome::Done
    }
}

fn dump<C: std::fmt::Debug>(tree: &ParseTree<C>, source: &str, id: incraparse::NodeId, depth: usize) {
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

fn main() {
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
    let mut tree: ParseTree<LangCtx> =
        ParseTree::new(0, Span::new(0, source.len(), 0), LangCtx::File);

    let report = engine.run(source, &mut tree, &SerialExecutor, &CancelToken::new());

    println!("report: {report:?}");
    println!("counts: {:?}\n", tree.status_counts());
    dump(&tree, source, tree.root(), 0);

    let statuses: Vec<Status> = tree.nodes().map(|id| tree.status(id)).collect();
    assert!(
        statuses.contains(&Status::Failed),
        "the `bad` function's empty return should fail"
    );
    assert!(
        !tree
            .nodes()
            .any(|id| matches!(tree.ctx(id), LangCtx::Function { name, .. } if name == "noise")),
        "the malformed `def noise(` should never have been captured"
    );
    println!("\ntree settled: {}", report.reached_fixpoint);
}
