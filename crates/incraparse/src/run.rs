//! The quickest way to try a schedule out.

use crate::cancel::CancelToken;
use crate::engine::Passes;
use crate::engine::{Engine, RunReport};
use crate::executor::SerialExecutor;
use crate::tree::ParseTree;

/// Parses `source` from scratch with `passes` and returns the settled tree
/// plus the run report — the quickest way to try a schedule out, in a test,
/// a doctest, or a first experiment:
///
/// ```
/// use incraparse::prelude::*;
///
/// let settle = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::Done);
/// let (tree, report) = incraparse::run("hello", (settle,), ());
///
/// assert!(report.reached_fixpoint);
/// assert_eq!(tree.status(tree.root()), Status::Done);
/// ```
///
/// It is exactly [`Engine::run`] over a fresh [`ParseTree::from_source`]
/// with the [`SerialExecutor`] — for cancellation, custom executors, or
/// incremental sessions, use [`Engine`](crate::Engine) and
/// [`Session`](crate::Session) directly.
pub fn run<C>(source: &str, passes: impl Passes<C>, root_ctx: C) -> (ParseTree<C>, RunReport)
where
    C: Clone + PartialEq + Send + 'static,
{
    let engine = Engine::with(passes);
    let mut tree = ParseTree::from_source(source, 0, root_ctx);
    let report = engine.run(source, &mut tree, &SerialExecutor, &CancelToken::new());
    (tree, report)
}
