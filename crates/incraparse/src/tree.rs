//! The arena-backed parse tree produced by engine runs.

use crate::job::Job;
use crate::node::{Node, NodeId};
use crate::outcome::Outcome;
use crate::span::Span;
use crate::status::{Status, StatusCounts};

/// What applying an outcome did to a node; used to fill the run report.
pub(crate) enum Applied {
    Expanded,
    Done,
    Failed,
}

/// A tree of source regions grown by the [`Engine`](crate::Engine).
///
/// The root covers the whole program; each node holds a [`Span`], a context
/// value `C`, and a [`Status`]. The engine grows the tree round by round:
/// every [`Unparsed`](Status::Unparsed) node (and every
/// [`Failed`](Status::Failed) node with passes left) is offered to the pass
/// scheduled for its round, which either expands it into children, accepts
/// it, or fails it for a later pass to retry.
///
/// Nodes are stored in an arena and referenced by stable [`NodeId`]s, which
/// makes incremental re-parses (invalidating only subtrees overlapping an
/// edit) straightforward in a future revision.
///
/// # Examples
///
/// ```
/// use incraparse::{ParseTree, Span};
///
/// let tree: ParseTree<()> = ParseTree::new(0, Span::new(0, 11, 0), ());
/// assert_eq!(tree.len(), 1);
/// assert_eq!(tree.status(tree.root()), incraparse::Status::Unparsed);
/// ```
pub struct ParseTree<C> {
    rev: u64,
    nodes: Vec<Node<C>>,
    root: NodeId,
}

impl<C> ParseTree<C> {
    /// Creates a tree whose root covers `root_span` of source revision
    /// `source_rev`, carrying `root_ctx`.
    pub fn new(source_rev: u64, root_span: Span, root_ctx: C) -> Self {
        Self {
            rev: source_rev,
            nodes: vec![Node::new_root(root_span, root_ctx)],
            root: NodeId(0),
        }
    }

    /// The source revision this tree was built against.
    pub fn source_rev(&self) -> u64 {
        self.rev
    }

