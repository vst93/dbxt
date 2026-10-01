use crate::prelude::*;
use crate::*;

// ─── DBX-parity feature helpers ──────────────────────────────────────────────

/// Parse a `#rrggbb` / `rrggbb` connection colour into a terminal colour.
pub(crate) fn parse_hex_color(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Default colour for a database family, used when a connection has no colour.
pub(crate) fn db_type_color(db_type: &str) -> Color {
    match db_type.to_ascii_lowercase().as_str() {
        "mysql" | "mariadb" | "tidb" | "doris" | "selectdb" | "starrocks" => Color::LightBlue,
        "postgres" | "postgresql" | "opengauss" | "gaussdb" | "kingbase" | "highgo"
        | "cockroachdb" | "redshift" | "dm" | "kwdb" => Color::LightCyan,
        "sqlite" | "libsql" | "turso" => Color::LightYellow,
        "redis" | "keydb" | "valkey" => Color::LightRed,
        "mongodb" | "mongo" => Color::LightGreen,
        "elasticsearch" | "meilisearch" | "opensearch" => Color::LightMagenta,
        "clickhouse" | "duckdb" | "sqlserver" | "oracle" => Color::Yellow,
        _ => Color::Gray,
    }
}

/// The colour to badge a connection with: its own colour when set, else a family
/// default so mysql / redis / mongo are visually distinct at a glance.
pub(crate) fn connection_color(cfg: &ConnectionConfig) -> Color {
    cfg.color
        .as_deref()
        .and_then(parse_hex_color)
        .unwrap_or_else(|| db_type_color(cfg.db_type.as_str()))
}

/// Build the EXPLAIN statement for the current dialect. `None` when the engine
/// has no single-statement EXPLAIN (SQL Server / Oracle need a session toggle).
pub(crate) fn explain_sql_for(db_type: &str, sql: &str) -> Option<String> {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return None;
    }
    let prefix = match db_type.to_ascii_lowercase().as_str() {
        "sqlite" | "libsql" | "turso" => "EXPLAIN QUERY PLAN ",
        "mysql" | "mariadb" | "tidb" | "doris" | "selectdb" | "starrocks" | "oceanbase"
        | "tdengine" | "clickhouse" | "duckdb" | "postgres" | "postgresql" | "opengauss"
        | "gaussdb" | "kingbase" | "highgo" | "cockroachdb" | "redshift" | "dm" | "kwdb" => {
            "EXPLAIN "
        }
        _ => return None,
    };
    Some(format!("{prefix}{sql}"))
}

