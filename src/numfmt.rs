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
    /// The default: the driver's own text, with no grouping or shortening.
    #[default]
    Original,
    /// Readability first, and the value stays recognisable.
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

// ── R77: UNIX-timestamp preview (the `v` cell popup) ──────────────────────────

/// Lowest epoch-seconds value treated as a timestamp: 2001-09-09. Below it a
/// 9-digit number is far more likely to be an ordinary id than a date.
pub(crate) const EPOCH_MIN: i64 = 1_000_000_000;
/// Highest epoch-seconds value treated as a timestamp: roughly the year 3237.
/// The range is deliberately wide but bounded, so an arbitrary large integer is
/// never guessed into a date.
pub(crate) const EPOCH_MAX: i64 = 40_000_000_000;

/// Parse a cell as a plain (signed) decimal integer and keep it only when it
/// falls in [`EPOCH_MIN`]..=[`EPOCH_MAX`]. `None` for `NULL`, text, floats,
/// scientific notation or an out-of-range value — a preview never guesses.
pub(crate) fn parse_epoch_secs(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let digits = match bytes.first() {
        Some(b'+') | Some(b'-') => &bytes[1..],
        _ => bytes,
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v: i64 = s.parse().ok()?;
    (EPOCH_MIN..=EPOCH_MAX).contains(&v).then_some(v)
}

/// The current Unix time in seconds, rounded down. `0` before 1970 (never on a
/// real clock), so a preview never panics.
pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Epoch seconds for a cell, but only for a numeric column (a digit string in a
/// `VARCHAR` is left alone). The value itself is never changed — this drives a
/// read-only preview line in the cell popup.
pub(crate) fn epoch_secs_from_cell(v: &Val, col_type: Option<&str>) -> Option<i64> {
    let t = col_type?;
    if !is_numeric_type(t) {
        return None;
    }
    match v {
        Val::Text(s) => parse_epoch_secs(s),
        Val::Null => None,
    }
}

/// Local civil-time fields for a Unix timestamp. Read from the C library on
/// Unix so the preview follows the machine's own timezone and DST rules; other
/// platforms fall back to UTC (no timezone guess is baked in).
pub(crate) fn local_time_fields(secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    local_tz::local_fields(secs)
}

#[cfg(unix)]
mod local_tz {
    use std::os::raw::{c_char, c_int, c_long};

    /// The one C struct we read; the layout matches glibc / musl / macOS.
    #[repr(C)]
    struct Tm {
        tm_sec: c_int,
        tm_min: c_int,
        tm_hour: c_int,
        tm_mday: c_int,
        tm_mon: c_int,
        tm_year: c_int,
        tm_wday: c_int,
        tm_yday: c_int,
        tm_isdst: c_int,
        tm_gmtoff: c_long,
        tm_zone: *const c_char,
    }

    extern "C" {
        fn localtime_r(timep: *const c_long, result: *mut Tm) -> *mut Tm;
    }

    pub(super) fn local_fields(secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
        let t: c_long = secs as c_long;
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        };
        // SAFETY: `localtime_r` fills the `Tm` we own and never retains the
        // pointer; `t` outlives the call. A null return means the timestamp is
        // out of range, which we map to `None`.
        let p = unsafe { localtime_r(&t, &mut tm) };
        if p.is_null() {
            return None;
        }
        Some((
            tm.tm_year + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
        ))
    }
}

#[cfg(not(unix))]
mod local_tz {
    /// Fallback: the civil UTC fields, computed without a timezone crate.
    pub(super) fn local_fields(secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        Some((y as i32, m as u32, d as u32, h as u32, mi as u32, s as u32))
    }
}

/// The unit a relative-time label counts in.
#[derive(Clone, Copy)]
enum TimeUnit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl TimeUnit {
    /// The bilingual template for `n` of this unit, before (`past`) or after
    /// `now`.
    fn label(self, n: u64, past: bool) -> String {
        let tpl = match (self, past) {
            (TimeUnit::Minute, true) => "{} 分钟前",
            (TimeUnit::Minute, false) => "{} 分钟后",
            (TimeUnit::Hour, true) => "{} 小时前",
            (TimeUnit::Hour, false) => "{} 小时后",
            (TimeUnit::Day, true) => "{} 天前",
            (TimeUnit::Day, false) => "{} 天后",
            (TimeUnit::Month, true) => "{} 个月前",
            (TimeUnit::Month, false) => "{} 个月后",
            (TimeUnit::Year, true) => "{} 年前",
            (TimeUnit::Year, false) => "{} 年后",
        };
        tf(tpl, &[&n])
    }
}

/// How far `secs` is from `now`, as a bilingual label (`3 天前` / `3 days
/// ago`). Purely arithmetic — no timezone involved, so it is deterministic.
pub(crate) fn relative_time_label(secs: i64, now: i64) -> String {
    let past = secs <= now;
    let d = (now - secs).unsigned_abs();
    if d < 60 {
        return t("刚刚").to_string();
    }
    let (n, unit) = if d < 3_600 {
        (d / 60, TimeUnit::Minute)
    } else if d < 86_400 {
        (d / 3_600, TimeUnit::Hour)
    } else if d < 30 * 86_400 {
        (d / 86_400, TimeUnit::Day)
    } else if d < 365 * 86_400 {
        (d / (30 * 86_400), TimeUnit::Month)
    } else {
        (d / (365 * 86_400), TimeUnit::Year)
    };
    unit.label(n, past)
}

/// The gray preview line appended to a cell popup for an epoch value:
/// `🕒 2024-06-01 12:34:56 · 3 天前`. Read-only — never touches the value.
pub(crate) fn epoch_display(secs: i64, now: i64) -> String {
    let local = local_time_fields(secs)
        .map(|(y, mo, d, h, mi, s)| format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
        .unwrap_or_else(|| "—".to_string());
    tf("🕒 {} · {}", &[&local, &relative_time_label(secs, now)])
}
