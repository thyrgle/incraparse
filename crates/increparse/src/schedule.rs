//! Round-indexed schedules of passes.

use crate::pass::Pass;

/// An ordered sequence of passes, indexed by round.
///
/// Round `r` of an [`Engine`](crate::Engine) run applies the pass at index `r`
/// of the schedule to every node that is ready for it, so a schedule doubles
/// as the fixed pass order of a run. Nodes created (or failed) in round `r`
/// are processed by round `r + 1`, which uses the next pass in the schedule.
///
/// # Examples
///
/// ```
/// use increparse::{Outcome, Pass, Schedule, Span};
///
/// struct WholeFile;
/// struct Statements;
///
/// struct NoopCtx;
///
/// impl Pass for WholeFile {
///     type Ctx = ();
///     fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
///         Outcome::Done
///     }
/// }
///
/// impl Pass for Statements {
///     type Ctx = ();
///     fn parse(&self, _source: &str, _span: Span, _ctx: &()) -> Outcome<()> {
///         Outcome::Done
///     }
/// }
///
/// let mut schedule = Schedule::new();
/// schedule.push(WholeFile);
/// schedule.push(Statements);
/// assert_eq!(schedule.len(), 2);
/// ```
pub struct Schedule<C> {
    passes: Vec<Box<dyn Pass<Ctx = C> + Send + Sync>>,
}

impl<C> Schedule<C> {
    /// Creates an empty schedule.
    pub fn new() -> Self {
        Self { passes: Vec::new() }
    }

    /// Appends `pass` to the schedule; its round index is the previous length.
    pub fn push<P>(&mut self, pass: P)
    where
        P: Pass<Ctx = C> + Send + Sync + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    /// Appends an already-boxed pass.
    pub fn push_boxed(&mut self, pass: Box<dyn Pass<Ctx = C> + Send + Sync>) {
        self.passes.push(pass);
    }

    /// Number of passes in the schedule; also the maximum number of rounds.
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Returns `true` if the schedule has no passes.
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }

    /// Returns the name of the pass scheduled for `round`, if any.
    pub fn pass_name(&self, round: usize) -> Option<&'static str> {
        self.passes.get(round).map(|p| p.name())
    }

    pub(crate) fn pass_at(&self, round: usize) -> Option<&(dyn Pass<Ctx = C> + Send + Sync)> {
        self.passes.get(round).map(|p| p.as_ref())
    }
}

impl<C> Default for Schedule<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> std::fmt::Debug for Schedule<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schedule")
            .field("passes", &self.passes.len())
            .finish()
    }
}