/// RFC 4180 CSV field: quote when the value contains a comma, quote, CR or LF.
pub(crate) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Serialise a grid to CSV. NULL becomes an empty field (the same convention
/// DBX's own CSV export uses).
pub(crate) fn grid_to_csv(grid: &Grid) -> String {
    let mut out = String::new();
    out.push_str(
        &grid
            .columns
            .iter()
            .map(|c| csv_field(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in &grid.rows {
        let fields: Vec<String> = (0..grid.columns.len())
            .map(|ci| csv_field(row.get(ci).map(Val::text).unwrap_or("")))
            .collect();
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

// ── CSV import: parsing, decoding, inference and alignment ───────────────────

/// Parse CSV text into rows of fields (RFC 4180, stdlib only). Handles quoted
/// fields, doubled quotes inside a quoted field, embedded delimiters and
/// newlines, LF / CRLF line endings, a leading UTF-8 BOM and blank lines.
pub(crate) fn parse_csv(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut chars = text.chars().peekable();
    if chars.peek() == Some(&'\u{feff}') {
        chars.next();
    }
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        if c == '"' && field.is_empty() {
            in_quotes = true;
            field_started = true;
        } else if c == delim {
            row.push(std::mem::take(&mut field));
            field_started = false;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if row.is_empty() && field.is_empty() && !field_started {
                // A blank line carries no record.
            } else {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            field_started = false;
        } else {
            field.push(c);
            field_started = true;
        }
    }
    if !row.is_empty() || !field.is_empty() || field_started {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Pick the delimiter from the first non-empty line: the candidate with the most
/// unquoted occurrences wins, defaulting to a comma when none appears.
pub(crate) fn detect_delimiter(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut best = (',', 0usize);
    for cand in [',', '\t', ';'] {
        let mut n = 0usize;
        let mut in_q = false;
        for ch in first.chars() {
            if ch == '"' {
                in_q = !in_q;
            } else if ch == cand && !in_q {
                n += 1;
            }
        }
        if n > best.1 {
            best = (cand, n);
        }
    }
    best.0
}

/// Decode CSV bytes to text, returning the text and an encoding label. UTF-8 is
/// used verbatim; anything else is decoded as GB18030 (a GBK superset, the
/// common Chinese encoding), falling back to a lossy UTF-8 replacement.
pub(crate) fn decode_csv_bytes(bytes: &[u8]) -> (String, String) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "UTF-8".to_string());
    }
    let (cow, _, had_errors) = encoding_rs::GB18030.decode(bytes);
    if !had_errors {
        return (cow.into_owned(), "GB18030/GBK".to_string());
    }
    (
        String::from_utf8_lossy(bytes).into_owned(),
        "UTF-8 (lossy)".to_string(),
    )
}

/// `YYYY-MM-DD` with a plausible month/day (a cheap sanity check, not a
/// calendar).
pub(crate) fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| s[r].bytes().all(|c| c.is_ascii_digit());
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let m: u32 = s[5..7].parse().unwrap_or(0);
    let d: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// `YYYY-MM-DD HH:MM[:SS]` (or a `T` separator).
pub(crate) fn looks_like_datetime(s: &str) -> bool {
    let Some((date, rest)) = s.split_once([' ', 'T']) else {
        return false;
    };
    if !looks_like_date(date) {
        return false;
    }
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return false;
    }
    if !parts
        .iter()
        .all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    let h: u32 = parts[0].parse().unwrap_or(99);
    let m: u32 = parts[1].parse().unwrap_or(99);
    h < 24 && m < 60
}

/// Infer a column's type from its non-empty sample values. Empty values are
/// ignored (they become NULL); a column with no values at all falls back to
/// text.
pub(crate) fn infer_col_type(values: &[&str]) -> ColType {
    let mut seen = 0usize;
    let mut all_int = true;
    let mut all_float = true;
    let mut all_bool = true;
    let mut all_date = true;
    let mut all_datetime = true;
    for v in values.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        seen += 1;
        if v.parse::<i64>().is_err() {
            all_int = false;
        }
        if v.parse::<f64>().is_err() {
            all_float = false;
        }
        if !(v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("false")) {
            all_bool = false;
        }
        if !looks_like_date(v) {
            all_date = false;
        }
        if !looks_like_datetime(v) {
            all_datetime = false;
        }
    }
    if seen == 0 {
        return ColType::Text;
    }
    if all_bool {
        ColType::Bool
    } else if all_int {
        ColType::Int
    } else if all_float {
        ColType::Float
    } else if all_datetime {
        ColType::DateTime
    } else if all_date {
        ColType::Date
    } else {
        ColType::Text
    }
}

/// Match CSV headers to table columns by trimmed, case-insensitive name (a
/// surrounding backtick or double quote is ignored). Returns the columns in
/// table order, the CSV headers that matched nothing, and the table columns
/// absent from the CSV.
pub(crate) fn align_import_columns(
    headers: &[String],
    infer_rows: &[Vec<String>],
    table_columns: &[ColumnInfo],
) -> (Vec<ImportCol>, Vec<String>, Vec<String>) {
    let norm = |s: &str| s.trim().trim_matches(['`', '"']).to_ascii_lowercase();
    let norm_headers: Vec<String> = headers.iter().map(|h| norm(h)).collect();
    let mut used = vec![false; headers.len()];
    let mut columns = Vec::with_capacity(table_columns.len());
    let mut missing = Vec::new();
    for col in table_columns {
        let want = norm(&col.name);
        let idx = norm_headers
            .iter()
            .position(|h| !h.is_empty() && *h == want);
        match idx {
            Some(i) => {
                used[i] = true;
                let vals: Vec<&str> = infer_rows
                    .iter()
                    .filter_map(|r| r.get(i))
                    .map(String::as_str)
                    .collect();
                columns.push(ImportCol {
                    name: col.name.clone(),
                    src: Some(i),
                    ty: infer_col_type(&vals),
                    data_type: col.data_type.clone(),
                });
            }
            None => {
                missing.push(col.name.clone());
                columns.push(ImportCol {
                    name: col.name.clone(),
                    src: None,
                    ty: ColType::Text,
                    data_type: col.data_type.clone(),
                });
            }
        }
    }
    let extra = headers
        .iter()
        .enumerate()
        .filter(|(i, h)| !used[*i] && !h.trim().is_empty())
        .map(|(_, h)| h.clone())
        .collect();
    (columns, extra, missing)
}

/// SQL literal for one CSV field. An empty field is SQL NULL (the import
/// convention); the inferred type decides whether a value stays bare, becomes
/// TRUE/FALSE or is quoted. A numeric target column always keeps a genuine
/// number bare, even when the sample looked like text.
pub(crate) fn import_literal(raw: &str, ty: ColType, data_type: &str) -> String {
    if raw.is_empty() {
        return "NULL".to_string();
    }
    if is_numeric_type(data_type) && raw.parse::<f64>().is_ok() {
        return raw.to_string();
    }
    match ty {
        ColType::Int if raw.parse::<i64>().is_ok() => raw.to_string(),
        ColType::Float if raw.parse::<f64>().is_ok() => raw.to_string(),
        ColType::Bool if raw.eq_ignore_ascii_case("true") => "TRUE".to_string(),
        ColType::Bool if raw.eq_ignore_ascii_case("false") => "FALSE".to_string(),
        _ => sql_literal(raw),
    }
}

/// `INSERT INTO t (cols…) VALUES (vals…);` for one import row.
pub(crate) fn import_insert_sql(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    columns: &[ImportCol],
    row: &[String],
) -> String {
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let present: Vec<&ImportCol> = columns.iter().filter(|c| c.src.is_some()).collect();
    let cols = present
        .iter()
        .map(|c| q(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = present
        .iter()
        .map(|c| {
            let raw = c
                .src
                .and_then(|i| row.get(i))
                .map(String::as_str)
                .unwrap_or("");
            import_literal(raw, c.ty, &c.data_type)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({});",
        table_ref(cfg.db_type, schema, table),
        cols,
        vals
    )
}

/// Split rows into transaction-sized chunks (the last one may be short).
pub(crate) fn import_chunks<T>(rows: &[T]) -> Vec<&[T]> {
    rows.chunks(IMPORT_CHUNK).collect()
}

/// Expand a leading `~` to `$HOME`.
pub(crate) fn expand_home(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if trimmed == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

/// Human-readable byte size for the preview header.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A delimiter's display label.
pub(crate) fn delim_label(d: char) -> &'static str {
    match d {
        '\t' => "TAB",
        ';' => ";",
        _ => ",",
    }
}

/// Row number (1-based) a stop-mode batch error refers to, when the backend
/// names the failing statement (`Statement N failed: …`).
pub(crate) fn import_row_of_error(base: usize, err: &str, chunk_len: usize) -> usize {
    if let Some(pos) = err.find("Statement ") {
        let rest = &err[pos + "Statement ".len()..];
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<usize>() {
            if (1..=chunk_len).contains(&n) {
                return base + n;
            }
        }
    }
    base + 1
}

// ── export generators ────────────────────────────────────────────────────────

/// A canonical integer: optional `-`, no leading zeros (except `0` itself).
pub(crate) fn is_canonical_int(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    if body.is_empty() || !body.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    body == "0" || !body.starts_with('0')
}

/// A canonical decimal: an integer part without leading zeros and an optional
/// fraction. `1e5` and `.5` stay strings (too easy to confuse with text).
pub(crate) fn is_canonical_float(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    if int.is_empty() || !int.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !(int == "0" || !int.starts_with('0')) {
        return false;
    }
    match frac {
        None => true,
        Some(f) => !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()),
    }
}

/// JSON scalar for a cell: NULL → null, a canonical boolean/number is emitted
/// natively, anything else stays a string (so `0123` never becomes 123).
pub(crate) fn json_scalar(s: &str) -> serde_json::Value {
    use serde_json::Value;
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }
    if is_canonical_int(s) {
        if let Ok(n) = s.parse::<i64>() {
            return Value::Number(n.into());
        }
    }
    if is_canonical_float(s) {
        if let Ok(f) = s.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
    }
    Value::String(s.to_string())
}

/// One JSON object per grid row (serde handles all escaping).
pub(crate) fn grid_row_object(
    grid: &Grid,
    row: &[Val],
) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = serde_json::Map::new();
    for (ci, name) in grid.columns.iter().enumerate() {
        let v = row.get(ci).cloned().unwrap_or(Val::Null);
        let jv = match v {
            Val::Null => serde_json::Value::Null,
            Val::Text(s) => json_scalar(&s),
        };
        obj.insert(name.clone(), jv);
    }
    obj
}

/// Pretty-printed JSON array of row objects.
pub(crate) fn grid_to_json_array(grid: &Grid) -> String {
    let arr: Vec<serde_json::Value> = grid
        .rows
        .iter()
        .map(|row| serde_json::Value::Object(grid_row_object(grid, row)))
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr))
        .unwrap_or_else(|_| "[]".to_string())
}

/// NDJSON: one compact JSON object per line.
pub(crate) fn grid_to_json_ndjson(grid: &Grid) -> String {
    let mut out = String::new();
    for row in &grid.rows {
        let obj = serde_json::Value::Object(grid_row_object(grid, row));
        if let Ok(s) = serde_json::to_string(&obj) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

/// Escape a Markdown table cell: pipes and backslashes are escaped, newlines
/// become `<br>`.
pub(crate) fn markdown_cell(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Markdown table. NULL renders as `NULL` (the grid's convention) while an empty
/// string stays empty.
pub(crate) fn grid_to_markdown(grid: &Grid) -> String {
    let mut out = String::new();
    out.push('|');
    for c in &grid.columns {
        out.push(' ');
        out.push_str(&markdown_cell(c));
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    for _ in &grid.columns {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in &grid.rows {
        out.push('|');
        for ci in 0..grid.columns.len() {
            out.push(' ');
            match row.get(ci) {
                None | Some(Val::Null) => out.push_str("NULL"),
                Some(Val::Text(s)) => out.push_str(&markdown_cell(s)),
            }
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}

// ── R111: plain-text aligned table ───────────────────────────────────────────
//
// The neutral, dependency-free format a CLI veteran pastes into a terminal,
// wiki or mail body: `+---+` rules with `|` column borders. Column width is the
// widest display width (Unicode aware — a CJK glyph counts 2), a NULL is an
// empty cell and values are passed through verbatim (CSV semantics, R105). A
// column whose every non-NULL value parses as a finite number is right-aligned
// (header included, so the column reads as one block); everything else is
// left-aligned. Cells are never wrapped or truncated — a faithful export wins
// over a pretty one — and a rule is drawn under every data row.

/// True when `s` parses as a finite number, used to classify a column.
pub(crate) fn text_table_numeric(s: &str) -> bool {
    s.trim().parse::<f64>().map(f64::is_finite).unwrap_or(false)
}

/// Widest display width of the header and every cell, per column.
pub(crate) fn text_table_widths(grid: &Grid) -> Vec<usize> {
    let mut widths: Vec<usize> = grid.columns.iter().map(|c| disp_width(c)).collect();
    for row in &grid.rows {
        for (ci, w) in widths.iter_mut().enumerate() {
            let cw = disp_width(row.get(ci).map(Val::text).unwrap_or(""));
            if cw > *w {
                *w = cw;
            }
        }
    }
    widths
}

/// Per-column right-alignment: true when the column has at least one non-NULL
/// value and every one of them is numeric.
pub(crate) fn text_table_right(grid: &Grid) -> Vec<bool> {
    (0..grid.columns.len())
        .map(|ci| {
            let mut any = false;
            for row in &grid.rows {
                if let Some(Val::Text(s)) = row.get(ci) {
                    if !text_table_numeric(s) {
                        return false;
                    }
                    any = true;
                }
            }
            any
        })
        .collect()
}

/// Append one `+---+` rule spanning every column width.
pub(crate) fn text_table_border_into(out: &mut String, widths: &[usize]) {
    out.push('+');
    for w in widths {
        for _ in 0..w + 2 {
            out.push('-');
        }
        out.push('+');
    }
    out.push('\n');
}

/// Append one `| cell | cell |` row, padding each cell to its column width on
/// the aligned side. `right[ci]` picks the alignment per column.
pub(crate) fn text_table_row_into(
    out: &mut String,
    cells: &[&str],
    widths: &[usize],
    right: &[bool],
) {
    out.push('|');
    for (ci, w) in widths.iter().enumerate() {
        let cell = cells.get(ci).copied().unwrap_or("");
        let pad = w.saturating_sub(disp_width(cell));
        out.push(' ');
        if right.get(ci).copied().unwrap_or(false) {
            for _ in 0..pad {
                out.push(' ');
            }
            out.push_str(cell);
        } else {
            out.push_str(cell);
            for _ in 0..pad {
                out.push(' ');
            }
        }
        out.push_str(" |");
    }
    out.push('\n');
}

/// Serialise a grid as a plain-text aligned table (see the section comment
/// above). An empty result set prints the header and its closing rule only.
pub(crate) fn grid_to_text(grid: &Grid) -> String {
    let widths = text_table_widths(grid);
    let right = text_table_right(grid);
    let mut out = String::new();
    text_table_border_into(&mut out, &widths);
    let header: Vec<&str> = grid.columns.iter().map(String::as_str).collect();
    text_table_row_into(&mut out, &header, &widths, &right);
    text_table_border_into(&mut out, &widths);
    let mut cells: Vec<&str> = Vec::with_capacity(widths.len());
    for row in &grid.rows {
        cells.clear();
        for ci in 0..widths.len() {
            cells.push(row.get(ci).map(Val::text).unwrap_or(""));
        }
        text_table_row_into(&mut out, &cells, &widths, &right);
        text_table_border_into(&mut out, &widths);
    }
    out
}

/// `(schema, table)` for an INSERT export, or `None` when it cannot be
/// determined.
pub(crate) fn export_insert_table(app: &App) -> Option<(String, String)> {
    if let Some(ps) = &app.page_state {
        return Some((ps.schema.clone(), ps.table.clone()));
    }
    if let Some(s) = &app.script {
        if let Some(t) = s
            .drilled
            .and_then(|i| s.outcomes.get(i))
            .and_then(|o| guess_table_from_sql(&o.sql))
        {
            return Some((String::new(), t));
        }
    }
    app.last_sql
        .as_deref()
        .and_then(guess_table_from_sql)
        .map(|t| (String::new(), t))
}

/// One `INSERT` per row, reusing the R13 row→INSERT generator.
pub(crate) fn grid_to_inserts(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    grid: &Grid,
    app: &App,
) -> String {
    grid.rows
        .iter()
        .map(|row| build_insert_sql(cfg, schema, table, grid, row, app))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Grouped multi-row `INSERT … VALUES (…),(…);` statements, `batch` rows each.
pub(crate) fn grid_to_batch_inserts(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    grid: &Grid,
    app: &App,
    batch: usize,
) -> String {
    let types: Vec<Option<String>> = grid
        .columns
        .iter()
        .map(|c| column_type(app, schema, table, c))
        .collect();
    batch_insert_sql(cfg, schema, table, &grid.columns, &grid.rows, &types, batch)
}

/// Pure multi-row INSERT generator (split out so it can be tested without an
/// `App`). `types` is one declared column type per column, if known.
pub(crate) fn batch_insert_sql(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    columns: &[String],
    rows: &[Vec<Val>],
    types: &[Option<String>],
    batch: usize,
) -> String {
    let batch = batch.max(1);
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let cols = columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let mut out = String::new();
    for chunk in rows.chunks(batch) {
        let groups = chunk
            .iter()
            .map(|row| {
                let vals = columns
                    .iter()
                    .enumerate()
                    .map(|(ci, _)| {
                        let v = row.get(ci).cloned().unwrap_or(Val::Null);
                        insert_literal(
                            &v,
                            types.get(ci).and_then(|t| t.as_deref()),
                            Some(cfg.db_type.as_str()),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({vals})")
            })
            .collect::<Vec<_>>()
            .join(",\n");
        out.push_str(&format!(
            "INSERT INTO {} ({}) VALUES\n{};\n",
            table_ref(cfg.db_type, schema, table),
            cols,
            groups
        ));
    }
    out
}

/// Serialise the focused grid in the requested export format.
pub(crate) fn render_export_content(
    app: &App,
    grid: &Grid,
    format: ExportFormat,
    table: Option<&(String, String)>,
) -> String {
    match format {
        ExportFormat::Csv => grid_to_csv(grid),
        // XLSX is binary and file-only; there is no string form to copy.
        ExportFormat::Xlsx => String::new(),
        ExportFormat::JsonArray => grid_to_json_array(grid),
        ExportFormat::JsonNdjson => grid_to_json_ndjson(grid),
        ExportFormat::Markdown => grid_to_markdown(grid),
        ExportFormat::Text => grid_to_text(grid),
        ExportFormat::Insert => match (app.selected.as_ref(), table) {
            (Some(cfg), Some((schema, t))) => grid_to_inserts(cfg, schema, t, grid, app),
            _ => String::new(),
        },
        ExportFormat::InsertBatch => match (app.selected.as_ref(), table) {
            (Some(cfg), Some((schema, t))) => {
                grid_to_batch_inserts(cfg, schema, t, grid, app, EXPORT_INSERT_BATCH)
            }
            _ => String::new(),
        },
    }
}

// ── streaming export (background file path) ───────────────────────────────────
//
// The clipboard path above still builds one String (clipboard payloads are
// bounded and the copy must happen on the UI thread). A file export, by
// contrast, streams straight into a `BufWriter`: peak memory stays at one row
// (or one INSERT batch) instead of the whole document, and the work runs on a
// blocking worker so the UI never freezes. Every writer below is byte-for-byte
// identical to its `grid_to_*` counterpart — the `export_stream_matches_*`
// tests pin that down.

/// Stream `format` into `w`. Identical bytes to [`render_export_content`], but
/// no intermediate document.
pub(crate) fn write_export<W: Write + Seek>(
    w: &mut W,
    cfg: Option<&ConnectionConfig>,
    schema: &str,
    table: &str,
    types: &[Option<String>],
    grid: &Grid,
    format: ExportFormat,
) -> std::io::Result<()> {
    match format {
        ExportFormat::Csv => write_csv(w, grid),
        ExportFormat::Xlsx => write_xlsx(w, grid),
        ExportFormat::JsonArray => write_json_array(w, grid),
        ExportFormat::JsonNdjson => write_json_ndjson(w, grid),
        ExportFormat::Markdown => write_markdown(w, grid),
        ExportFormat::Text => write_text(w, grid),
        ExportFormat::Insert => match cfg {
            Some(cfg) => write_inserts(w, cfg, schema, table, types, grid),
            None => Ok(()),
        },
        ExportFormat::InsertBatch => match cfg {
            Some(cfg) => {
                write_batch_inserts(w, cfg, schema, table, types, grid, EXPORT_INSERT_BATCH)
            }
            None => Ok(()),
        },
    }
}

/// Stream `grid` as a single-sheet XLSX workbook through the kernel streaming
/// XLSX writer. The header is the grid's column names (comments are never
/// looked up — zero queries). A NULL becomes an empty cell, and every other
/// value is passed through verbatim as its raw text; the kernel turns a numeric
/// column's text into a numeric cell (with its scale preserved) and leaves the
/// rest as inline strings.
pub(crate) fn write_xlsx<W: Write + Seek>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    let columns = grid.columns.clone();
    let column_types = grid.types.clone();
    let column_comments: Vec<Option<String>> = vec![None; columns.len()];
    let mut writer = start_streaming_xlsx_workbook_with_options(
        w,
        Some("Result"),
        &columns,
        &column_types,
        &column_comments,
        &[],
        None,
        false,
        false,
    )
    .map_err(std::io::Error::other)?;
    let mut values: Vec<serde_json::Value> = Vec::with_capacity(columns.len());
    for row in &grid.rows {
        values.clear();
        for ci in 0..columns.len() {
            values.push(match row.get(ci) {
                Some(Val::Text(s)) => serde_json::Value::String(s.clone()),
                _ => serde_json::Value::Null,
            });
        }
        writer.write_row(&values).map_err(std::io::Error::other)?;
    }
    writer.finish().map_err(std::io::Error::other)?;
    Ok(())
}

/// Build the whole XLSX workbook in memory. Only for bounded payloads (tests
/// and the small-result helper); the file pipeline streams via [`write_xlsx`].
#[allow(dead_code)]
pub(crate) fn grid_to_xlsx(grid: &Grid) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    write_xlsx(&mut cursor, grid).expect("in-memory XLSX write cannot fail");
    cursor.into_inner()
}

/// The default destination filename for an XLSX export: `{base}.xlsx`, with any
/// path separator neutralised so the default stays one filename in the working
/// directory.
pub(crate) fn xlsx_default_filename(base: &str) -> String {
    let safe: String = base
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("{safe}.{}", ExportFormat::Xlsx.extension())
}

pub(crate) fn write_csv<W: Write>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    for (ci, c) in grid.columns.iter().enumerate() {
        if ci > 0 {
            w.write_all(b",")?;
        }
        w.write_all(csv_field(c).as_bytes())?;
    }
    w.write_all(b"\n")?;
    // One row per buffer: memory stays bounded while a whole row still goes out
    // in a single `write_all`.
    let mut line = String::new();
    for row in &grid.rows {
        line.clear();
        for ci in 0..grid.columns.len() {
            if ci > 0 {
                line.push(',');
            }
            line.push_str(&csv_field(row.get(ci).map(Val::text).unwrap_or("")));
        }
        line.push('\n');
        w.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// Pretty-printed JSON array, one object at a time. `to_string_pretty` of a
/// single object is context-free, so prefixing each of its lines with two
/// spaces reproduces the object's place inside the pretty array exactly.
pub(crate) fn write_json_array<W: Write>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    if grid.rows.is_empty() {
        return w.write_all(b"[]");
    }
    w.write_all(b"[\n")?;
    for (i, row) in grid.rows.iter().enumerate() {
        if i > 0 {
            w.write_all(b",\n")?;
        }
        let obj = serde_json::Value::Object(grid_row_object(grid, row));
        let s = serde_json::to_string_pretty(&obj).map_err(std::io::Error::other)?;
        for (j, line) in s.split('\n').enumerate() {
            if j > 0 {
                w.write_all(b"\n")?;
            }
            w.write_all(b"  ")?;
            w.write_all(line.as_bytes())?;
        }
    }
    w.write_all(b"\n]")
}

pub(crate) fn write_json_ndjson<W: Write>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    for row in &grid.rows {
        let obj = serde_json::Value::Object(grid_row_object(grid, row));
        if let Ok(s) = serde_json::to_string(&obj) {
            w.write_all(s.as_bytes())?;
            w.write_all(b"\n")?;
        }
    }
    Ok(())
}

pub(crate) fn write_markdown<W: Write>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    w.write_all(b"|")?;
    for c in &grid.columns {
        w.write_all(b" ")?;
        w.write_all(markdown_cell(c).as_bytes())?;
        w.write_all(b" |")?;
    }
    w.write_all(b"\n|")?;
    for _ in &grid.columns {
        w.write_all(b" --- |")?;
    }
    w.write_all(b"\n")?;
    let mut line = String::new();
    for row in &grid.rows {
        line.clear();
        line.push('|');
        for ci in 0..grid.columns.len() {
            line.push(' ');
            match row.get(ci) {
                None | Some(Val::Null) => line.push_str("NULL"),
                Some(Val::Text(s)) => line.push_str(&markdown_cell(s)),
            }
            line.push_str(" |");
        }
        line.push('\n');
        w.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// Stream the plain-text aligned table (byte-identical to [`grid_to_text`]).
/// The column widths need one full pass, but that is a `Vec<usize>` of column
/// count — the rows themselves are still written one line at a time.
pub(crate) fn write_text<W: Write>(w: &mut W, grid: &Grid) -> std::io::Result<()> {
    let widths = text_table_widths(grid);
    let right = text_table_right(grid);
    let mut line = String::new();
    text_table_border_into(&mut line, &widths);
    w.write_all(line.as_bytes())?;
    line.clear();
    let header: Vec<&str> = grid.columns.iter().map(String::as_str).collect();
    text_table_row_into(&mut line, &header, &widths, &right);
    w.write_all(line.as_bytes())?;
    line.clear();
    text_table_border_into(&mut line, &widths);
    w.write_all(line.as_bytes())?;
    let mut cells: Vec<&str> = Vec::with_capacity(widths.len());
    for row in &grid.rows {
        line.clear();
        cells.clear();
        for ci in 0..widths.len() {
            cells.push(row.get(ci).map(Val::text).unwrap_or(""));
        }
        text_table_row_into(&mut line, &cells, &widths, &right);
        w.write_all(line.as_bytes())?;
        line.clear();
        text_table_border_into(&mut line, &widths);
        w.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// One `INSERT` per row, separated by `\n` (no trailing newline), matching
/// [`grid_to_inserts`].
pub(crate) fn write_inserts<W: Write>(
    w: &mut W,
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    types: &[Option<String>],
    grid: &Grid,
) -> std::io::Result<()> {
    for (i, row) in grid.rows.iter().enumerate() {
        if i > 0 {
            w.write_all(b"\n")?;
        }
        w.write_all(build_insert_sql_types(cfg, schema, table, grid, row, types).as_bytes())?;
    }
    Ok(())
}

/// Grouped multi-row `INSERT … VALUES (…),(…);` — one statement per `batch`
/// rows, streamed batch by batch, matching [`batch_insert_sql`].
pub(crate) fn write_batch_inserts<W: Write>(
    w: &mut W,
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    types: &[Option<String>],
    grid: &Grid,
    batch: usize,
) -> std::io::Result<()> {
    let batch = batch.max(1);
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let cols = grid
        .columns
        .iter()
        .map(|c| q(c))
        .collect::<Vec<_>>()
        .join(", ");
    for chunk in grid.rows.chunks(batch) {
        // One statement per chunk: bounded by `batch` rows, and written in one
        // pass so the buffered writer sees a few large writes instead of
        // thousands of tiny ones.
        let mut out = String::new();
        out.push_str(&format!(
            "INSERT INTO {} ({}) VALUES\n",
            table_ref(cfg.db_type, schema, table),
            cols
        ));
        for (i, row) in chunk.iter().enumerate() {
            if i > 0 {
                out.push_str(",\n");
            }
            out.push('(');
            for (ci, _) in grid.columns.iter().enumerate() {
                if ci > 0 {
                    out.push_str(", ");
                }
                let v = row.get(ci).cloned().unwrap_or(Val::Null);
                out.push_str(&insert_literal(
                    &v,
                    types.get(ci).and_then(|t| t.as_deref()),
                    Some(cfg.db_type.as_str()),
                ));
            }
            out.push(')');
        }
        out.push_str(";\n");
        w.write_all(out.as_bytes())?;
    }
    Ok(())
}

/// True when the active grid has columns hidden to the right, i.e. horizontal
/// panning would actually change what is on screen.
pub(crate) fn has_h_scroll(app: &App) -> bool {
    if app.grid_kind == GridKind::Columns {
        return false;
    }
    // The common case (table data / query results) borrows the grid instead of
    // cloning it: this runs on every wheel event.
    if let Some(grid) = app.grid.as_ref() {
        let n = grid.columns.len();
        if n == 0 {
            return false;
        }
        return n > app.pinned_grid_cols().len() + visible_now_pinned(app, grid, app.col_offset);
    }
    let Some(grid) = active_grid(app) else {
        return false;
    };
    let n = grid.columns.len();
    if n == 0 {
        return false;
    }
    n > app.pinned_grid_cols().len() + visible_now_pinned(app, &grid, app.col_offset)
}

/// Column count of the active grid, without cloning its rows.
pub(crate) fn active_col_count(app: &App) -> Option<usize> {
    if let Some(s) = &app.script {
        if let Some(i) = s.drilled {
            let full = &s.outcomes.get(i)?.grid;
            return Some(if app.col_hidden.is_empty() {
                full.columns.len()
            } else {
                filter_grid(full, &app.col_hidden).columns.len()
            });
        }
    }
    app.grid.as_ref().map(|g| g.columns.len())
}

/// R91: how many non-pinned columns fit starting at `off`, using the geometry
/// the last render captured and skipping the pinned columns (they sit in the
/// left block). Exact rather than remembered, so a pan can place the cell cursor
/// where the renderer will actually keep the window; the cached widths mean a
/// wheel event never rescans the result set.
pub(crate) fn visible_now_pinned(app: &App, grid: &Grid, off: usize) -> usize {
    let avail = app.grid_avail.max(MIN_CELL_WIDTH);
    let max_cell = app.grid_max_cell.max(MIN_CELL_WIDTH);
    let pinned = app.pinned_grid_cols();
    if let Some((epoch, cell, widths)) = &app.width_cache {
        if *epoch == app.grid_epoch
            && *cell == max_cell
            && widths.len() == grid.columns.len()
            && app.grid.as_ref().is_some_and(|g| std::ptr::eq(g, grid))
        {
            return visible_scroll_cols(widths, off, avail, &pinned).max(1);
        }
    }
    let widths = natural_widths_fmt(grid, max_cell, app.num_fmt);
    visible_scroll_cols(&widths, off, avail, &pinned).max(1)
}

/// R91: [`visible_now_pinned`] for the active grid.
pub(crate) fn active_visible_cols_pinned(app: &App, off: usize) -> usize {
    if let Some(grid) = app.grid.as_ref() {
        return visible_now_pinned(app, grid, off);
    }
    match active_grid(app) {
        Some(g) => visible_now_pinned(app, &g, off),
        None => 1,
    }
}

/// Pure core of `pan_columns`: move the window by `delta` columns and place the
/// cell cursor inside it. `vis` is the number of columns that fit at the new
/// origin, so the result is exactly what `window_for_cursor` will keep.
#[cfg(test)]
pub(crate) fn pan_window(
    n: usize,
    frozen: usize,
    off: usize,
    cursor: usize,
    vis: usize,
    delta: i32,
) -> (usize, usize) {
    if n == 0 {
        return (off, cursor);
    }
    let min_off = frozen.min(n - 1);
    let next = (off as i32 + delta).clamp(min_off as i32, n as i32 - 1) as usize;
    let cursor = if cursor >= frozen {
        let hi = (next + vis.max(1) - 1).min(n - 1);
        cursor.clamp(next, hi)
    } else {
        cursor
    };
    (next, cursor)
}

/// R91: [`pan_window`] for an arbitrary pinned-column set. `off` and the result
/// are the window origin *within the scrollable columns*; the pinned ones are
/// skipped. The cursor is kept inside the new window (pinned cursors stay put).
pub(crate) fn pan_window_pinned(
    n: usize,
    pinned: &[usize],
    off: usize,
    cursor: usize,
    vis: usize,
    delta: i32,
) -> (usize, usize) {
    if n == 0 {
        return (off, cursor);
    }
    let scroll: Vec<usize> = (0..n).filter(|c| !pinned.contains(c)).collect();
    if scroll.is_empty() {
        return (off, cursor);
    }
    let pos = scroll.iter().position(|&c| c == off).unwrap_or(0);
    let tpos = (pos as i32 + delta).clamp(0, scroll.len() as i32 - 1) as usize;
    let next = scroll[tpos];
    let cursor = if pinned.contains(&cursor) {
        cursor
    } else {
        let win = scroll_window_cols(n, next, vis.max(1), pinned);
        if win.contains(&cursor) {
            cursor
        } else if cursor < next {
            next
        } else {
            win.last().copied().unwrap_or(next)
        }
    };
    (next, cursor)
}

/// R47b: how long the horizontal scroll bar stays visible after a horizontal
/// scroll. Long enough to read the thumb position, short enough that the bottom
/// border returns to a plain line at rest.
pub(crate) const HBAR_VISIBLE_MS: u64 = 2500;

/// Pure visibility test for the auto-hiding horizontal scroll bar.
pub(crate) fn hbar_should_show(until: Option<Instant>, now: Instant) -> bool {
    until.is_some_and(|deadline| now < deadline)
}

/// Pan the visible column *window* by `delta` columns and pull the cell cursor
/// along so it never leaves the screen.
///
/// The window itself moves: a wheel notch, a swipe step or a `◀`/`▶` tap has to
/// change what is on screen immediately. (Moving only the cursor, as the first
/// implementation did, made a horizontal swipe look dead until the cursor had
/// walked past the right edge of the window.)
/// Returns true when a grid with a horizontal overflow handled it.
pub(crate) fn pan_columns(app: &mut App, delta: i32) -> bool {
    if !has_h_scroll(app) {
        return false;
    }
    let Some(n) = active_col_count(app) else {
        return false;
    };
    let pinned = app.pinned_grid_cols();
    let Some(first) = next_scroll_col(0, n, &pinned) else {
        return false;
    };
    let target = (app.col_offset as i32 + delta).clamp(first as i32, n as i32 - 1) as usize;
    let anchor = next_scroll_col(target.min(n - 1), n, &pinned).unwrap_or(first);
    let vis = active_visible_cols_pinned(app, anchor);
    let (off, cursor) = pan_window_pinned(
        n,
        &pinned,
        next_scroll_col(app.col_offset.min(n - 1), n, &pinned).unwrap_or(first),
        app.col_cursor,
        vis,
        delta,
    );
    app.col_offset = off;
    app.col_cursor = cursor;
    // R47b: a horizontal scroll re-summons the auto-hiding progress bar.
    app.poke_hbar();
    true
}

/// Does this wheel event mean "pan columns" rather than "scroll rows"?
///
/// A terminal is only required to put a modifier into a wheel event's SGR button
/// byte if it chooses to; many PC terminals and tmux never set the SHIFT bit for
/// Shift+wheel (and some swallow Shift+wheel for their own horizontal scroll), so
/// the app cannot rely on SHIFT alone. ALT and CONTROL are reported far more
/// reliably, and `Ctrl-G` pan mode works regardless of what the terminal sends.
pub(crate) fn wheel_wants_pan(
    focus: Focus,
    mods: KeyModifiers,
    pan_mode: bool,
    has_h_scroll: bool,
) -> bool {
    let modifier_pan =
        mods.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    focus == Focus::Preview && (pan_mode || modifier_pan) && has_h_scroll
}

/// Does this wheel event mean "pan columns" rather than "scroll rows"?
pub(crate) fn wheel_pans_columns(app: &App, m: &MouseEvent) -> bool {
    wheel_wants_pan(app.focus, m.modifiers, app.pan_mode, has_h_scroll(app))
}

/// How many recent mouse events the `DBXT_MOUSE_DEBUG` overlay keeps.
pub(crate) const MOUSE_DEBUG_LINES: usize = 6;

/// The exact wire encoding that would produce this event, so a user can report
/// (or replay) the bytes their terminal sends. crossterm parses both the modern
/// SGR encoding and the legacy X10 one, so both are printed.
pub(crate) fn mouse_wire_hint(m: &MouseEvent) -> String {
    let base = match m.kind {
        MouseEventKind::Down(MouseButton::Left) => 0,
        MouseEventKind::Down(MouseButton::Middle) => 1,
        MouseEventKind::Down(MouseButton::Right) => 2,
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(MouseButton::Left) => 32,
        MouseEventKind::Drag(MouseButton::Middle) => 33,
        MouseEventKind::Drag(MouseButton::Right) => 34,
        MouseEventKind::Moved => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };
    let mut cb = base;
    if m.modifiers.contains(KeyModifiers::SHIFT) {
        cb += 4;
    }
    if m.modifiers.contains(KeyModifiers::ALT) {
        cb += 8;
    }
    if m.modifiers.contains(KeyModifiers::CONTROL) {
        cb += 16;
    }
    let x = m.column as u32 + 1;
    let y = m.row as u32 + 1;
    let end = if matches!(m.kind, MouseEventKind::Up(_)) {
        'm'
    } else {
        'M'
    };
    let mut out = format!("SGR \\x1b[<{cb};{x};{y}{end}");
    // Legacy X10 encoding: three bytes after `ESC [ M`, each offset by 32.
    if x <= 223 && y <= 223 && cb + 32 <= 255 {
        out.push_str(&format!(
            " | X10 \\x1b[M {}+32 {}+32 {}+32",
            cb, m.column, m.row
        ));
    }
    out
}

/// One line describing a mouse event, with the encoding it arrived in.
pub(crate) fn describe_mouse(m: &MouseEvent) -> String {
    let mods = if m.modifiers.is_empty() {
        String::new()
    } else {
        format!(" mods={:?}", m.modifiers)
    };
    format!(
        "{:?} @({},{}){} · {}",
        m.kind,
        m.column,
        m.row,
        mods,
        mouse_wire_hint(m)
    )
}

/// Human-readable description of the mouse/resize events we trace.
pub(crate) fn describe_event(ev: &Event) -> Option<String> {
    match ev {
        Event::Mouse(m) => Some(format!("Mouse {}", describe_mouse(m))),
        Event::Resize(w, h) => Some(format!("Resize {w}x{h}")),
        _ => None,
    }
}

/// Short one-line form used by the status bar, so a long wire encoding never
/// crowds out the page / row / column readout next to it.
pub(crate) fn describe_event_short(ev: &Event) -> Option<String> {
    match ev {
        Event::Mouse(m) => {
            let mods = if m.modifiers.is_empty() {
                String::new()
            } else {
                format!(" mods={:?}", m.modifiers)
            };
            Some(format!(
                "Mouse {:?} @({},{}){}",
                m.kind, m.column, m.row, mods
            ))
        }
        Event::Resize(w, h) => Some(format!("Resize {w}x{h}")),
        _ => None,
    }
}

/// Append a mouse/resize event to the trace file and remember it for the status
/// bar. Only mouse and resize events are traced: never keystrokes, so a password
/// typed into the connection form can never be written to disk.
pub(crate) fn trace_event(app: &mut App, ev: &Event) {
    let Some(desc) = describe_event(ev) else {
        return;
    };
    if let Some(path) = &app.trace_path {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{desc}");
        }
    }
    app.last_event = describe_event_short(ev);
}

/// Short tab label for a query: its first non-empty line.
pub(crate) fn query_tab_title(sql: &str) -> String {
    let first = sql
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    truncate_disp(first, 40)
}

impl App {
    /// Store a freshly fetched grid: the unfiltered original is kept so the
    /// column-visibility filter can be re-applied later, and the display grid is
    /// the filtered view (hidden columns + the result-row search). The structure
    /// field list is never filtered.
    pub(crate) fn set_grid(&mut self, grid: Grid) {
        self.grid_full = Some(grid);
        // R101: a fresh result replaces whatever snapshot diff was on screen.
        self.result_diff = None;
        self.rebuild_view();
    }

    /// R91: the pinned columns for the last render, or a prefix of `grid_frozen`
    /// when only the count was set (tests set the count directly).
    pub(crate) fn pinned_grid_cols(&self) -> Vec<usize> {
        if !self.grid_frozen_cols.is_empty() {
            self.grid_frozen_cols.clone()
        } else {
            (0..self.grid_frozen).collect()
        }
    }

    pub(crate) fn clear_grid(&mut self) {
        self.grid = None;
        self.grid_full = None;
        // R101: the grid a snapshot diff described is gone.
        self.result_diff = None;
        self.result_rows.clear();
        // R91: a reference row belongs to the result set it was pinned on.
        self.ref_row = None;
        // The render-captured pin geometry no longer matches a new grid.
        self.grid_frozen_cols.clear();
        self.grid_frozen = 0;
        // R57: a row selection belongs to the grid it was started on.
        self.row_sel_anchor = None;
        // A value locate belongs to the grid it was started on; a new result
        // (another table, a query, a tab switch) starts clean.
        self.locate_needle.clear();
        self.locate_col = None;
        self.locate_prompt = None;
        // R56: a row-number jump belongs to the grid it was started on.
        self.goto_prompt = None;
        self.clear_col_filter();
        self.grid_epoch = self.grid_epoch.wrapping_add(1);
        self.width_cache = None;
    }

    /// R52: the active column filter as `(column name, needle)` for the row
    /// filter, or `None` when no column filter is set.
    pub(crate) fn col_filter_spec(&self) -> Option<(&str, &str)> {
        let name = self.col_filter_name.as_deref()?;
        Some((name, self.col_filter_needle.as_str()))
    }

    /// Drop the column filter (Esc, a new result, or a connection switch).
    /// Returns `true` when something was actually cleared.
    pub(crate) fn clear_col_filter(&mut self) -> bool {
        let had = self.col_filter_name.is_some()
            || !self.col_filter_needle.is_empty()
            || self.col_filter_prompt.is_some();
        self.col_filter_name = None;
        self.col_filter_needle.clear();
        self.col_filter_prompt = None;
        had
    }

    /// R64: drop the in-result cell-find needle, prompt and hit list. The cursor
    /// is left where it is (matching [`clear_locate`]). Returns `true` when
    /// something was actually cleared.
    pub(crate) fn clear_cell_find(&mut self) -> bool {
        let had = self.cell_find_prompt.is_some()
            || !self.cell_find_needle.is_empty()
            || !self.cell_find_hits.is_empty();
        self.cell_find_prompt = None;
        self.cell_find_needle.clear();
        self.cell_find_hits.clear();
        self.cell_find_idx = 0;
        self.cell_find_capped = false;
        had
    }

    /// Re-apply the session column selection to the grid on screen.
    pub(crate) fn reapply_col_filter(&mut self) {
        self.rebuild_view();
    }

    /// Recompute the displayed grid from `grid_full` by applying the hidden-column
    /// set, the column filter and the result-row search, and rebuild the
    /// display→source row map that the popups and `y` (copy as INSERT) rely on.
    pub(crate) fn rebuild_view(&mut self) {
        // The displayed grid is about to change: invalidate the width cache.
        self.grid_epoch = self.grid_epoch.wrapping_add(1);
        self.width_cache = None;
        // R57: the display→source row map is rebuilt below, so any row selection
        // indexed into the old view is dropped rather than pointing at a
        // different row after a filter change.
        self.row_sel_anchor = None;
        let Some(full) = self.grid_full.clone() else {
            self.grid = None;
            self.result_rows.clear();
            return;
        };
        let cols = if self.grid_kind == GridKind::Columns {
            full.clone()
        } else {
            filter_grid(&full, &self.col_hidden)
        };
        let col = self.col_filter_spec();
        let row_active = !self.result_needle.trim().is_empty();
        let col_active = col.is_some_and(|(_, n)| !n.trim().is_empty());
        if self.grid_kind == GridKind::Columns || (!row_active && !col_active) {
            self.result_rows = (0..cols.rows.len()).collect();
            self.grid = Some(cols);
            return;
        }
        let map = kept_row_indices(&cols, &self.result_needle, col);
        let rows = map.iter().map(|&i| cols.rows[i].clone()).collect();
        self.grid = Some(Grid {
            columns: cols.columns,
            types: cols.types,
            rows,
            note: cols.note,
        });
        self.result_rows = map;
    }

    /// Natural column widths for `grid` at `max_cell`, served from the cache
    /// unless the displayed grid or the width cap changed. This is the single
    /// hot-path cost the render loop used to pay on every frame. R76: `mode`
    /// feeds the big-number display layer, so the cached widths match what the
    /// render pass actually draws (the cache is cleared whenever `mode`
    /// changes).
    pub(crate) fn column_widths(
        &mut self,
        grid: &Grid,
        max_cell: usize,
        mode: NumFmt,
    ) -> Vec<usize> {
        if let Some((epoch, cell, widths)) = &self.width_cache {
            if *epoch == self.grid_epoch && *cell == max_cell && widths.len() == grid.columns.len()
            {
                return widths.clone();
            }
        }
        let widths = natural_widths_fmt(grid, max_cell, mode);
        self.width_cache = Some((self.grid_epoch, max_cell, widths.clone()));
        widths
    }

    /// Index of the focused display row in the *unfiltered* grid. The result-row
    /// search keeps a display→source map, so this is the identity when no row
    /// filter is active. Handles both the top-level grid and a drilled script
    /// result.
    pub(crate) fn full_row_index(&self) -> Option<usize> {
        full_row_at(self, self.sel)
    }

    /// Persist the in-memory config (best-effort, silent on failure).
    pub(crate) fn persist(&self) {
        if let Some(p) = &self.config_path {
            self.config.save(p);
        }
    }

    /// Save the current table's hidden-column set under its
    /// `(database, schema, table)`.
    pub(crate) fn persist_cols(&mut self) {
        let Some(ps) = self.page_state.clone() else {
            return;
        };
        let db = self.current_db();
        self.config.entry(&db, &ps.schema, &ps.table).hidden = self.col_hidden.clone();
        self.persist();
    }

    /// Copy the on-screen query result back into its tab before leaving it.
    pub(crate) fn save_result_tab(&mut self) {
        let idx = self.result_tab;
        if self.grid_kind == GridKind::TableData || idx >= self.result_tabs.len() {
            return;
        }
        if let Some(tab) = self.result_tabs.get_mut(idx) {
            tab.grid = self.grid.clone();
            tab.grid_full = self.grid_full.clone();
            tab.script = self.script.clone();
            tab.kind = self.grid_kind;
            tab.sel = self.sel;
            tab.col_offset = self.col_offset;
            tab.col_cursor = self.col_cursor;
            // R112: the freeze toggle rides with the tab.
            tab.freeze_first = self.freeze_first;
        }
    }

    /// Show the active tab's stored view.
    pub(crate) fn restore_result_tab(&mut self) {
        let Some(tab) = self.result_tabs.get(self.result_tab).cloned() else {
            return;
        };
        // R103: `g m` materializes the tab actually on screen, so the active
        // statement follows the tab flip (a script list clears it).
        self.last_sql = tab.sql.clone();
        self.grid_full = tab.grid_full.clone();
        self.script = tab.script;
        self.grid_kind = tab.kind;
        if self.script.is_some() {
            // A result search belongs to a data grid, not the script list.
            self.result_needle.clear();
            self.result_filter = None;
            self.clear_col_filter();
            self.locate_needle.clear();
            self.locate_col = None;
            self.locate_prompt = None;
        }
        self.rebuild_view();
        self.sel = tab.sel.min(
            self.grid
                .as_ref()
                .map(|g| g.rows.len())
                .unwrap_or(0)
                .saturating_sub(1),
        );
        self.col_offset = tab.col_offset;
        self.col_cursor = tab.col_cursor;
        // R112: each tab keeps its own first-column freeze state.
        self.freeze_first = tab.freeze_first;
        // R91: the reference row belongs to the result that was on screen; a
        // tab flip shows a different grid.
        self.ref_row = None;
        self.page_state = None;
        self.cell_popup = None;
        self.row_popup = None;
    }
}

/// Record a fresh query result as a new tab and show it.
pub(crate) fn push_result_tab(
    app: &mut App,
    title: String,
    grid: Option<Grid>,
    script: Option<ScriptView>,
    kind: GridKind,
) {
    app.save_result_tab();
    app.result_tabs.push(ResultTab {
        title,
        sql: None,
        grid: grid.as_ref().map(|g| {
            if kind == GridKind::Columns {
                g.clone()
            } else {
                filter_grid(g, &app.col_hidden)
            }
        }),
        grid_full: grid.clone(),
        script: script.clone(),
        kind,
        sel: 0,
        col_offset: 0,
        col_cursor: 0,
        // R112: a fresh tab starts with the first column unfrozen.
        freeze_first: false,
    });
    app.result_tab = app.result_tabs.len() - 1;
    // Cap the history so a long session cannot grow without bound.
    if app.result_tabs.len() > 20 {
        app.result_tabs.remove(0);
        app.result_tab = app.result_tab.saturating_sub(1);
    }
    app.grid_kind = kind;
    if let Some(g) = grid {
        app.set_grid(g);
    } else {
        app.clear_grid();
    }
    app.script = script;
    app.sel = 0;
    app.col_offset = 0;
    app.col_cursor = 0;
    // R112: a new tab is born unfrozen.
    app.freeze_first = false;
    app.page_state = None;
    app.cell_popup = None;
    app.row_popup = None;
}

/// Show a result in place of the active tab (used by Ctrl-N "load more").
pub(crate) fn replace_result_tab(
    app: &mut App,
    title: String,
    grid: Option<Grid>,
    script: Option<ScriptView>,
    kind: GridKind,
) {
    if app.result_tabs.is_empty() {
        push_result_tab(app, title, grid, script, kind);
        return;
    }
    let idx = app.result_tab.min(app.result_tabs.len() - 1);
    // R103: a load-more replaces the same result, so its source SQL carries over.
    let keep_sql = app.result_tabs[idx].sql.clone();
    // R112: a load-more is the same result, so its freeze state carries over too.
    let keep_freeze = app.result_tabs[idx].freeze_first;
    app.result_tabs[idx] = ResultTab {
        title,
        sql: keep_sql,
        grid: grid.as_ref().map(|g| {
            if kind == GridKind::Columns {
                g.clone()
            } else {
                filter_grid(g, &app.col_hidden)
            }
        }),
        grid_full: grid.clone(),
        script: script.clone(),
        kind,
        sel: 0,
        col_offset: 0,
        col_cursor: 0,
        freeze_first: keep_freeze,
    };
    app.result_tab = idx;
    app.grid_kind = kind;
    if let Some(g) = grid {
        app.set_grid(g);
    } else {
        app.clear_grid();
    }
    app.script = script;
    app.sel = 0;
    app.col_offset = 0;
    app.col_cursor = 0;
    // R112: the load-more keeps the tab's freeze state.
    app.freeze_first = keep_freeze;
    app.page_state = None;
    app.cell_popup = None;
    app.row_popup = None;
}

/// `[` / `]`: flip between the query results kept in this session.
pub(crate) fn switch_result_tab(app: &mut App, delta: i32) {
    if app.result_tabs.len() < 2 {
        app.status = t("仅 1 个结果标签").into();
        return;
    }
    app.save_result_tab();
    let n = app.result_tabs.len() as i32;
    app.result_tab = (app.result_tab as i32 + delta).rem_euclid(n) as usize;
    app.restore_result_tab();
    app.status = tf(
        "结果 {}/{}",
        &[&(app.result_tab + 1), &(app.result_tabs.len())],
    );
}

/// True when the active result tab carries an edit that has not been committed:
/// the diff-style cell edit dialog or the red write confirmation is open. Such a
/// tab is not closed by `Alt-W` (the hint asks the user to confirm or cancel
/// first); every other tab closes immediately, purely client-side, with no
/// confirmation overlay.
pub(crate) fn result_tab_pending_edit(app: &App) -> bool {
    app.edit_dialog.is_some() || app.confirm.is_some()
}

/// `Alt-W`: close the active query-result tab. The last tab is never closed
/// (the grid would have nowhere to go), and a tab with a pending edit is left
/// alone so the confirmation is not lost. Everything here is client-side: no
/// query, no socket, and the dropped grid is only in-memory state.
pub(crate) fn close_result_tab(app: &mut App) {
    if app.result_tabs.is_empty() {
        app.status = t("当前没有结果标签").into();
        return;
    }
    if app.grid_kind != GridKind::Query {
        // Closing a query tab while a table page is on screen would yank the
        // user into another result, so the action stays scoped to query views.
        app.status = t("当前不是查询结果 · 标签不可关").into();
        return;
    }
    if result_tab_pending_edit(app) {
        app.status = t("有未确认的编辑 · 先确认或 Esc 取消").into();
        return;
    }
    if app.result_tabs.len() <= 1 {
        app.status = t("最后一个结果标签不可关闭").into();
        return;
    }
    let idx = app.result_tab.min(app.result_tabs.len() - 1);
    app.result_tabs.remove(idx);
    if app.result_tab >= app.result_tabs.len() {
        app.result_tab = app.result_tabs.len() - 1;
    }
    app.restore_result_tab();
    app.status = tf("已关闭结果标签 · 剩 {}", &[&(app.result_tabs.len())]);
}

/// `Alt-F` in the results pane (R48): pin / unpin the current grid. A pinned
/// grid keeps showing in a strip above the live one, so switching to another
/// table / database lets the two be compared up-and-down. Only data grids make
/// sense to pin; an empty pane or a script list reports why.
pub(crate) fn toggle_pin_results(app: &mut App) {
    if app.pinned_result.is_some() {
        app.pinned_result = None;
        app.status = t("📌 已解除钉住").into();
        return;
    }
    let Some(grid) = app.grid.clone() else {
        app.status = t("无可钉住的结果（先打开一张表或执行查询）").into();
        return;
    };
    if grid.columns.is_empty() {
        app.status = t("无可钉住的结果").into();
        return;
    }
    let title = grid_title(app);
    app.pinned_result = Some(PinnedResult {
        title,
        grid,
        kind: app.grid_kind,
    });
    app.status = t("📌 已钉住结果区 · 切换表/库仍显示 · Alt-F 解除").into();
}

/// `Ctrl-P`: run the editor's SQL through the dialect's EXPLAIN.
pub(crate) fn explain_current(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let sql = app.editor_sql();
    match explain_sql_for(cfg.db_type.as_str(), &sql) {
        Some(explain) => {
            app.loading = true;
            // R88: EXPLAIN is its own run — never inherit an editor scope label.
            app.pending_scope = None;
            app.status = format!("{} EXPLAIN…", cfg.db_type.as_str());
            let db = app.current_db();
            let epoch = app.register_query(&cfg, explain.clone());
            app.spawn(
                tx,
                Op::Query(Box::new(cfg), db, explain, QUERY_MAX_ROWS, "editor", epoch),
            );
        }
        None => {
            app.status = tf(
                "{} 不支持单语句 EXPLAIN（请手动执行）",
                &[&(cfg.db_type.as_str())],
            );
        }
    }
}

/// `Ctrl-Y`: open the export overlay for the focused result grid.
pub(crate) fn open_export(app: &mut App) {
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可导出的结果").into();
        return;
    };
    if grid.columns.is_empty() {
        app.status = t("没有可导出的列").into();
        return;
    }
    let rows = grid.rows.len();
    app.export_open = true;
    app.export_list.select(Some(0));
    app.export_pending = None;
    app.export_path = None;
    // R108: a fresh picker never inherits a stale all-tabs flow.
    app.batch_export_pending = None;
    app.batch_export_confirm = None;
    app.status = if rows > EXPORT_SLOW_ROWS {
        tf("选择导出格式（{} 行，生成可能耗时）", &[&rows])
    } else {
        tf("选择导出格式（{} 行）", &[&rows])
    };
}

/// Format-picker keys for the export overlay.
pub(crate) fn export_key(app: &mut App, k: KeyEvent) {
    let n = EXPORT_FORMATS.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.export_open = false;
            app.flash(t("已取消导出").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app
                .export_list
                .selected()
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0);
            app.export_list.select(Some(i));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app
                .export_list
                .selected()
                .map(|i| (i + 1).min(n - 1))
                .unwrap_or(0);
            app.export_list.select(Some(i));
        }
        KeyCode::Enter => {
            let idx = app.export_list.selected().unwrap_or(0).min(n - 1);
            choose_export_format(app, EXPORT_FORMATS[idx]);
        }
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < n {
                choose_export_format(app, EXPORT_FORMATS[idx]);
            }
        }
        // R108: the "all tabs" section below the format list. `A` packages
        // every result tab as a worksheet, `S` as a ZIP of per-tab `.sql`
        // files. Both are scoped to this modal — no new global key.
        KeyCode::Char('A') | KeyCode::Char('a') => {
            begin_batch_export(app, BatchExportKind::Xlsx)
        }
        KeyCode::Char('S') | KeyCode::Char('s') => {
            begin_batch_export(app, BatchExportKind::SqlZip)
        }
        _ => {}
    }
}

/// Pick a format and move on to the destination prompt.
pub(crate) fn choose_export_format(app: &mut App, format: ExportFormat) {
    // Excel is built entirely in memory, so a huge result would exhaust RAM.
    // Refuse before anything is generated and point at the streaming CSV path.
    if format == ExportFormat::Xlsx {
        let rows = active_grid_rows(app);
        if rows > EXPORT_XLSX_MAX_ROWS {
            app.status = tf(
                "结果 {} 行超过 Excel 导出上限（{} 行），请改用 CSV",
                &[&rows, &EXPORT_XLSX_MAX_ROWS],
            );
            app.export_open = false;
            return;
        }
    }
    let table = if matches!(format, ExportFormat::Insert | ExportFormat::InsertBatch) {
        match export_insert_table(app) {
            Some(t) => Some(t),
            None => {
                app.status =
                    t("无法确定表名，INSERT 导出不可用（先浏览表或含 FROM 的查询）").into();
                app.export_open = false;
                return;
            }
        }
    } else {
        None
    };
    let mut ta = TextArea::default();
    if format.file_only() {
        // The clipboard cannot carry a binary workbook, so the default
        // filename is prefilled and Enter writes it straight away.
        let base = export_insert_table(app)
            .map(|(_, t)| t)
            .unwrap_or_else(|| "query".to_string());
        ta.insert_str(xlsx_default_filename(&base));
        ta.set_placeholder_text(t("Excel 仅支持写入文件 · 请输入文件名"));
    } else {
        ta.set_placeholder_text(t("留空 = 复制到剪贴板 · 输入路径 = 写入文件"));
    }
    app.export_pending = Some(ExportPending { format, table });
    app.export_path = Some(ta);
    app.export_open = false;
}

/// Destination prompt: blank copies via OSC 52, otherwise writes a file.
pub(crate) fn export_path_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // R108: the all-tabs export reuses this prompt but always writes a file.
    if app.batch_export_pending.is_some() {
        batch_export_path_key(app, tx, k);
        return;
    }
    let Some(mut ta) = app.export_path.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.export_pending = None;
        app.flash(t("已取消导出").into());
        return;
    }
    if k.code != KeyCode::Enter {
        ta.input(k);
        app.export_path = Some(ta);
        return;
    }
    let Some(pending) = app.export_pending.take() else {
        return;
    };
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可导出的结果").into();
        return;
    };
    let input = ta.lines().join("\n");
    let path = input.trim();
    let label = pending.format.label();
    if path.is_empty() {
        // A binary workbook has no OSC 52 form: keep the prompt open and ask
        // for a filename instead of silently exporting nothing.
        if pending.format.file_only() {
            app.export_pending = Some(pending);
            app.export_path = Some(ta);
            app.status = t("Excel 导出仅支持写入文件，请输入文件名").into();
            return;
        }
        // Clipboard export stays synchronous: the payload is bounded by what a
        // terminal can carry and the OSC 52 write must run on the UI thread.
        let content = render_export_content(app, &grid, pending.format, pending.table.as_ref());
        let n = content.chars().count();
        match clipboard_copy(&content) {
            Some(p) => {
                app.status = tf(
                    "✓ 已导出 {} 到剪贴板（{} 字符）· 兜底 {}",
                    &[&label, &n, &(p.display())],
                )
            }
            None => app.status = tf("✓ 已导出 {} 到剪贴板（{} 字符）", &[&label, &n]),
        }
        return;
    }
    // File export runs on a background worker and streams to disk: the UI keeps
    // painting (spinner) and peak memory stays at one row, not the whole file.
    let expanded = expand_home(path);
    let cfg = app.selected.clone();
    let (schema, table) = pending.table.clone().unwrap_or_default();
    let types = grid_column_types(app, &schema, &table, &grid);
    let rows = grid.rows.len();
    app.loading = true;
    app.status = tf(
        "导出中… {} · {} 行 → {}",
        &[&label, &rows, &(expanded.display())],
    );
    app.spawn(
        tx,
        Op::Export(Box::new(ExportJob {
            format: pending.format,
            path: expanded,
            grid,
            cfg,
            schema,
            table,
            types,
        })),
    );
}

// ── R108: export every result tab at once ─────────────────────────────────────

/// `A` / `S` in the export picker: package every result tab. Opens the red
/// confirmation when the payload is large, otherwise goes straight to the
/// destination prompt. Purely client-side — the grids are already in memory.
pub(crate) fn begin_batch_export(app: &mut App, kind: BatchExportKind) {
    let tabs = collect_batch_tabs(app);
    if tabs.is_empty() {
        app.export_open = false;
        app.status = t("没有可导出的结果 Tab").into();
        return;
    }
    if kind == BatchExportKind::SqlZip && app.selected.is_none() {
        app.export_open = false;
        app.status = t("✗ 未选择连接").into();
        return;
    }
    if let Some((n, rows)) = batch_guard(&tabs) {
        app.export_open = false;
        app.status = tf(
            "全部 Tab 导出需确认 · {} 个 Tab · {} 行 · Enter 继续 · Esc 取消",
            &[&n, &rows],
        );
        app.batch_export_confirm = Some(BatchExportConfirm { kind, tabs });
        return;
    }
    open_batch_export_path(app, kind, tabs);
}

/// Open the destination prompt for an all-tabs export. Both formats are
/// file-only, so the default `{db}-results-{HHMMSS}.{ext}` filename is
/// prefilled and blank input keeps it.
pub(crate) fn open_batch_export_path(app: &mut App, kind: BatchExportKind, tabs: Vec<BatchTab>) {
    let filename = batch_default_filename(&app.current_db(), kind);
    let mut ta = TextArea::default();
    ta.insert_str(&filename);
    ta.set_placeholder_text(t("输入路径支持 ~"));
    let tab_count = tabs.len();
    let rows = batch_rows(&tabs);
    app.batch_export_pending = Some(BatchExportPending { kind, tabs });
    app.export_path = Some(ta);
    app.export_open = false;
    app.status = tf(
        "导出全部 Tab · {} 个 · {} 行 · Enter 写入 · Esc 取消",
        &[&tab_count, &rows],
    );
}

/// Keys for the `>20` tabs / `>200_000` rows confirmation layer.
pub(crate) fn batch_export_confirm_key(app: &mut App, _tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(c) = app.batch_export_confirm.take() else {
                return;
            };
            open_batch_export_path(app, c.kind, c.tabs);
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.batch_export_confirm = None;
            app.flash(t("已取消导出").into());
        }
        _ => {}
    }
}

/// Destination prompt for an all-tabs export (always a file).
pub(crate) fn batch_export_path_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut ta) = app.export_path.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.batch_export_pending = None;
        app.flash(t("已取消导出").into());
        return;
    }
    if k.code != KeyCode::Enter {
        ta.input(k);
        app.export_path = Some(ta);
        return;
    }
    let Some(pending) = app.batch_export_pending.take() else {
        return;
    };
    let input = ta.lines().join("\n");
    let input = input.trim();
    let chosen = if input.is_empty() {
        batch_default_filename(&app.current_db(), pending.kind)
    } else {
        input.to_string()
    };
    let expanded = expand_home(&chosen);
    // The ZIP manifest records just the file name, never a directory.
    let file_name = PathBuf::from(&chosen)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| chosen.clone());
    let base = file_name
        .strip_suffix(&format!(".{}", pending.kind.extension()))
        .unwrap_or(&file_name)
        .to_string();
    let tab_count = pending.tabs.len();
    let rows = batch_rows(&pending.tabs);
    app.loading = true;
    app.status = tf(
        "导出中… {} · {} 个 Tab · {} 行 → {}",
        &[&pending.kind.label(), &tab_count, &rows, &(expanded.display())],
    );
    app.spawn(
        tx,
        Op::BatchExport(Box::new(BatchExportJob {
            kind: pending.kind,
            path: expanded,
            tabs: pending.tabs,
            cfg: app.selected.clone(),
            base,
        })),
    );
}

