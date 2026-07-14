//! Format-preserving text model for the in-app text editor.
//!
//! Retro disk files come from systems with different conventions — CR line endings
//! (Commodore, Apple), LF (Amiga/Unix), CRLF (DOS/CP/M) — and non-ASCII bytes that
//! must survive a round-trip untouched. This module separates two concerns so the
//! editor can be simple and correct:
//!
//! - **Line-ending + trailing-newline handling** (codec-*independent*): detect the
//!   dominant EOL on open, split into logical lines, and re-apply the same EOL +
//!   trailing-newline state on save. A consistent-EOL file that is opened and saved
//!   unedited is **byte-identical**.
//! - **Byte ↔ text mapping** via a pluggable [`TextCodec`]. v1 ships [`RawLatin1`]
//!   (every byte 0–255 ↔ one `char`, lossless). Real character sets (PETSCII,
//!   ATASCII, high-bit Apple), selected by the image's `FsKind`, drop in later by
//!   adding a codec — the line/editor logic doesn't change.

use crate::imagefs::FsKind;

/// Maps raw file bytes to a `String` for editing and back. Line endings are handled
/// by [`TextDoc`], *not* here — a codec only maps the bytes of a single line.
pub trait TextCodec {
    fn decode(&self, bytes: &[u8]) -> String;
    fn encode(&self, text: &str) -> Vec<u8>;
}

/// v1 codec: raw / Latin-1. Byte `b` ↔ `char` `b`, so every byte 0–255 round-trips
/// exactly and non-ASCII bytes are never corrupted. Characters typed above U+00FF
/// (unusual for these files) encode to `?`.
pub struct RawLatin1;

impl TextCodec for RawLatin1 {
    fn decode(&self, bytes: &[u8]) -> String {
        bytes.iter().map(|&b| b as char).collect()
    }
    fn encode(&self, text: &str) -> Vec<u8> {
        text.chars()
            .map(|c| u8::try_from(c as u32).unwrap_or(b'?'))
            .collect()
    }
}

static RAW_LATIN1: RawLatin1 = RawLatin1;

/// The codec to use for a file in an image of this filesystem. v1 always returns
/// raw/Latin-1; this is the single place a PETSCII/ATASCII/Apple codec would be
/// selected once added.
pub fn codec_for_fs(_fs: FsKind) -> &'static dyn TextCodec {
    &RAW_LATIN1
}

/// A line-ending style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    Cr,
    Crlf,
}

impl Eol {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::Cr => b"\r",
            Eol::Crlf => b"\r\n",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Cr => "CR",
            Eol::Crlf => "CRLF",
        }
    }
}

/// An editable text file, split into logical lines with the original line-ending
/// style and trailing-newline state remembered so a save re-creates them.
pub struct TextDoc {
    /// Logical lines, decoded, without their line endings.
    pub lines: Vec<String>,
    /// The line ending to write between lines (the dominant one on open).
    eol: Eol,
    /// Whether the original file ended with a line ending.
    trailing_newline: bool,
}

impl TextDoc {
    /// Rebuild a document from edited lines while keeping the original EOL +
    /// trailing-newline state (which the editor carries alongside its line buffer).
    pub fn from_parts(lines: Vec<String>, eol: Eol, trailing_newline: bool) -> Self {
        TextDoc {
            lines,
            eol,
            trailing_newline,
        }
    }

    /// Whether the file ended with a line ending (re-applied on save).
    pub fn trailing_newline(&self) -> bool {
        self.trailing_newline
    }

    /// Decode `bytes` into an editable document, detecting the line-ending style.
    pub fn open(bytes: &[u8], codec: &dyn TextCodec) -> Self {
        let eol = detect_eol(bytes);
        let sep = eol.as_bytes();
        let trailing_newline = !bytes.is_empty() && bytes.ends_with(sep);
        let mut segments = split_bytes(bytes, sep);
        if trailing_newline {
            // Drop the empty segment the final separator produced, so it's
            // represented by `trailing_newline` rather than a phantom last line.
            segments.pop();
        }
        let lines = segments.iter().map(|s| codec.decode(s)).collect();
        TextDoc {
            lines,
            eol,
            trailing_newline,
        }
    }

