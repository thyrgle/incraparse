//! Incremental editing: describing text changes and driving re-parses.

use crate::cancel::CancelToken;
use crate::engine::Engine;
use crate::executor::Executor;
use crate::span::Span;
use crate::tree::ParseTree;

/// A single text edit: the old-text range `[start, old_end)` is replaced by
/// the new-text range `[start, new_end)`.
///
/// Positions refer to the text *before* the edit for `start` and `old_end`,
/// and to the text *after* the edit for `new_end`.
///
/// # Examples
///
/// ```
/// use incraparse::Edit;
///
/// // Insert "hello" at offset 4.
/// let insert = Edit::insert(4, 5);
/// assert_eq!(insert, Edit::replace(4, 4, 9));
///
/// // Delete the range 10..14.
/// let delete = Edit::delete(10, 14);
/// assert_eq!(delete.new_end, 10);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// Start of the edited range (valid in both old and new coordinates).
    pub start: usize,
    /// End of the edited range in old-text coordinates.
    pub old_end: usize,
    /// End of the replacement in new-text coordinates.
    pub new_end: usize,
}

impl Edit {
    /// Replaces the old range `[start, old_end)` with the new range
    /// `[start, new_end)`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `start > old_end` or `start > new_end`.
    pub fn replace(start: usize, old_end: usize, new_end: usize) -> Self {
        debug_assert!(start <= old_end, "edit start must not exceed old_end");
        debug_assert!(start <= new_end, "edit start must not exceed new_end");
        Self {
            start,
            old_end,
            new_end,
        }
    }

    /// Inserts `len` bytes at position `at`.
    pub fn insert(at: usize, len: usize) -> Self {
        Self::replace(at, at, at + len)
    }

    /// Deletes the range `[start, end)`.
    pub fn delete(start: usize, end: usize) -> Self {
        Self::replace(start, end, start)
    }

    /// Maps a position from old coordinates to new coordinates.
    ///
    /// Positions inside the deleted region collapse onto `new_end`; a pure
    /// insertion point stays put (it maps to itself via the `start` branch).
    pub(crate) fn map_pos(&self, pos: usize) -> usize {
        if pos <= self.start {
            pos
        } else if pos >= self.old_end {
            self.new_end + (pos - self.old_end)
        } else {
            self.new_end
        }
    }

    /// Maps a span *end* position, as [`map_pos`](Self::map_pos) does for
    /// starts, except for pure insertions: a region ending exactly at the
    /// insertion point absorbs the inserted bytes, so a region that ended at
    /// the end of the file still ends at the (new) end of the file. Without
    /// this, appending text at EOF would leave every region — including the
    /// root — short, and the appended bytes would silently escape parsing.
    pub(crate) fn map_end(&self, pos: usize) -> usize {
        if self.start == self.old_end {
            if pos < self.start {
                pos
            } else {
                self.new_end + (pos - self.old_end)
            }
        } else if pos <= self.start {
            pos
        } else if pos < self.old_end {
            self.new_end
        } else {
            self.new_end + (pos - self.old_end)
        }
    }

    /// Returns `true` if the edit can change the text a span covers.
    ///
    /// Replacements use half-open overlap; pure insertions use closed
    /// containment, so an insert at a span's boundary (e.g. at the end of
    /// the file) still touches the enclosing regions.
    pub(crate) fn touches(&self, span: &Span) -> bool {
        if self.start == self.old_end {
            span.start <= self.start && self.start <= span.end
        } else {
            span.start < self.old_end && self.start < span.end
        }
    }
}

/// Maps `span` from old coordinates to new coordinates, tagging it with the
/// post-edit revision `rev`.
pub(crate) fn map_span(span: Span, edit: &Edit, rev: u64) -> Span {
    Span::new(edit.map_pos(span.start), edit.map_end(span.end), rev)
}

