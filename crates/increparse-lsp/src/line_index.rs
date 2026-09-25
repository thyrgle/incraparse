//! Mapping between LSP `(line, character)` positions and byte offsets.

use crate::encoding::PositionEncoding;
use lsp_types::Position;

/// A line-start table over one text snapshot.
///
/// Rebuild one per text revision ([`Document`](crate::Document) does this
/// automatically); conversions are then O(log lines) plus O(line length) for
/// the column walk.
///
/// # Examples
///
/// ```
/// use increparse_lsp::{LineIndex, PositionEncoding};
/// use lsp_types::Position;
///
/// let text = "héllo\nwörld";
/// let index = LineIndex::new(text);
///
/// // "héllo" is 6 bytes / 5 UTF-16 units; end of line 0:
/// assert_eq!(
///     index.offset(text, Position::new(0, 5), PositionEncoding::Utf16),
///     Some(6)
/// );
/// assert_eq!(
///     index.position(text, 6, PositionEncoding::Utf16),
///     Position::new(0, 5)
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    line_starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Builds the index for `text`.
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            line_starts,
            len: text.len(),
        }
    }

    /// Number of lines (a trailing newline starts a final empty line).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte offset where `line` begins.
    pub fn line_start(&self, line: u32) -> Option<usize> {
        self.line_starts.get(line as usize).copied()
    }

    /// Byte offset just past the last byte of `line`, excluding the line
    /// terminator.
    pub fn line_end(&self, text: &str, line: u32) -> Option<usize> {
        let start = self.line_start(line)?;
        let next = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.len);
        let mut end = next;
        if end > start && text.as_bytes()[end - 1] == b'\n' {
            end -= 1;
        }
        Some(end)
    }

    /// Converts a `Position` to a byte offset.
    ///
    /// Out-of-range lines clamp to the last line; overshooting columns clamp
    /// to the end of the line (LSP clients legitimately send both while
    /// typing). Returns `None` only if `position.line` overflows `u32`.
    pub fn offset(
        &self,
        text: &str,
        position: Position,
        encoding: PositionEncoding,
    ) -> Option<usize> {
        let line = position.line.min(self.line_count() as u32 - 1);
        let start = self.line_start(line)?;
        let end = self.line_end(text, line)?;
        let line_text = text
            .get(start..end)
            .unwrap_or_else(|| text.get(start..).unwrap_or(""));
        Some(start + encoding.offset_of_units(line_text, position.character))
    }

    /// Converts a byte offset to a `Position`.
    ///
    /// Out-of-bounds offsets clamp to the text length; offsets inside a
    /// multibyte character or a line terminator round down to the char
    /// boundary before them.
    pub fn position(&self, text: &str, offset: usize, encoding: PositionEncoding) -> Position {
        let offset = offset.min(self.len);
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let start = self.line_starts[line];
        let mut end = offset;
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        let line_text = text.get(start..end).unwrap_or("");
        Position {
            line: line as u32,
            character: encoding.units_of(line_text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "abc\ndefé\n\nx😀\n";

    #[test]
    fn line_structure() {
        let index = LineIndex::new(TEXT);
        assert_eq!(index.line_count(), 5);
        assert_eq!(index.line_start(0), Some(0));
        assert_eq!(index.line_start(1), Some(4));
        assert_eq!(index.line_start(2), Some(10));
        assert_eq!(index.line_start(3), Some(11));
        assert_eq!(index.line_start(4), Some(17));
        assert_eq!(index.line_start(5), None);
        assert_eq!(index.line_end(TEXT, 0), Some(3));
        assert_eq!(index.line_end(TEXT, 1), Some(9));
        assert_eq!(index.line_end(TEXT, 2), Some(10));
        assert_eq!(index.line_end(TEXT, 3), Some(16));
        assert_eq!(index.line_end(TEXT, 4), Some(17));
    }

    #[test]
    fn round_trip_utf16() {
        let index = LineIndex::new(TEXT);
        let enc = PositionEncoding::Utf16;

        // Line 3 is "x😀": 'x' at byte 11, the 4-byte emoji at 12..16.
        let pos = Position {
            line: 3,
            character: 1,
        };
        assert_eq!(index.offset(TEXT, pos, enc), Some(12));
        assert_eq!(index.position(TEXT, 12, enc), pos);

        // Character 2 lands inside the surrogate pair: clamps past it, and
        // the position normalizes to the end of the line (3 units).
        let overshoot = Position {
            line: 3,
            character: 2,
        };
        assert_eq!(index.offset(TEXT, overshoot, enc), Some(16));
        assert_eq!(
            index.position(TEXT, 16, enc),
            Position {
                line: 3,
                character: 3
            }
        );
    }

    #[test]
    fn round_trip_utf8_and_utf32() {
        let index = LineIndex::new(TEXT);

        // UTF-8 columns are byte offsets within the line.
        assert_eq!(
            index.offset(
                TEXT,
                Position {
                    line: 3,
                    character: 2
                },
                PositionEncoding::Utf8
            ),
            Some(13)
        );
        // Offset 13 is inside the emoji: rounds down to its first byte.
        assert_eq!(
            index.position(TEXT, 13, PositionEncoding::Utf8),
            Position {
                line: 3,
                character: 1
            }
        );

        // UTF-32 columns are code points.
        assert_eq!(
            index.offset(
                TEXT,
                Position {
                    line: 3,
                    character: 2
                },
                PositionEncoding::Utf32
            ),
            Some(16)
        );
        assert_eq!(
            index.position(TEXT, 16, PositionEncoding::Utf32),
            Position {
                line: 3,
                character: 2
            }
        );
    }

    #[test]
    fn out_of_range_clamps() {
        let index = LineIndex::new(TEXT);
        let enc = PositionEncoding::Utf16;

        assert_eq!(
            index.offset(
                TEXT,
                Position {
                    line: 99,
                    character: 0
                },
                enc
            ),
            Some(17)
        );
        assert_eq!(
            index.offset(
                TEXT,
                Position {
                    line: 0,
                    character: 99
                },
                enc
            ),
            Some(3)
        );
        assert_eq!(
            index.position(TEXT, 999, enc),
            Position {
                line: 4,
                character: 0
            }
        );
    }

    #[test]
    fn empty_text_degenerates() {
        let index = LineIndex::new("");
        assert_eq!(index.line_count(), 1);
        assert_eq!(
            index.offset(
                "",
                Position {
                    line: 0,
                    character: 0
                },
                PositionEncoding::Utf16
            ),
            Some(0)
        );
        assert_eq!(
            index.position("", 0, PositionEncoding::Utf16),
            Position {
                line: 0,
                character: 0
            }
        );
    }
}
