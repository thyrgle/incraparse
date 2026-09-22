//! LSP adapter for [`incraparse`]: a framework-agnostic bridge between the
//! Language Server Protocol and incremental multi-pass parsing.
//!
//! The crate depends only on [`lsp_types`] — no server framework, no async
//! runtime, no I/O. You keep your server loop (`lsp-server`, `tower-lsp`,
//! `async-lsp`, a hand-rolled loop) and use this crate for the plumbing that
//! is easy to get wrong:
//!
//! * **Position encoding** — LSP positions are `(line, character)` pairs in
//!   UTF-8, UTF-16, or UTF-32 code units (negotiated via the
//!   `positionEncoding` capability); incraparse spans are byte offsets.
//!   [`LineIndex`] converts between the two, correctly, across multibyte
//!   text.
//! * **[`Document`]** — one open file: its text, its [`Session`], the client
//!   version, and the negotiated encoding. `didOpen`/`didChange` events go
//!   in; a run report comes out. Incremental change events are translated
//!   into byte-range [`Edit`]s so the tree reuses everything the edit did
//!   not touch.
//! * **Diagnostics** — walk the settled tree, hand [`Failed`](incraparse::Status)
//!   regions to your language-specific hook, and get back publishable
//!   `Diagnostic`s with correctly converted ranges.
//!
//! # Examples
//!
//! ```
//! use incraparse::{Engine, Outcome, Pass, Schedule, SerialExecutor, Span, Status};
//! use incraparse_lsp::{Document, PositionEncoding};
//! use lsp_types::Uri;
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! enum Ctx { File }
//!
//! struct Accept;
//! impl Pass for Accept {
//!     type Ctx = Ctx;
//!     fn parse(&self, _source: &str, _span: Span, _ctx: &Ctx) -> Outcome<Ctx> {
//!         Outcome::Done
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let uri: Uri = "file:///hello.txt".parse()?;
//! let mut doc = Document::open(uri, 0, "héllo wörld".into(), PositionEncoding::Utf16, Ctx::File);
//!
//! let mut schedule = Schedule::new();
//! schedule.push(Accept);
//! let engine = Engine::new(schedule);
//!
//! // The client edits "héllo" -> "héy": a same-length replace at byte 3.
//! // Translate the client's change events into `Edit`s via
//! // `Document::apply_changes`, then inspect the report:
//! let text = doc.text().to_string();
//! let report = engine.run(
//!     &text,
//!     doc.session_mut().tree_mut(),
//!     &SerialExecutor,
//!     &incraparse::CancelToken::new(),
//! );
//! assert!(report.reached_fixpoint);
//! # Ok(())
//! # }
//! ```
//!
//! A complete (small) server lives in `examples/mini_lang_server.rs`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod document;
mod encoding;
mod line_index;

pub use document::Document;
pub use encoding::PositionEncoding;
pub use line_index::LineIndex;
