//! The fixpoint engine that drives rounds of passes over a parse tree.

use crate::cancel::CancelToken;
use crate::executor::Executor;
use crate::job::Job;
use crate::outcome::Outcome;
use crate::pass::Pass;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineConfig {
    /// When `true`, every child span must be *strictly* smaller than its
    /// parent — an extra invariant for "always divides" passes. When
    /// `false` (the default), a child may cover its parent exactly, which
    /// is legitimate: a file containing exactly one function parses as that
    /// one function. Either way children must stay **contained** in their
    /// parent and on the same source revision.
    ///
    /// Termination does not depend on this setting: every node is processed
    /// at most once per scheduled pass, and runs are bounded by
    /// [`max_rounds`](Self::max_rounds).
    pub enforce_shrink: bool,
    /// Hard cap on rounds per run. `None` (the default) means one round per
    /// scheduled pass.
    pub max_rounds: Option<usize>,
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
/// let engine = Engine::with((MarkDone,));
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

/// Types that can become a [`Schedule`]: tuples of passes (1–12, mixed
/// types welcome) and vectors of boxed passes.
///
/// Not intended to be implemented outside this crate; use it as a bound for
/// APIs like [`Engine::with`].
#[doc(hidden)]
pub trait Passes<C> {
    fn into_schedule(self) -> Schedule<C>;
}

macro_rules! impl_passes_for_tuple {
    ($($name:ident),+) => {
        impl<C, $($name),+> Passes<C> for ($($name,)+)
        where
            $($name: Pass<Ctx = C> + Send + Sync + 'static,)+
        {
            fn into_schedule(self) -> Schedule<C> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;
                let mut schedule = Schedule::new();
                $( schedule.push($name); )+
                schedule
            }
        }
    };
}

impl_passes_for_tuple!(P1);
impl_passes_for_tuple!(P1, P2);
impl_passes_for_tuple!(P1, P2, P3);
impl_passes_for_tuple!(P1, P2, P3, P4);
impl_passes_for_tuple!(P1, P2, P3, P4, P5);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7, P8);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7, P8, P9);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11);
impl_passes_for_tuple!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12);

impl<C> Passes<C> for Vec<Box<dyn Pass<Ctx = C> + Send + Sync>> {
    fn into_schedule(self) -> Schedule<C> {
        let mut schedule = Schedule::new();
        for pass in self {
            schedule.push_boxed(pass);
        }
        schedule
    }
}

impl<C> Engine<C> {
    /// Creates an engine with default [`EngineConfig`].
    pub fn new(schedule: Schedule<C>) -> Self {
        Self {
            schedule,
            config: EngineConfig::default(),
        }
    }

    /// Creates an engine from passes directly — a tuple of mixed pass types
    /// (up to 12) or a `Vec` of boxed passes:
    ///
    /// ```
    /// use incraparse::{pass_fn, Engine, Outcome, Span};
    ///
    /// let a = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::Done);
    /// let b = pass_fn(|_source: &str, _span, _ctx: &()| Outcome::Done);
    /// let engine = Engine::with((a, b)); // round 0 runs `a`, round 1 runs `b`
    /// ```
    pub fn with(passes: impl Passes<C>) -> Self {
        Self::new(passes.into_schedule())
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
    /// When re-expanding a node that already has children (after
    /// [`ParseTree::edit`]), produced children are matched against the
    /// existing ones by span and context — equal children are reused
    /// whole, so only the edited regions are re-parsed. This is why `C`
    /// must implement `PartialEq`.
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
        C: Clone + PartialEq + Send + 'static,
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