// ── CSV import: entry points and modal keys ──────────────────────────────────

/// Resolve the table an `I` import targets: the table open in the data browser
/// (only while the results pane is focused), else the sidebar's highlighted
/// table. The focus check matters — after browsing a table and returning to the
/// sidebar, the highlighted table may be a different one.
pub(crate) fn import_target_table(app: &App) -> Option<(String, String)> {
    if app.focus == Focus::Preview && app.grid_kind == GridKind::TableData {
        if let Some(ps) = &app.page_state {
            return Some((ps.schema.clone(), ps.table.clone()));
        }
    }
    app.selected_table()
        .map(|t| (app.schema.clone(), t.name.clone()))
}

/// `I`: open the CSV import flow for the current table.
pub(crate) fn open_import_prompt(app: &mut App) {
    if app.backend_kind != Backend::Sql {
        app.status = t("仅 SQL 连接支持 CSV 导入").into();
        return;
    }
    if readonly_conn_block(app) {
        return;
    }
    let Some((schema, table)) = import_target_table(app) else {
        app.status = t("先选中一张表再按 I 导入").into();
        return;
    };
    let db = app.current_db();
    let mut input = TextArea::default();
    input.set_placeholder_text(t("CSV 文件路径（支持 ~）"));
    app.import_prompt = Some(ImportPrompt {
        input,
        table,
        schema,
        db,
        error: None,
    });
    app.status = t("导入 CSV · 输入文件路径 · Enter 预览 · Esc 取消").into();
}

