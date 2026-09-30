//! R76: big-number display formatting for result cells.
//!
//! A pure display layer: the cell text the grid draws can gain grouping
//! separators (`1,234,567`) or collapse to a base-1000 suffix (`1.2M`), while
//! the underlying [`Val`] — what `Y` copies and the edit dialog pre-fills —
//! stays exactly as the driver returned it.
//!
//! Only a column the driver reports as numeric is touched (a `VARCHAR` holding
//! `1234567` is left alone), and only once its integer part reaches six digits,
//! so a small id never grows a comma it did not need.

use crate::prelude::*;

/// How a numeric result cell is drawn. `Original` is the driver's text
/// untouched; `Thousands` inserts grouping separators; `Abbrev` collapses a
/// large value to a base-1000 suffix (`1.2M`). Persisted in `tui.json`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum NumFmt {
    Original,
    /// The default: readability first, and the value stays recognisable.
    #[default]
    Thousands,
    Abbrev,
}

impl NumFmt {
    /// The `#` cycle: original → thousands → abbreviated → original.
    pub(crate) fn next(self) -> Self {
        match self {
            NumFmt::Original => NumFmt::Thousands,
            NumFmt::Thousands => NumFmt::Abbrev,
            NumFmt::Abbrev => NumFmt::Original,
        }
    }

    /// Stable on-disk key for `tui.json`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            NumFmt::Original => "original",
            NumFmt::Thousands => "thousands",
            NumFmt::Abbrev => "abbrev",
        }
    }

    /// Parse the on-disk key, tolerating a couple of aliases.
    pub(crate) fn from_key(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "original" | "raw" | "off" => Some(NumFmt::Original),
            "thousands" | "comma" => Some(NumFmt::Thousands),
            "abbrev" | "abbreviated" | "short" => Some(NumFmt::Abbrev),
            _ => None,
        }
    }

    /// Bilingual status label for the `#` flash.
    pub(crate) fn label(self) -> &'static str {
        match self {
            NumFmt::Original => t("原样"),
            NumFmt::Thousands => t("千分位"),
            NumFmt::Abbrev => t("缩写"),
        }
    }
}

/// True when a driver-reported type name names a numeric column. Reuses the
/// import layer's [`is_numeric_type`] so the two “is this a number?” decisions
/// can never drift apart (`DECIMAL(10,2)` / `INT UNSIGNED` qualify, `POINT` and
/// `INTERVAL` — which merely contain “INT” — do not).
fn numeric_column(t: &str) -> bool {
    is_numeric_type(t)
}

/// A plain decimal literal split into its sign, integer digits and the tail
/// (`.` plus fraction). `None` for anything that is not a plain decimal number
/// — `NULL`, `''`, text, or scientific notation, which is left untouched.
struct Decimal<'a> {
    neg: bool,
    int: &'a str,
    tail: &'a str,
}

fn split_decimal(raw: &str) -> Option<Decimal<'_>> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    // Scientific notation and any other exponent form are left as the driver
    // wrote them (grouping an `e+21` would be a lie about the value's shape).
    if body.is_empty() || body.contains(['e', 'E']) {
        return None;
    }
    let (int, tail) = match body.find('.') {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !tail.is_empty() {
        let frac = &tail[1..];
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    Some(Decimal { neg, int, tail })
}

/// The significant integer digits (leading zeros dropped), never empty.
fn significant_digits(int: &str) -> &str {
    let t = int.trim_start_matches('0');
    if t.is_empty() {
        "0"
    } else {
        t
    }
}

/// Insert `,` every three digits from the right.
fn group_digits(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Base-1000 suffixes, `K` = 10³ … `Y` = 10²⁴.
const UNITS: [&str; 9] = ["", "K", "M", "G", "T", "P", "E", "Z", "Y"];

/// Collapse a decimal literal to one decimal place plus a unit (`1234567` →
/// `1.2M`, `3400000000` → `3.4G`).
fn abbreviate(neg: bool, int: &str, tail: &str) -> String {
    let digits = significant_digits(int).len();
    let mut idx = ((digits.saturating_sub(1)) / 3).min(UNITS.len() - 1);
    let value: f64 = format!("{int}{tail}").parse().unwrap_or(0.0);
    let mut scaled = value / 1000f64.powi(idx as i32);
    // Rounding can push 999.9K up to 1000.0K — promote it to the next unit.
    if scaled.abs() >= 1000.0 && idx + 1 < UNITS.len() {
        idx += 1;
        scaled = value / 1000f64.powi(idx as i32);
    }
    let mut num = format!("{scaled:.1}");
    if let Some(stripped) = num.strip_suffix(".0") {
        num = stripped.to_string();
    }
    format!("{}{}{}", if neg { "-" } else { "" }, num, UNITS[idx])
}

/// The display form of one raw cell string under `mode`, or `None` when the
/// value is not a large number in a numeric column. `col_type` is the
/// driver-reported type; an empty/unknown type is left untouched so a schemaless
/// grid never misreads a numeric-looking string.
pub(crate) fn format_number(raw: &str, col_type: Option<&str>, mode: NumFmt) -> Option<String> {
    if mode == NumFmt::Original {
        return None;
    }
    let col_type = col_type.map(str::trim).filter(|t| !t.is_empty())?;
    if !numeric_column(col_type) {
        return None;
    }
    let d = split_decimal(raw)?;
    if significant_digits(d.int).len() < 6 {
        return None;
    }
    match mode {
        NumFmt::Original => None,
        NumFmt::Thousands => {
            let mut out = String::with_capacity(raw.len() + 4);
            if d.neg {
                out.push('-');
            }
            out.push_str(&group_digits(d.int));
            out.push_str(d.tail);
            Some(out)
        }
        NumFmt::Abbrev => Some(abbreviate(d.neg, d.int, d.tail)),
    }
}

/// The `(text, style)` a cell should be drawn with, applying [`format_number`]
/// on top of [`value_display`]. The raw value is untouched, so copy / edit keep
/// the original text.
pub(crate) fn display_value(v: &Val, col_type: Option<&str>, mode: NumFmt) -> (String, Style) {
    let (text, style) = value_display(v);
    match v {
        Val::Text(s) if !s.is_empty() => {
            let shown = format_number(s, col_type, mode).unwrap_or_else(|| s.clone());
            (shown, style)
        }
        _ => (text, style),
    }
}

/// A cell value's display width under `mode`, capped at the inline abbreviation
/// so a long value never stretches its column past what is actually drawn.
pub(crate) fn cell_text_width_fmt(v: &Val, col_type: Option<&str>, mode: NumFmt) -> usize {
    let (text, _) = display_value(v, col_type, mode);
    let w = disp_width(&text);
    if w > CELL_TEXT_MAX {
        CELL_TEXT_MAX - 1
    } else {
        w
    }
}
