//! Node identifiers and internal node storage.

use crate::span::Span;
use crate::status::Status;

/// A stable identifier for a node within a [`ParseTree`](crate::ParseTree).
///
/// Identifiers remain valid for the lifetime of the tree they came from; they
/// are meaningless across trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

/// Internal node storage for a [`ParseTree`](crate::ParseTree).
///
/// Trees are grown by the [`Engine`](crate::Engine); access from outside the
/// crate goes through the read-only methods on `ParseTree`.
#[derive(Debug, Clone)]
pub(crate) struct Node<C> {
    pub span: Span,
    pub ctx: C,
    pub status: Status,
    /// Index of the round that last processed (or created) this node.
    ///
    /// * The root starts at `0` and is processed by round `0`.
    /// * Children created in round `r` start at `r + 1`, so round `r + 1`
    ///   picks them up.
    /// * A node that fails in round `r` is retried in round `r + 1`.
    pub depth: usize,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// How many passes have tried and failed to parse this node.
    pub attempts: usize,
}

impl<C> Node<C> {
    pub(crate) fn new_root(span: Span, ctx: C) -> Self {
        Self {
            span,
            ctx,
            status: Status::Unparsed,
            depth: 0,
            parent: None,
            children: Vec::new(),
            attempts: 0,
        }
    }
}
