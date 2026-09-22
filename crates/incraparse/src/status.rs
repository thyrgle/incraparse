//! Lifecycle states of nodes in a parse tree.

use std::fmt;

/// Lifecycle state of a node in a [`ParseTree`](crate::ParseTree).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    /// The node is waiting to be processed by the pass scheduled for its round.
    Unparsed,
    /// The pass expanded the node; its children carry the remaining work.
    Expanded,
    /// The pass accepted the node as fully parsed; it has no children.
    Done,
    /// The pass that processed the node could not parse it.
    ///
    /// A failed node is retried once per remaining pass in the schedule; once
    /// every pass has had a turn the failure is permanent. Kept in the tree so
    /// consumers (e.g. an LSP) still see the coarse structure around errors.
    Failed,
}

impl Status {
    /// Returns `true` if the status is [`Status::Unparsed`].
    pub fn is_unparsed(self) -> bool {
        matches!(self, Status::Unparsed)
    }

    /// Returns `true` if the status is [`Status::Failed`].
    pub fn is_failed(self) -> bool {
        matches!(self, Status::Failed)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Status::Unparsed => "unparsed",
            Status::Expanded => "expanded",
            Status::Done => "done",
            Status::Failed => "failed",
        };
        f.write_str(name)
    }
}

/// Per-status node counts for a whole tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Number of nodes with status [`Status::Unparsed`].
    pub unparsed: usize,
    /// Number of nodes with status [`Status::Expanded`].
    pub expanded: usize,
    /// Number of nodes with status [`Status::Done`].
    pub done: usize,
    /// Number of nodes with status [`Status::Failed`].
    pub failed: usize,
}

impl StatusCounts {
    /// Returns the count recorded for `status`.
    pub fn get(&self, status: Status) -> usize {
        match status {
            Status::Unparsed => self.unparsed,
            Status::Expanded => self.expanded,
            Status::Done => self.done,
            Status::Failed => self.failed,
        }
    }

    /// Total number of nodes counted (the sum over all statuses).
    pub fn total(&self) -> usize {
        self.unparsed + self.expanded + self.done + self.failed
    }
}
