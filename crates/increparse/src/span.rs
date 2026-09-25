//! A half-open byte range into a source text, tagged with a source revision.

use std::fmt;
use std::ops::Range;

/// A half-open byte range `[start, end)` into a source text, tagged with the
/// revision of that text it refers to.
///
/// Spans are the currency of this crate: every node in a
/// [`ParseTree`](crate::ParseTree) covers a span of the source, and passes
/// consume a span and produce child spans.
///
/// # Examples
///
/// ```
/// use increparse::Span;
///
/// let outer = Span::new(0, 10, 0);
/// let inner = Span::new(2, 5, 0);
///
/// assert_eq!(outer.len(), 10);
/// assert!(outer.contains(&inner));
/// assert!(outer.overlaps(&inner));
/// assert!(!inner.contains(&outer));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Byte offset of the first byte covered by the span.
    pub start: usize,
    /// Byte offset one past the last byte covered by the span.
    pub end: usize,
    /// Revision of the source text this span indexes into.
    pub rev: u64,
}

impl Span {
    /// Creates a span over `[start, end)` of source revision `rev`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `start > end`.
    pub fn new(start: usize, end: usize, rev: u64) -> Self {
        debug_assert!(start <= end, "span start must not exceed its end");
        Self { start, end, rev }
    }

    /// Length of the span in bytes.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Returns `true` if the span covers zero bytes.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Returns `true` if `other` lies entirely within `self`.
    pub fn contains(&self, other: &Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Returns `true` if `self` contains the byte `offset`:
    /// `start <= offset < end`. Zero-width spans never contain anything.
    pub fn contains_offset(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }

    /// Returns `true` if the two spans share at least one byte.
    pub fn overlaps(&self, other: &Span) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Converts the span to a `Range<usize>` suitable for slicing source text.
    pub fn to_range(&self) -> Range<usize> {
        self.start..self.end
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}@{}", self.start, self.end, self.rev)
    }
}
