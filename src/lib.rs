//! Multi-pass fixpoint parsing for editors, LSPs, and compilers.
//!
//! `incraparse` is **not** a parser combinator library. It is the missing
//! piece *around* one: an engine that executes **schedules of passes** over a
//! growing parse tree until the tree settles into a fixpoint.
//!
//! # The idea
//!
//! Parsing a real program in one monolithic sweep is brittle — one syntax
//! error in a function body can hide the whole file's structure. Instead,
//! `incraparse` lets you parse in widening rounds of understanding:
//!
//! * **Round 0** runs a cheap, error-tolerant pass over the whole file that
//!   matches coarse structure, e.g. `def name(params) { * }`, where `*` is an
//!   explicit *hole*: a child region the pass could not or did not parse.
//! * **Round 1** runs a deeper pass over each hole, with the round-0 context
//!   threaded in (`name`, `params`), expanding statements inside bodies.
//! * Later rounds keep expanding until every region is either accepted or
//!   permanently failed.
//!
//! Because failed regions stay in the tree as leaves, consumers — an LSP
//! answering "what functions does this file define?" for example — still see
//! the coarse structure even when deep passes are still failing. That is the
//! error-resilience payoff.
//!
//! # The moving parts
//!
//! | Piece | Role |
//! |-------|------|
//! | [`Pass`] | Your parse logic: `(source, span, ctx) -> Outcome<Ctx>`. Wraps any combinator (`nom`, `chumsky`, PEG, regex, hand-rolled). |
//! | [`Schedule`] | Ordered passes; round `r` uses pass `r`. |
//! | [`ParseTree`] | Arena of `(span, ctx, status)` nodes; the result of a run. |
//! | [`Engine`] | Drives rounds to a fixpoint, with cancellation and pluggable parallelism. |
//! | [`Executor`] | How a round's batch runs: [`SerialExecutor`] (default) or [`RayonExecutor`] (feature `parallel`). |
//! | [`CancelToken`] | Cooperative cancellation at job granularity — built for LSP-style "user typed again" restarts. |
//!
//! # Termination by construction
//!
//! Passes may only produce child spans **contained in** and (by default)
//! **strictly smaller than** their parent. The engine rejects any outcome
//! that violates this, so regions strictly shrink down the tree: the tree is
//! finite, every node is processed at most once per pass, and a run performs
//! at most `schedule.len()` rounds.
//!
//! # A taste
//!
//! A tiny language of function definitions, parsed in three passes. (The
//! full, runnable version with real scanning is in `examples/mini_lang.rs`.)
//!
//! ```
//! use incraparse::{Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Span, Status};
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! enum Ctx {
//!     File,
//!     Function { name: String },
//! }
//!
//! /// Round 0: accept the whole file. (A real pass would scan for `def`s
//! /// and expand each one into a child region.)
//! struct Functions;
//!
//! impl Pass for Functions {
//!     type Ctx = Ctx;
//!
//!     fn parse(&self, _source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
//!         match ctx {
//!             Ctx::File => Outcome::Expand(vec![(
//!                 Span::new(span.start + 4, span.end, span.rev),
//!                 Ctx::Function { name: "main".into() },
//!             )]),
//!             Ctx::Function { .. } => Outcome::Done,
//!         }
//!     }
//! }
//!
//! # fn main() {
//! let mut schedule = Schedule::new();
//! // Round 0 finds the function region; round 1 re-runs the pass on the
//! // nodes round 0 created (children always start one round deeper).
//! schedule.push(Functions);
//! schedule.push(Functions);
//!
//! let source = "def main() { }";
//! let mut tree = ParseTree::new(0, Span::new(0, source.len(), 0), Ctx::File);
//!
//! let engine = Engine::new(schedule);
//! let report = engine.run(source, &mut tree, &SerialExecutor, &incraparse::CancelToken::new());
//!
//! assert!(report.reached_fixpoint);
//! assert_eq!(tree.status(tree.root()), Status::Expanded);
//! let func = tree.children(tree.root())[0];
//! assert_eq!(tree.ctx(func), &Ctx::Function { name: "main".into() });
//! assert_eq!(tree.status(func), Status::Done);
//! # }
//! ```
//!
//! # Roadmap
//!
//! * Edit invalidation: stable [`NodeId`]s make it cheap to re-run only the
//!   subtrees overlapping an edit (this is the "incremental" in incremental
//!   parsing).
//! * `incraparse-lsp`: an adapter crate wiring the engine into an LSP server
//!   loop with background runs and cancellation between batches.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cancel;
mod engine;
mod executor;
mod job;
mod node;
mod outcome;
mod pass;
mod schedule;
mod span;
mod status;
mod tree;

pub use cancel::CancelToken;
pub use engine::{Engine, EngineConfig, RunReport};
pub use executor::{Executor, SerialExecutor};
pub use job::Job;
pub use node::NodeId;
pub use outcome::Outcome;
pub use pass::Pass;
pub use schedule::Schedule;
pub use span::Span;
pub use status::{Status, StatusCounts};
pub use tree::ParseTree;

#[cfg(feature = "parallel")]
pub use executor_rayon::RayonExecutor;
#[cfg(feature = "parallel")]
mod executor_rayon;