/// File-path prompt keys.
pub(crate) fn import_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut p) = app.import_prompt.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        // Invalidate any plan request still in flight so a slow read cannot pop
        // the preview open after the user cancelled.
        app.import_gen = app.import_gen.wrapping_add(1);
        app.flash(t("已取消导入").into());
        return;
    }
    if k.code != KeyCode::Enter {
        p.input.input(k);
        app.import_prompt = Some(p);
        return;
    }
    let path = p.input.lines().join("\n").trim().to_string();
    if path.is_empty() {
        p.error = Some(t("请输入文件路径").to_string());
        app.import_prompt = Some(p);
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        p.error = Some(t("✗ 未选择连接").to_string());
        app.import_prompt = Some(p);
        return;
    };
    p.error = None;
    let (table, schema, db) = (p.table.clone(), p.schema.clone(), p.db.clone());
    app.import_prompt = Some(p);
    app.import_gen = app.import_gen.wrapping_add(1);
    let gen = app.import_gen;
    app.loading = true;
    app.status = tf("解析 {}…", &[&path]);
    app.spawn(
        tx,
        Op::ImportPlan {
            cfg: Box::new(cfg),
            db,
            schema,
            table,
            path: PathBuf::from(path),
            gen,
        },
    );
}

/// Preview / confirmation layer keys.
pub(crate) fn import_plan_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut plan) = app.import_plan.take() else {
        return;
    };
    match k.code {
        KeyCode::Esc => {
            app.import_scroll = 0;
            app.flash(t("已取消导入").into());
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.import_scroll = app.import_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.import_scroll = app.import_scroll.saturating_add(1);
        }
        KeyCode::Char('m') => {
            plan.mode = match plan.mode {
                ImportMode::Append => ImportMode::Overwrite,
                ImportMode::Overwrite => ImportMode::Append,
            };
        }
        KeyCode::Char('s') => {
            plan.on_error = match plan.on_error {
                ImportOnError::Stop => ImportOnError::Skip,
                ImportOnError::Skip => ImportOnError::Stop,
            };
        }
        KeyCode::Enter => {
            if let Some(err) = plan.error.clone() {
                app.status = format!("✗ {err}");
                app.import_plan = Some(plan);
                return;
            }
            if readonly_conn_block(app) {
                app.import_plan = Some(plan);
                return;
            }
            start_import(app, tx, &plan);
            return;
        }
        _ => {}
    }
    app.import_plan = Some(plan);
}

