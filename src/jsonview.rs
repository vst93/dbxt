//! R74/R89: pretty-print a JSON object/array cell in the cell popup (`v`),
//! and preview/decode Unicode escapes.
//!
//! PostgreSQL `json` / `jsonb`, a MongoDB document's nested object/array and a
//! Redis hash value that happens to hold JSON all arrive at the grid as plain
//! text. When that text is a JSON **object or array** the cell popup shows a
//! re-indented form — indentation and line breaks only, no token colouring.
//! A scalar (`42`, `"x"`, `true`, `null`) or non-JSON text is left verbatim.
//! `J` toggles back to the raw value and `y`/`Y` always copies the raw value,
//! so the pretty form never leaks onto the clipboard.
//!
//! R89 also covers Unicode escape conversion: a value that carries `\uXXXX`
//! escapes (including surrogate pairs) gets a grey decoded line at the bottom
//! of the popup, and `U` cycles the popup body through
//! `raw → escaped-decoded → whole-value re-escape` (read-only display state).

use crate::prelude::*;

/// Parse `text` and return its pretty-printed form when it is a JSON object or
/// array. Scalars and non-JSON text return `None`, so the popup keeps showing
/// those verbatim. Cheap shapes (leading `{` / `[`) are rejected before the
/// parser runs, so a large non-JSON cell costs one byte comparison.
pub(crate) fn pretty_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    match trimmed.as_bytes().first() {
        Some(b'{') | Some(b'[') => {}
        _ => return None,
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if !matches!(
        value,
        serde_json::Value::Object(_) | serde_json::Value::Array(_)
    ) {
        return None;
    }
    serde_json::to_string_pretty(&value).ok()
}

/// The pretty JSON split into per-line spans (one entry per logical line, so the
/// renderer's own wrapping still applies). R89: every line is a single,
/// uncoloured span — the pretty view is indentation + line breaks only, no
/// token colouring. Joining the spans of a line yields that line's plain text.
pub(crate) fn pretty_json_spans(pretty: &str) -> Vec<Vec<PopupSpan>> {
    pretty
        .split('\n')
        .map(|line| {
            vec![PopupSpan {
                text: line.to_string(),
                style: Style::default(),
            }]
        })
        .collect()
}

// ── R89: Unicode escape conversion (the `U` view + the grey decode line) ─────

/// Which Unicode view the cell popup is showing. `Raw` is the stored value
/// (`y`/`Y` always copy this); `Decoded` interprets the JSON escape sequences;
/// `Escaped` re-escapes non-ASCII characters as `\uXXXX`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UMode {
    Raw,
    Decoded,
    Escaped,
}

/// Does `s` contain a `\uXXXX` escape sequence (a backslash, `u` and four hex
/// digits)? Drives the grey decode preview line. Surrogate pairs match too,
/// because their first half is a valid `\uXXXX`.
pub(crate) fn has_unicode_escape(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i + 6 <= cs.len() {
        if cs[i] == '\\' && cs[i + 1] == 'u' && (2..6).all(|k| cs[i + k].is_ascii_hexdigit()) {
            return true;
        }
        i += 1;
    }
    false
}