    /// Re-encode the document to bytes, re-applying the original EOL + trailing
    /// newline. For a consistent-EOL file, an unedited round-trip is byte-exact.
    pub fn to_bytes(&self, codec: &dyn TextCodec) -> Vec<u8> {
        let sep = self.eol.as_bytes();
        let mut out = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(sep);
            }
            out.extend_from_slice(&codec.encode(line));
        }
        if self.trailing_newline {
            out.extend_from_slice(sep);
        }
        out
    }

    pub fn eol(&self) -> Eol {
        self.eol
    }
}

/// The dominant line-ending style. Standalone `\r`, `\n`, and `\r\n` are counted
/// disjointly. A file with no line ending defaults to LF (only matters if the user
/// later adds a newline).
fn detect_eol(bytes: &[u8]) -> Eol {
    let (mut crlf, mut cr, mut lf) = (0u32, 0u32, 0u32);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 2;
            }
            b'\r' => {
                cr += 1;
                i += 1;
            }
            b'\n' => {
                lf += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if crlf == 0 && cr == 0 && lf == 0 {
        Eol::Lf
    } else if crlf >= cr && crlf >= lf {
        Eol::Crlf
    } else if cr >= lf {
        Eol::Cr
    } else {
        Eol::Lf
    }
}

/// Split `bytes` on every non-overlapping occurrence of `sep` (like `str::split`,
/// so N separators yield N+1 pieces). `sep` is never empty here.
fn split_bytes<'a>(bytes: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= bytes.len() {
        if &bytes[i..i + sep.len()] == sep {
            out.push(&bytes[start..i]);
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&bytes[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The correctness core: open then save (unedited) must be byte-identical for a
    /// consistent-EOL file, across styles, trailing-newline states, and byte values.
    fn assert_round_trip(bytes: &[u8]) {
        let doc = TextDoc::open(bytes, &RawLatin1);
        assert_eq!(doc.to_bytes(&RawLatin1), bytes, "round-trip changed the bytes");
    }

    #[test]
    fn round_trips_every_eol_and_trailing_state() {
        assert_round_trip(b"line one\nline two\n"); // LF, trailing
        assert_round_trip(b"line one\nline two"); // LF, no trailing
        assert_round_trip(b"a\r\nb\r\nc\r\n"); // CRLF, trailing
        assert_round_trip(b"a\r\nb"); // CRLF, no trailing
        assert_round_trip(b"c64\rline\r"); // CR (Commodore), trailing
        assert_round_trip(b"c64\rline"); // CR, no trailing
    }

    #[test]
    fn round_trips_edge_cases() {
        assert_round_trip(b""); // empty
        assert_round_trip(b"\n"); // just a newline
        assert_round_trip(b"single line no newline");
        assert_round_trip(b"trailing\ttabs\tkept\n"); // tabs preserved
        assert_round_trip(b"blank\n\nlines\n"); // empty lines
    }

    #[test]
    fn high_bit_bytes_are_lossless() {
        // Every byte 0x00..=0xFF except the EOL bytes, plus an LF terminator.
        let mut bytes: Vec<u8> = (0u8..=0xFF).filter(|&b| b != b'\r' && b != b'\n').collect();
        bytes.push(b'\n');
        assert_round_trip(&bytes);
    }

    #[test]
    fn eol_is_detected_by_dominance() {
        assert_eq!(TextDoc::open(b"a\nb\nc\r\n", &RawLatin1).eol(), Eol::Lf);
        assert_eq!(TextDoc::open(b"a\r\nb\r\nc\n", &RawLatin1).eol(), Eol::Crlf);
        assert_eq!(TextDoc::open(b"a\rb\rc\n", &RawLatin1).eol(), Eol::Cr);
        assert_eq!(TextDoc::open(b"no breaks", &RawLatin1).eol(), Eol::Lf);
    }

    #[test]
    fn edits_reapply_the_original_format() {
        // Open a CRLF file, change a line, save → the edit uses CRLF + keeps trailing.
        let mut doc = TextDoc::open(b"hello\r\nworld\r\n", &RawLatin1);
        doc.lines[1] = "commodore".to_string();
        assert_eq!(doc.to_bytes(&RawLatin1), b"hello\r\ncommodore\r\n");
    }

    #[test]
    fn lines_are_split_without_their_endings() {
        let doc = TextDoc::open(b"one\r\ntwo\r\n", &RawLatin1);
        assert_eq!(doc.lines, vec!["one".to_string(), "two".to_string()]);
    }
}