/// Turn the preview plan into a backend job and start it.
pub(crate) fn start_import(app: &mut App, tx: &Tx, plan: &ImportPlan) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let columns: Vec<ImportCol> = plan.present().into_iter().cloned().collect();
    let job = ImportJob {
        cfg: Box::new(cfg),
        db: plan.db.clone(),
        schema: plan.schema.clone(),
        table: plan.table.clone(),
        columns,
        rows: plan.rows.clone(),
        mode: plan.mode,
        on_error: plan.on_error,
    };
    let total = job.rows.len();
    app.import_progress = Some((0, total));
    app.import_scroll = 0;
    app.import_plan = None;
    app.loading = true;
    app.status = tf("导入 {} 行 → {}…", &[&total, &plan.table]);
    app.spawn(tx, Op::Import(Box::new(job)));
}

/// Completion overlay keys.
pub(crate) fn import_report_key(app: &mut App, k: KeyEvent) {
    if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
        app.import_report = None;
        app.flash(t("已关闭导入报告").into());
    }
}

// ── connection bundle import / export (Alt-E / Alt-I) ───────────────────────
//
// dbxt keeps its connections in DBX's shared store. This section moves that
// list in and out as a self-describing JSON bundle and migrates the two
// dominant third-party formats (DBeaver `data-sources.json`, Navicat `.ncx`
// XML). Every write goes through `LocalBackend`; no file is touched behind its
// back. Passwords are excluded by default on export and are never decrypted
// from the DBeaver / Navicat stores (their ciphers are not ours to break).

/// Destination offered by the `Alt-E` overlay.
pub(crate) const CONN_EXPORT_DEFAULT_PATH: &str = "~/dbxt-connections.json";
/// Schema version of the dbxt connection bundle.
pub(crate) const CONN_BUNDLE_VERSION: u32 = 1;
/// Suffix appended to a duplicate name under the “both” policy.
pub(crate) const CONN_IMPORT_SUFFIX: &str = "-imported";

/// Which tool produced the bundle being imported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnSource {
    Dbxt,
    DBeaver,
    Navicat,
}

impl ConnSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ConnSource::Dbxt => "dbxt",
            ConnSource::DBeaver => "DBeaver",
            ConnSource::Navicat => "Navicat",
        }
    }
}

/// Duplicate-name resolution for one imported connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DupPolicy {
    /// Leave the existing connection untouched.
    Skip,
    /// Replace it (remove-then-add, same id).
    Overwrite,
    /// Import alongside it as `name-imported`.
    Both,
}

impl DupPolicy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            DupPolicy::Skip => t("跳过"),
            DupPolicy::Overwrite => t("覆盖"),
            DupPolicy::Both => t("都要"),
        }
    }
    pub(crate) fn marker(self) -> &'static str {
        match self {
            DupPolicy::Skip => t("重·跳过"),
            DupPolicy::Overwrite => t("重·覆盖"),
            DupPolicy::Both => t("重·另存"),
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            DupPolicy::Skip => DupPolicy::Overwrite,
            DupPolicy::Overwrite => DupPolicy::Both,
            DupPolicy::Both => DupPolicy::Skip,
        }
    }
}

/// The SSH tunnel of a normalized imported connection.
#[derive(Clone, Default, Debug)]
pub(crate) struct ImportSsh {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) password: Option<String>,
    pub(crate) key_path: Option<String>,
    pub(crate) passphrase: Option<String>,
    pub(crate) use_agent: bool,
    pub(crate) agent_sock: Option<String>,
    pub(crate) auth_method: String,
}

/// A connection parsed from any supported format, before it becomes a
/// [`ConnectionConfig`]. `db_type` is `None` when the source driver did not map.
#[derive(Clone, Default, Debug)]
pub(crate) struct ImportConn {
    pub(crate) name: String,
    /// Raw driver / provider / connection type, for the skipped list.
    pub(crate) driver: String,
    pub(crate) db_type: Option<String>,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) user: String,
    pub(crate) password: Option<String>,
    pub(crate) database: Option<String>,
    pub(crate) ssl: bool,
    /// Read-only flag carried by a dbxt bundle (kernel `ConnectionConfig::read_only`).
    pub(crate) read_only: bool,
    pub(crate) color: Option<String>,
    pub(crate) ssh: Option<ImportSsh>,
    /// True for DBeaver / Navicat, whose passwords are encrypted upstream.
    pub(crate) needs_password: bool,
}

/// One selectable row of the import preview.
#[derive(Clone, Debug)]
pub(crate) struct ConnImportRow {
    pub(crate) conn: ImportConn,
    /// The name collides with an existing saved connection.
    pub(crate) dup: bool,
    pub(crate) policy: DupPolicy,
    pub(crate) selected: bool,
}

/// Which scope a pending overwrite confirmation applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnOverwriteScope {
    All,
    Row(usize),
}

/// The parsed, previewed import awaiting confirmation.
#[derive(Clone, Debug)]
pub(crate) struct ConnImportPlan {
    pub(crate) source: ConnSource,
    pub(crate) origin: String,
    pub(crate) rows: Vec<ConnImportRow>,
    /// Drivers that did not map to a dbxt engine: `driver · name`.
    pub(crate) skipped: Vec<String>,
    pub(crate) cursor: usize,
    /// The red overwrite confirmation, when one is pending.
    pub(crate) confirm: Option<ConnOverwriteScope>,
}

/// The `Alt-E` export overlay state.
pub(crate) struct ConnExport {
    pub(crate) path: TextArea<'static>,
    /// 0 = path, 1 = password toggle, 2 = export action.
    pub(crate) field: usize,
    pub(crate) editing: bool,
    pub(crate) include_passwords: bool,
    /// Red confirmation shown before enabling password export.
    pub(crate) confirm_pw: bool,
}

pub(crate) fn json_str(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

pub(crate) fn value_to_port(v: &serde_json::Value) -> Option<u16> {
    match v {
        serde_json::Value::Number(n) => n.as_u64().and_then(|x| u16::try_from(x).ok()),
        serde_json::Value::String(s) => s.trim().parse::<u16>().ok(),
        _ => None,
    }
}

/// Truthy for bools and the string spellings DBeaver / Navicat use.
pub(crate) fn value_truthy(v: Option<&serde_json::Value>) -> bool {
    match v {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(serde_json::Value::String(s)) => {
            let s = s.trim().to_ascii_lowercase();
            matches!(
                s.as_str(),
                "true" | "1" | "yes" | "on" | "require" | "required" | "verify-ca" | "verify-full"
            )
        }
        _ => false,
    }
}

/// First non-empty string among `names` in a JSON object.
pub(crate) fn cfg_str(
    m: &serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) -> Option<String> {
    names.iter().find_map(|n| json_str(m.get(*n)))
}

/// `jdbc:mysql://host:3306/shop?x=1` → `shop`. A URL without a `host:port/db`
/// shape (e.g. `jdbc:duckdb:/tmp/x.duckdb`) yields `None` rather than a bogus
/// path fragment.
pub(crate) fn database_from_jdbc_url(url: &str) -> Option<String> {
    let after = url.split_once("://")?.1;
    let path = after.split('/').nth(1)?;
    let path = path.split(['?', ';']).next().unwrap_or(path);
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

pub(crate) fn normalize_driver_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// Map a DBeaver provider / Navicat connection type to a dbxt engine name.
/// Canonical dbxt names pass straight through; the alias table covers the
/// foreign spellings (`postgresql`, `mariadb`, `mysql8`, …).
pub(crate) fn map_driver_to_db_type(raw: &str) -> Option<&'static str> {
    let key = normalize_driver_key(raw);
    if key.is_empty() {
        return None;
    }
    if let Ok(dt) = parse_database_type(&key) {
        return Some(dt.as_str());
    }
    let trimmed = key.trim_end_matches(|c: char| c.is_ascii_digit());
    let trimmed = trimmed.trim_end_matches("jdbc").trim_end_matches('-');
    map_driver_alias(&key).or_else(|| map_driver_alias(trimmed))
}

pub(crate) fn map_driver_alias(key: &str) -> Option<&'static str> {
    Some(match key {
        "mysql" | "mariadb" | "mysql5" | "mysql8" | "mysqlconnector" => "mysql",
        "postgresql" | "pgsql" | "pg" | "postgresql9" | "postgresql10" | "postgresql11"
        | "postgresql12" | "postgresql13" | "postgresql14" | "postgresql15" | "postgresql16" => {
            "postgres"
        }
        "sqlite3" => "sqlite",
        "mssql" | "jtds" | "sqlserver2005" | "sqlserver2008" | "sqlserver2012"
        | "sqlserver2014" | "sqlserver2016" | "sqlserver2017" | "sqlserver2019"
        | "sqlserver2022" | "microsoftsqlserver" => "sqlserver",
        "oracleoci" | "oraclethin" => "oracle",
        "presto" | "prestodb" | "prestosql" => "prestosql",
        "hive2" | "hivejdbc" | "hiveserver2" => "hive",
        "dm" | "dm8" | "dameng8" => "dameng",
        "kingbasees" | "kingbase8" | "kingbasev8" => "kingbase",
        "manticore" => "manticoresearch",
        "ucanaccess" | "msaccess" => "access",
        "oceanbase" => "oceanbase-oracle",
        "sparksql" => "spark",
        _ => return None,
    })
}

