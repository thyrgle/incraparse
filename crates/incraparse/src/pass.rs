//! The [`Pass`] trait: a single stage in a parse schedule.

use crate::outcome::Outcome;
use crate::span::Span;

/// One stage of a parse schedule.
///
/// A pass is a pure function from a source region plus its context to an
/// [`Outcome`]: the region either expands into smaller child regions, is
/// accepted as parsed, or fails (later passes may retry it).
///
/// `incraparse` is deliberately combinator-agnostic: a pass can wrap any
/// parsing technique — `nom`, `chumsky`, a PEG, regexes, or hand-rolled
/// scanning — behind this trait. The engine only cares about the outcome.
///
/// # Contract
///
/// * Passes must be pure: they must not mutate shared state and must produce
///   the same output for the same `(source, span, ctx)` input.
/// * Every child span returned from [`Outcome::Expand`] must be contained in
///   the input span and on the same source revision. By default a child may
///   cover its parent exactly; with
///   [`EngineConfig::enforce_shrink`](crate::EngineConfig::enforce_shrink)
///   it must be strictly smaller. The engine rejects outcomes that violate
///   this, marking the node [`Failed`](crate::Status::Failed).
/// * Passes must be `Send + Sync` so the engine can run batches on any
///   [`Executor`](crate::Executor).
///
/// # Examples
///
/// ```
/// use incraparse::{Outcome, Pass, Span};
///
/// /// Accepts any region whose text starts with "ok".
/// struct AcceptOk;
///
/// impl Pass for AcceptOk {
///     type Ctx = ();
///
///     fn parse(&self, source: &str, span: Span, _ctx: &()) -> Outcome<()> {
///         let text = &source[span.to_range()];
///         if text.starts_with("ok") {
///             Outcome::Done
///         } else {
///             Outcome::Failed
///         }
///     }
/// }
/// ```
pub trait Pass {
    /// Context threaded into every node this pass processes.
    type Ctx;

    /// Attempts to parse `source[span]` given the node's context.
    ///
    /// `source` is a snapshot of the full source text the tree was built
    /// against; `span` selects this node's region within it.
    fn parse(&self, source: &str, span: Span, ctx: &Self::Ctx) -> Outcome<Self::Ctx>;

    /// A human-readable name used in diagnostics; defaults to the type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
