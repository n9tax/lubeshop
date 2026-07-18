//! Detokenise a Commodore BASIC v2 program (PRG) into a readable LISTing.
//!
//! A `.prg` starts with a 2-byte little-endian load address, then a chain of
//! lines — each a 2-byte link to the next line, a 2-byte line number, the
//! tokenised body, and a `0` terminator; a `0` link ends the program. BASIC
//! keywords are single bytes `>= 0x80`; everything else is literal PETSCII, and
//! inside quotes nothing is a token. This is display-only (we never re-tokenise),
//! so it's best-effort: return `None` when the bytes don't look like BASIC so the
//! caller can fall back to showing them as plain text.

/// BASIC v2 keyword tokens, indexed from `0x80` (`0x80` = END … `0xCB` = GO).
const TOKENS: [&str; 76] = [
    "END", "FOR", "NEXT", "DATA", "INPUT#", "INPUT", "DIM", "READ", "LET", "GOTO",
    "RUN", "IF", "RESTORE", "GOSUB", "RETURN", "REM", "STOP", "ON", "WAIT", "LOAD",
    "SAVE", "VERIFY", "DEF", "POKE", "PRINT#", "PRINT", "CONT", "LIST", "CLR",
    "CMD", "SYS", "OPEN", "CLOSE", "GET", "NEW", "TAB(", "TO", "FN", "SPC(",
    "THEN", "NOT", "STEP", "+", "-", "*", "/", "^", "AND", "OR", ">", "=", "<",
    "SGN", "INT", "ABS", "USR", "FRE", "POS", "SQR", "RND", "LOG", "EXP", "COS",
    "SIN", "TAN", "ATN", "PEEK", "LEN", "STR$", "VAL", "ASC", "CHR$", "LEFT$",
    "RIGHT$", "MID$", "GO",
];

fn u16le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// True for the handful of load addresses BASIC programs start at (PET `$0401`,
/// C64 `$0801`, VIC-20 `$1001`/`$1201`, C16/Plus4 `$1001`, C128 `$1C01`). Used to
/// screen out non-program files before parsing.
fn is_basic_load_addr(addr: u16) -> bool {
    addr & 0x00FF == 0x01 && matches!(addr >> 8, 0x04 | 0x08 | 0x10 | 0x12 | 0x1C | 0x1D)
}

/// Render one PETSCII byte for the listing: readable ASCII passes through;
/// shifted letters fold to A–Z; control/colour/graphics codes become a middot.
fn petscii_to_char(b: u8) -> char {
    match b {
        0x20..=0x5E => b as char,
        0xC1..=0xDA => (b - 0x80) as char,
        _ => '·',
    }
}

/// Detokenise `bytes` into a BASIC listing, or `None` if they don't parse as a
/// Commodore BASIC program.
pub fn detokenize(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || !is_basic_load_addr(u16le(bytes, 0)) {
        return None;
    }

    let mut out = String::new();
    let mut pos = 2;
    let mut lines = 0u32;
    loop {
        if pos + 2 > bytes.len() {
            break; // ran off the end cleanly enough
        }
        let link = u16le(bytes, pos);
        if link == 0 {
            break; // proper end-of-program marker
        }
        if pos + 4 > bytes.len() {
            return None;
        }
        let line_no = u16le(bytes, pos + 2);
        if line_no > 63999 {
            return None; // not a valid BASIC line number → probably not BASIC
        }
        pos += 4;

        let mut line = String::new();
        let mut in_quote = false;
        let mut terminated = false;
        while pos < bytes.len() {
            let b = bytes[pos];
            pos += 1;
            if b == 0 {
                terminated = true;
                break;
            }
            if b == 0x22 {
                in_quote = !in_quote;
                line.push('"');
            } else if !in_quote && b >= 0x80 {
                match b {
                    0xFF => line.push('π'),
                    _ => match TOKENS.get((b - 0x80) as usize) {
                        Some(kw) => line.push_str(kw),
                        None => line.push('·'), // 0xCC..=0xFE: unused in v2
                    },
                }
            } else {
                line.push(petscii_to_char(b));
            }
        }
        if !terminated {
            return None; // truncated line → don't trust it as BASIC
        }
        out.push_str(&format!("{line_no} {line}\n"));
        lines += 1;
        if lines > 100_000 {
            break; // runaway guard
        }
    }

    (lines > 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detokenizes_a_simple_program() {
        // 10 PRINT "HI"  — load $0801, one line, then a 0 link.
        let prg = [
            0x01, 0x08, // load address $0801
            0x0C, 0x08, // link → $080C
            0x0A, 0x00, // line number 10
            0x99, 0x20, 0x22, 0x48, 0x49, 0x22, 0x00, // PRINT "HI"\0
            0x00, 0x00, // end of program
        ];
        assert_eq!(detokenize(&prg).as_deref(), Some("10 PRINT \"HI\"\n"));
    }

    #[test]
    fn tokens_inside_quotes_stay_literal() {
        // A token byte (0x99 = PRINT) inside quotes must render as a literal
        // character, not expand to the keyword.
        let prg = [
            0x01, 0x08, 0x0C, 0x08, 0x0A, 0x00, // line 10
            0x99, 0x20, 0x22, 0x99, 0x22, 0x00, // PRINT "<0x99>"\0
            0x00, 0x00,
        ];
        assert_eq!(detokenize(&prg).as_deref(), Some("10 PRINT \"·\"\n"));
    }

    #[test]
    fn rejects_non_basic_bytes() {
        assert_eq!(detokenize(b"Just a normal text file.\n"), None);
        assert_eq!(detokenize(&[0x00, 0x00]), None); // too short / bad load addr
    }
}