pub(crate) fn ssh_export_value(
    ssh: &SshTunnelConfig,
    include_passwords: bool,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("host".into(), serde_json::json!(ssh.host));
    m.insert("port".into(), serde_json::json!(ssh.port));
    m.insert("user".into(), serde_json::json!(ssh.user));
    if !ssh.auth_method.is_empty() {
        m.insert("auth_method".into(), serde_json::json!(ssh.auth_method));
    }
    if ssh.use_ssh_agent {
        m.insert("use_agent".into(), serde_json::json!(true));
    }
    if !ssh.ssh_agent_sock_path.is_empty() {
        m.insert(
            "agent_sock".into(),
            serde_json::json!(ssh.ssh_agent_sock_path),
        );
    }
    if !ssh.key_path.is_empty() {
        m.insert("key_path".into(), serde_json::json!(ssh.key_path));
    }
    if include_passwords {
        if !ssh.password.is_empty() {
            m.insert("password".into(), serde_json::json!(ssh.password));
        }
        if !ssh.key_passphrase.is_empty() {
            m.insert(
                "key_passphrase".into(),
                serde_json::json!(ssh.key_passphrase),
            );
        }
    }
    serde_json::Value::Object(m)
}

pub(crate) fn connection_export_value(
    cfg: &ConnectionConfig,
    include_passwords: bool,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), serde_json::json!(cfg.name));
    m.insert("db_type".into(), serde_json::json!(cfg.db_type.as_str()));
    m.insert("host".into(), serde_json::json!(cfg.host));
    m.insert("port".into(), serde_json::json!(cfg.port));
    m.insert("user".into(), serde_json::json!(cfg.username));
    if include_passwords {
        m.insert("password".into(), serde_json::json!(cfg.password));
    }
    if let Some(db) = &cfg.database {
        m.insert("database".into(), serde_json::json!(db));
    }
    m.insert("ssl".into(), serde_json::json!(cfg.ssl));
    if cfg.read_only {
        m.insert("read_only".into(), serde_json::json!(true));
    }
    if let Some(color) = &cfg.color {
        if !color.is_empty() {
            m.insert("color".into(), serde_json::json!(color));
        }
    }
    if let Some(ssh) = first_ssh_layer(cfg) {
        m.insert("ssh".into(), ssh_export_value(ssh, include_passwords));
    }
    serde_json::Value::Object(m)
}

/// Serialize the whole connection list into the self-describing dbxt bundle.
pub(crate) fn conn_bundle_json(conns: &[ConnectionConfig], include_passwords: bool) -> String {
    let value = serde_json::json!({
        "format": "dbxt-connections",
        "version": CONN_BUNDLE_VERSION,
        "generated_by": "dbxt",
        "exported_at": now_iso8601(),
        "connection_count": conns.len(),
        "connections": conns
            .iter()
            .map(|c| connection_export_value(c, include_passwords))
            .collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

pub(crate) fn parse_ssh_value(m: &serde_json::Map<String, serde_json::Value>) -> ImportSsh {
    let mut ssh = ImportSsh::default();
    ssh.host = cfg_str(m, &["host"]).unwrap_or_default();
    ssh.port = m.get("port").and_then(value_to_port).unwrap_or(22);
    ssh.user = cfg_str(m, &["user", "username"]).unwrap_or_default();
    ssh.password = json_str(m.get("password"));
    ssh.key_path = cfg_str(m, &["key_path", "keyPath"]);
    ssh.passphrase = cfg_str(m, &["key_passphrase", "keyPassphrase"]);
    ssh.use_agent = value_truthy(m.get("use_agent"));
    ssh.agent_sock = cfg_str(m, &["agent_sock", "agentSock"]);
    ssh.auth_method = cfg_str(m, &["auth_method", "authMethod"]).unwrap_or_else(|| {
        if ssh.key_path.is_some() {
            "key".into()
        } else if ssh.use_agent {
            "agent".into()
        } else {
            "password".into()
        }
    });
    ssh
}

pub(crate) fn parse_dbxt_bundle(arr: &[serde_json::Value]) -> Result<Vec<ImportConn>, String> {
    let mut out = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let mut c = ImportConn::default();
        c.name = cfg_str(obj, &["name"]).unwrap_or_default();
        c.driver = cfg_str(obj, &["db_type", "dbType", "type"]).unwrap_or_default();
        c.db_type = map_driver_to_db_type(&c.driver).map(str::to_string);
        c.host = cfg_str(obj, &["host"]).unwrap_or_default();
        c.port = obj.get("port").and_then(value_to_port);
        c.user = cfg_str(obj, &["user", "username"]).unwrap_or_default();
        c.password = json_str(obj.get("password"));
        c.database = cfg_str(obj, &["database"]);
        c.ssl = value_truthy(obj.get("ssl"));
        c.read_only = value_truthy(obj.get("read_only"));
        c.color = cfg_str(obj, &["color"]);
        if let Some(ssh) = obj.get("ssh").and_then(|v| v.as_object()) {
            c.ssh = Some(parse_ssh_value(ssh));
        }
        if c.name.trim().is_empty() {
            c.name = c.host.clone();
        }
        out.push(c);
    }
    Ok(out)
}

pub(crate) fn parse_dbeaver_ssh(m: &serde_json::Map<String, serde_json::Value>) -> ImportSsh {
    let key_path = cfg_str(
        m,
        &["private-key-path", "privateKeyPath", "key-path", "keyPath"],
    );
    let auth = cfg_str(m, &["auth-type", "authType"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let use_agent = auth.contains("agent");
    let auth_method = if key_path.is_some() || auth.contains("key") {
        "key".to_string()
    } else if use_agent {
        "agent".to_string()
    } else {
        "password".to_string()
    };
    ImportSsh {
        host: cfg_str(m, &["host"]).unwrap_or_default(),
        port: m.get("port").and_then(value_to_port).unwrap_or(22),
        user: cfg_str(m, &["user", "username"]).unwrap_or_default(),
        key_path,
        use_agent,
        auth_method,
        ..ImportSsh::default()
    }
}

pub(crate) fn parse_dbeaver_json(v: &serde_json::Value) -> Result<Vec<ImportConn>, String> {
    let map = v
        .get("connections")
        .and_then(|c| c.as_object())
        .ok_or_else(|| t("不是 DBeaver data-sources.json（缺少 connections 对象）").to_string())?;
    let mut out = Vec::new();
    for (id, item) in map {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let cfg = obj.get("configuration").and_then(|c| c.as_object());
        let mut c = ImportConn::default();
        c.name = cfg_str(obj, &["name"]).unwrap_or_else(|| id.clone());
        c.driver = cfg_str(obj, &["provider", "driver"])
            .or_else(|| cfg.and_then(|c| cfg_str(c, &["provider", "driver"])))
            .unwrap_or_default();
        c.db_type = map_driver_to_db_type(&c.driver).map(str::to_string);
        if let Some(cfg) = cfg {
            c.host =
                cfg_str(cfg, &["host", "serverName", "hostName", "server"]).unwrap_or_default();
            c.port = cfg.get("port").and_then(value_to_port);
            c.user = cfg_str(cfg, &["user", "username"]).unwrap_or_default();
            c.database = cfg_str(cfg, &["database", "databaseName", "db"]);
            if c.database.is_none() {
                if let Some(url) = cfg_str(cfg, &["url", "jdbcUrl"]) {
                    c.database = database_from_jdbc_url(&url);
                }
            }
            let ssl_mode = cfg_str(cfg, &["sslMode", "ssl_mode"])
                .map(|s| {
                    matches!(
                        s.to_ascii_lowercase().as_str(),
                        "require" | "required" | "verify-ca" | "verify-full"
                    )
                })
                .unwrap_or(false);
            c.ssl = value_truthy(cfg.get("ssl")) || ssl_mode;
        }
        if c.user.is_empty() {
            c.user = cfg_str(obj, &["user", "username"]).unwrap_or_default();
        }
        // `ssh-tunnel` is a sibling of `configuration`; older files nest it.
        let ssh = obj
            .get("ssh-tunnel")
            .or_else(|| obj.get("ssh_tunnel"))
            .or_else(|| cfg.and_then(|c| c.get("ssh-tunnel")))
            .or_else(|| cfg.and_then(|c| c.get("ssh_tunnel")));
        if let Some(s) = ssh.and_then(|v| v.as_object()) {
            let parsed = parse_dbeaver_ssh(s);
            if !parsed.host.is_empty() {
                c.ssh = Some(parsed);
            }
        }
        // The password lives AES-encrypted in credentials-config.json; never read it.
        c.needs_password = true;
        if c.name.trim().is_empty() {
            c.name = id.clone();
        }
        out.push(c);
    }
    Ok(out)
}

/// Navicat `.ncx` connections, parsed with a tolerant hand-written scanner
/// (no extra dependency). Both child elements and attributes are accepted, and
/// tag names are matched case-insensitively.
pub(crate) fn parse_navicat_xml(text: &str) -> Vec<ImportConn> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0usize;
    let needle = "<connection";
    while let Some(rel) = lower[pos..].find(needle) {
        let start = pos + rel;
        let after = lower.as_bytes().get(start + needle.len()).copied();
        // Skip `<connections>` (the root element).
        if !matches!(
            after,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            pos = start + needle.len();
            continue;
        }
        let block_end; // exclusive end of this connection's text
        if let Some(end_rel) = lower[start..].find("</connection>") {
            block_end = start + end_rel + "</connection>".len();
        } else if let Some(gt) = text[start..].find('>') {
            block_end = start + gt + 1;
        } else {
            break;
        }
        let block = &text[start..block_end];
        pos = block_end;
        let conn = parse_navicat_conn(block);
        if conn.name.trim().is_empty() && conn.host.trim().is_empty() {
            continue;
        }
        out.push(conn);
    }
    out
}

pub(crate) fn parse_navicat_conn(block: &str) -> ImportConn {
    let mut c = ImportConn::default();
    c.name = xml_field(
        block,
        &["name", "connectionname", "connection_name", "connname"],
    )
    .unwrap_or_default();
    c.driver = xml_field(
        block,
        &["conntype", "conn_type", "type", "servertype", "dbtype"],
    )
    .unwrap_or_default();
    c.db_type = map_driver_to_db_type(&c.driver).map(str::to_string);
    c.host = xml_field(block, &["host", "hostname", "server", "address"]).unwrap_or_default();
    c.port = xml_field(block, &["port"]).and_then(|p| p.trim().parse::<u16>().ok());
    c.user = xml_field(block, &["username", "user", "uid"]).unwrap_or_default();
    c.database = xml_field(block, &["database", "databasename", "db", "initialcatalog"]);
    c.ssl = xml_field(block, &["usessl", "ssl", "sslmode", "sslenabled"])
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(
                v.as_str(),
                "true" | "1" | "yes" | "on" | "require" | "required"
            ) || v.contains("verify")
        })
        .unwrap_or(false);
    c.color = xml_field(block, &["color"]);
    let ssh_on = xml_field(block, &["ssh_enabled", "sshenabled", "usessh", "sshtunnel"])
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "true" | "1" | "yes" | "on")
        })
        .unwrap_or(false);
    let ssh_host = xml_field(block, &["ssh_host", "sshhost"]);
    if ssh_on || ssh_host.is_some() {
        let mut ssh = ImportSsh::default();
        ssh.host = ssh_host.unwrap_or_default();
        ssh.port = xml_field(block, &["ssh_port", "sshport"])
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or(22);
        ssh.user = xml_field(block, &["ssh_user", "ssh_username", "sshuser"]).unwrap_or_default();
        ssh.key_path = xml_field(
            block,
            &[
                "ssh_keypath",
                "ssh_key_path",
                "ssh_privatekeypath",
                "sshprivatekeypath",
            ],
        );
        ssh.use_agent = xml_field(block, &["ssh_useagent", "sshagent", "ssh_use_agent"])
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        ssh.auth_method = if ssh.key_path.is_some() {
            "key".into()
        } else if ssh.use_agent {
            "agent".into()
        } else {
            "password".into()
        };
        c.ssh = Some(ssh);
    }
    // The Navicat password (and SSH password) is encrypted; never read it.
    c.needs_password = true;
    c
}

/// Element text (`<name>value</name>`) or an attribute (`name="value"`), case-insensitive.
pub(crate) fn xml_field(block: &str, names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(v) = xml_element(block, name) {
            return Some(v);
        }
        if let Some(v) = xml_attr(block, name) {
            return Some(v);
        }
    }
    None
}

pub(crate) fn xml_element(block: &str, name: &str) -> Option<String> {
    let lower = block.to_ascii_lowercase();
    let needle = format!("<{}", name.to_ascii_lowercase());
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(&needle) {
        let start = from + rel;
        let after = lower.as_bytes().get(start + needle.len()).copied();
        if !matches!(
            after,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            from = start + needle.len();
            continue;
        }
        let gt = lower[start..].find('>').map(|g| start + g)?;
        if lower.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
            return None;
        }
        let close = format!("</{}>", name.to_ascii_lowercase());
        let end = lower[gt..].find(&close).map(|e| gt + e)?;
        return Some(unescape_xml(block[gt + 1..end].trim()));
    }
    None
}

pub(crate) fn xml_attr(block: &str, name: &str) -> Option<String> {
    let lower = block.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let needle = name.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(&needle) {
        let start = from + rel;
        let prev = if start == 0 {
            None
        } else {
            bytes.get(start - 1).copied()
        };
        if !matches!(
            prev,
            None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'>')
        ) {
            from = start + needle.len();
            continue;
        }
        let mut j = start + needle.len();
        while matches!(bytes.get(j), Some(b' ') | Some(b'\t')) {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            from = start + needle.len();
            continue;
        }
        j += 1;
        while matches!(bytes.get(j), Some(b' ') | Some(b'\t')) {
            j += 1;
        }
        let quote = match bytes.get(j) {
            Some(q @ (b'"' | b'\'')) => *q,
            _ => {
                from = start + needle.len();
                continue;
            }
        };
        let vstart = j + 1;
        let mut k = vstart;
        while k < bytes.len() && bytes[k] != quote {
            k += 1;
        }
        return Some(unescape_xml(block[vstart..k].trim()));
    }
    None
}

pub(crate) fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Auto-detect and parse a connection file: the dbxt bundle, a DBeaver
/// `data-sources.json`, or a Navicat `.ncx` XML.
pub(crate) fn sniff_connections(text: &str) -> Result<(ConnSource, Vec<ImportConn>), String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(t("文件为空").into());
    }
    if trimmed.starts_with('<') {
        let conns = parse_navicat_xml(trimmed);
        if conns.is_empty() {
            return Err(t("未找到 Navicat 连接节点（<Connection>）").into());
        }
        return Ok((ConnSource::Navicat, conns));
    }
    let v: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| tf("JSON 解析失败: {}", &[&e]))?;
    if let Some(arr) = v.get("connections").and_then(|c| c.as_array()) {
        return Ok((ConnSource::Dbxt, parse_dbxt_bundle(arr)?));
    }
    if v.get("connections").and_then(|c| c.as_object()).is_some() {
        return Ok((ConnSource::DBeaver, parse_dbeaver_json(&v)?));
    }
    Err(t("无法识别的连接文件（dbxt / DBeaver / Navicat）").into())
}

