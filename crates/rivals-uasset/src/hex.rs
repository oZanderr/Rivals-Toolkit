//! Renders package bytes as an offset-labelled hex dump.
//!
//! One formatter for the undecoded-bytes preview, the CLI and the desktop viewer, so an offset
//! quoted by a trace or a failure message means the same thing everywhere.

use serde::Serialize;

/// Rows are cut on 16-byte boundaries of the file offset, not of the slice, so a given offset
/// always lands in the same column whatever range is being shown.
pub const ROW_BYTES: usize = 16;

/// Bytes outside the slice at the start of the first row, which keeps token index and file offset
/// in step for callers that highlight a range.
const ABSENT: &str = "..";

#[derive(Debug, Clone, Serialize)]
pub struct HexRow {
    /// File offset of the row's first column, always a multiple of [`ROW_BYTES`].
    pub offset: u64,
    /// Exactly [`ROW_BYTES`] space-separated tokens, `..` where the row runs outside the slice.
    pub hex: String,
    /// The same bytes as text, `.` for anything not printable.
    pub ascii: String,
}

/// Hex rows for `bytes`, which begin at `base` in the package.
pub fn rows(bytes: &[u8], base: u64) -> Vec<HexRow> {
    let start = base - base % ROW_BYTES as u64;
    let end = base + bytes.len() as u64;
    let mut rows = Vec::new();
    let mut offset = start;
    while offset < end {
        let mut hex = Vec::with_capacity(ROW_BYTES);
        let mut ascii = String::with_capacity(ROW_BYTES);
        for column in 0..ROW_BYTES as u64 {
            match byte_at(bytes, base, offset + column) {
                Some(byte) => {
                    hex.push(format!("{byte:02X}"));
                    ascii.push(if byte.is_ascii_graphic() {
                        byte as char
                    } else {
                        '.'
                    });
                }
                None => {
                    hex.push(ABSENT.to_string());
                    ascii.push(' ');
                }
            }
        }
        rows.push(HexRow {
            offset,
            hex: hex.join(" "),
            ascii,
        });
        offset += ROW_BYTES as u64;
    }
    rows
}

/// The same dump as text, for the CLI and for the undecoded-bytes preview.
pub fn render(bytes: &[u8], base: u64, limit: Option<usize>) -> String {
    let all = rows(bytes, base);
    let shown = limit.map_or(all.len(), |rows| rows.min(all.len()));
    let mut out = String::new();
    for row in all.iter().take(shown) {
        out.push_str(&format!(
            "0x{:<8X} {}  {}\n",
            row.offset, row.hex, row.ascii
        ));
    }
    if all.len() > shown {
        // The first row may start before the slice, so count what the rows showed, not rows times
        // width.
        let first_row = base - base % ROW_BYTES as u64;
        let shown_bytes = (first_row + (shown * ROW_BYTES) as u64)
            .saturating_sub(base)
            .min(bytes.len() as u64);
        out.push_str(&format!(
            "... {} more bytes",
            bytes.len() as u64 - shown_bytes
        ));
    }
    out.trim_end().to_string()
}

fn byte_at(bytes: &[u8], base: u64, offset: u64) -> Option<u8> {
    bytes
        .get(usize::try_from(offset.checked_sub(base)?).ok()?)
        .copied()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A slice starting mid-row shows fewer bytes in its first row than the row is wide, which
    /// once made the remainder count underflow and panic.
    #[test]
    fn a_limited_render_of_a_slice_starting_mid_row_counts_its_remainder() {
        let bytes = [0u8; 20];
        let text = render(&bytes, 0x1FE, Some(1));
        assert!(text.ends_with("... 18 more bytes"), "{text}");
        let whole = render(&bytes, 0x1FE, Some(3));
        assert_eq!(whole.lines().count(), 3);
        assert!(!whole.contains("more bytes"), "{whole}");
    }

    /// An offset has to land in the same column no matter where the shown range starts, so a slice
    /// that begins mid-row is padded rather than shifting everything left.
    #[test]
    fn a_row_starts_on_a_sixteen_byte_boundary_of_the_file_offset() {
        let rows = rows(&[0xAA, 0xBB], 0x1005);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].offset, 0x1000);
        let tokens: Vec<&str> = rows[0].hex.split(' ').collect();
        assert_eq!(tokens.len(), ROW_BYTES);
        assert_eq!(tokens[0], "..");
        assert_eq!(tokens[5], "AA");
        assert_eq!(tokens[6], "BB");
        assert_eq!(tokens[7], "..");
    }

    #[test]
    fn bytes_spanning_a_boundary_split_across_rows() {
        let rows = rows(&[1; 20], 0x10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, 0x10);
        assert_eq!(rows[1].offset, 0x20);
        assert!(rows[1].hex.starts_with("01 01 01 01 .."));
    }

    #[test]
    fn printable_bytes_show_in_the_text_column() {
        let rows = rows(b"Hi\0", 0);
        assert_eq!(rows[0].ascii.trim_end(), "Hi.");
    }

    #[test]
    fn the_text_form_labels_every_row_with_its_offset() {
        let text = render(&[0xDE, 0xAD], 0x40, None);
        assert!(text.starts_with("0x40       DE AD"), "{text}");
    }
}
