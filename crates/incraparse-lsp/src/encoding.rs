//! Negotiation of the LSP `positionEncoding` capability.

use lsp_types::PositionEncodingKind;

/// The character-unit convention a client uses for `Position.character`.
///
/// LSP 3.17+ clients advertise the encodings they support via the
/// `general.positionEncodings` client capability and the server picks one in
/// its `InitializeResult`; older clients always mean UTF-16. All conversions
/// in this crate funnel through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionEncoding {
    /// `Position.character` counts UTF-8 bytes (code units of UTF-8).
    Utf8,
    /// `Position.character` counts UTF-16 code units — the LSP default and
    /// what most clients (VS Code included) use.
    Utf16,
    /// `Position.character` counts Unicode code points (UTF-32 code units).
    Utf32,
}

impl PositionEncoding {
    /// The capability to advertise in `InitializeResult.capabilities.position_encoding`.
    pub fn capability(self) -> PositionEncodingKind {
        match self {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
            PositionEncoding::Utf32 => PositionEncodingKind::UTF32,
        }
    }

    /// Selects an encoding from a client-advertised capability list,
    /// preferring UTF-8 (cheapest to convert) over UTF-16 over UTF-32.
    ///
    /// Returns `None` — and callers should fall back to
    /// [`PositionEncoding::Utf16`], the LSP default — if the client
    /// advertises none of the supported encodings.
    pub fn negotiate(offered: &[PositionEncodingKind]) -> Option<Self> {
        let from_str = |s: &str| match s {
            "utf-8" => Some(PositionEncoding::Utf8),
            "utf-16" => Some(PositionEncoding::Utf16),
            "utf-32" => Some(PositionEncoding::Utf32),
            _ => None,
        };
        offered
            .iter()
            .filter_map(|kind| from_str(kind.as_str()))
            .min_by_key(|enc| match enc {
                PositionEncoding::Utf8 => 0,
                PositionEncoding::Utf16 => 1,
                PositionEncoding::Utf32 => 2,
            })
    }

    /// Number of code units `text` occupies in this encoding.
    pub(crate) fn units_of(self, text: &str) -> u32 {
        match self {
            PositionEncoding::Utf8 => text.len() as u32,
            PositionEncoding::Utf16 => text.chars().map(char::len_utf16).sum::<usize>() as u32,
            PositionEncoding::Utf32 => text.chars().count() as u32,
        }
    }

    /// Byte offset of the `units`-th code unit boundary within `text`.
    ///
    /// For UTF-8 the units are bytes, so the result is simply
    /// `min(units, text.len())`. For UTF-16/UTF-32, positions that land
    /// inside a multi-unit character (e.g. the middle of a surrogate pair)
    /// round up to the next char boundary, and positions past the end of
    /// `text` clamp to `text.len()`.
    pub(crate) fn offset_of_units(self, text: &str, units: u32) -> usize {
        if self == PositionEncoding::Utf8 {
            return text.len().min(units as usize);
        }
        let mut seen = 0u32;
        for (byte_idx, ch) in text.char_indices() {
            if seen >= units {
                return byte_idx;
            }
            seen += match self {
                PositionEncoding::Utf8 => unreachable!(),
                PositionEncoding::Utf16 => ch.len_utf16() as u32,
                PositionEncoding::Utf32 => 1,
            };
        }
        text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "aé😀b"; // 'a' (1) 'é' (2) '😀' (4, 2 utf16 units) 'b' (1)

    #[test]
    fn units_of_counts_per_encoding() {
        assert_eq!(PositionEncoding::Utf8.units_of(TEXT), 8);
        assert_eq!(PositionEncoding::Utf16.units_of(TEXT), 5);
        assert_eq!(PositionEncoding::Utf32.units_of(TEXT), 4);
    }

    #[test]
    fn offset_of_units_hits_char_boundaries() {
        let cases = [
            (PositionEncoding::Utf8, 0usize, 0usize),
            (PositionEncoding::Utf8, 1, 1),
            (PositionEncoding::Utf8, 3, 3),
            (PositionEncoding::Utf8, 7, 7),
            (PositionEncoding::Utf8, 8, 8),
            (PositionEncoding::Utf8, 100, 8),
            (PositionEncoding::Utf16, 0, 0),
            (PositionEncoding::Utf16, 1, 1),
            (PositionEncoding::Utf16, 2, 3),
            (PositionEncoding::Utf16, 3, 7),
            (PositionEncoding::Utf16, 4, 7),
            (PositionEncoding::Utf16, 5, 8),
            (PositionEncoding::Utf16, 99, 8),
            (PositionEncoding::Utf32, 0, 0),
            (PositionEncoding::Utf32, 2, 3),
            (PositionEncoding::Utf32, 3, 7),
            (PositionEncoding::Utf32, 4, 8),
        ];
        for (enc, units, expected) in cases {
            assert_eq!(
                enc.offset_of_units(TEXT, units as u32),
                expected,
                "{enc:?} @{units}"
            );
        }
    }

    #[test]
    fn negotiate_prefers_utf8_then_utf16() {
        let utf8 = PositionEncodingKind::UTF8;
        let utf16 = PositionEncodingKind::UTF16;
        let utf32 = PositionEncodingKind::UTF32;

        assert_eq!(PositionEncoding::negotiate(&[]), None);
        assert_eq!(
            PositionEncoding::negotiate(&[utf16.clone()]),
            Some(PositionEncoding::Utf16)
        );
        assert_eq!(
            PositionEncoding::negotiate(&[utf16.clone(), utf8.clone()]),
            Some(PositionEncoding::Utf8)
        );
        assert_eq!(
            PositionEncoding::negotiate(&[utf32.clone(), utf16.clone()]),
            Some(PositionEncoding::Utf16)
        );
        assert_eq!(
            PositionEncoding::negotiate(&[utf32]),
            Some(PositionEncoding::Utf32)
        );
    }
}