pub(crate) fn build_conn_import_plan(
    source: ConnSource,
    origin: String,
    conns: Vec<ImportConn>,
    existing: &[ConnectionConfig],
) -> ConnImportPlan {
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for c in conns {
        if c.db_type.is_none() {
            let driver = if c.driver.trim().is_empty() {
                t("未知驱动").to_string()
            } else {
                c.driver.clone()
            };
            skipped.push(format!("{driver} · {}", c.name));
            continue;
        }
        let dup = !c.name.trim().is_empty() && existing.iter().any(|e| e.name == c.name);
        rows.push(ConnImportRow {
            conn: c,
            dup,
            policy: DupPolicy::Skip,
            selected: true,
        });
    }
    ConnImportPlan {
        source,
        origin,
        rows,
        skipped,
        cursor: 0,
        confirm: None,
    }
}

pub(crate) fn unique_import_name(base: &str, used: &[String]) -> String {
    let base = if base.trim().is_empty() {
        "imported"
    } else {
        base.trim()
    };
    let mut candidate = format!("{base}{CONN_IMPORT_SUFFIX}");
    let mut n = 2usize;
    while used.iter().any(|u| u == &candidate) {
        candidate = format!("{base}{CONN_IMPORT_SUFFIX}{n}");
        n += 1;
    }
    candidate
}

pub(crate) fn import_ssh_to_layer(conn_name: &str, s: &ImportSsh) -> SshTunnelConfig {
    SshTunnelConfig {
        id: Uuid::new_v4().to_string(),
        name: format!("{conn_name} · SSH"),
        enabled: true,
        host: s.host.clone(),
        port: if s.port == 0 { 22 } else { s.port },
        user: s.user.clone(),
        password: s.password.clone().unwrap_or_default(),
        key_path: s.key_path.clone().unwrap_or_default(),
        key_passphrase: s.passphrase.clone().unwrap_or_default(),
        connect_timeout_secs: dbx_core::models::connection::default_ssh_connect_timeout_secs(),
        expose_lan: false,
        use_ssh_agent: s.use_agent,
        ssh_agent_sock_path: s.agent_sock.clone().unwrap_or_default(),
        auth_method: s.auth_method.clone(),
        allow_exec_channel_proxy: false,
        profile_id: String::new(),
    }
}

pub(crate) fn import_conn_to_config(
    c: &ImportConn,
    name: String,
    id: String,
) -> Result<ConnectionConfig, String> {
    let raw = c
        .db_type
        .clone()
        .ok_or_else(|| tf("不支持的驱动: {}", &[&c.driver]))?;
    let dt = parse_database_type(&raw)?;
    let port = c
        .port
        .or_else(|| dbx_core::database_manifest::default_port(&dt))
        .unwrap_or(0);
    let mut cfg = new_connection_config(
        id,
        name.clone(),
        dt,
        c.host.clone(),
        port,
        c.user.clone(),
        c.password.clone().unwrap_or_default(),
        c.database.clone(),
        c.ssl,
        None,
    )?;
    if let Some(color) = &c.color {
        if !color.is_empty() {
            cfg.color = Some(color.clone());
        }
    }
    cfg.read_only = c.read_only;
    if let Some(ssh) = &c.ssh {
        cfg.transport_layers = vec![TransportLayerConfig::Ssh(import_ssh_to_layer(&name, ssh))];
    }
    Ok(cfg)
}

/// The configs (and duplicate skips) an import will produce. Split out so the
/// duplicate policy is unit-testable without a backend.
pub(crate) struct ImportTargets {
    pub(crate) items: Vec<(Option<String>, ConnectionConfig)>,
    pub(crate) skipped: usize,
    pub(crate) needs_password: usize,
    pub(crate) errors: Vec<String>,
}

pub(crate) fn resolve_import_targets(
    rows: &[ConnImportRow],
    existing: &[ConnectionConfig],
) -> ImportTargets {
    let mut items = Vec::new();
    let mut skipped = 0usize;
    let mut needs_password = 0usize;
    let mut errors = Vec::new();
    let mut used: Vec<String> = existing.iter().map(|c| c.name.clone()).collect();
    for row in rows {
        if !row.selected {
            continue;
        }
        let existing_match = existing.iter().find(|c| c.name == row.conn.name);
        let (name, replace_id) = match (row.dup, row.policy) {
            (true, DupPolicy::Skip) => {
                skipped += 1;
                continue;
            }
            (true, DupPolicy::Overwrite) => match existing_match {
                Some(e) => (e.name.clone(), Some(e.id.clone())),
                None => (row.conn.name.clone(), None),
            },
            (true, DupPolicy::Both) => (unique_import_name(&row.conn.name, &used), None),
            (false, _) => (row.conn.name.clone(), None),
        };
        if row.conn.needs_password {
            needs_password += 1;
        }
        let id = replace_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        match import_conn_to_config(&row.conn, name.clone(), id) {
            Ok(cfg) => {
                used.push(name);
                items.push((replace_id, cfg));
            }
            Err(e) => errors.push(e),
        }
    }
    ImportTargets {
        items,
        skipped,
        needs_password,
        errors,
    }
}

pub(crate) fn conn_import_status(
    added: usize,
    skipped: usize,
    needs_password: usize,
    failed: &[String],
) -> String {
    if !failed.is_empty() {
        return format!("✗ {}", tf("导入失败: {}", &[&failed.join("; ")]));
    }
    let mut parts = vec![tf("导入 {} 条", &[&added])];
    if skipped > 0 {
        parts.push(tf("跳过重复 {}", &[&skipped]));
    }
    if needs_password > 0 {
        parts.push(tf("{} 条需补密码", &[&needs_password]));
    }
    format!("✓ {}", parts.join(" · "))
}

/// `Alt-E`: open the export overlay for the whole connection list.
pub(crate) fn open_conn_export(app: &mut App) {
    if app.connections.is_empty() {
        app.status = t("没有可导出的连接").into();
        return;
    }
    let mut path = TextArea::default();
    path.insert_str(CONN_EXPORT_DEFAULT_PATH);
    app.conn_export = Some(Box::new(ConnExport {
        path,
        field: 0,
        editing: false,
        include_passwords: false,
        confirm_pw: false,
    }));
    app.status = tf(
        "导出 {} 条连接 · Enter 导出 · y 复制 · p 含密码 · Esc 取消",
        &[&app.connections.len()],
    );
}

/// `Alt-I`: open the connection-file path prompt.
pub(crate) fn open_conn_import(app: &mut App) {
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t(
        "连接文件路径（dbxt / DBeaver data-sources.json / Navicat .ncx，支持 ~）",
    ));
    app.conn_import_path = Some(ta);
    app.status = t("导入连接 · 输入文件路径 · Enter 预览 · Esc 取消").into();
}

pub(crate) fn export_conns_clipboard(app: &mut App, include_passwords: bool) {
    let json = conn_bundle_json(&app.connections, include_passwords);
    let n = app.connections.len();
    let bytes = json.len();
    match clipboard_copy(&json) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制 {} 条连接 JSON（{} 字节）· 兜底 {}",
                &[&n, &bytes, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制 {} 条连接 JSON（{} 字节）", &[&n, &bytes]),
    }
}

pub(crate) fn export_conns_file(app: &mut App, path_input: &str, include_passwords: bool) {
    let raw = path_input.trim();
    let path = if raw.is_empty() {
        expand_home(CONN_EXPORT_DEFAULT_PATH)
    } else {
        expand_home(raw)
    };
    let json = conn_bundle_json(&app.connections, include_passwords);
    let n = app.connections.len();
    match std::fs::write(&path, json.as_bytes()) {
        Ok(()) => {
            app.status = if include_passwords {
                tf(
                    "✓ 已导出 {} 条连接（含明文密码）→ {}",
                    &[&n, &(path.display())],
                )
            } else {
                tf("✓ 已导出 {} 条连接 → {}", &[&n, &(path.display())])
            };
        }
        Err(e) => app.status = tf("✗ 写入失败: {}", &[&e]),
    }
}

pub(crate) fn conn_export_key(app: &mut App, k: KeyEvent) {
    let Some(mut ex) = app.conn_export.take() else {
        return;
    };
    if ex.confirm_pw {
        match k.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                ex.include_passwords = true;
                ex.confirm_pw = false;
                app.status = t("⚠ 已开启含密码导出：明文密码将写入文件，请妥善保管").into();
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                ex.confirm_pw = false;
                app.flash(t("已取消含密码导出").into());
            }
            _ => {}
        }
        app.conn_export = Some(ex);
        return;
    }
    if ex.editing {
        match k.code {
            KeyCode::Enter | KeyCode::Esc => ex.editing = false,
            _ => {
                ex.path.input(k);
            }
        }
        app.conn_export = Some(ex);
        return;
    }
    let n = 3usize;
    let toggle_pw = |ex: &mut ConnExport, app: &mut App| {
        if ex.include_passwords {
            ex.include_passwords = false;
            app.status = t("已关闭含密码导出").into();
        } else {
            ex.confirm_pw = true;
            app.status = t("⚠ 含密码导出：明文密码将写入文件 · Enter 确认 / Esc 取消").into();
        }
    };
    match k.code {
        KeyCode::Esc => {
            app.flash(t("已取消导出").into());
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => ex.field = (ex.field + n - 1) % n,
        KeyCode::Down | KeyCode::Char('j') => ex.field = (ex.field + 1) % n,
        KeyCode::Char('e') | KeyCode::Char('E') => ex.editing = true,
        KeyCode::Char('p') | KeyCode::Char('P') => toggle_pw(&mut ex, app),
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            export_conns_clipboard(app, ex.include_passwords);
            return;
        }
        KeyCode::Char('i') | KeyCode::Char('I') => {
            open_conn_import(app);
            return;
        }
        KeyCode::Enter => match ex.field {
            0 => ex.editing = true,
            1 => toggle_pw(&mut ex, app),
            _ => {
                let path_input = ex.path.lines().join("\n");
                export_conns_file(app, &path_input, ex.include_passwords);
                return;
            }
        },
        _ => {}
    }
    app.conn_export = Some(ex);
}

pub(crate) fn conn_import_path_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc => {
            app.conn_import_path = None;
            app.flash(t("已取消导入连接").into());
        }
        KeyCode::Enter => {
            let raw = app
                .conn_import_path
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            if raw.is_empty() {
                app.status = t("文件路径不能为空").into();
                return;
            }
            let path = expand_home(&raw);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    match sniff_connections(&text) {
                        Ok((source, conns)) => {
                            let plan = build_conn_import_plan(
                                source,
                                path.display().to_string(),
                                conns,
                                &app.connections,
                            );
                            let rows = plan.rows.len();
                            let dups = plan.rows.iter().filter(|r| r.dup).count();
                            let skipped = plan.skipped.len();
                            app.conn_import_path = None;
                            app.conn_import_plan = Some(Box::new(plan));
                            app.status = tf(
                                "{} 格式 · {} 条待导入 · {} 条同名 · 跳过 {} 个未知驱动 · Enter 导入",
                                &[&source.label(), &rows, &dups, &skipped],
                            );
                        }
                        Err(e) => app.status = format!("✗ {e}"),
                    }
                }
                Err(e) => app.status = tf("✗ 无法读取文件: {}", &[&e]),
            }
        }
        _ => {
            if let Some(ta) = app.conn_import_path.as_mut() {
                ta.input(k);
            }
        }
    }
}

pub(crate) fn conn_import_plan_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut plan) = app.conn_import_plan.take() else {
        return;
    };
    // The red overwrite layer owns the keyboard while it is up.
    if let Some(scope) = plan.confirm {
        match k.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                match scope {
                    ConnOverwriteScope::All => {
                        for row in plan.rows.iter_mut() {
                            if row.dup {
                                row.policy = DupPolicy::Overwrite;
                            }
                        }
                    }
                    ConnOverwriteScope::Row(i) => {
                        if let Some(row) = plan.rows.get_mut(i) {
                            row.policy = DupPolicy::Overwrite;
                        }
                    }
                }
                plan.confirm = None;
                app.status = t("⚠ 覆盖同名连接：导入时先删除原有配置").into();
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                plan.confirm = None;
                app.flash(t("已取消覆盖").into());
            }
            _ => {}
        }
        app.conn_import_plan = Some(plan);
        return;
    }
    let rows = plan.rows.len();
    match k.code {
        KeyCode::Esc => {
            app.flash(t("已取消导入连接").into());
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => plan.cursor = plan.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if rows > 0 {
                plan.cursor = (plan.cursor + 1).min(rows - 1);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(row) = plan.rows.get_mut(plan.cursor) {
                row.selected = !row.selected;
            }
        }
        KeyCode::Char('s') => {
            for row in plan.rows.iter_mut() {
                if row.dup {
                    row.policy = DupPolicy::Skip;
                }
            }
            app.status = t("重复策略：跳过").into();
        }
        KeyCode::Char('b') => {
            for row in plan.rows.iter_mut() {
                if row.dup {
                    row.policy = DupPolicy::Both;
                }
            }
            app.status = tf("重复策略：都要（加后缀 {}）", &[&CONN_IMPORT_SUFFIX]);
        }
        KeyCode::Char('r') => {
            if plan.rows.iter().any(|r| r.dup) {
                plan.confirm = Some(ConnOverwriteScope::All);
                app.status = t("⚠ 覆盖同名连接：将先删除原有配置 · Enter 确认 / Esc 取消").into();
            } else {
                app.status = t("没有同名连接").into();
            }
        }
        KeyCode::Char('d') => {
            if let Some(row) = plan.rows.get(plan.cursor).cloned() {
                if row.dup {
                    let next = row.policy.next();
                    if next == DupPolicy::Overwrite {
                        plan.confirm = Some(ConnOverwriteScope::Row(plan.cursor));
                        app.status = tf(
                            "⚠ 覆盖同名连接 {} · Enter 确认 / Esc 取消",
                            &[&row.conn.name],
                        );
                    } else if let Some(target) = plan.rows.get_mut(plan.cursor) {
                        target.policy = next;
                        app.status = tf("{} · {}", &[&row.conn.name, &next.label()]);
                    }
                }
            }
        }
        KeyCode::Enter => {
            run_conn_import(app, tx, &plan);
            return;
        }
        _ => {}
    }
    app.conn_import_plan = Some(plan);
}

pub(crate) fn run_conn_import(app: &mut App, tx: &Tx, plan: &ConnImportPlan) {
    let targets = resolve_import_targets(&plan.rows, &app.connections);
    if !targets.errors.is_empty() {
        app.status = format!("✗ {}", targets.errors.join("; "));
        return;
    }
    if targets.items.is_empty() {
        app.conn_import_plan = None;
        app.status = if targets.skipped > 0 {
            tf("没有可导入的连接（跳过重复 {}）", &[&targets.skipped])
        } else {
            t("没有可导入的连接").into()
        };
        return;
    }
    let count = targets.items.len();
    let items: Vec<(Option<String>, Box<ConnectionConfig>)> = targets
        .items
        .into_iter()
        .map(|(id, cfg)| (id, Box::new(cfg)))
        .collect();
    let skipped = targets.skipped;
    let needs_password = targets.needs_password;
    app.conn_import_plan = None;
    app.loading = true;
    app.status = tf("导入 {} 条连接…", &[&count]);
    app.spawn(
        tx,
        Op::ImportConns {
            items,
            skipped,
            needs_password,
        },
    );
}

/// `Ctrl-O`: open the saved-SQL favourites overlay for the current connection.
pub(crate) fn open_snippets(app: &mut App, tx: &Tx) {
    app.snippet_needle.clear();
    app.snippet_filter = None;
    app.snippet_confirm = None;
    app.snippet_list.select(Some(0));
    open_snippets_impl(app, tx, false);
}

