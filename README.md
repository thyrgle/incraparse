# incraparse

Multi-pass **fixpoint parsing** for editors, LSPs, and compilers.

This repository is a cargo workspace with two crates:

| Crate | Role |
|-------|------|
| [`crates/incraparse`](crates/incraparse) | The engine: passes, schedules, fixpoint rounds, incremental edits, executors, cancellation. Zero required dependencies. |
| [`crates/incraparse-lsp`](crates/incraparse-lsp) | Framework-agnostic LSP adapter: position encodings, `Document` change translation, diagnostics bridge. Depends only on `incraparse` + `lsp-types`. |

`incraparse` is *not* another parser combinator library. It is the missing
piece **around** them: an engine that executes **schedules of passes** over a
growing parse tree until the tree settles — and a pass can wrap *any* parsing
technique (`nom`, `chumsky`, a PEG, regexes, or hand-rolled scanning).

## The idea

Parsing a real program in one monolithic sweep is brittle: one syntax error in
a function body can hide the structure of an entire file. `incraparse` lets
you parse in widening rounds of understanding instead.

Picture a small language with definitions like:

```text
def func_name(params) { ... }
```

- **Round 0** runs a cheap, error-tolerant pass over the whole file that
  matches only the skeleton `def func_name(params) { * }` — the `*` is an
  explicit *hole*: a child region the pass did not parse, pushed onto the tree
  with its own context (`function_name`, `params`).
- **Round 1** runs a deeper pass over each hole, with that context threaded
  in, expanding statements inside bodies.
- Later rounds keep expanding until every region is either accepted
  (`Done`) or permanently failed.

Because failed regions stay in the tree as leaves, consumers — say, an LSP
answering "what functions does this file define?" — still see the coarse
structure even while deep passes are failing. That is the error-resilience
payoff, and it is why the design suits language servers.

## How a run works

Each node in the [`ParseTree`] holds a [`Span`] into the source, a context
value, and a [`Status`]:

```text
Unparsed ──▶ Expanded ──┐   (children carry the remaining work)
    │                   │
    ├──▶ Done           │   (region accepted, no children)
    └──▶ Failed ──┐     │
                  │     │
   retried by the next pass in the schedule; once passes run
   out, the failure is permanent and the node stays as a leaf
```

- **Round `r`** applies `schedule[r]` to every node ready for it (nodes
  created or failed in round `r-1` are ready for round `r`).
- A run reaches its **fixpoint** when a round finds no ready nodes — or hits
  the round cap (one round per scheduled pass, by default).
- The engine merges batch results in job order, so runs are **deterministic**
  regardless of the executor.

### Termination by construction

Passes may only produce child spans **contained in** their parent and on the
same source revision. The engine rejects any outcome that violates this,
marking the node failed. Since round `r` only processes nodes at depth `r`
and runs are capped at one round per scheduled pass, every node is processed
at most once per pass and no schedule can loop forever. (For "always
divides" passes, `EngineConfig::enforce_shrink` additionally requires
children to be strictly smaller; by default a child may cover its parent
exactly — e.g. a file containing exactly one function.)

## Quick start

```rust
use incraparse::{Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Span, Status};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ctx {
    File,
    Function { name: String },
}

// Round 0: a real pass would scan for `def name(...) { ... }` skeletons and
// expand each one into a child region. Here we fake one function.
struct Functions;

impl Pass for Functions {
    type Ctx = Ctx;

    fn parse(&self, _source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
        match ctx {
            Ctx::File => Outcome::Expand(vec![(
                Span::new(span.start + 4, span.end, span.rev),
                Ctx::Function { name: "main".into() },
            )]),
            Ctx::Function { .. } => Outcome::Done,
        }
    }
}

let mut schedule = Schedule::new();
schedule.push(Functions); // round 0: find functions
schedule.push(Functions); // round 1: settle what round 0 created

let source = "def main() { }";
let mut tree = ParseTree::new(0, Span::new(0, source.len(), 0), Ctx::File);

let engine = Engine::new(schedule);
let report = engine.run(source, &mut tree, &SerialExecutor, &incraparse::CancelToken::new());

assert!(report.reached_fixpoint);
assert_eq!(tree.status(tree.root()), Status::Expanded);
assert_eq!(tree.ctx(tree.children(tree.root())[0]), &Ctx::Function { name: "main".into() });
```

A fuller, runnable version — `def name(params) { return expr; }` parsed in
three passes, with a malformed definition and an empty return surviving in the
tree — lives in `examples/mini_lang.rs`:

```sh
cargo run --example mini_lang
```

## Incremental edits

The tree is built to be *re-parsed*, not rebuilt. A [`Session`] wraps a
[`ParseTree`] that can absorb [`Edit`]s: every span is remapped into the new
coordinates and only the nodes the edit touched are reset for re-parsing.
When the next run re-expands their parents, produced children are matched
against the surviving ones **by span and context** — equal children keep
their identity, their status, and their whole subtree:

```rust,ignore
session.edit(Edit::insert(source.len(), appended.len()));
let report = session.run(&engine, &source, &SerialExecutor, &CancelToken::new());
// report.nodes_processed == 3  — root scan + the new function's chain only;
// every pre-existing function kept its NodeId and parsed subtree.
```