    /// The root node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Total number of live nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|node| node.alive).count()
    }

    /// Returns `true` if the tree has no nodes at all.
    ///
    /// A freshly constructed tree always contains its root, so this is only
    /// observable on trees built through interior paths; it exists to pair
    /// with [`len`](Self::len).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns `true` if the tree contains only the root.
    pub fn is_root_only(&self) -> bool {
        self.nodes.len() == 1
    }

    /// The region of the source covered by `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn span(&self, id: NodeId) -> Span {
        self.nodes[id.0].span
    }

    /// The context carried by `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn ctx(&self, id: NodeId) -> &C {
        &self.nodes[id.0].ctx
    }

    /// The lifecycle status of `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn status(&self, id: NodeId) -> Status {
        self.nodes[id.0].status
    }

    /// The round index `id` will be (or was last) processed at.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn depth(&self, id: NodeId) -> usize {
        self.nodes[id.0].depth
    }

    /// How many passes have tried and failed to parse `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn attempts(&self, id: NodeId) -> usize {
        self.nodes[id.0].attempts
    }

    /// The parent of `id`, or `None` for the root.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.0].parent
    }

    /// The children of `id`, in the order the pass produced them.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0].children
    }

    /// An iterator over every live node id in the tree, in creation order.
    ///
    /// Nodes dropped by an incremental re-parse (see [`edit`](Self::edit))
    /// are not yielded; their ids remain reserved and must not be reused.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.alive)
            .map(|(index, _)| NodeId(index))
    }

    /// The slice of `source` covered by `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree, or if the node's span is out
    /// of bounds for `source`.
    pub fn text<'a>(&self, source: &'a str, id: NodeId) -> &'a str {
        &source[self.span(id).to_range()]
    }

    /// Applies a text edit in place: every live span is remapped into the
    /// new coordinates and tagged with a fresh source revision, and every
    /// node the edit could have changed is reset to
    /// [`Unparsed`](Status::Unparsed) for re-parsing on the next run.
    ///
    /// A touched node restarts at its home round (the round whose pass first
    /// processed it), with its failure history cleared — edited text gets a
    /// genuinely fresh chance at every pass, including ones it had
    /// previously exhausted.
    ///
    /// Touched nodes keep their children attached: the next run re-expands
    /// their parents and matches the new children against the old ones, so
    /// unchanged regions are reused instead of re-parsed (see the
    /// [`Session`](crate::Session) docs).
    pub fn edit(&mut self, edit: crate::session::Edit) {
        let new_rev = self.rev + 1;
        self.rev = new_rev;
        for node in &mut self.nodes {
            if !node.alive {
                continue;
            }
            let touched = edit.touches(&node.span);
            node.span = crate::session::map_span(node.span, &edit, new_rev);
            if touched {
                node.status = Status::Unparsed;
                node.depth -= node.attempts;
                node.attempts = 0;
            }
        }
    }

    /// Per-status node counts for the whole tree (live nodes only).
    pub fn status_counts(&self) -> StatusCounts {
        let mut counts = StatusCounts::default();
        for node in &self.nodes {
            if !node.alive {
                continue;
            }
            match node.status {
                Status::Unparsed => counts.unparsed += 1,
                Status::Expanded => counts.expanded += 1,
                Status::Done => counts.done += 1,
                Status::Failed => counts.failed += 1,
            }
        }
        counts
    }

    /// Returns `true` if `id` still has work a pass could do within
    /// `max_rounds` rounds: it is [`Unparsed`](Status::Unparsed), or
    /// [`Failed`](Status::Failed) with a retry left in the schedule.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn is_pending(&self, id: NodeId, max_rounds: usize) -> bool {
        let node = &self.nodes[id.0];
        node.alive
            && (node.status.is_unparsed() || (node.status.is_failed() && node.depth < max_rounds))
    }

    /// Returns `true` if `id` and all of its descendants have no work left
    /// within `max_rounds` rounds.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not part of this tree.
    pub fn is_settled(&self, id: NodeId, max_rounds: usize) -> bool {
        !self.is_pending(id, max_rounds)
            && self
                .children(id)
                .iter()
                .all(|child| self.is_settled(*child, max_rounds))
    }

    /// Every node id with work remaining within `max_rounds` rounds.
    pub fn pending(&self, max_rounds: usize) -> Vec<NodeId> {
        self.nodes()
            .filter(|id| self.is_pending(*id, max_rounds))
            .collect()
    }

    /// Collects the jobs for all live nodes ready to be processed in `round`.
    pub(crate) fn ready_jobs(&self, round: usize) -> Vec<Job<C>>
    where
        C: Clone,
    {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.alive
                    && node.depth == round
                    && matches!(node.status, Status::Unparsed | Status::Failed)
            })
            .map(|(index, node)| Job {
                node: NodeId(index),
                span: node.span,
                ctx: node.ctx.clone(),
                pass_index: round,
            })
            .collect()
    }

    /// Merges the outcome of processing `id` in `round` back into the tree.
    ///
    /// Child spans must live on the same source revision as their parent and
    /// be contained in it; when `enforce_shrink` is set they must also be
    /// strictly smaller. Violating outcomes mark the node failed.
    ///
    /// When the node already has children (it was invalidated by an edit and
    /// is being re-expanded), each produced child is matched against the
    /// existing children by span and context: a match is *reused* — same
    /// node id, status, and subtree — and unmatched old children are dropped
    /// along with their subtrees.
    pub(crate) fn apply(
        &mut self,
        id: NodeId,
        outcome: Outcome<C>,
        round: usize,
        enforce_shrink: bool,
    ) -> Applied
    where
        C: PartialEq,
    {
        let parent_span = self.nodes[id.0].span;
        match outcome {
            Outcome::Done => {
                self.detach_children(id);
                self.set_status(id, Status::Done);
                Applied::Done
            }
            Outcome::Failed => {
                self.detach_children(id);
                self.mark_failed(id, round);
                Applied::Failed
            }
            Outcome::Expand(children) => {
                if children.is_empty() {
                    self.detach_children(id);
                    self.set_status(id, Status::Done);
                    return Applied::Done;
                }
                let valid = children.iter().all(|(span, _)| {
                    span.rev == parent_span.rev
                        && parent_span.contains(span)
                        && (!enforce_shrink || span.len() < parent_span.len())
                });
                if !valid {
                    self.detach_children(id);
                    self.mark_failed(id, round);
                    return Applied::Failed;
                }
                let next_depth = round + 1;

                let old_children = std::mem::take(&mut self.nodes[id.0].children);
                let mut reused = vec![None; children.len()];
                let mut orphans = old_children;
                for (slot, (span, ctx)) in children.iter().enumerate() {
                    if let Some(position) = orphans.iter().position(|child| {
                        let node = &self.nodes[child.0];
                        node.alive && node.span == *span && node.ctx == *ctx
                    }) {
                        reused[slot] = Some(orphans.remove(position));
                    }
                }
                for orphan in &orphans {
                    self.detach_recursive(*orphan);
                }

                let mut child_ids = Vec::with_capacity(children.len());
                for (slot, (span, ctx)) in children.into_iter().enumerate() {
                    if let Some(kept) = reused[slot] {
                        child_ids.push(kept);
                        continue;
                    }
                    let child_index = self.nodes.len();
                    child_ids.push(NodeId(child_index));
                    self.nodes.push(Node {
                        span,
                        ctx,
                        status: Status::Unparsed,
                        depth: next_depth,
                        parent: Some(id),
                        children: Vec::new(),
                        attempts: 0,
                        alive: true,
                    });
                }
                let node = &mut self.nodes[id.0];
                node.status = Status::Expanded;
                node.children = child_ids;
                Applied::Expanded
            }
        }
    }

    fn detach_children(&mut self, id: NodeId) {
        let children = std::mem::take(&mut self.nodes[id.0].children);
        for child in children {
            self.detach_recursive(child);
        }
    }

    fn detach_recursive(&mut self, id: NodeId) {
        self.nodes[id.0].alive = false;
        let children = std::mem::take(&mut self.nodes[id.0].children);
        for child in children {
            self.detach_recursive(child);
        }
    }

    fn set_status(&mut self, id: NodeId, status: Status) {
        self.nodes[id.0].status = status;
    }

    fn mark_failed(&mut self, id: NodeId, round: usize) {
        let node = &mut self.nodes[id.0];
        node.status = Status::Failed;
        node.depth = round + 1;
        node.attempts += 1;
    }
}

impl<C> std::fmt::Debug for ParseTree<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseTree")
            .field("rev", &self.rev)
            .field("nodes", &self.nodes.len())
            .finish()
    }
}
