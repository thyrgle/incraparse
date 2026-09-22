# Build a Language Server for MiniLang

A hands-on tutorial: parse a toy language with [incraparse](..), turn it into
a Language Server Protocol (LSP) server, and get it running in **VS Code** and
**Neovim** — live diagnostics and document symbols included.

The finished product of every step lives in the repository at
[`crates/incraparse-lsp/examples/mini_lang_server.rs`](../crates/incraparse-lsp/examples/mini_lang_server.rs);
diff your code against it whenever something looks off.

## 0. What we're building

```
┌─────────┐   JSON-RPC over stdio   ┌────────────────┐   incraparse API   ┌──────────────┐
│  editor │ ◄─────────────────────► │ minilang-lsp   │ ◄────────────────► │  parse tree  │
│         │   didOpen/didChange,    │ (this tutorial)│                    │  (multi-pass │
└─────────┘   publishDiagnostics,  └────────────────┘                    │   fixpoint)  │
              documentSymbol                                             └──────────────┘
```

Our toy language, **MiniLang**, is a file of function definitions:

```minilang
def add(a, b) { return a + b; }
def zero() { return 0; }
```

By the end, an editor will show a red squiggle under a broken `return`,
clean up the moment you fix it, and list all functions in the outline —
even while other functions in the same file are still broken. That last
part is incraparse's whole point: each pass either parses a region or
leaves it as a surviving `Failed` leaf, so one error never hides the file.

**Prerequisites:** Rust (`rustup`), plus VS Code and/or Neovim (≥ 0.8).
You'll need `npm` for the VS Code part.

---

## 1. Project setup

```console
$ cargo new minilang-lsp
$ cd minilang-lsp
```

Point `Cargo.toml` at incraparse (adjust the `path` to wherever this
repository lives on your machine, or use the versions from crates.io):

```toml
[package]
name = "minilang-lsp"
version = "0.1.0"
edition = "2021"

[dependencies]
incraparse = "0.1"
incraparse-lsp = "0.1"
lsp-types = "0.97"
lsp-server = "0.10"
serde_json = "1"
serde = "1"
```

## 2. Parsing MiniLang in passes

incraparse is *not* a parser combinator library — it runs **schedules of
passes** over a growing parse tree until the tree settles. Each pass is a
function from `(source text, region, context)` to an `Outcome`:

- `Outcome::Expand(children)` — this region contains smaller regions
  (with their own context) that later passes should look at,
- `Outcome::Done` — this region is fully parsed,
- `Outcome::Failed` — I can't parse this; maybe a later pass can.

Round `r` of a run applies pass `r` of the schedule to every region that is
ready for it. Our server uses three passes:

| Round | Pass | Input | Output |
|---|---|---|---|
| 0 | `FunctionsPass` | whole file | one region per `def name(params) { ... }` |
| 1 | `BodyPass` | function bodies | one region per `return …;` statement |
| 2 | `ReturnPass` | return statements | `Done` if the expression is non-empty, else `Failed` |

Create `src/main.rs` and start with the context type and shared scanning
helpers:

```rust
use std::collections::HashMap;
use std::error::Error;

use incraparse::{CancelToken, Engine, Outcome, Pass, Schedule, SerialExecutor, Span};
use incraparse_lsp::{diagnostics, DiagnosticsOptions, Document, PositionEncoding};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbol, DocumentSymbolParams, OneOf, PublishDiagnosticsParams,
};
use lsp_server::{Connection, Message, Notification};

#[derive(Clone, Debug, PartialEq, Eq)]
enum LangCtx {
    /// The whole file. Only the first pass accepts this.
    File,
    /// One `def` — remembers its name and parameter names.
    Function { name: String, params: Vec<String> },
    /// One `return …;` — remembers which function it came from.
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

/// Given the byte offset of `{`, returns the offset of its matching `}`.
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
```

Now the three passes. `FunctionsPass` scans the file for `def` keywords and
expands each well-formed definition into a child region. Anything malformed
(a missing `)` on that line, say) is simply *skipped* — the pass keeps
scanning, so the rest of the file still parses:

```rust
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
            // Parameter lists live on one line: bounded resync on failure.
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
```

`BodyPass` runs on every function region and expands its `return`
statements. This is where **context threading** happens: the pass reads the
function's name out of `ctx` and stamps it onto each statement region:

```rust
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
```

`ReturnPass` is the checker: a `return;` with nothing after it fails and
stays in the tree as a `Failed` leaf — a durable marker an editor can turn
into a squiggle:

```rust
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
```

## 3. The LSP server loop

Editors and servers speak JSON-RPC over stdio. The lifecycle your loop must
handle:

1. `initialize` — you advertise capabilities (we speak UTF-16 positions,
   incremental text sync, and `documentSymbol`),
2. `initialized`, then a stream of notifications:
   `textDocument/didOpen` / `…/didChange` / `…/didClose`,
