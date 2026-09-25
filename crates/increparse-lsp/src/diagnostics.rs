//! Turning failed parse regions into publishable LSP diagnostics.

use increparse::{NodeId, Span, Status};
use lsp_types::Diagnostic;

use crate::Document;

/// One parse-tree node offered to the user's diagnostic hook.
#[derive(Debug, Clone, Copy)]
pub struct FailedNode<'a, C> {
    /// The node's id (usable for further tree queries).
    pub id: NodeId,
    /// The node's parse context.
    pub ctx: &'a C,
    /// Why the node is a candidate: [`Status::Failed`], or
    /// [`Status::Unparsed`] when the schedule ran out with work left.
    pub status: Status,
    /// The node's region.
    pub span: Span,
}

/// Options for [`diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiagnosticsOptions {
    /// When `false` (the default), a node whose *ancestor* is already a
    /// failure is skipped — one broken function yields one diagnostic, not
    /// one per unparsed statement inside it. When `true`, every failing
    /// node is reported.
    pub cascades: bool,
}

/// Builds diagnostics from a settled (or cancelled, or round-capped) parse.
///
/// The tree is walked from the root; every failing node is offered to `hook`
/// (unless its ancestor already failed and `options.cascades` is `false`).
/// The hook turns a [`FailedNode`] into a `Diagnostic` — or `None` to stay
/// silent, e.g. for regions whose context type makes the failure expected.
///
/// Diagnostics returned with the default (zero) range get their range filled
/// in from the node's span, converted with the document's position encoding;
/// hooks that compute their own ranges keep them.
///
/// # Examples
///
/// ```
/// use increparse::{Engine, Outcome, Pass, Schedule, SerialExecutor, Span, Status};
/// use increparse_lsp::{diagnostics, DiagnosticsOptions, Document, PositionEncoding};
/// use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Uri};
///
/// #[derive(Clone, Debug, PartialEq, Eq)]
/// enum Ctx { File, Bad }
///
/// struct Split;
/// impl Pass for Split {
///     type Ctx = Ctx;
///     fn parse(&self, source: &str, span: Span, ctx: &Ctx) -> Outcome<Ctx> {
///         match ctx {
///             Ctx::File if span.len() >= 2 => Outcome::Expand(vec![(
///                 Span::new(span.start + 1, span.end, span.rev),
///                 Ctx::Bad,
///             )]),
///             Ctx::File => Outcome::Done,
///             Ctx::Bad if source[span.to_range()].contains('!') => Outcome::Failed,
///             Ctx::Bad => Outcome::Done,
///         }
///     }
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut schedule = Schedule::new();
/// schedule.push(Split);
/// schedule.push(Split);
/// let engine = Engine::new(schedule);
///
/// let uri: Uri = "file:///x.txt".parse()?;
/// let mut doc = Document::open(uri, 1, "".into(), "ok!".into(), PositionEncoding::Utf16, Ctx::File);
/// doc.apply_changes(&engine, 1, &[], &SerialExecutor, &increparse::CancelToken::new());
///
/// let diags = diagnostics(&doc, DiagnosticsOptions::default(), |node| {
///     let _ = node.status;
///     Some(Diagnostic {
///         severity: Some(DiagnosticSeverity::ERROR),
///         message: "this region failed to parse".into(),
///         ..Diagnostic::default()
///     })
/// });
///
/// assert_eq!(diags.len(), 1);
/// assert_eq!(diags[0].range.start, Position { line: 0, character: 1 });
/// # Ok(())
/// # }
/// ```
pub fn diagnostics<C, F>(
    doc: &Document<C>,
    options: DiagnosticsOptions,
    mut hook: F,
) -> Vec<Diagnostic>
where
    C: Clone + PartialEq + Send + 'static,
    F: FnMut(FailedNode<'_, C>) -> Option<Diagnostic>,
{
    let tree = doc.session().tree();
    let mut out = Vec::new();
    walk(doc, tree.root(), false, options, &mut hook, &mut out);
    out
}

fn walk<C, F>(
    doc: &Document<C>,
    id: increparse::NodeId,
    under_failure: bool,
    options: DiagnosticsOptions,
    hook: &mut F,
    out: &mut Vec<Diagnostic>,
) where
    C: Clone + PartialEq + Send + 'static,
    F: FnMut(FailedNode<'_, C>) -> Option<Diagnostic>,
{
    let tree = doc.session().tree();
    let status = tree.status(id);
    let failing = matches!(status, Status::Failed | Status::Unparsed);

    if failing && (!under_failure || options.cascades) {
        let span = tree.span(id);
        let node = FailedNode {
            id,
            ctx: tree.ctx(id),
            status,
            span,
        };
        if let Some(mut diagnostic) = hook(node) {
            if diagnostic.range == Diagnostic::default().range {
                diagnostic.range = doc.range(span);
            }
            out.push(diagnostic);
        }
    }

    let child_under_failure = under_failure || (failing && !options.cascades);
    for child in tree.children(id) {
        walk(doc, *child, child_under_failure, options, hook, out);
    }
}
