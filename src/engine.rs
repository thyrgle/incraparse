//! The fixpoint engine that drives rounds of passes over a parse tree.

use crate::cancel::CancelToken;
use crate::executor::Executor;
use crate::job::Job;
use crate::outcome::Outcome;
use crate::schedule::Schedule;
use crate::tree::ParseTree;

/// Summary of a single [`Engine::run`].
///
/// Interpretation: `reached_fixpoint` means every node settled. If neither
/// `reached_fixpoint` nor `cancelled` is set, the run consumed all its rounds
/// with work left over — typically nodes still [`Unparsed`](crate::Status::Unparsed)
/// because the schedule has no pass for their round. Query
/// [`ParseTree::pending`] to find them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunReport {
    /// Number of rounds that processed at least one node.
    pub rounds_run: usize,
    /// Number of node/pass attempts executed.
    pub nodes_processed: usize,
    /// Number of attempts whose outcome was a failure (including
    /// contract-violating expansions).
    pub nodes_failed: usize,
    /// `true` if the run ended because no node had work left.
    pub reached_fixpoint: bool,
    /// `true` if the run ended because the [`CancelToken`] fired.
    pub cancelled: bool,
}

/// Tunables for an [`Engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Require every child span to be strictly smaller than its parent
    /// (default `true`). This is the termination guarantee: region sizes
    /// strictly decrease down the tree, so the tree is finite and each node
    /// is processed at most once per pass.
    pub enforce_shrink: bool,
    /// Hard cap on rounds per run. `None` (the default) means one round per
    /// scheduled pass.
    pub max_rounds: Option<usize>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            enforce_shrink: true,
            max_rounds: None,
        }
    }
}

/// The multi-pass fixpoint driver.
///
/// An engine owns a [`Schedule`] of passes and runs them over a
/// [`ParseTree`] in rounds:
///
/// * **Round `r`** applies `schedule[r]` to every node whose `depth == r`
///   and whose status is [`Unparsed`](Status::Unparsed) or
///   [`Failed`](Status::Failed).
/// * Nodes expanded in round `r` create children at depth `r + 1`; nodes
///   that fail are retried at depth `r + 1` by the next pass.
/// * The run reaches its **fixpoint** when a round finds no ready nodes, or
///   after `max_rounds` rounds.
///
/// # Examples
///
/// ```
/// use incraparse::{Engine, Outcome, ParseTree, Pass, Schedule, SerialExecutor, Span};
///
/// struct MarkDone;
///
/// impl Pass for MarkDone {
///     type Ctx = ();
///     fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
///         Outcome::Done
///     }
/// }
///
/// let mut schedule = Schedule::new();
/// schedule.push(MarkDone);
///
/// let engine = Engine::new(schedule);
/// let mut tree = ParseTree::new(0, Span::new(0, 3, 0), ());
/// let report = engine.run("abc", &mut tree, &SerialExecutor, &incraparse::CancelToken::new());
///
/// assert!(report.reached_fixpoint);
/// assert_eq!(tree.status(tree.root()), incraparse::Status::Done);
/// ```
pub struct Engine<C> {
    schedule: Schedule<C>,
    config: EngineConfig,
}

impl<C> Engine<C> {
    /// Creates an engine with default [`EngineConfig`].
    pub fn new(schedule: Schedule<C>) -> Self {
        Self {
            schedule,
            config: EngineConfig::default(),
        }
    }

    /// Creates an engine with an explicit configuration.
    pub fn with_config(schedule: Schedule<C>, config: EngineConfig) -> Self {
        Self { schedule, config }
    }

    /// The pass schedule this engine runs.
    pub fn schedule(&self) -> &Schedule<C> {
        &self.schedule
    }

    /// The engine's configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The effective round cap: the configured cap, or one round per pass.
    pub fn max_rounds(&self) -> usize {
        self.config.max_rounds.unwrap_or(self.schedule.len())
    }

    /// Runs passes over `tree` until the fixpoint, the round cap, or
    /// cancellation.
    ///
    /// `source` must be the same text (at the revision the tree was created
    /// with) that passes will slice via their spans. The tree is updated
    /// in place; if the run is cancelled partway, the merged prefix of each
    /// batch keeps the tree consistent and a later call resumes where this
    /// one stopped.
    ///
    /// A run over an already-settled tree reports `reached_fixpoint` without
    /// doing any work. A run over a non-settled tree with an empty schedule
    /// does no work and reports neither fixpoint nor cancellation — the
    /// root simply stays [`Unparsed`](crate::Status::Unparsed).
    pub fn run<E>(
        &self,
        source: &str,
        tree: &mut ParseTree<C>,
        exec: &E,
        cancel: &CancelToken,
    ) -> RunReport
    where
        C: Clone + Send + 'static,
        E: Executor,
    {
        let mut report = RunReport::default();
        let max_rounds = self.max_rounds();

        for round in 0..max_rounds {
            if cancel.is_cancelled() {
                report.cancelled = true;
                return report;
            }

            let jobs = tree.ready_jobs(round);
            if jobs.is_empty() {
                report.reached_fixpoint = true;
                return report;
            }
            let batch_size = jobs.len();

            let executed = exec.execute(jobs, |job| self.execute_job(source, job), cancel);

            let merged = executed.len();
            for (job, outcome) in executed {
                if matches!(
                    tree.apply(job.node, outcome, round, self.config.enforce_shrink),
                    crate::tree::Applied::Failed
                ) {
                    report.nodes_failed += 1;
                }
            }

            report.rounds_run += 1;
            report.nodes_processed += merged;

            if merged < batch_size {
                report.cancelled = true;
                return report;
            }
        }

        report.reached_fixpoint = tree.pending(max_rounds).is_empty();
        report
    }

    fn execute_job(&self, source: &str, job: &Job<C>) -> Outcome<C>
    where
        C: Clone + Send + 'static,
    {
        match self.schedule.pass_at(job.pass_index) {
            Some(pass) => pass.parse(source, job.span, &job.ctx),
            None => Outcome::Failed,
        }
    }
}
