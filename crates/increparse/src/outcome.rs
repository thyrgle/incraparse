//! The result a pass reports for a node.

use crate::span::Span;

impl<C> Outcome<C> {
    /// Expands the region into a single child — the common case.
    ///
    /// Equivalent to `Outcome::Expand(vec![(span, ctx)])`.
    pub fn one(span: Span, ctx: C) -> Self {
        Outcome::Expand(vec![(span, ctx)])
    }
}

/// What a [`Pass`](crate::Pass) instructs the engine to do with a node.
///
/// # Examples
///
/// ```
/// use increparse::{Outcome, Span};
///
/// let outcome: Outcome<()> = Outcome::Expand(vec![(Span::new(0, 3, 0), ())]);
/// assert!(matches!(outcome, Outcome::Expand(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<C> {
    /// Replaces the node's unparsed region with smaller child regions, each
    /// carrying its own context for later rounds.
    ///
    /// An empty vector is treated as [`Outcome::Done`].
    Expand(Vec<(Span, C)>),
    /// Accepts the region as parsed; no children are created.
    Done,
    /// The pass cannot parse this region. Later passes in the schedule may
    /// retry it.
    Failed,
}
