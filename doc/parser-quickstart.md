# Your first parser in 30 minutes

This guide builds a **parser** for a tiny configuration language — no LSP,
no editor, no servers. Just incraparse, a few closures, and a tree you can
inspect. When it works, continue to [`doc/lsp-tutorial.md`](lsp-tutorial.md)
to put it in an editor. (Would you rather not write Rust at all?
[Section 5](#5-the-same-parser-in-pure-lua) builds the same language in
pure Lua.)

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

## 5. The same parser, in pure Lua

No Rust at all? `incraparse-lua` defines an entire language server — the
passes, the diagnostics, the outline — in **one Lua file**, run by a small
binary. Here is the same INI language, same three-pass schedule. Save this
as `ini.lua`:

```lua
-- The INI language from the quickstart, in pure Lua.
-- Run with: incraparse-lua-server ini.lua

-- Round 0: the whole file -> one region per `[section]`.
-- A section spans from its `[` to the next `[` (or end of file).
local function sections_pass(source, span, ctx)
  if ctx.File == nil then
    return "failed"
  end

  local children = {}
  local i = span.start + 1 -- Lua strings are 1-based; spans are 0-based
  while i <= span["end"] do
    local open = source:find("%[", i)
    if not open then
      break
    end
    local close = source:find("]", open + 1, true)
    if not close then
      break
    end
    local next_open = source:find("%[", close + 1)
    local sec_end = next_open and (next_open - 1) or span["end"]
    children[#children + 1] = {
      start = open - 1,
      ["end"] = sec_end,
      ctx = { Section = { name = source:sub(open + 1, close - 1) } },
    }
    i = sec_end + 1
  end
  return { expand = children }
end

-- Round 1: sections -> `key = value` regions, with the section's name
-- threaded into each key's context.
local function keys_pass(source, span, ctx)
  local section = ctx.Section and ctx.Section.name
  if section == nil then
    return "failed"
  end

  local children = {}
  local offset = span.start
  local line_start = span.start + 1
  while line_start <= span["end"] do
    local nl = source:find("\n", line_start, true)
    local line_end, next_start
    if nl then
      line_end = nl - 1 -- 0-based, exclusive of the newline
      next_start = nl + 1
    else
      line_end = span["end"]
      next_start = span["end"] + 1
    end
    local key, value = source:sub(line_start, line_end):match("^(.-)=(.*)$")
    if key then
      children[#children + 1] = {
        start = offset,
        ["end"] = line_end,
        ctx = {
          Key = {
            key = key:match("^%s*(.-)%s*$"),
            value = value:match("^%s*(.-)%s*$"),
            section = section,
          },
        },
      }
    end
    offset = line_end + 1
    line_start = next_start
  end
  return { expand = children }
end

-- Round 2: the checker — an empty value fails.
local function validate_pass(_source, _span, ctx)
  if ctx.Key == nil then
    return "failed"
  end
  if ctx.Key.value == "" then
    return "failed"
  end
  return "done"
end

return {
  name = "ini",
  root_ctx = { File = true },
  passes = { sections_pass, keys_pass, validate_pass },

  diagnostic = function(source, node)
    local key = node.ctx and node.ctx.Key
    if key and key.value == "" then
      return {
        message = "empty value for `" .. key.key .. "` in [" .. key.section .. "]",
        severity = 1,
      }
    end
    return nil
  end,

  symbols = function(nodes)
    local out = {}
    for _, n in ipairs(nodes) do
      local c = n.ctx
      if c and c.Section then
        out[#out + 1] = {
          name = "[" .. c.Section.name .. "]",
          start = n.start,
          ["end"] = n["end"],
        }
      end
    end
    return out
  end,
}
```

Install the binary that runs it (or `cargo install --path crates/incraparse-lua`
from a checkout of this repository):

```console
$ cargo install incraparse-lua-server
```

A language server speaks LSP over stdio — which you can drive from a
terminal, no editor required. Save this as `poke.py` (Python 3, stdlib
only): it opens a broken INI file, prints the diagnostic, fixes the line,
prints the now-empty diagnostics, then asks for the outline:

```python
import json, subprocess

p = subprocess.Popen(
    ["incraparse-lua-server", "ini.lua"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE)

def send(msg):
    body = json.dumps(msg).encode()
    p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    p.stdin.flush()

def read():
    headers = {}
    while True:
        line = p.stdout.readline().decode().strip()
        if not line:
            break
        k, _, v = line.partition(":")
        headers[k.strip()] = v.strip()
    return json.loads(p.stdout.read(int(headers["Content-Length"])))

TEXT = "[owner]\nname = Ada\nborn = 1815\n\n[database]\nenabled = true\nbroken =\n"

send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}})
read()  # capabilities

send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
    "textDocument": {"uri": "file:///t.ini", "languageId": "ini", "version": 0,
                     "text": TEXT}}})
for d in read()["params"]["diagnostics"]:
    print("diagnostic:", d["message"], "at line", d["range"]["start"]["line"] + 1)

# fix it in place -> the diagnostic clears
send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
    "textDocument": {"uri": "file:///t.ini", "version": 1},
    "contentChanges": [{"range": {"start": {"line": 6, "character": 8},
                                  "end": {"line": 6, "character": 8}},
                        "text": "ok"}]}})
print("after fix:", read()["params"]["diagnostics"])

send({"jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
      "params": {"textDocument": {"uri": "file:///t.ini"}}})
print("symbols:", [s["name"] for s in read()["result"]])

p.terminate()
```

```console
$ python3 poke.py
diagnostic: empty value for `broken` in [database] at line 7
after fix: []
symbols: ['[owner]', '[database]']
```

The engine guarantees are the same as on the Rust path: the key with the
empty value stayed in the tree as a `Failed` leaf — that's what the
`diagnostic` hook turned into a squiggle — and fixing it re-parsed one
line, with both sections above carried over untouched.

To put it in an editor, it's the exact same wiring as the LSP tutorial's
VS Code (section 5) and Neovim (section 6) recipes — only the command
changes, to `incraparse-lua-server /path/to/ini.lua`. The full details,
including ready-made Neovim and VS Code snippets, are in
[Appendix A of the LSP tutorial](lsp-tutorial.md#appendix-a-a-language-server-in-pure-lua).

Three Lua-specific things to know:

- Spans arrive as **0-based** byte offsets (`span.start`, `span["end"]`)
  while Lua strings are 1-based: add 1 on the way in
  (`source:sub(span.start + 1, span["end"])`), subtract 1 on the way out
  (`start = open - 1`). Ranges you return are 0-based and end-exclusive.
- `end` is a Lua keyword, hence the `["end"]` spellings.
- Outcomes are `{ expand = { ... } }`, `"done"`, or `"failed"`/`nil`. A Lua
  error thrown inside a pass is caught and treated as `Failed` — a broken
  pass never takes the server down. Contexts are matched by **deep
  equality** after an edit, so unchanged regions are reused, not re-parsed.

## Next steps

- **Put it in an editor:** [`doc/lsp-tutorial.md`](lsp-tutorial.md) wires a
  parser like this into VS Code and Neovim as a real language server, with
  diagnostics and an outline.
- **Bigger grammars:** when hand-rolled scanning stops being fun, wrap your
  existing `nom` or `chumsky` grammar — see the adapter crates and the
  appendix at the end of the LSP tutorial.
- **Prefer Lua?** [Section 5](#5-the-same-parser-in-pure-lua) rebuilds this
  exact language in pure Lua — one file, no Rust — and
  [`incraparse-lua`](../crates/incraparse-lua) runs it as a language server.