An edit inside one function body re-parses that function; every other
function is carried over untouched. Fixing a syntax error heals the region
in place — the node keeps its identity and retries every pass, including
ones it had previously exhausted. See `examples/mini_lang.rs` for a full
walkthrough (append a function, then fix a broken return — 3 nodes re-parsed
per edit instead of the whole file).

## Language servers

`incraparse-lsp` bridges the engine to the Language Server Protocol without
picking a server framework for you (only `lsp-types` — no tokio, no I/O):

- **Position encodings**: LSP positions are `(line, character)` in UTF-8,
  UTF-16, or UTF-32 code units; spans are byte offsets. `LineIndex` +
  `PositionEncoding` convert both ways, correctly, across multibyte text,
  negotiating the encoding via the `positionEncoding` capability.
- **`Document<C>`**: one open file — text, `Session`, client version.
  Feed it the `didChange` payload; each change event is translated into a
  byte-range `Edit` (so unchanged regions are reused) and the engine runs
  once per batch, synchronously, on whatever thread you choose.
- **Diagnostics**: `diagnostics(&doc, options, hook)` walks the settled tree
  and lets your hook turn failing regions into publishable `Diagnostic`s
  with encoding-correct ranges — including after a *cancelled* run, which is
  the coarse-structure-survives-errors payoff.

`crates/incraparse-lsp/examples/mini_lang_server.rs` is a complete small
server (diagnostics + document symbols) with an end-to-end stdio smoke test
in `crates/incraparse-lsp/tests/server_smoke.rs`.

## Executors and cancellation

A round's batch of nodes goes through the [`Executor`] trait:

- [`SerialExecutor`] — jobs run one at a time on the calling thread (default).
- [`RayonExecutor`] — jobs run on a rayon thread pool; enable the `parallel`
  feature. Useful for batch compilation; results still merge in job order.

For LSP-style interactive use, run the engine on a background thread with a
[`CancelToken`]: when the user types again, cancel the run (the engine checks
at job granularity), invalidate the subtrees overlapping the edit, and start a
fresh run. The responsiveness comes from cancellation *between batches*, not
from intra-round parallelism — which is why the default build has no
concurrency machinery at all.

## API tour

| Piece | Role |
|-------|------|
| [`Pass`] | Your parse logic: `(&str, Span, &Ctx) -> Outcome<Ctx>`. |
| [`Schedule`] | Ordered passes; round `r` uses pass `r`. |
| [`ParseTree`] | Arena of `(span, ctx, status)` nodes with stable [`NodeId`]s. |
| [`Engine`] | Drives rounds to a fixpoint. |
| [`Session`] | Long-lived document: edits + re-parses with subtree reuse. |
| [`Edit`] | One text change: `replace(start, old_end, new_end)`. |
| [`Outcome`] | `Expand(children)` / `Done` / `Failed`. |
| [`RunReport`] | Rounds run, work done, failures, fixpoint/cancellation flags. |

## Roadmap

- Finer-grained reuse hooks (e.g. matching by user-supplied keys instead of
  `PartialEq`).
- Optional background-run helper for LSP documents (request_parse /
  on_settled on a worker thread).

## Status

v0.1.0 — core semantics are settling; the API may still change.

[`Pass`]: https://docs.rs/incraparse/latest/incraparse/trait.Pass.html
[`Schedule`]: https://docs.rs/incraparse/latest/incraparse/struct.Schedule.html
[`ParseTree`]: https://docs.rs/incraparse/latest/incraparse/struct.ParseTree.html
[`Engine`]: https://docs.rs/incraparse/latest/incraparse/struct.Engine.html
[`Session`]: https://docs.rs/incraparse/latest/incraparse/struct.Session.html
[`Edit`]: https://docs.rs/incraparse/latest/incraparse/struct.Edit.html
[`Outcome`]: https://docs.rs/incraparse/latest/incraparse/enum.Outcome.html
[`RunReport`]: https://docs.rs/incraparse/latest/incraparse/struct.RunReport.html
[`Span`]: https://docs.rs/incraparse/latest/incraparse/struct.Span.html
[`Status`]: https://docs.rs/incraparse/latest/incraparse/enum.Status.html
[`NodeId`]: https://docs.rs/incraparse/latest/incraparse/struct.NodeId.html
[`Executor`]: https://docs.rs/incraparse/latest/incraparse/trait.Executor.html
[`SerialExecutor`]: https://docs.rs/incraparse/latest/incraparse/struct.SerialExecutor.html
[`RayonExecutor`]: https://docs.rs/incraparse/latest/incraparse/struct.RayonExecutor.html
[`CancelToken`]: https://docs.rs/incraparse/latest/incraparse/struct.CancelToken.html
[`LineIndex`]: https://docs.rs/incraparse-lsp/latest/incraparse_lsp/struct.LineIndex.html
[`PositionEncoding`]: https://docs.rs/incraparse-lsp/latest/incraparse_lsp/enum.PositionEncoding.html
