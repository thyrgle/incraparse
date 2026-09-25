//! Units of work handed to executors.

use crate::node::NodeId;
use crate::span::Span;

/// One ready node prepared for execution by a round.
///
/// The engine snapshots the node's span and context into the job so executors
/// can run jobs without holding borrows on the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job<C> {
    /// The tree node this job was built from.
    pub node: NodeId,
    /// The node's region of the source.
    pub span: Span,
    /// Snapshot of the node's context.
    pub ctx: C,
    /// Index of the scheduled pass that will process this job.
    pub pass_index: usize,
}