/// Decode the JSON-standard escape sequences in `s`: `\uXXXX` (including a
/// surrogate pair), `\n` `\t` `\r` `\b` `\f` `\"` `\\` `\/`. An unknown escape
/// is kept verbatim; malformed `\u` runs and lone/low surrogates are an error.
pub(crate) fn decode_escapes(s: &str) -> Result<String, ()> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&next) = cs.get(i + 1) else {
            out.push('\\');
            i += 1;
            continue;
        };
        match next {
            'n' => {
                out.push('\n');
                i += 2;
            }
            't' => {
                out.push('\t');
                i += 2;
            }
            'r' => {
                out.push('\r');
                i += 2;
            }
            'b' => {
                out.push('\u{08}');
                i += 2;
            }
            'f' => {
                out.push('\u{0C}');
                i += 2;
            }
            '"' => {
                out.push('"');
                i += 2;
            }
            '\\' => {
                out.push('\\');
                i += 2;
            }
            '/' => {
                out.push('/');
                i += 2;
            }
            'u' => {
                // A `\u` run that is not four hex digits is not a Unicode escape
                // (e.g. a Windows path `C:\user`); keep it literal.
                let Some(hi) = parse_hex4(&cs, i + 2) else {
                    out.push('\\');
                    i += 1;
                    continue;
                };
                i += 6;
                let code = if (0xD800..=0xDBFF).contains(&hi) {
                    // High surrogate: a low surrogate must follow.
                    if cs.get(i) != Some(&'\\') || cs.get(i + 1) != Some(&'u') {
                        return Err(());
                    }
                    let Some(lo) = parse_hex4(&cs, i + 2) else {
                        return Err(());
                    };
                    if !(0xDC00..=0xDFFF).contains(&lo) {
                        return Err(());
                    }
                    i += 6;
                    0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                } else if (0xDC00..=0xDFFF).contains(&hi) {
                    // A lone low surrogate is invalid.
                    return Err(());
                } else {
                    hi
                };
                out.push(char::from_u32(code).ok_or(())?);
            }
            _ => {
                out.push('\\');
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Four hex digits starting at `at`, or `None` when any is missing / not hex.
fn parse_hex4(cs: &[char], at: usize) -> Option<u32> {
    let mut v = 0u32;
    for k in 0..4 {
        v = v * 16 + cs.get(at + k)?.to_digit(16)?;
    }
    Some(v)
}

/// Re-escape every non-ASCII character as `\uXXXX` (a surrogate pair above the
/// BMP). ASCII, including the surrounding JSON punctuation, is left as-is.
pub(crate) fn escape_unicode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp <= 0x7F {
            out.push(c);
        } else if cp <= 0xFFFF {
            out.push_str(&format!("\\u{cp:04X}"));
        } else {
            let v = cp - 0x1_0000;
            let hi = 0xD800 + (v >> 10);
            let lo = 0xDC00 + (v & 0x3FF);
            out.push_str(&format!("\\u{hi:04X}\\u{lo:04X}"));
        }
    }
    out
}

/// The three display views derived from a cell's raw text, computed once when
/// the cell popup opens:
///
/// * `decoded` — `Some` when the raw text decodes cleanly (identity for a value
///   with no escapes), `None` when a `\u` escape is malformed.
/// * `escaped` — `Some` when the decoded text has a non-ASCII character to
///   escape; `None` for a pure-ASCII value (so `U` has no third state).
/// * `preview` — the grey bottom line, only when the raw text actually contains
///   a `\uXXXX` escape.
pub(crate) fn unicode_views(raw: &str) -> (Option<String>, Option<String>, Option<PopupLine>) {
    let Ok(decoded) = decode_escapes(raw) else {
        return (None, None, None);
    };
    let escaped = decoded
        .chars()
        .any(|c| c as u32 > 0x7F)
        .then(|| escape_unicode(&decoded));
    let preview = has_unicode_escape(raw).then(|| PopupLine {
        text: tf("解码: {}", &[&decoded]),
        style: Style::default().fg(Color::DarkGray),
    });
    (Some(decoded), escaped, preview)
}

/// The text the cell popup body should show under the current `U` view. `y`/`Y`
/// never read this — they always copy [`CellPopup::raw`].
pub(crate) fn u_display_text(popup: &CellPopup) -> String {
    match popup.u_mode {
        UMode::Raw => popup.raw.clone(),
        UMode::Decoded => popup.decoded.clone().unwrap_or_else(|| popup.raw.clone()),
        UMode::Escaped => popup.escaped.clone().unwrap_or_else(|| popup.raw.clone()),
    }
}

/// Advance the `U` view: `raw → decoded → escaped → raw`. A pure-ASCII value has
/// no `escaped` view, so it cycles `raw ↔ decoded`; a value whose `\u` escape is
/// malformed has no decoded view and returns `Err` (the status bar reports it).
pub(crate) fn advance_u_mode(popup: &mut CellPopup) -> Result<UMode, ()> {
    let next = match popup.u_mode {
        UMode::Raw => {
            if popup.decoded.is_none() {
                return Err(());
            }
            UMode::Decoded
        }
        UMode::Decoded => {
            if popup.escaped.is_some() {
                UMode::Escaped
            } else {
                UMode::Raw
            }
        }
        UMode::Escaped => UMode::Raw,
    };
    popup.u_mode = next;
    Ok(next)
}
