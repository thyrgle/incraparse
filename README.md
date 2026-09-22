# incraparse

Multi-pass **fixpoint parsing** for editors, LSPs, and compilers.

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

Passes may only produce child spans **contained in** and (by default)
**strictly smaller than** their parent. The engine rejects any outcome that
violates this, marking the node failed. Regions therefore strictly shrink down
the tree: the tree is finite, every node is processed at most once per pass,
and no schedule can loop forever.

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
| [`Outcome`] | `Expand(children)` / `Done` / `Failed`. |
| [`RunReport`] | Rounds run, work done, failures, fixpoint/cancellation flags. |

## Roadmap

- **Edit invalidation**: stable node IDs make it cheap to re-run only the
  subtrees whose spans overlap an edit — the "incremental" in incremental
  parsing.
- **`incraparse-lsp`**: an adapter crate wiring the engine into an LSP server
  loop (background runs, cancellation, partial results).

## Status

v0.1.0 — core semantics are settling; the API may still change.

[`Pass`]: https://docs.rs/incraparse/latest/incraparse/trait.Pass.html
[`Schedule`]: https://docs.rs/incraparse/latest/incraparse/struct.Schedule.html
[`ParseTree`]: https://docs.rs/incraparse/latest/incraparse/struct.ParseTree.html
[`Engine`]: https://docs.rs/incraparse/latest/incraparse/struct.Engine.html
[`Outcome`]: https://docs.rs/incraparse/latest/incraparse/enum.Outcome.html
[`RunReport`]: https://docs.rs/incraparse/latest/incraparse/struct.RunReport.html
[`Span`]: https://docs.rs/incraparse/latest/incraparse/struct.Span.html
[`Status`]: https://docs.rs/incraparse/latest/incraparse/enum.Status.html
[`NodeId`]: https://docs.rs/incraparse/latest/incraparse/struct.NodeId.html
[`Executor`]: https://docs.rs/incraparse/latest/incraparse/trait.Executor.html
[`SerialExecutor`]: https://docs.rs/incraparse/latest/incraparse/struct.SerialExecutor.html
[`RayonExecutor`]: https://docs.rs/incraparse/latest/incraparse/struct.RayonExecutor.html
[`CancelToken`]: https://docs.rs/incraparse/latest/incraparse/struct.CancelToken.html