/// A long-lived document: a [`ParseTree`] that can absorb [`Edit`]s.
///
/// `Session::edit` remaps every span into the new coordinates, bumps the
/// source revision, and resets only the nodes the edit could have changed
/// back to [`Unparsed`](crate::Status::Unparsed) — their children stay
/// attached so the next run can *reuse* them.
///
/// Reuse works because the engine matches a re-expansion's children against
/// the existing children by span and context: any child whose span and
/// context are unchanged keeps its identity, its status, and its whole
/// subtree. Only the edited chain (and anything whose context references
/// changed text) gets re-parsed — the rest of the tree is carried over as-is.
///
/// # Examples
///
/// ```
/// use incraparse::{CancelToken, Engine, Edit, Outcome, Pass, ParseTree, Schedule, Session, Span, Status};
///
/// #[derive(Clone, Debug, PartialEq, Eq)]
/// enum Ctx { File, Region }
///
/// struct Split;
/// impl Pass for Split {
///     type Ctx = Ctx;
///     fn parse(&self, _source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
///         match ctx {
///             Ctx::File if span.len() >= 4 => Outcome::Expand(vec![
///                 (Span::new(span.start, span.start + 2, span.rev), Ctx::Region),
///                 (Span::new(span.start + 2, span.end, span.rev), Ctx::Region),
///             ]),
///             Ctx::File => Outcome::Failed,
///             Ctx::Region => Outcome::Done,
///         }
///     }
/// }
///
/// # fn main() {
/// let mut schedule = Schedule::new();
/// schedule.push(Split);
/// schedule.push(Split);
/// let engine = Engine::new(schedule);
///
/// let source = "abcd";
/// let mut session: Session<Ctx> =
///     Session::new(0, Span::new(0, source.len(), 0), Ctx::File);
/// session.run(&engine, source, &incraparse::SerialExecutor, &CancelToken::new());
/// let left = session.tree().children(session.tree().root())[0];
/// assert_eq!(session.tree().status(left), Status::Done);
///
/// // Same-length edit inside the right region: the left region is reused.
/// let source = "abCd";
/// session.edit(Edit::replace(2, 3, 3));
/// session.run(&engine, source, &incraparse::SerialExecutor, &CancelToken::new());
///
/// assert_eq!(session.revision(), 1);
/// assert_eq!(session.tree().children(session.tree().root())[0], left);
/// assert_eq!(session.tree().status(left), Status::Done);
/// # }
/// ```
pub struct Session<C> {
    tree: ParseTree<C>,
}

impl<C: Clone + PartialEq + Send + 'static> Session<C> {
    /// Creates a session whose tree root covers `root_span` of source
    /// revision `source_rev`, carrying `root_ctx`.
    pub fn new(source_rev: u64, root_span: Span, root_ctx: C) -> Self {
        Self {
            tree: ParseTree::new(source_rev, root_span, root_ctx),
        }
    }

    /// The current source revision; bumped by every [`edit`](Self::edit).
    pub fn revision(&self) -> u64 {
        self.tree.source_rev()
    }

    /// The parse tree accumulated so far.
    pub fn tree(&self) -> &ParseTree<C> {
        &self.tree
    }

    /// Mutable access to the tree, for queries that need it (e.g. custom
    /// traversal helpers); prefer [`tree`](Self::tree) otherwise.
    pub fn tree_mut(&mut self) -> &mut ParseTree<C> {
        &mut self.tree
    }

    /// Applies a text edit: remaps spans, bumps the revision, and resets the
    /// touched nodes for re-parsing on the next [`run`](Self::run).
    pub fn edit(&mut self, edit: Edit) {
        self.tree.edit(edit);
    }

    /// Runs the engine over the current tree and `source`, returning the
    /// run report.
    ///
    /// `source` must be the text at the session's current
    /// [`revision`](Self::revision).
    pub fn run<E>(
        &mut self,
        engine: &Engine<C>,
        source: &str,
        exec: &E,
        cancel: &CancelToken,
    ) -> crate::engine::RunReport
    where
        E: Executor,
    {
        engine.run(source, &mut self.tree, exec, cancel)
    }
}