/// `Alt-P`: open the same overlay but paste the chosen snippet at the cursor.
pub(crate) fn open_snippets_at_cursor(app: &mut App, tx: &Tx) {
    app.snippet_needle.clear();
    app.snippet_filter = None;
    app.snippet_confirm = None;
    app.snippet_list.select(Some(0));
    open_snippets_impl(app, tx, true);
}

pub(crate) fn open_snippets_impl(app: &mut App, tx: &Tx, insert_at_cursor: bool) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("先选择连接").into();
        return;
    };
    app.snippet_insert = insert_at_cursor;
    app.loading = true;
    app.status = t("加载 SQL 片段…").into();
    app.spawn(tx, Op::Snippets(Box::new(cfg)));
}

/// Rebuild `snippet_view` from the `/` needle (case-insensitive substring on the
/// label or the SQL text), keeping the cursor on a valid row. The list *is* the
/// filter view, so an empty needle shows every favourite.
pub(crate) fn recompute_snippet_view(app: &mut App) {
    let needle = app.snippet_needle.trim().to_lowercase();
    app.snippet_view = app
        .snippets
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            needle.is_empty()
                || s.label.to_lowercase().contains(&needle)
                || s.sql.to_lowercase().contains(&needle)
        })
        .map(|(i, _)| i)
        .collect();
    let n = app.snippet_view.len();
    if n == 0 {
        app.snippet_list.select(None);
    } else {
        let sel = app.snippet_list.selected().unwrap_or(0).min(n - 1);
        app.snippet_list.select(Some(sel));
    }
}

/// The favourite under the panel cursor (indexes through the filter view).
pub(crate) fn snippet_selected(app: &App) -> Option<&SnippetRow> {
    let sel = app.snippet_list.selected()?;
    let idx = *app.snippet_view.get(sel)?;
    app.snippets.get(idx)
}

pub(crate) fn snippet_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The `/` filter and the `d` delete confirmation are modal layers on top of
    // the list.
    if app.snippet_filter.is_some() {
        snippet_filter_key(app, k);
        return;
    }
    if app.snippet_confirm.is_some() {
        snippet_confirm_key(app, tx, k);
        return;
    }
    let n = app.snippet_view.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.snippet_open = false;
            app.snippet_insert = false;
            app.snippet_needle.clear();
            app.flash(t("已关闭 SQL 收藏").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app
                .snippet_list
                .selected()
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0);
            if n > 0 {
                app.snippet_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app
                .snippet_list
                .selected()
                .map(|i| (i + 1).min(n.saturating_sub(1)))
                .unwrap_or(0);
            if n > 0 {
                app.snippet_list.select(Some(i));
            }
        }
        KeyCode::Char('r') if k.modifiers.is_empty() => {
            open_snippets_impl(app, tx, app.snippet_insert)
        }
        // `s`: save the editor's SQL as a new DBX favourite.
        KeyCode::Char('s') if k.modifiers.is_empty() => open_snippet_name(app),
        // `/`: filter the list by label / SQL text (as-you-type).
        KeyCode::Char('/') if k.modifiers.is_empty() => {
            let mut ta = TextArea::from([app.snippet_needle.clone()]);
            ta.move_cursor(CursorMove::End);
            app.snippet_filter = Some(ta);
            app.status = t("按名称 / SQL 内容过滤收藏 · Enter 保留 · Esc 清除").into();
        }
        // `d`: delete the focused favourite, behind a one-step confirmation.
        KeyCode::Char('d') | KeyCode::Delete => {
            let Some(row) = snippet_selected(app) else {
                app.status = t("没有可删除的收藏").into();
                return;
            };
            app.snippet_confirm = Some(row.id.clone());
            app.status = t("删除收藏确认 · Enter 执行 · Esc 取消").into();
        }
        KeyCode::Enter => {
            let Some(row) = snippet_selected(app).cloned() else {
                return;
            };
            let name = row.label.clone();
            let sql = row.sql.clone();
            if app.snippet_insert {
                // R41: drop the snippet in at the cursor (replacing the
                // selection when there is one) instead of appending.
                app.editor.insert_str(&sql);
                app.focus = Focus::Editor;
            } else {
                let existing = app.editor_sql();
                let merged = if existing.trim().is_empty() {
                    sql
                } else {
                    format!("{}\n{sql}", existing.trim_end())
                };
                app.set_editor_text(&merged);
                app.focus = Focus::Editor;
            }
            app.snippet_open = false;
            app.snippet_insert = false;
            app.snippet_needle.clear();
            app.status = tf("✓ 已插入 {}", &[&(name)]);
        }
        _ => {}
    }
}

/// The `/` filter input: as-you-type narrowing, Enter keeps the needle, Esc
/// clears it (same state machine as the history panel's filter).
pub(crate) fn snippet_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.snippet_filter = None;
            app.status = tf(
                "收藏过滤「{}」· 命中 {}",
                &[&(app.snippet_needle), &(app.snippet_view.len())],
            );
        }
        KeyCode::Esc => {
            app.snippet_filter = None;
            app.snippet_needle.clear();
            recompute_snippet_view(app);
            app.flash(t("已清除收藏过滤").into());
        }
        _ => {
            if let Some(ta) = app.snippet_filter.as_mut() {
                ta.input(k);
            }
            app.snippet_needle = app
                .snippet_filter
                .as_ref()
                .and_then(|ta| ta.lines().first().cloned())
                .unwrap_or_default();
            recompute_snippet_view(app);
            if !app.snippet_view.is_empty() {
                app.snippet_list.select(Some(0));
            }
        }
    }
}

/// The `d` delete confirmation layer.
pub(crate) fn snippet_confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(id) = app.snippet_confirm.take() else {
                return;
            };
            app.status = t("删除该条收藏…").into();
            app.spawn(tx, Op::SnippetDelete { id });
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.snippet_confirm = None;
            app.flash(t("已取消删除").into());
        }
        _ => {}
    }
}

/// Name prompt for "save the current SQL as a favourite".
pub(crate) fn open_snippet_name(app: &mut App) {
    let sql = app.editor_sql();
    if sql.trim().is_empty() {
        app.status = t("编辑器为空：先写 SQL 再收藏").into();
        return;
    }
    let mut ta = TextArea::from([query_tab_title(&sql)]);
    // Select the default so typing replaces it, but it stays visible/editable.
    ta.select_all();
    app.snippet_name = Some(ta);
}

pub(crate) fn snippet_name_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let mut name = app
                .snippet_name
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.snippet_name = None;
            if name.is_empty() {
                app.status = t("片段名称不能为空").into();
                return;
            }
            // DBX stores snippet names with a `.sql` suffix.
            if !name.to_ascii_lowercase().ends_with(".sql") {
                name.push_str(".sql");
            }
            let Some(cfg) = app.selected.clone() else {
                return;
            };
            let sql = app.editor_sql();
            app.status = tf("保存片段 {} …", &[&(name)]);
            app.spawn(tx, Op::SaveSnippet(Box::new(cfg), name, sql));
        }
        KeyCode::Esc => {
            app.snippet_name = None;
            app.flash(t("已取消收藏").into());
        }
        _ => {
            if let Some(t) = app.snippet_name.as_mut() {
                t.input(k);
            }
        }
    }
}

// ── R71: built-in SQL template panel (`Alt-T` in the editor) ────────────────

/// One built-in SQL template. Pure text with `{{…}}` placeholders: inserting it
/// is a buffer edit, never a query, so the panel works offline / on a read-only
/// connection. The list is read-only — user-owned SQL lives in the `Ctrl-O`
/// favourites, and the panel title says so.
pub(crate) struct SqlTemplate {
    /// Chinese label, run through [`t`] for the bilingual UI.
    pub(crate) label: &'static str,
    pub(crate) sql: &'static str,
}

/// The built-in templates, ordered from read to destructive. `{{table}}` /
/// `{{col}}` / `{{value}}` / `{{type}}` / `{{name}}` are the placeholders the
/// caret walks with Tab.
pub(crate) static TEMPLATES: &[SqlTemplate] = &[
    SqlTemplate {
        label: "SELECT 查询（WHERE）",
        sql: "SELECT {{col}}\nFROM {{table}}\nWHERE {{col}} = {{value}};",
    },
    SqlTemplate {
        label: "INSERT 插入",
        sql: "INSERT INTO {{table}} ({{col}})\nVALUES ({{value}});",
    },
    SqlTemplate {
        label: "UPDATE 更新（WHERE）",
        sql: "UPDATE {{table}}\nSET {{col}} = {{value}}\nWHERE {{col}} = {{value}};",
    },
    SqlTemplate {
        label: "DELETE 删除（WHERE）",
        sql: "DELETE FROM {{table}}\nWHERE {{col}} = {{value}};",
    },
    SqlTemplate {
        label: "CREATE INDEX 建索引",
        sql: "CREATE INDEX {{name}}\nON {{table}} ({{col}});",
    },
    SqlTemplate {
        label: "ALTER ADD COLUMN 加列",
        sql: "ALTER TABLE {{table}}\nADD COLUMN {{col}} {{type}};",
    },
    SqlTemplate {
        label: "TRUNCATE 清空表",
        sql: "TRUNCATE TABLE {{table}};",
    },
    SqlTemplate {
        label: "DROP TABLE 删表",
        sql: "DROP TABLE {{table}};",
    },
];

/// `Alt-T` in the editor: open the built-in template panel. No connection is
/// needed — the templates are pure text.
pub(crate) fn open_template_panel(app: &mut App) {
    app.template_needle.clear();
    app.template_filter = None;
    app.template_view = (0..TEMPLATES.len()).collect();
    app.template_list.select(Some(0));
    app.template_open = true;
    app.focus = Focus::Editor;
    app.status = t("SQL 模板 · 内置只读 · Enter 插入光标处 · / 过滤").into();
}

/// Rebuild `template_view` from the `/` needle (case-insensitive substring on
/// the label or the SQL text), keeping the cursor on a valid row.
pub(crate) fn recompute_template_view(app: &mut App) {
    let needle = app.template_needle.trim().to_lowercase();
    app.template_view = TEMPLATES
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            needle.is_empty()
                || t(s.label).to_lowercase().contains(&needle)
                || s.label.to_lowercase().contains(&needle)
                || s.sql.to_lowercase().contains(&needle)
        })
        .map(|(i, _)| i)
        .collect();
    let n = app.template_view.len();
    if n == 0 {
        app.template_list.select(None);
    } else {
        let sel = app.template_list.selected().unwrap_or(0).min(n - 1);
        app.template_list.select(Some(sel));
    }
}

/// The template under the panel cursor (indexes through the filter view).
pub(crate) fn template_selected(app: &App) -> Option<&'static SqlTemplate> {
    let sel = app.template_list.selected()?;
    let idx = *app.template_view.get(sel)?;
    TEMPLATES.get(idx)
}

pub(crate) fn template_key(app: &mut App, k: KeyEvent) {
    // The `/` filter is a modal layer on top of the list.
    if app.template_filter.is_some() {
        template_filter_key(app, k);
        return;
    }
    let n = app.template_view.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.template_open = false;
            app.template_needle.clear();
            app.flash(t("已关闭 SQL 模板").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app
                .template_list
                .selected()
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0);
            if n > 0 {
                app.template_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app
                .template_list
                .selected()
                .map(|i| (i + 1).min(n.saturating_sub(1)))
                .unwrap_or(0);
            if n > 0 {
                app.template_list.select(Some(i));
            }
        }
        // `/`: filter the list by label / SQL text (as-you-type).
        KeyCode::Char('/') if k.modifiers.is_empty() => {
            let mut ta = TextArea::from([app.template_needle.clone()]);
            ta.move_cursor(CursorMove::End);
            app.template_filter = Some(ta);
            app.status = t("按名称 / SQL 内容过滤模板 · Enter 保留 · Esc 清除").into();
        }
        KeyCode::Enter => {
            let Some(tpl) = template_selected(app) else {
                return;
            };
            let sql = tpl.sql;
            let label = t(tpl.label);
            // Pure text into the buffer: no connection, no query (zero-query
            // red line). The caret lands on the first `{{…}}` placeholder and
            // selects it, so typing replaces it; Tab walks the rest.
            app.editor.insert_str(sql);
            app.focus = Focus::Editor;
            app.template_open = false;
            app.template_needle.clear();
            app.template_filter = None;
            app.template_active = true;
            let phs = editor_placeholders(app.editor.lines());
            if let Some(p) = phs.first().copied() {
                select_placeholder(app, p);
                app.status = tf("✓ 已插入模板「{}」· Tab 跳占位符", &[&(label)]);
            } else {
                app.template_active = false;
                app.status = tf("✓ 已插入模板「{}」", &[&(label)]);
            }
        }
        _ => {}
    }
}

/// The `/` filter input: as-you-type narrowing, Enter keeps the needle, Esc
/// clears it (same state machine as the favourites panel's filter).
pub(crate) fn template_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.template_filter = None;
            app.status = tf(
                "模板过滤「{}」· 命中 {}",
                &[&(app.template_needle), &(app.template_view.len())],
            );
        }
        KeyCode::Esc => {
            app.template_filter = None;
            app.template_needle.clear();
            recompute_template_view(app);
            app.flash(t("已清除模板过滤").into());
        }
        _ => {
            if let Some(ta) = app.template_filter.as_mut() {
                ta.input(k);
            }
            app.template_needle = app
                .template_filter
                .as_ref()
                .and_then(|ta| ta.lines().first().cloned())
                .unwrap_or_default();
            recompute_template_view(app);
            if !app.template_view.is_empty() {
                app.template_list.select(Some(0));
            }
        }
    }
}

/// `p` in the connection picker: pre-fill the form with a copy of a connection.
pub(crate) fn duplicate_connection(app: &mut App) {
    let Some(idx) = app.conn_list.selected() else {
        return;
    };
    let Some(cfg) = app.connections.get(idx).cloned() else {
        return;
    };
    app.form = form_from_connection(&cfg, tf("{} (副本)", &[&(cfg.name)]), None);
    app.page = Page::NewConn;
    app.status = tf("复制连接 {} · 改参数后 Enter 保存", &[&(cfg.name)]);
}

/// A fresh, unique `xxx-copy` name for a duplicated connection (R45):
/// `prod` → `prod-copy`, and `prod-copy-2`, `prod-copy-3` when taken.
pub(crate) fn copy_connection_name(base: &str, existing: &[String]) -> String {
    let first = format!("{base}-copy");
    if !existing.iter().any(|n| n == &first) {
        return first;
    }
    let mut i = 2u32;
    loop {
        let cand = format!("{base}-copy-{i}");
        if !existing.iter().any(|n| n == &cand) {
            return cand;
        }
        i += 1;
    }
}

/// `Y` in the sidebar tree: copy the connection under the cursor in one step
/// (no form), keeping its password and SSH tunnel, and show the new root as soon
/// as the save lands. Built for test / prod twins that differ only by host (R45).
pub(crate) fn copy_connection_at_cursor(app: &mut App, tx: &Tx) {
    let Some(SideRow::Conn { idx, .. }) = app.side_rows.get(app.side_sel).cloned() else {
        app.status = t("把光标移到连接行上再按 Y 复制").into();
        return;
    };
    let Some(cfg) = side_root_cfg(app, idx).cloned() else {
        return;
    };
    let existing: Vec<String> = app.connections.iter().map(|c| c.name.clone()).collect();
    let name = copy_connection_name(&cfg.name, &existing);
    let original = cfg.name.clone();
    let mut copy = cfg;
    copy.id = Uuid::new_v4().to_string();
    copy.name = name.clone();
    app.status = tf("复制连接 {} → {}…", &[&original, &name]);
    app.spawn(tx, Op::CopyConn(Box::new(copy)));
}

/// `e` in the connection picker: open the selected connection in the form
/// (including its SSH tunnel section) and update it in place on save.
pub(crate) fn edit_connection(app: &mut App) {
    let Some(idx) = app.conn_list.selected() else {
        return;
    };
    let Some(cfg) = app.connections.get(idx).cloned() else {
        return;
    };
    app.form = form_from_connection(&cfg, cfg.name.clone(), Some(cfg.id.clone()));
    app.page = Page::NewConn;
    app.status = tf("编辑连接 {} · Enter 保存", &[&(cfg.name)]);
}
