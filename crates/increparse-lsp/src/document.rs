//! One open editor file: text, session, version, and position encoding.

use increparse::{CancelToken, Edit, Engine, Executor, RunReport, Session, Span};
use lsp_types::{Location, Position, Range, TextDocumentContentChangeEvent, Uri};

use crate::encoding::PositionEncoding;
use crate::line_index::LineIndex;

/// A single open document, bridging LSP change events and an increparse
/// [`Session`].
///
/// `apply_changes` is the whole workflow: hand it the `didChange` payload and
/// an engine; it splices the text, translates every change into a byte-range
/// [`Edit`] (so the parse tree reuses everything the edit did not touch),
/// and runs the engine synchronously. How you thread that call — inline in
/// your server loop, or on a background thread with a [`CancelToken`] — is
/// your framework's business.
///
/// # Examples
///
/// ```
/// use increparse::{CancelToken, Engine, Outcome, Pass, Schedule, SerialExecutor, Span};
/// use increparse_lsp::{Document, PositionEncoding};
/// use lsp_types::{TextDocumentContentChangeEvent, Uri};
///
/// #[derive(Clone, Debug, PartialEq, Eq)]
/// enum Ctx { File }
///
/// struct Accept;
/// impl Pass for Accept {
///     type Ctx = Ctx;
///     fn parse(&self, _source: &str, _span: Span, _ctx: &Ctx) -> Outcome<Ctx> {
///         Outcome::Done
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut schedule = Schedule::new();
/// schedule.push(Accept);
/// let engine = Engine::new(schedule);
///
/// let uri: Uri = "file:///w.txt".parse()?;
/// let mut doc = Document::open(uri, 1, "abc".into(), PositionEncoding::Utf16, Ctx::File);
///
/// // A client insert at (0, 1): "abc" -> "aXbc".
/// let change = TextDocumentContentChangeEvent {
///     range: Some(lsp_types::Range {
///         start: lsp_types::Position { line: 0, character: 1 },
///         end: lsp_types::Position { line: 0, character: 1 },
///     }),
///     range_length: None,
///     text: "X".into(),
/// };
/// let report = doc.apply_changes(
///     &engine, 2, &[change], &SerialExecutor, &CancelToken::new(),
/// );
///
/// assert!(report.reached_fixpoint);
/// assert_eq!(doc.version(), 2);
/// assert_eq!(doc.text(), "aXbc");
/// assert_eq!(doc.revision(), 1);
/// # Ok(())
/// # }
/// ```
pub struct Document<C> {
    uri: Uri,
    text: String,
    session: Session<C>,
    version: i32,
    encoding: PositionEncoding,
    index: LineIndex,
}

impl<C: Clone + PartialEq + Send + 'static> Document<C> {
    /// Opens a document at `version` with the full initial `text`.
    ///
    /// The tree root covers the whole text and carries `root_ctx`.
    pub fn open(
        uri: Uri,
        version: i32,
        text: String,
        encoding: PositionEncoding,
        root_ctx: C,
    ) -> Self {
        let index = LineIndex::new(&text);
        let session = Session::new(0, Span::new(0, text.len(), 0), root_ctx);
        Self {
            uri,
            text,
            session,
            version,
            encoding,
            index,
        }
    }

    /// The document's URI.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The client's document version.
    pub fn version(&self) -> i32 {
        self.version
    }

    /// The negotiated position encoding.
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// The line index of the current text.
    pub fn line_index(&self) -> &LineIndex {
        &self.index
    }

    /// The parse session (tree, revisions, edits).
    pub fn session(&self) -> &Session<C> {
        &self.session
    }

    /// Mutable access to the session, for custom workflows.
    pub fn session_mut(&mut self) -> &mut Session<C> {
        &mut self.session
    }

    /// Current source revision of the parse tree (bumped once per applied
    /// change event).
    pub fn revision(&self) -> u64 {
        self.session.revision()
    }

    /// Converts a tree span to an LSP location in this document — the
    /// convenient shape for go-to-definition answers.
    pub fn location(&self, span: Span) -> Location {
        Location {
            uri: self.uri.clone(),
            range: self.range(span),
        }
    }

    /// Converts a tree span to an LSP range in the document's encoding.
    pub fn range(&self, span: Span) -> Range {
        Range {
            start: self.index.position(&self.text, span.start, self.encoding),
            end: self.index.position(&self.text, span.end, self.encoding),
        }
    }

    /// Converts an LSP position to a byte offset (clamped like
    /// [`LineIndex::offset`]).
    pub fn offset(&self, position: Position) -> usize {
        self.index
            .offset(&self.text, position, self.encoding)
            .unwrap_or(self.text.len())
    }

    /// Applies a `didChange` batch and runs the engine once.
    ///
    /// Events are applied in order, each interpreted against the text left
    /// by the previous one, per the LSP spec. A full-text event (`range ==
    /// None`) replaces the whole document; ranged events splice their text
    /// into the given range. Positions past the end of a line or document
    /// are clamped, matching common client behavior while typing.
    ///
    /// `version` becomes the document's new version and is returned by
    /// [`version`](Self::version); the parse tree sees one revision bump per
    /// applied event and a single run at the end.
    pub fn apply_changes<E>(
        &mut self,
        engine: &Engine<C>,
        version: i32,
        changes: &[TextDocumentContentChangeEvent],
        exec: &E,
        cancel: &CancelToken,
    ) -> RunReport
    where
        E: Executor,
    {
        for change in changes {
            match change.range {
                None => {
                    let edit = Edit::replace(0, self.text.len(), change.text.len());
                    self.text = change.text.clone();
                    self.session.edit(edit);
                }
                Some(range) => {
                    let start = self.offset(range.start);
                    let end = self.offset(range.end).max(start);
                    let edit = Edit::replace(start, end, start + change.text.len());
                    self.text.replace_range(start..end, &change.text);
                    self.session.edit(edit);
                }
            }
            self.index = LineIndex::new(&self.text);
        }
        self.version = version;
        self.session.run(engine, &self.text, exec, cancel)
    }
}
