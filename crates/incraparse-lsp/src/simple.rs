//! The builder-shaped `Language` implementation for the common case.

use std::sync::Arc;

use incraparse::Engine;
use lsp_types::{Diagnostic, DocumentSymbol, SymbolKind};

use crate::document::Document;
use crate::encoding::PositionEncoding;
use crate::server::Language;
use crate::FailedNode;

/// The type of the [`SimpleLanguage::diagnostic_fn`] hook.
pub type DiagnosticFn<C> =
    dyn Fn(&Document<C>, &FailedNode<'_, C>) -> Option<Diagnostic> + Send + Sync;

/// The type of the [`SimpleLanguage::symbols_fn`] hook.
pub type SymbolsFn<C> = dyn Fn(&Document<C>) -> Vec<DocumentSymbol> + Send + Sync;

/// The type of the [`SimpleLanguage::label_fn`] hook.
pub type LabelFn<C> = dyn Fn(&C) -> Option<NodeLabel> + Send + Sync;

/// A [`Language`] built from closures — the Rust equivalent of a Lua
/// language definition.
///
/// For the common server you never implement the [`Language`] trait;
/// describe the language and hand the builder to
/// [`serve`](crate::serve):
///
/// ```
/// use incraparse::Engine;
/// use incraparse_lsp::{Document, FailedNode, SimpleLanguage};
/// use lsp_types::Diagnostic;
///
/// # #[derive(Clone, Debug, PartialEq, Eq)]
/// # enum Ctx { File }
/// # fn build(engine: Engine<Ctx>) {
/// let language = SimpleLanguage::new(engine, Ctx::File)
///     .diagnostic_fn(|_doc, node| {
///         Some(Diagnostic { message: "could not parse".into(), ..Diagnostic::default() })
///     })
///     .label_fn(|ctx| Some(incraparse_lsp::NodeLabel::new("region")));
/// # let _ = language;
/// # }
/// ```
pub struct SimpleLanguage<C> {
    engine: Engine<C>,
    root_ctx: C,
    encoding: PositionEncoding,
    diagnostic_fn: Option<Arc<DiagnosticFn<C>>>,
    symbols_fn: Option<Arc<SymbolsFn<C>>>,
    label_fn: Option<Arc<LabelFn<C>>>,
}

/// A display name (and optional detail) for one tree node — what
/// [`SimpleLanguage::label_fn`] returns to power the outline view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLabel {
    /// The symbol's name.
    pub name: String,
    /// Optional detail shown next to the name (e.g. a parameter list).
    pub detail: Option<String>,
}

impl NodeLabel {
    /// Creates a label with no detail.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: None,
        }
    }

    /// Sets the detail string.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl<C: Clone + PartialEq + Send + 'static> SimpleLanguage<C> {
    /// Creates a builder over an engine and a root context.
    pub fn new(engine: Engine<C>, root_ctx: C) -> Self {
        Self {
            engine,
            root_ctx,
            encoding: PositionEncoding::Utf16,
            diagnostic_fn: None,
            symbols_fn: None,
            label_fn: None,
        }
    }

    /// Sets the negotiated position encoding (default UTF-16).
    pub fn encoding(mut self, encoding: PositionEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// How a failing region becomes a diagnostic; `None` stays silent.
    /// Diagnostics returned with an empty range are filled in from the
    /// node's span.
    pub fn diagnostic_fn(
        mut self,
        f: impl Fn(&Document<C>, &FailedNode<'_, C>) -> Option<Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        self.diagnostic_fn = Some(Arc::new(f));
        self
    }

    /// Full-control outline symbols. Mutually exclusive in spirit with
    /// [`label_fn`](Self::label_fn) — when both are set, this wins.
    pub fn symbols_fn(
        mut self,
        f: impl Fn(&Document<C>) -> Vec<DocumentSymbol> + Send + Sync + 'static,
    ) -> Self {
        self.symbols_fn = Some(Arc::new(f));
        self
    }

    /// Names tree nodes; every named node becomes an outline symbol with
    /// the node's range — symbols without writing a tree walk.
    pub fn label_fn(mut self, f: impl Fn(&C) -> Option<NodeLabel> + Send + Sync + 'static) -> Self {
        self.label_fn = Some(Arc::new(f));
        self
    }
}

/// `C` must additionally be `Sync` because the stored closures accept
/// `&C` from any thread.
impl<C: Clone + PartialEq + Send + Sync + 'static> Language<C> for SimpleLanguage<C> {
    fn supports_symbols(&self) -> bool {
        self.symbols_fn.is_some() || self.label_fn.is_some()
    }

    fn engine(&self) -> &Engine<C> {
        &self.engine
    }

    fn root_ctx(&self) -> C {
        self.root_ctx.clone()
    }

    fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    fn diagnostic(&self, doc: &Document<C>, node: FailedNode<'_, C>) -> Option<Diagnostic> {
        let f = self.diagnostic_fn.as_ref()?;
        f(doc, &node)
    }

    fn symbols(&self, doc: &Document<C>) -> Vec<DocumentSymbol> {
        if let Some(f) = &self.symbols_fn {
            return f(doc);
        }
        if let Some(label) = &self.label_fn {
            let tree = doc.session().tree();
            return tree
                .nodes()
                .filter_map(|id| {
                    let label = label(tree.ctx(id))?;
                    let range = doc.range(tree.span(id));
                    Some(DocumentSymbol {
                        name: label.name,
                        detail: label.detail,
                        kind: SymbolKind::FUNCTION,
                        range,
                        selection_range: range,
                        children: None,
                        tags: None,
                        #[allow(deprecated)]
                        deprecated: None,
                    })
                })
                .collect();
        }
        Vec::new()
    }
}
