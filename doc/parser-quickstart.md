# Your first parser in 30 minutes

This guide builds a **parser** for a tiny configuration language — no LSP,
no editor, no servers. Just incraparse, a few closures, and a tree you can
inspect. When it works, continue to [`doc/lsp-tutorial.md`](lsp-tutorial.md)
to put it in an editor.

The language is the classic INI format — deliberately *not* a
functions-and-statements language, to show that incraparse doesn't care
what shape your language has:

```ini
[owner]
name = Ada
born = 1815

[database]
enabled = true
```

## 1. Setup

```console
$ cargo new ini-parser
$ cd ini-parser
$ cargo add incraparse
```

## 2. The language, in three passes

incraparse runs **schedules of passes** over a growing tree. A pass is a
closure from `(source, region, context)` to an outcome:

- `{ expand = ... }` — the region contains smaller regions; their contexts
  describe what they are,
- `"done"` — fully parsed,
- `"failed"` — couldn't handle it; a later pass may retry.

For INI: round 0 finds `[section]` regions; round 1 finds `key = value`
lines inside each section. Replace `src/main.rs` with:

```rust
use incraparse::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum IniCtx {
    File,
    Section { name: String },
    Key { key: String, value: String, section: String },
}

/// Round 0: the whole file -> one region per `[section]`.
/// A section spans from its `[` to the next `[` (or end of file).
fn sections_pass(source: &str, span: Span, ctx: &IniCtx) -> Outcome<IniCtx> {
    if !matches!(ctx, IniCtx::File) {
        return Outcome::Failed;
    }
    let mut children = Vec::new();
    let bytes = source.as_bytes();
    let mut i = span.start;
    while i < span.end {
        if bytes[i] == b'[' {
            let Some(close) = source[i..].find(']').map(|j| i + j) else {
                i += 1;
                continue;
            };
            let name = source[i + 1..close].to_string();
            let end = source[close + 1..]
                .find('[')
                .map(|j| close + 1 + j)
                .unwrap_or(span.end);
            children.push((Span::new(i, end, span.rev), IniCtx::Section { name }));
            i = end;
        } else {
            i += 1;
        }
    }
    Outcome::Expand(children)
}

/// Round 1: sections -> `key = value` regions, with the section's name
/// threaded into each key's context.
fn keys_pass(source: &str, span: Span, ctx: &IniCtx) -> Outcome<IniCtx> {
    let IniCtx::Section { name } = ctx else {
        return Outcome::Failed;
    };
    let section = name.clone();
    let mut children = Vec::new();
    let mut offset = span.start;
    for line in source[span.to_range()].split_inclusive('\n') {
        let text = line.trim_end();
        if let Some((key, value)) = text.split_once('=') {
            children.push((
                Span::new(offset, offset + text.len(), span.rev),
                IniCtx::Key {
                    key: key.trim().to_string(),
                    value: value.trim().to_string(),
                    section: section.clone(),
                },
            ));
        }
        offset += line.len();
    }
    Outcome::Expand(children)
}

/// Round 2: the checker — an empty value fails.
fn validate_pass(_source: &str, _span: Span, ctx: &IniCtx) -> Outcome<IniCtx> {
    let IniCtx::Key { value, .. } = ctx else {
        return Outcome::Failed;
    };
    if value.is_empty() {
        Outcome::Failed
    } else {
        Outcome::Done
    }
}

fn main() {
    let source = "[owner]\nname = Ada\nborn = 1815\n\n[database]\nenabled = true\n";
    let (tree, report) = run(
        source,
        (pass_fn(sections_pass), pass_fn(keys_pass), pass_fn(validate_pass)),
        IniCtx::File,
    );

    println!("{report:?}");
    println!("{:?}", tree.status_counts());
    for id in tree.nodes() {
        println!("{:?} {:?} {:?}", id, tree.span(id), tree.ctx(id));
    }
}
```

```console
$ cargo run
```

You should see: the root `Expanded` into two `Section` regions, each
`Expanded` into `Key` regions that are all `Done`, and
`reached_fixpoint: true` — the schedule ran until nothing was left to do.

## 3. When you get it wrong (and you will)

Passes have one contract: **every child region must be contained in the
region it was parsed from.** Break it on purpose — say, a copy-paste bug
that emits a child past the section's end:

```rust,ignore
// Oops: `source.len()` instead of the section end.
children.push((Span::new(offset, source.len(), span.rev), IniCtx::Key { /* ... */ }));
```

The engine refuses the bad child and the run report tells you exactly what
happened, in plain language:

```text
report.violations = [Violation {
    node: NodeId(1), round: 1, pass: Some("PassFn"),
    span: 9..72, kind: OutsideParent,
}]
violations[0].to_string() ==
  "PassFn (round 1) produced child region 9..72, which escapes its parent
   region — children must be contained in the region they were parsed from"
```

No silent corruption, no panic — the offending region just fails, and the
rest of the file keeps parsing.

## 4. Testing your passes

`incraparse::run` is designed for tests too:

```rust
#[test]
fn sections_are_found() {
    let (tree, report) = run(
        "[a]\nx = 1\n[b]\ny = 2\n",
        (pass_fn(sections_pass), pass_fn(keys_pass), pass_fn(validate_pass)),
        IniCtx::File,
    );
    assert!(report.reached_fixpoint);
    let names: Vec<&str> = tree
        .children(tree.root())
        .iter()
        .map(|id| match tree.ctx(*id) {
            IniCtx::Section { name } => name.as_str(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(names, ["a", "b"]);
}
```

## Next steps

- **Put it in an editor:** [`doc/lsp-tutorial.md`](lsp-tutorial.md) wires a
  parser like this into VS Code and Neovim as a real language server, with
  diagnostics and an outline.
- **Bigger grammars:** when hand-rolled scanning stops being fun, wrap your
  existing `nom` or `chumsky` grammar — see the adapter crates and the
  appendix at the end of the LSP tutorial.
- **Prefer Lua?** `incraparse-lua` defines whole servers in one Lua file.
