//! R74: pretty-print a JSON object/array cell in the cell popup (`v`).
//!
//! PostgreSQL `json` / `jsonb`, a MongoDB document's nested object/array and a
//! Redis hash value that happens to hold JSON all arrive at the grid as plain
//! text. When that text is a JSON **object or array** the cell popup shows a
//! re-indented form with a light key / string / number colouring; a scalar
//! (`42`, `"x"`, `true`, `null`) or non-JSON text is left verbatim. `J` toggles
//! back to the raw value and `y`/`Y` always copies the raw value, so the pretty
//! form never leaks onto the clipboard.

use crate::prelude::*;

/// Colour for a JSON object key (`"name":`).
fn key_style() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Colour for a JSON string value.
fn string_style() -> Style {
    Style::default().fg(Color::Green)
}

/// Colour for a JSON number.
fn number_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// Colour for `true` / `false`.
fn literal_style() -> Style {
    Style::default().fg(Color::Magenta)
}

/// Colour for `null`.
fn null_literal_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Colour for structural punctuation (`{ } [ ] , :`).
fn punct_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

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

/// The pretty JSON split into per-line styled token runs (one entry per logical
/// line, so the renderer's own wrapping still applies). Joining the tokens of a
/// line yields that line's plain text.
pub(crate) fn pretty_json_spans(pretty: &str) -> Vec<Vec<PopupSpan>> {
    pretty.split('\n').map(span_line).collect()
}

/// Tokenise one pretty-JSON line into styled spans. This is a deliberately
/// shallow scanner — enough to tell a key from a string value from a number —
/// not a full highlighter.
fn span_line(line: &str) -> Vec<PopupSpan> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<PopupSpan> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let end = i.min(chars.len());
            let text: String = chars[start..end].iter().collect();
            // A string followed by `:` is a key; anything else is a value.
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let style = if j < chars.len() && chars[j] == ':' {
                key_style()
            } else {
                string_style()
            };
            push_span(&mut out, text, style);
        } else if c == '-' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            push_span(&mut out, chars[start..i].iter().collect(), number_style());
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = match word.as_str() {
                "true" | "false" => literal_style(),
                "null" => null_literal_style(),
                _ => Style::default(),
            };
            push_span(&mut out, word, style);
        } else {
            push_span(&mut out, c.to_string(), punct_style());
            i += 1;
        }
    }
    out
}

/// Append `text` with `style`, merging into the previous span when the style is
/// unchanged so a line does not accumulate one span per character.
fn push_span(out: &mut Vec<PopupSpan>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(PopupSpan { text, style });
}
