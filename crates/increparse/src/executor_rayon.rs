//! Batch execution on a rayon thread pool.
//!
//! Enabled by the `parallel` feature. Job results are merged by the engine in
//! input order, so runs stay deterministic despite parallel execution.

use crate::cancel::CancelToken;
use crate::executor::Executor;
use crate::job::Job;
use crate::outcome::Outcome;

/// An [`Executor`] that runs a round's job batch on a rayon thread pool.
///
/// Useful for batch compilation workloads where intra-round parallelism pays
/// off. Cancellation is checked before each job starts; jobs already in
/// flight are allowed to finish, but their results past the first cancelled
/// slot are discarded and the affected nodes simply remain pending for the
/// next run.
///
/// For LSP-style interactive use, prefer [`SerialExecutor`] on a background
/// thread with a [`CancelToken`] — the responsiveness comes from cancelling
/// between batches, not from intra-round parallelism.
///
/// # Examples
///
/// ```
/// use increparse::{CancelToken, Engine, Outcome, ParseTree, Pass, RayonExecutor, Schedule, Span, Status};
///
/// struct MarkDone;
/// impl Pass for MarkDone {
///     type Ctx = ();
///     fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
///         Outcome::Done
///     }
/// }
///
/// # fn main() {
/// let mut schedule = Schedule::new();
/// schedule.push(MarkDone);
///
/// let engine = Engine::new(schedule);
/// let mut tree = ParseTree::new(0, Span::new(0, 3, 0), ());
/// let report = engine.run("abc", &mut tree, &RayonExecutor, &CancelToken::new());
///
/// assert!(report.reached_fixpoint);
/// assert_eq!(tree.status(tree.root()), Status::Done);
/// # }
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct RayonExecutor;

impl Executor for RayonExecutor {
    fn execute<C, F>(
        &self,
        jobs: Vec<Job<C>>,
        run: F,
        cancel: &CancelToken,
    ) -> Vec<(Job<C>, Outcome<C>)>
    where
        C: Send + 'static,
        F: Fn(&Job<C>) -> Outcome<C> + Send + Sync,
    {
        use rayon::prelude::*;

        jobs.into_par_iter()
            .map(|job| {
                if cancel.is_cancelled() {
                    None
                } else {
                    let outcome = run(&job);
                    Some((job, outcome))
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .take_while(|slot| slot.is_some())
            .flatten()
            .collect()
    }
}