3. requests like `textDocument/documentSymbol`,
4. `shutdown` + `exit` to end.

`incraparse-lsp` does the fiddly parts: `Document` splices text edits,
converts LSP change events into byte-range `Edit`s (so the parse tree
**reuses** every region the edit didn't touch), and `diagnostics()` walks
the tree for failing regions with editor-correct ranges.

Add the engine, the diagnostics publisher, and the symbol provider:

```rust
fn make_engine() -> Engine<LangCtx> {
    let mut schedule = Schedule::new();
    schedule.push(FunctionsPass); // round 0
    schedule.push(BodyPass); // round 1
    schedule.push(ReturnPass); // round 2
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
            source: Some("minilang".into()),
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
```

The main loop itself. Note that `main_loop` takes the `Connection` **by
value** — after `exit`, `io_threads.join()` can only return once the
connection's writer side has been dropped, so holding it across `join()`
deadlocks the shutdown:

```rust
type Documents = HashMap<lsp_types::Uri, Doc>;

#[allow(clippy::mutable_key_type)]
fn main_loop(
    connection: Connection,
    engine: Engine<LangCtx>,
    documents: Documents,
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
                        connection
                            .sender
                            .send(Message::Response(lsp_server::Response::new_err(
                                req.id,
                                lsp_server::ErrorCode::MethodNotFound as i32,
                                "method not supported".into(),
                            )))?;
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
                            item.text,
                            PositionEncoding::Utf16,
                            LangCtx::File,
                        );
                        doc.apply_changes(
                            &engine,
                            item.version,
                            &[], // nothing to apply on open
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
                        connection
                            .sender
                            .send(Message::Notification(lsp_server::Notification::new(
                                "textDocument/publishDiagnostics".into(),
                                PublishDiagnosticsParams {
                                    uri,
                                    diagnostics: Vec::new(),
                                    version: None,
                                },
                            )))?;
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

    main_loop(connection, make_engine(), HashMap::new())?;
    io_threads.join()?;
    Ok(())
}
```

One behavioral note worth internalizing: `didChange` gives you *deltas*
(we advertised `INCREMENTAL` sync). `apply_changes` translates each delta
into an `incraparse::Edit`, and the parse tree responds by re-parsing **only
the edited chain** — everything else is matched by span + context and
reused. On a 10,000-line file, fixing one typo re-parses one function, not
the file.

## 4. Run it standalone

```console
$ cargo build --release
$ ls target/release/minilang-lsp
```

Before involving an editor, poke the server by hand. Save this as
`poke.py` (Python 3, stdlib only) and run `python3 poke.py`:

```python
import json, subprocess, threading

p = subprocess.Popen(
    ["target/release/minilang-lsp"],
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

def outprint(x):
    print(json.dumps(x, indent=2)[:400])

send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}})
outprint(read())  # server capabilities

send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
    "textDocument": {"uri": "file:///t.mini", "languageId": "mini", "version": 0,
                     "text": "def bad(x) { return; }"}}})
outprint(read())  # publishDiagnostics: one error

send({"jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
      "params": {"textDocument": {"uri": "file:///t.mini"}}})
outprint(read())  # one symbol: bad

p.terminate()
```

Expected: capabilities mention `"positionEncoding": "utf-16"`; the
diagnostics publish has one error at characters 13–20 of line 0 (the empty
`return;`); the symbol list contains `bad`.

## 5. VS Code

VS Code needs a thin **extension** whose only job is launching our binary
and handing over the pipe. The `vscode-languageclient` npm package does the
protocol side.

Create the extension next to your server project (any location works):

```
minilang-vscode/
├── package.json
└── extension.js
```

`package.json`:

```json
{
  "name": "minilang",
  "displayName": "MiniLang Language Server",
  "publisher": "you",
  "version": "0.1.0",
  "engines": { "vscode": "^1.80.0" },
  "main": "./extension.js",
  "activationEvents": [],
  "contributes": {
    "languages": [
      { "id": "minilang", "extensions": [".mini"] }
    ]
  },
  "dependencies": {
    "vscode-languageclient": "^9.0.0"
  }
}
```

Notes: the `languages` contribution registers the `.mini` file extension,
and modern VS Code derives the `onLanguage:minilang` activation event from
it automatically. `engines.vscode` just gates the extension version.

`extension.js` — edit the server path to your build output:

```js
const { workspace } = require("vscode");
const {
  LanguageClient,
} = require("vscode-languageclient/node");

// EDIT ME: absolute path to the binary built in step 4.
const SERVER = "/home/you/code/minilang-lsp/target/release/minilang-lsp";

let client;

function activate(context) {
  const serverOptions = {
    // `run` is used for normal installs, `debug` when hosted via F5.
    run: { command: SERVER, transport: 0 /* stdio */ },
    debug: { command: SERVER, transport: 0 },
  };
  const clientOptions = {
    documentSelector: [{ language: "minilang" }],
  };
  client = new LanguageClient(
    "minilang",
    "MiniLang Language Server",
    serverOptions,
    clientOptions
  );
  context.subscriptions.push(client.start());
}

function deactivate() {
  return client?.stop();
}

module.exports = { activate, deactivate };
```

Install it:

```console
$ cd minilang-vscode && npm install
```

Then either:

- **Dev-host (fastest loop):** open the `minilang-vscode` folder in VS Code
  and press **F5** ("Run Extension"). A second VS Code window opens with
  the extension loaded; open a `.mini` file there.
- **Install for real:** copy (or symlink) the folder into
  `~/.vscode/extensions/` and restart VS Code.

Now open any `.mini` file. Type `def f(x) { return; }` — a red squiggle
appears under the statement; change it to `return x;` and watch the
diagnostic vanish without a full-file flicker. Open the outline
(`Ctrl+Shift+O`) to see your functions with their `(a, b)` details.

Server logs: **View → Output → MiniLang Language Server**.

## 6. Neovim (built-in client, no plugins)

Neovim ≥ 0.8 ships an LSP client; `vim.lsp.start` is the one-call setup.

**1. Filetype detection** — add to your `init.lua`:

```lua
vim.filetype.add({
  extension = { mini = "minilang" },
})
```

**2. Start the server per buffer** — create
`~/.config/nvim/ftplugin/minilang.lua` (this directory name must match the
filetype; the file runs every time a `minilang` buffer opens):

```lua
-- EDIT ME: absolute path to the binary built in step 4.
local server = vim.env.HOME .. "/code/minilang-lsp/target/release/minilang-lsp"

vim.lsp.start({
  name = "minilang-lsp",
  cmd = { server },
  root_dir = vim.fs.dirname(vim.fs.find({ ".git" }, { upward = true })[1]
    or vim.api.nvim_buf_get_name(0)),
})
```

(`vim.lsp.start` is idempotent per root: reopening a buffer in the same
project reuses the running client.)

Open a `.mini` file, then check `:checkhealth vim.lsp` or:

```vim
:lua print(vim.inspect(vim.lsp.get_clients({ name = "minilang-lsp" })[1] ~= nil))
```

Diagnostics appear as virtual text / signs automatically. For the outline,
`:lua vim.lsp.buf.document_symbol()` (or bind it — e.g.
`vim.keymap.set("n", "gO", vim.lsp.buf.document_symbol)` in the ftplugin).

Server logs: `:LspLog`.

Classic Vim (not Neovim) has no built-in client — coc.nvim and ALE both
work with this exact binary if you need it; the tutorial keeps to Neovim.

## 7. Try it

Paste this into a `.mini` file — two good functions around a broken one:

```minilang
def add(a, b) { return a + b; }
def bad(x) { return; }
def noise( { return broken;
def zero() { return 0; }
```

You should see:

- **one** diagnostic, on `bad`'s `return;` — the malformed `noise` line is
  skipped by `FunctionsPass`, and `zero` still parses *after* it. This is
  the error-resilience payoff: coarse structure survives broken regions.
- Fix `return;` → `return x;` and the diagnostic disappears immediately
  (only that function was re-parsed).
- The outline lists `add`, `bad`, `zero` — but never `noise`.

## 8. Troubleshooting

| Symptom | Check |
|---|---|
| Editor says the server "exited with status" or nothing happens | Run the binary alone (step 4) — does it print nothing and wait? Good. Then check the absolute path in `extension.js` / `minilang.lua`. |
| No diagnostics but no errors either | Is the file extension recognized? VS Code: bottom-right shows the language id (`minilang`); Neovim: `:set ft?` must say `minilang`. |
| Wrong squiggle positions in files with emoji/accented characters | An encoding mismatch — we negotiated UTF-16; make sure the client didn't force something else. `incraparse-lsp` converts both ways via `LineIndex`. |
| Server dies on exit / hangs at shutdown | `main_loop` must own the `Connection` (see the note in step 3); `io_threads.join()` must run after it returns. |
| Where are the logs? | VS Code: Output panel → *MiniLang Language Server*. Neovim: `:LspLog`. |

## 9. Exercises

1. **Hover:** implement `textDocument/hover` — for a `Return` region, echo
   `` `function` → expr `` using `doc.session().tree()`.
2. **Go-to-definition:** record each function's *name* span during
   `FunctionsPass` (add it to `LangCtx::Function`), then answer definition
   requests by matching identifiers against those spans.
3. **A completion pass:** add a fourth pass that expands parameter lists
   into per-parameter regions; feed them to `textDocument/completion`.
4. **Better recovery:** make `FunctionsPass` parse the whole `def` line even
   when the body brace is missing, and emit a dedicated "unclosed brace"
   diagnostic from the tree.

Happy parsing — and when MiniLang outgrows hand-rolled scanning, wrap
`nom` or `chumsky` in a `Pass`; the engine doesn't care.
