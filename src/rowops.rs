use crate::prelude::*;
use crate::*;

/// 1-based absolute row number of the cursor across all pages. The result-row
/// search keeps a display→source map, so the reported number is the source row.
pub(crate) fn cursor_abs_row(app: &App) -> usize {
    let src = app.full_row_index().unwrap_or(app.sel);
    match &app.page_state {
        Some(ps) => abs_row(ps.page, ps.page_size, src),
        None => src + 1,
    }
}

/// Show the focused cell's full value in a modal (truncated cells stay readable).
pub(crate) fn open_cell_popup(app: &mut App) {
    let Some(grid) = active_grid(app) else {
        return;
    };
    let Some(row) = grid.rows.get(app.sel) else {
        return;
    };
    let Some(v) = row.get(app.col_cursor) else {
        return;
    };
    let col = grid
        .columns
        .get(app.col_cursor)
        .cloned()
        .unwrap_or_default();
    let col = fix_double_encoding(&col);
    let (text, style) = value_display(v);
    // The raw value (never the pretty form) is what `y`/`Y` copies.
    let raw = cell_copy_text(v);
    let pretty = pretty_json(&raw).map(|p| pretty_json_spans(&p));
    let show_pretty = pretty.is_some();
    let title = tf(
        "{} · 第 {} 行 · {} 字符",
        &[&col, &(cursor_abs_row(app)), &(text.chars().count())],
    );
    let mut lines = vec![PopupLine { text, style }];
    // R77: a whole-number cell that lands in the epoch range gets a gray,
    // read-only human-readable time line at the bottom of the popup. Preview
    // only — the value itself, and everything `y`/`Y` copies, is untouched.
    if let Some(secs) = epoch_secs_from_cell(v, grid.col_type(app.col_cursor)) {
        lines.push(PopupLine {
            text: epoch_display(secs, now_unix_secs()),
            style: Style::default().fg(Color::DarkGray),
        });
    }
    app.popup_cache = None;
    app.cell_popup = Some(CellPopup {
        title,
        lines,
        scroll: 0,
        col,
        raw,
        pretty,
        show_pretty,
    });
}

/// Open the focused row as a vertical `column = value` list. Uses the unfiltered
/// grid so a column hidden with Ctrl-Shift-H is still readable here. The title
/// carries the absolute row number plus the primary-key value(s) when the
/// browsed table's metadata identifies them (`第 12 行 · id=4821`).
pub(crate) fn open_row_popup(app: &mut App) {
    let Some(grid) = full_grid(app) else {
        return;
    };
    let idx = app.full_row_index().unwrap_or(app.sel);
    let Some(row) = grid.rows.get(idx) else {
        return;
    };
    let mut lines: Vec<PopupLine> = Vec::new();
    let mut cols: Vec<String> = Vec::new();
    let mut shown_vals: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    for (ci, col) in grid.columns.iter().enumerate() {
        let (shown, style) = match row.get(ci) {
            Some(Val::Null) | None => ("NULL".to_string(), null_style()),
            Some(Val::Text(s)) if s.is_empty() => ("''".to_string(), empty_string_style()),
            Some(Val::Text(s)) => (s.clone(), Style::default()),
        };
        cols.push(fix_double_encoding(col));
        // The narrow stacked layout draws the value on its own line.
        shown_vals.push(shown.clone());
        // The popup's `y` copies this same text, so both paths share one mapping.
        values.push(cell_copy_text(row.get(ci).unwrap_or(&Val::Null)));
        lines.push(PopupLine {
            text: format!("{} = {}", fix_double_encoding(col), shown),
            style,
        });
    }
    let abs = cursor_abs_row(app);
    let title = match row_pk_locator(app, &grid, row) {
        Some(pk) => tf("第 {} 行 · {}", &[&abs, &pk]),
        None => tf("第 {} 行 · {} 列", &[&abs, &(grid.columns.len())]),
    };
    // R86: precompute the pretty JSON body for every field that holds an object
    // or array, so the in-place `J` expansion never re-parses while scrolling.
    let pretty: Vec<Option<Vec<Vec<PopupSpan>>>> = values
        .iter()
        .map(|v| pretty_json(v).map(|p| pretty_json_spans(&p)))
        .collect();
    let expanded = vec![false; values.len()];
    app.popup_cache = None;
    app.row_popup = Some(RowPopup {
        title,
        lines,
        cols,
        shown: shown_vals,
        values,
        pretty,
        expanded,
        row_abs: abs,
        scroll: 0,
        cursor: 0,
        filter: String::new(),
        filtering: false,
        count: String::new(),
    });
}

/// `id=4821` / `id=4821, tenant=7` for the focused row, when the grid is a
/// browsed table (its metadata identifies the primary key) or a MongoDB
/// document grid (whose alignment key is `_id`). A query result has no table
/// metadata, so it falls back to the plain row number.
pub(crate) fn row_pk_locator(app: &App, grid: &Grid, row: &[Val]) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    match app.grid_kind {
        GridKind::TableData => {
            if let Some(meta) = &app.table_meta {
                let same = app
                    .page_state
                    .as_ref()
                    .is_some_and(|p| p.table == meta.table && p.schema == meta.schema);
                if same {
                    for c in &meta.columns {
                        if c.is_primary_key {
                            names.push(c.name.clone());
                        }
                    }
                }
            }
        }
        GridKind::MongoDocs if grid.columns.iter().any(|c| c == "_id") => {
            names.push("_id".to_string());
        }
        _ => {}
    }
    if names.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for name in &names {
        if let Some(ci) = grid
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
        {
            let shown = row
                .get(ci)
                .map(|v| value_display(v).0)
                .unwrap_or_else(|| "NULL".to_string());
            parts.push(format!("{}={}", fix_double_encoding(name), shown));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Indices of the row-popup lines whose column name *or* displayed value matches
/// the active filter (case-insensitive substring), so a wide row can be narrowed
/// by either the field name or the value it holds. An empty filter keeps every
/// line.
pub(crate) fn row_popup_visible(popup: &RowPopup) -> Vec<usize> {
    if popup.filter.is_empty() {
        return (0..popup.lines.len()).collect();
    }
    let needle = popup.filter.to_lowercase();
    popup
        .cols
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            c.to_lowercase().contains(&needle)
                || popup
                    .shown
                    .get(*i)
                    .is_some_and(|v| v.to_lowercase().contains(&needle))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The selected entry index (into `lines`) of the row popup, if any.
pub(crate) fn row_popup_selected(popup: &RowPopup) -> Option<usize> {
    let visible = row_popup_visible(popup);
    if visible.is_empty() {
        return None;
    }
    let cur = popup.cursor.min(visible.len() - 1);
    Some(visible[cur])
}

/// `Enter` / `v` inside the row popup: open the selected column's full value as
/// the cell popup on top. The row popup stays open underneath, so `Esc` in the
/// cell popup returns here before closing the row.
pub(crate) fn drill_row_popup_cell(app: &mut App) {
    let Some(popup) = app.row_popup.as_ref() else {
        return;
    };
    let Some(ei) = row_popup_selected(popup) else {
        return;
    };
    let col = popup.cols.get(ei).cloned().unwrap_or_default();
    let text = popup.values.get(ei).cloned().unwrap_or_default();
    let style = popup.lines.get(ei).map(|l| l.style).unwrap_or_default();
    let abs = popup.row_abs;
    let title = tf(
        "{} · 第 {} 行 · {} 字符",
        &[&col, &abs, &(text.chars().count())],
    );
    let pretty = pretty_json(&text).map(|p| pretty_json_spans(&p));
    let show_pretty = pretty.is_some();
    app.popup_cache = None;
    app.cell_popup = Some(CellPopup {
        title,
        lines: vec![PopupLine {
            text: text.clone(),
            style,
        }],
        scroll: 0,
        col,
        raw: text,
        pretty,
        show_pretty,
    });
}

/// `y` / `Y` inside the row popup: copy the selected value, naming the column in
/// the status so a wide table's many columns stay unambiguous. Shares the grid's
/// copy path ([`copy_named_value`]) so both read the same.
pub(crate) fn copy_row_popup_value(app: &mut App) {
    let Some(popup) = app.row_popup.as_ref() else {
        return;
    };
    let Some(ei) = row_popup_selected(popup) else {
        app.status = t("没有可复制的列").into();
        return;
    };
    let col = popup.cols.get(ei).cloned().unwrap_or_default();
    let text = popup.values.get(ei).cloned().unwrap_or_default();
    copy_named_value(app, &col, &text);
}

/// R86: `J` in the row popup expands / collapses the selected field's JSON
/// object/array in place (a `(J 美化)` marker flags the fields that qualify).
/// Pure client-side: it only changes how the already-loaded value is drawn, so
/// `y`/`Y` still copy the raw text.
pub(crate) fn toggle_row_popup_json(app: &mut App) {
    let ei = {
        let Some(popup) = app.row_popup.as_ref() else {
            return;
        };
        let visible = row_popup_visible(popup);
        let cursor = if visible.is_empty() {
            0
        } else {
            popup.cursor.min(visible.len() - 1)
        };
        let Some(&ei) = visible.get(cursor) else {
            app.status = t("没有可美化的字段").into();
            return;
        };
        if !popup.pretty.get(ei).is_some_and(|p| p.is_some()) {
            app.status = t("该字段不是 JSON 对象/数组，无法美化").into();
            return;
        }
        ei
    };
    let now = if let Some(popup) = app.row_popup.as_mut() {
        let now = !popup.expanded.get(ei).copied().unwrap_or(false);
        if let Some(e) = popup.expanded.get_mut(ei) {
            *e = now;
        }
        popup.count.clear();
        now
    } else {
        return;
    };
    app.status = if now {
        t("已就地展开 JSON 字段（再按 J 收起）").into()
    } else {
        t("已收起 JSON 字段").into()
    };
}

/// Keys for the row popup. `j`/`k` or `n`/`p` (with an optional count) move the
/// entry cursor, `/` filters by column name or value, `y`/`Y` copy the selected
/// value, `J` expands a JSON field in place, and `Enter` / `v` drill into the
/// full cell popup. `Esc` / `q` close the row.
pub(crate) fn row_popup_key(app: &mut App, k: KeyEvent) {
    // R86: `J` expands / collapses the selected field's JSON object/array in
    // place. Handled before the mutable borrow below (while a `/` filter is
    // being typed, `J` stays a literal character).
    let filtering = app.row_popup.as_ref().is_some_and(|p| p.filtering);
    if !filtering
        && !k
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && k.code == KeyCode::Char('J')
    {
        toggle_row_popup_json(app);
        return;
    }
    let Some(popup) = app.row_popup.as_mut() else {
        return;
    };
    // Typing the `/` column filter: printable characters extend it, Enter keeps
    // it (and hands control back to the cursor), Esc clears and closes the input.
    if popup.filtering {
        match k.code {
            KeyCode::Esc => {
                popup.filtering = false;
                popup.filter.clear();
                popup.cursor = 0;
                popup.scroll = 0;
            }
            KeyCode::Enter => popup.filtering = false,
            KeyCode::Backspace => {
                popup.filter.pop();
                popup.cursor = 0;
                popup.scroll = 0;
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                popup.filter.push(c);
                popup.cursor = 0;
                popup.scroll = 0;
            }
            _ => {}
        }
        return;
    }
    // A leading digit is a count prefix for the next motion (5j).
    if let KeyCode::Char(c @ '1'..='9') = k.code {
        if k.modifiers.is_empty() && popup.count.len() < 4 {
            popup.count.push(c);
            return;
        }
    }
    let count: usize = parse_count(&popup.count).map(|n| n as usize).unwrap_or(1);
    let visible = row_popup_visible(popup);
    let last = visible.len().saturating_sub(1);
    let cur = popup.cursor.min(last);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.row_popup = None;
            app.flash(t("已关闭行详情").into());
        }
        KeyCode::Enter | KeyCode::Char('v') => {
            popup.count.clear();
            drill_row_popup_cell(app);
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            popup.count.clear();
            copy_row_popup_value(app);
        }
        // R82: `c` in a MongoDB document's row popup prompts for a dotted path
        // and copies the extracted sub-value (client-side over the loaded page).
        KeyCode::Char('c') if app.grid_kind == GridKind::MongoDocs => {
            popup.count.clear();
            open_mongo_path_prompt(app);
        }
        // `?` from inside the popup opens the context mini help (which shows
        // this popup's own keys); Esc returns to the row.
        KeyCode::Char('?') => {
            popup.count.clear();
            open_help(app);
        }
        KeyCode::Char('/') => {
            popup.count.clear();
            popup.filtering = true;
            popup.filter.clear();
            popup.cursor = 0;
            popup.scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => {
            popup.cursor = (cur + count).min(last);
            popup.count.clear();
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') => {
            popup.cursor = cur.saturating_sub(count);
            popup.count.clear();
        }
        KeyCode::PageDown => {
            popup.cursor = (cur + 10 * count).min(last);
            popup.count.clear();
        }
        KeyCode::PageUp => {
            popup.cursor = cur.saturating_sub(10 * count);
            popup.count.clear();
        }
        KeyCode::Home => {
            popup.cursor = 0;
            popup.count.clear();
        }
        KeyCode::End => {
            popup.cursor = last;
            popup.count.clear();
        }
        _ => {}
    }
}

// ── edit / insert templates ──

/// Escape a value as a standard SQL string literal (quote doubled, backslash
/// escaped). Fine for MySQL's default mode and standard SQL alike.
pub(crate) fn sql_literal(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
}

pub(crate) fn is_numeric_type(t: &str) -> bool {
    let lower = t.trim().to_ascii_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("");
    matches!(
        base,
        "int"
            | "integer"
            | "bigint"
            | "smallint"
            | "tinyint"
            | "mediumint"
            | "int2"
            | "int4"
            | "int8"
            | "serial"
            | "bigserial"
            | "decimal"
            | "numeric"
            | "float"
            | "float4"
            | "float8"
            | "double"
            | "real"
            | "number"
            | "money"
            | "unsigned"
    )
}

/// Column type from the browsed table's metadata, when available. The schema is
/// matched too, so a `public.orders` metadata set is never applied to
/// `inv.orders`.
pub(crate) fn column_type(app: &App, schema: &str, table: &str, col: &str) -> Option<String> {
    let meta = app.table_meta.as_ref()?;
    if meta.table != table || meta.schema != schema {
        return None;
    }
    meta.columns
        .iter()
        .find(|c| c.name == col)
        .map(|c| c.data_type.clone())
}

/// Render a cell value as a SQL literal, keeping numeric columns unquoted when
/// the value really is a number.
pub(crate) fn val_literal(v: &Val, data_type: Option<&str>) -> String {
    match v {
        Val::Null => "NULL".to_string(),
        Val::Text(s) if s.is_empty() => "''".to_string(),
        Val::Text(s) => {
            if data_type.map(is_numeric_type).unwrap_or(false) && s.parse::<f64>().is_ok() {
                s.clone()
            } else if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") {
                s.to_ascii_uppercase()
            } else {
                sql_literal(s)
            }
        }
    }
}

/// True for column types whose values are raw bytes and should be copied as a
/// hex literal rather than a quoted string.
pub(crate) fn is_binary_type(t: &str) -> bool {
    let lower = t.trim().to_ascii_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("");
    matches!(
        base,
        "blob"
            | "tinyblob"
            | "mediumblob"
            | "longblob"
            | "binary"
            | "varbinary"
            | "bytea"
            | "image"
            | "bytes"
    )
}

/// The bare base of a declared type: `numeric(12,2)` → `numeric`,
/// `timestamp with time zone` → `timestamp`, `text[]` → `text[]`.
pub(crate) fn base_type(t: &str) -> String {
    t.trim()
        .to_ascii_lowercase()
        .split(['(', ' '])
        .next()
        .unwrap_or("")
        .to_string()
}

/// Date/time families whose NOT NULL placeholder should be `CURRENT_TIMESTAMP`
/// rather than an empty string.
pub(crate) fn is_temporal_type(base: &str) -> bool {
    matches!(
        base,
        "timestamp" | "timestamptz" | "datetime" | "date" | "time" | "timetz"
    )
}

/// True for a column the server fills itself — auto-increment, PostgreSQL
/// `serial`, an identity column, or a generated expression — so an INSERT
/// template must omit it instead of writing a literal that skips the sequence.
pub(crate) fn is_server_generated_column(c: &ColumnInfo) -> bool {
    let extra = c.extra.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    if extra.contains("auto_increment") || extra.contains("generated") {
        return true;
    }
    if matches!(extra.as_str(), "serial" | "bigserial" | "smallserial") {
        return true;
    }
    c.column_default
        .as_deref()
        .map(|d| d.trim().to_ascii_lowercase().starts_with("nextval("))
        .unwrap_or(false)
}

/// The placeholder a quick-insert template writes for one column. A declared
/// default becomes `DEFAULT`; otherwise the value is chosen from the column's
/// type and nullability so the generated statement is valid on the first try
/// (PostgreSQL rejects `''` for boolean / timestamp / numeric columns, which is
/// exactly what the old numeric-else-empty rule produced).
pub(crate) fn insert_placeholder(c: &ColumnInfo) -> String {
    if c.column_default
        .as_deref()
        .map(|d| !d.trim().is_empty())
        .unwrap_or(false)
    {
        return "DEFAULT".to_string();
    }
    if c.is_nullable {
        return "NULL".to_string();
    }
    if let Some(first) = c.enum_values.as_ref().and_then(|v| v.first()) {
        return sql_literal(first);
    }
    let base = base_type(&c.data_type);
    if is_numeric_type(&c.data_type) {
        "0".to_string()
    } else if matches!(base.as_str(), "bool" | "boolean") {
        "FALSE".to_string()
    } else if is_temporal_type(&base) {
        "CURRENT_TIMESTAMP".to_string()
    } else if matches!(base.as_str(), "json" | "jsonb") || base.ends_with("[]") || base == "array" {
        "'{}'".to_string()
    } else {
        "''".to_string()
    }
}

/// True for the engines that speak PostgreSQL's dialect (double-quoted
/// identifiers, `bytea`, `EXPLAIN (FORMAT TEXT)`): PostgreSQL proper plus the
/// compatible forks dbxt already badges with the same colour.
pub(crate) fn is_postgres_family(db_type: &str) -> bool {
    matches!(
        db_type.to_ascii_lowercase().as_str(),
        "postgres"
            | "postgresql"
            | "opengauss"
            | "gaussdb"
            | "kingbase"
            | "highgo"
            | "cockroachdb"
            | "redshift"
            | "dm"
            | "kwdb"
    )
}

/// Engines whose sidebar gets a schema layer.
///
/// The kernel reports schema awareness for a wide set, including embedded
/// engines (SQLite, DuckDB) where a `main`-only picker is noise and an extra
/// `list_schemas` round-trip buys nothing. Those are filtered out here; a
/// server that still reports no schemas falls back to the flat list, so a
/// mis-guess degrades to the pre-R26 behaviour instead of breaking.
pub(crate) fn schema_picker_engine(db_type: DatabaseType) -> bool {
    is_schema_aware(db_type)
        && !matches!(
            db_type,
            DatabaseType::Sqlite
                | DatabaseType::Rqlite
                | DatabaseType::Turso
                | DatabaseType::CloudflareD1
                | DatabaseType::DuckDb
                | DatabaseType::Tdengine
                | DatabaseType::Iris
                | DatabaseType::Informix
                | DatabaseType::Access
                | DatabaseType::Jdbc
        )
}

/// `schema.table` for display, or just `table` when there is no schema (MySQL,
/// Redis-less SQL engines, SQL results whose table was guessed from SQL).
pub(crate) fn qualified_display(schema: &str, table: &str) -> String {
    if schema.trim().is_empty() {
        table.to_string()
    } else {
        format!("{schema}.{table}")
    }
}

/// The per-table persistence key used by the count cache and `tui.json`. The
/// schema is folded in so `public.orders` and `inv.orders` never share a row
/// count, a column-visibility set or a saved sort. An empty schema keeps the
/// bare table name, so pre-R26 MySQL configs still match.
pub(crate) fn table_pref_key(schema: &str, table: &str) -> String {
    qualified_display(schema, table)
}

/// Quoted, schema-qualified relation name for SQL. An empty schema yields the
/// unqualified `"table"` the pre-R26 code produced.
pub(crate) fn table_ref(db_type: DatabaseType, schema: &str, table: &str) -> String {
    qualified_table_name(Some(db_type), Some(schema), table)
}

/// Uppercase hex for the bytes of `s` (the fallback when a binary cell is not
/// already in the kernel's `0x…` form).
pub(crate) fn hex_of_bytes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// The hex digits of a `0x…` / `\x…` binary rendering, when `s` is one. The
/// kernel hands binary columns to the UI as `0x<hex>`, so recovering the bytes
/// here keeps a copied row from being hex-encoded a second time.
pub(crate) fn binary_hex_digits(s: &str) -> Option<&str> {
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("\\x"))?;
    if !hex.is_empty() && hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(hex)
    } else {
        None
    }
}

/// A binary literal in the target dialect. PostgreSQL's `X'…'` is a bit string,
/// not `bytea`, so it needs the `'\x…'::bytea` form; every other engine uses the
/// portable `X'…'` hex literal.
pub(crate) fn binary_literal(hex: &str, db_type: Option<&str>) -> String {
    if db_type.map(is_postgres_family).unwrap_or(false) {
        format!("'\\x{hex}'::bytea")
    } else {
        format!("X'{hex}'")
    }
}

/// One element of a PostgreSQL array literal. Strings are quoted; numbers and
/// booleans stay bare so the resulting `ARRAY[…]` infers the right element type.
pub(crate) fn array_element_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(true) => "TRUE".to_string(),
        serde_json::Value::Bool(false) => "FALSE".to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => sql_literal(s),
        other => sql_literal(&other.to_string()),
    }
}

/// PostgreSQL array cells arrive as JSON (`["a","b"]`); an INSERT literal needs
/// the `ARRAY[…]` form instead (and `'{}'` for the empty array, whose type an
/// empty `ARRAY[]` would leave ambiguous). Returns `None` when the cell is not a
/// JSON array, so the caller can fall back to the ordinary string literal.
pub(crate) fn array_literal(s: &str, data_type: &str) -> Option<String> {
    let items: Vec<serde_json::Value> = serde_json::from_str(s).ok()?;
    let ty = data_type.trim();
    let cast = if ty.ends_with("[]") {
        format!("::{ty}")
    } else {
        String::new()
    };
    if items.is_empty() {
        return Some(format!("'{{}}'{cast}"));
    }
    let lits: Vec<String> = items.iter().map(array_element_literal).collect();
    Some(format!("ARRAY[{}]{cast}", lits.join(", ")))
}

/// Literal used by the copy-row-as-INSERT action: binary columns become a hex
/// literal, array columns a PostgreSQL `ARRAY[…]`, everything else follows the
/// edit layer's rules.
pub(crate) fn insert_literal(v: &Val, data_type: Option<&str>, db_type: Option<&str>) -> String {
    if let (Val::Text(s), Some(dt)) = (v, data_type) {
        if is_binary_type(dt) {
            let hex = binary_hex_digits(s)
                .map(str::to_string)
                .unwrap_or_else(|| hex_of_bytes(s));
            return binary_literal(&hex, db_type);
        }
        if dt.trim().ends_with("[]") {
            if let Some(lit) = array_literal(s, dt) {
                return lit;
            }
        }
    }
    val_literal(v, data_type)
}

/// Build `INSERT INTO t (cols…) VALUES (vals…)` for one row of `grid`.
pub(crate) fn build_insert_sql(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    grid: &Grid,
    row: &[Val],
    app: &App,
) -> String {
    let types = grid_column_types(app, schema, table, grid);
    build_insert_sql_types(cfg, schema, table, grid, row, &types)
}

/// Declared type per grid column, resolved from the browsed table's metadata
/// (one lookup per column instead of one per cell). The streaming export and
/// `build_insert_sql` both use this.
pub(crate) fn grid_column_types(
    app: &App,
    schema: &str,
    table: &str,
    grid: &Grid,
) -> Vec<Option<String>> {
    grid.columns
        .iter()
        .map(|c| column_type(app, schema, table, c))
        .collect()
}

/// `build_insert_sql` with precomputed column types, so a background export can
/// build a row without an `App`.
pub(crate) fn build_insert_sql_types(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    grid: &Grid,
    row: &[Val],
    types: &[Option<String>],
) -> String {
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let cols = grid
        .columns
        .iter()
        .map(|c| q(c))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = grid
        .columns
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
    format!(
        "INSERT INTO {} ({})\nVALUES ({});",
        table_ref(cfg.db_type, schema, table),
        cols,
        vals
    )
}

pub(crate) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Read the (possibly quoted, possibly `schema.`) identifier at the start of
/// `s`, returning its last dot-separated segment.
pub(crate) fn read_ident(s: &str) -> Option<String> {
    let mut rest = s.trim_start();
    let mut last: Option<String> = None;
    loop {
        let bytes = rest.as_bytes();
        let Some(&first) = bytes.first() else {
            break;
        };
        let (seg, consumed) = match first {
            b'`' | b'"' => {
                let close = first as char;
                let Some(i) = rest[1..].find(close) else {
                    break;
                };
                (rest[1..1 + i].to_string(), i + 2)
            }
            b'[' => {
                let Some(i) = rest[1..].find(']') else {
                    break;
                };
                (rest[1..1 + i].to_string(), i + 2)
            }
            b if is_ident_byte(b) => {
                let end = rest
                    .bytes()
                    .position(|b| !is_ident_byte(b))
                    .unwrap_or(rest.len());
                (rest[..end].to_string(), end)
            }
            _ => break,
        };
        last = Some(seg);
        rest = &rest[consumed..];
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
            continue;
        }
        break;
    }
    last
}

/// Best-effort table name for a query result: the identifier after the first
/// `FROM` / `JOIN` / `UPDATE` / `INTO` keyword.
pub(crate) fn guess_table_from_sql(sql: &str) -> Option<String> {
    let lower = sql.to_lowercase();
    let bytes = lower.as_bytes();
    for kw in ["from", "join", "update", "into"] {
        let mut i = 0;
        while let Some(pos) = lower[i..].find(kw) {
            let start = i + pos;
            let end = start + kw.len();
            let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
            let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                let rest = sql[end..].trim_start();
                // `FROM (SELECT …)` is a derived table, not a name.
                if !rest.starts_with('(') {
                    if let Some(name) = read_ident(rest) {
                        return Some(name);
                    }
                }
            }
            i = end;
        }
    }
    None
}

// ── clipboard (OSC 52 + file fallback) ──

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Where the clipboard file fallback is written (also printed in the status bar
/// so a terminal that drops OSC 52 still has the text).
pub(crate) fn clipboard_file_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DBXT_CLIPBOARD_FILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("DBXT_NO_CLIPBOARD").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    Some(base.join("dbxt").join("clipboard.txt"))
}

/// Copy `text` to the terminal clipboard with OSC 52, and write the same text to
/// a file as a fallback. Never returns an error: an unsupported terminal simply
/// ignores the escape sequence, and the file path (if any) is reported so the
/// text is still reachable.
pub(crate) fn clipboard_copy(text: &str) -> Option<PathBuf> {
    let path = clipboard_file_path();
    let wrote = match &path {
        Some(p) => {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(p, text).is_ok()
        }
        None => false,
    };
    if std::env::var_os("DBXT_CLIPBOARD").is_none_or(|v| v != "off") {
        let b64 = base64_encode(text.as_bytes());
        let seq = if std::env::var_os("TMUX").is_some() {
            // tmux swallows a raw OSC; wrap it in a DCS passthrough with ESC
            // doubled (needs `set -g set-clipboard on` to reach the terminal).
            format!("\x1bPtmux;\x1b\x1b]52;c;{b64}\x07\x1b\\")
        } else if std::env::var_os("STY").is_some() {
            format!("\x1bP\x1b]52;c;{b64}\x07\x1b\\")
        } else {
            format!("\x1b]52;c;{b64}\x07")
        };
        let mut out = std::io::stdout();
        let _ = out.write_all(seq.as_bytes());
        let _ = out.flush();
    }
    if wrote {
        path
    } else {
        None
    }
}

/// Clipboard text for one cell: the same shapes the row popup and the grid show
/// (`NULL` for a SQL NULL, `''` for the empty string), so what lands on the
/// clipboard is unambiguous when pasted back into SQL.
pub(crate) fn cell_copy_text(v: &Val) -> String {
    value_display(v).0
}

/// `Y` in the results grid: copy just the focused cell's value (the row-level
/// `y` keeps copying the whole row as `INSERT`, R22). The column name is named in
/// the status so a wide table stays unambiguous.
pub(crate) fn copy_cell_value(app: &mut App) {
    if app.grid_kind == GridKind::Columns
        || (app.struct_view == StructView::Ddl && app.ddl.is_some())
    {
        app.status = t("表结构视图没有可复制的单元格").into();
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表没有可复制的单元格（先 Enter 进入某条语句的结果）").into();
        return;
    }
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可复制的单元格").into();
        return;
    };
    let Some(name) = grid.columns.get(app.col_cursor).cloned() else {
        app.status = t("没有可复制的单元格").into();
        return;
    };
    let Some(val) = grid.rows.get(app.sel).and_then(|r| r.get(app.col_cursor)) else {
        app.status = t("没有可复制的单元格").into();
        return;
    };
    let text = cell_copy_text(val);
    copy_named_value(app, &fix_double_encoding(&name), &text);
}

/// Copy one value to the clipboard, naming its column in the status. Shared by
/// the grid's `Y` and the row popup's `y`/`Y` so every “copy one value” path
/// reads the same.
pub(crate) fn copy_named_value(app: &mut App, name: &str, text: &str) {
    let n = text.chars().count();
    // NULL / '' / a short text read best in the status; a long cell would blow it up.
    let short = truncate_disp(&one_line(text), 24);
    match clipboard_copy(text) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制「{}」= {} · {} 字符 · 兜底 {}",
                &[&name, &short, &n, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制「{}」= {} · {} 字符", &[&name, &short, &n]),
    }
}

/// `y` in the results pane: copy the focused row as an `INSERT` statement.
pub(crate) fn copy_row_sql(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图没有可复制的数据行").into();
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表没有可复制的行（先 Enter 进入某条语句的结果）").into();
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let Some(full) = full_grid(app) else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let Some(orig) = app.full_row_index() else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let Some(row) = full.rows.get(orig).cloned() else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let Some((schema, table)) = table_target(app) else {
        app.status = t("无法从当前结果确定表名（仅表格浏览与含 FROM 的查询支持 y）").into();
        return;
    };
    let sql = build_insert_sql(&cfg, &schema, &table, &full, &row, app);
    let n = sql.chars().count();
    match clipboard_copy(&sql) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制 INSERT（{} 字符）· OSC52 剪贴板 · 兜底 {}",
                &[&(n), &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制 INSERT（{} 字符）· OSC52 剪贴板", &[&(n)]),
    }
}

/// The `(schema, table)` a result grid belongs to: the browsed table, else the
/// table guessed from a drilled statement / the last executed SQL. An empty
/// schema when it cannot be known. Shared by `y` (copy as INSERT) and R57's
/// batch `d` / `c`.
pub(crate) fn table_target(app: &App) -> Option<(String, String)> {
    if let Some(ps) = &app.page_state {
        return Some((ps.schema.clone(), ps.table.clone()));
    }
    if let Some(s) = &app.script {
        return s
            .drilled
            .and_then(|i| s.outcomes.get(i))
            .and_then(|o| guess_table_from_sql(&o.sql))
            .map(|t| (String::new(), t));
    }
    app.last_sql
        .as_deref()
        .and_then(guess_table_from_sql)
        .map(|t| (String::new(), t))
}

// ── R57: results row selection (`V`) + batch statements (`Y` / `d` / `c`) ──

/// Everything needed to turn selected result rows into `DELETE` / `UPDATE`
/// statements. Built only when the result is tied to a browsable table with a
/// real primary key whose columns all appear in the grid — an expression /
/// aggregate / join result has neither, and `d` / `c` must refuse, never guess.
pub(crate) struct BatchTarget {
    pub(crate) db_type: DatabaseType,
    pub(crate) schema: String,
    pub(crate) table: String,
    /// Grid column names, in display order (indexed by row position).
    pub(crate) columns: Vec<String>,
    /// Declared type per grid column (drives literal quoting).
    pub(crate) types: Vec<Option<String>>,
    /// Primary-key column names, in key order.
    pub(crate) pks: Vec<String>,
}

impl BatchTarget {
    pub(crate) fn col_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// `key = literal` for one primary-key column of `row`, using the column's
    /// declared type so text is quoted and numbers stay bare.
    pub(crate) fn pk_eq(&self, name: &str, row: &[Val]) -> Option<String> {
        let ci = self.col_index(name)?;
        let v = row.get(ci).cloned().unwrap_or(Val::Null);
        Some(format!(
            "{} = {}",
            quote_table_identifier(Some(self.db_type), name),
            val_literal(&v, self.types.get(ci).and_then(|t| t.as_deref()))
        ))
    }
}

/// The batch target for the currently focused result grid, or `None` when there
/// is no browsable table / primary key to key the statements by.
pub(crate) fn batch_target(app: &App) -> Option<BatchTarget> {
    let full = full_grid(app)?;
    let (schema, table) = table_target(app)?;
    let cfg = app.selected.as_ref()?;
    let meta = app
        .table_meta
        .as_ref()
        .filter(|m| m.table == table && m.schema == schema)?;
    let pks: Vec<String> = meta
        .columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.clone())
        .collect();
    if pks.is_empty() {
        return None;
    }
    // Every key column must be in the result, or the `WHERE` cannot be written.
    if !pks.iter().all(|k| full.columns.iter().any(|c| c == k)) {
        return None;
    }
    let types = full
        .columns
        .iter()
        .map(|c| column_type(app, &schema, &table, c))
        .collect();
    Some(BatchTarget {
        db_type: cfg.db_type,
        schema,
        table,
        columns: full.columns.clone(),
        types,
        pks,
    })
}

/// R57 `d`: one `DELETE` for the selected rows. A one-column key uses
/// `pk IN (…)`; a composite key falls back to a portable
/// `(k1 = … AND k2 = …) OR (…)` chain (row-value `IN` is not universal). Values
/// are escaped by the shared literal rules. This is a statement for review —
/// dbxt never runs it.
pub(crate) fn batch_delete_sql(t: &BatchTarget, rows: &[Vec<Val>]) -> String {
    let table = table_ref(t.db_type, &t.schema, &t.table);
    if t.pks.len() == 1 {
        let pk = &t.pks[0];
        let vals = rows
            .iter()
            .filter_map(|r| {
                let ci = t.col_index(pk)?;
                let v = r.get(ci).cloned().unwrap_or(Val::Null);
                Some(val_literal(&v, t.types.get(ci).and_then(|x| x.as_deref())))
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "DELETE FROM {table}\nWHERE {} IN ({});",
            quote_table_identifier(Some(t.db_type), pk),
            vals
        );
    }
    let clauses = rows
        .iter()
        .filter_map(|r| {
            let parts = t
                .pks
                .iter()
                .filter_map(|k| t.pk_eq(k, r))
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| format!("({})", parts.join(" AND ")))
        })
        .collect::<Vec<_>>()
        .join("\n   OR ");
    format!("DELETE FROM {table}\nWHERE {clauses};")
}

/// R57 `c`: an `UPDATE … SET <every non-key column> = <current value> WHERE
/// <pk> = …;` template per selected row. Every value is the row's *current*
/// value, so the statement is a no-op until the user edits a `SET` value; it is
/// a starting point in the editor, never executed.
pub(crate) fn batch_update_sql(t: &BatchTarget, rows: &[Vec<Val>]) -> String {
    let table = table_ref(t.db_type, &t.schema, &t.table);
    let mut stmts: Vec<String> = Vec::new();
    for r in rows {
        let sets = t
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !t.pks.iter().any(|k| k == *c))
            .map(|(ci, c)| {
                let v = r.get(ci).cloned().unwrap_or(Val::Null);
                format!(
                    "{} = {}",
                    quote_table_identifier(Some(t.db_type), c),
                    val_literal(&v, t.types.get(ci).and_then(|x| x.as_deref()))
                )
            })
            .collect::<Vec<_>>();
        let Some(first) = t.pks.first().and_then(|k| t.pk_eq(k, r)) else {
            continue;
        };
        let mut parts = vec![first];
        parts.extend(t.pks.iter().skip(1).filter_map(|k| t.pk_eq(k, r)));
        let clause = parts.join(" AND ");
        if sets.is_empty() {
            stmts.push(tf("-- {}：仅有主键列，无可更新列", &[&table]));
        } else {
            stmts.push(format!(
                "UPDATE {table}\nSET {}\nWHERE {clause};",
                sets.join(", ")
            ));
        }
    }
    stmts.join("\n\n")
}

/// R57: the results pane currently has selectable rows (a real data grid, not
/// the structure list, DDL text or the script statement list).
pub(crate) fn row_select_available(app: &App) -> bool {
    if app.grid_kind == GridKind::Columns {
        return false;
    }
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return false;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        return false;
    }
    active_grid(app).is_some_and(|g| !g.rows.is_empty())
}

/// R57: `V` enters row-select mode with the cursor row as the anchor.
pub(crate) fn enter_row_select(app: &mut App) {
    if !row_select_available(app) {
        app.status = t("当前视图没有可选行").into();
        return;
    }
    app.row_sel_anchor = Some(app.sel);
    app.status = row_select_status(app);
}

/// The row-select mode status line: the block range and the operation keys.
pub(crate) fn row_select_status(app: &App) -> String {
    let Some(anchor) = app.row_sel_anchor else {
        return String::new();
    };
    let (lo, hi) = (anchor.min(app.sel), anchor.max(app.sel));
    tf(
        "行选 {}-{}（{} 行）· Ctrl-A 全选 · ↑↓ 移动 · Shift+↑↓ / v 扩展 · Y 复制 · d 删除语句 · c 更新模板 · Esc 退出",
        &[&(lo + 1), &(hi + 1), &(hi - lo + 1)],
    )
}

/// R86: `Ctrl-A` inside row-select mode selects every row on the current page
/// (anchor at the first row, cursor at the last), mirroring the Redis / MongoDB
/// key browser's `a` select-all gesture. Pure client-side, no query.
pub(crate) fn row_select_all(app: &mut App) {
    let n = result_row_count(app);
    if n == 0 {
        app.status = t("当前视图没有可选行").into();
        return;
    }
    app.row_sel_anchor = Some(0);
    app.sel = n - 1;
    app.status = row_select_status(app);
}

/// R57: row-select mode keys. Returns `true` when consumed; a key the mode does
/// not use exits the mode (so the rest of the grid keymap stays one keystroke
/// away) and returns `false` for the caller to handle normally.
pub(crate) fn row_select_key(app: &mut App, tx: &Tx, k: KeyEvent) -> bool {
    // R86: `Ctrl-A` selects every row on the page (the Redis / Mongo `a`
    // equivalent). Handled before the generic modifier bailout, which would
    // otherwise exit the mode.
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && k.code == KeyCode::Char('a')
    {
        row_select_all(app);
        return true;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) || k.modifiers.contains(KeyModifiers::ALT) {
        app.row_sel_anchor = None;
        return false;
    }
    let anchor = app.row_sel_anchor.unwrap_or(app.sel);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Esc => {
            app.row_sel_anchor = None;
            app.flash(t("已退出行选").into());
            true
        }
        // Plain ↑/↓ move the anchor and the cursor together (collapsing the
        // block to one row); Shift+↑/↓ stretch it from the anchor.
        KeyCode::Up | KeyCode::Char('k') => {
            move_cursor(app, tx, -1);
            app.row_sel_anchor = Some(if shift { anchor } else { app.sel });
            app.status = row_select_status(app);
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_cursor(app, tx, 1);
            app.row_sel_anchor = Some(if shift { anchor } else { app.sel });
            app.status = row_select_status(app);
            true
        }
        // `v` / `V` extend the block one row down (vim's linewise stretch).
        KeyCode::Char('v') | KeyCode::Char('V') => {
            move_cursor(app, tx, 1);
            app.row_sel_anchor = Some(anchor);
            app.status = row_select_status(app);
            true
        }
        KeyCode::Char('Y') => {
            row_select_copy(app);
            true
        }
        KeyCode::Char('d') => {
            row_select_delete_sql(app);
            true
        }
        KeyCode::Char('c') => {
            row_select_update_sql(app);
            true
        }
        _ => {
            app.row_sel_anchor = None;
            false
        }
    }
}

/// R57 `Y`: copy the selected rows as TSV (with a header line), so a block can
/// be pasted straight into a spreadsheet / another issue.
pub(crate) fn row_select_copy(app: &mut App) {
    let Some(full) = full_grid(app) else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let rows = selected_full_rows(app);
    if rows.is_empty() {
        app.status = t("没有可复制的行").into();
        return;
    }
    let text = rows_to_tsv(&full.columns, &rows);
    let n = rows.len();
    app.row_sel_anchor = None;
    match clipboard_copy(&text) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制 {} 行（TSV，含列头）· 兜底 {}",
                &[&n, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制 {} 行（TSV，含列头）", &[&n]),
    }
}

/// The TSV body for R57 `Y`: a header line then one tab-joined line per row.
/// NULL (`NULL`) and the empty string (`''`) keep their display shapes, so a
/// paste never silently turns one into the other.
pub(crate) fn rows_to_tsv(columns: &[String], rows: &[Vec<Val>]) -> String {
    let mut text = columns
        .iter()
        .map(|c| fix_double_encoding(c))
        .collect::<Vec<_>>()
        .join("\t");
    for r in rows {
        text.push('\n');
        text.push_str(&r.iter().map(cell_copy_text).collect::<Vec<_>>().join("\t"));
    }
    text
}

/// R57 `d`: generate `DELETE …` for the selected rows and hand it to the editor
/// for review. Nothing is executed — the zero-write red line holds.
pub(crate) fn row_select_delete_sql(app: &mut App) {
    let rows = selected_full_rows(app);
    if rows.is_empty() {
        app.status = t("没有可操作的行").into();
        return;
    }
    let Some(t) = batch_target(app) else {
        app.row_sel_anchor = None;
        app.status = t("无主键，跳过（表达式 / 聚合 / 无主键结果不支持批量删除）").into();
        return;
    };
    let sql = batch_delete_sql(&t, &rows);
    let n = rows.len();
    let pk = t.pks.join(", ");
    app.row_sel_anchor = None;
    app.set_editor_text(&sql);
    app.focus = Focus::Editor;
    app.status = tf(
        "✓ 已生成 DELETE（{} 行 · 主键 {}）→ 编辑器待确认，未执行",
        &[&n, &pk],
    );
}

/// R57 `c`: generate an `UPDATE … SET …` template for the selected rows and
/// hand it to the editor for review. Nothing is executed.
pub(crate) fn row_select_update_sql(app: &mut App) {
    let rows = selected_full_rows(app);
    if rows.is_empty() {
        app.status = t("没有可操作的行").into();
        return;
    }
    let Some(t) = batch_target(app) else {
        app.row_sel_anchor = None;
        app.status = t("无主键，跳过（表达式 / 聚合 / 无主键结果不支持批量更新）").into();
        return;
    };
    let sql = batch_update_sql(&t, &rows);
    let n = rows.len();
    let pk = t.pks.join(", ");
    app.row_sel_anchor = None;
    app.set_editor_text(&sql);
    app.focus = Focus::Editor;
    app.status = tf(
        "✓ 已生成 UPDATE 模板（{} 行 · 主键 {}）→ 编辑器待确认，未执行",
        &[&n, &pk],
    );
}

/// New value typed in the edit dialog → SQL literal. A blank box (or `NULL`,
/// any case) means SQL NULL; `''` means the empty string; `'text'` is taken as
/// a literal string; anything else is coerced like a cell value (numbers stay
/// bare for numeric columns, text gets quoted).
pub(crate) fn new_value_literal(input: &str, data_type: Option<&str>) -> String {
    let t = input.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if let Some(inner) = strip_string_literal(t) {
        return sql_literal(&inner);
    }
    val_literal(&Val::Text(t.to_string()), data_type)
}

/// If `s` is a single-quoted SQL string literal (`'…'`, `''` escaping a quote),
/// decode its contents. Used only by the edit dialog, so a user can type `''`
/// for the empty string (now that a blank box means NULL) and `'text'` for text
/// that would otherwise be read as a number.
pub(crate) fn strip_string_literal(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        Some(s[1..s.len() - 1].replace("''", "'"))
    } else {
        None
    }
}

/// Seed text for the edit box so that submitting it unchanged round-trips: a
/// NULL opens blank, and text the plain-input rules would misread (the empty
/// string, `NULL`, `true`/`false`, a literal `''`) opens quoted.
pub(crate) fn edit_prefill(v: &Val) -> String {
    match v {
        Val::Null => String::new(),
        Val::Text(s) => {
            if new_value_literal(s, None) == sql_literal(s) {
                s.clone()
            } else {
                format!("'{}'", s.replace('\'', "''"))
            }
        }
    }
}

pub(crate) fn build_update_sql(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    column: &str,
    data_type: Option<&str>,
    input: &str,
    where_clause: &str,
) -> String {
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    format!(
        "UPDATE {}\nSET {} = {}\nWHERE {};",
        table_ref(cfg.db_type, schema, table),
        q(column),
        new_value_literal(input, data_type),
        where_clause
    )
}

impl EditDialog {
    pub(crate) fn update_sql(&self) -> String {
        build_update_sql(
            &self.cfg,
            &self.schema,
            &self.table,
            &self.column,
            self.data_type.as_deref(),
            &self.new_input.lines().join(" "),
            &self.where_clause,
        )
    }
    pub(crate) fn sql(&self) -> String {
        match self.kind {
            EditKind::Update => self.update_sql(),
            EditKind::Insert => self.insert_sql.clone(),
        }
    }
}

/// Build the `WHERE` clause that identifies one row: the table's primary key
/// when the column metadata is loaded, otherwise every column (with a warning
/// flag). Returns `(clause, keys, no_pk)`; `1 = 1` is the last resort when even
/// the column list is unusable.
pub(crate) fn row_where_clause(
    app: &App,
    grid: &Grid,
    row: &[Val],
    schema: &str,
    table: &str,
) -> (String, Vec<String>, bool) {
    let (keys, no_pk) = match app
        .table_meta
        .as_ref()
        .filter(|m| m.table == table && m.schema == schema)
    {
        Some(meta) => {
            let pks: Vec<String> = meta
                .columns
                .iter()
                .filter(|c| c.is_primary_key)
                .map(|c| c.name.clone())
                .collect();
            if pks.is_empty() {
                (grid.columns.clone(), true)
            } else {
                (pks, false)
            }
        }
        None => (grid.columns.clone(), true),
    };
    let db_type = app.selected.as_ref().map(|c| c.db_type);
    let q = |name: &str| quote_table_identifier(db_type, name);
    let mut conds: Vec<String> = Vec::new();
    for k in &keys {
        let Some(ci) = grid.columns.iter().position(|c| c == k) else {
            continue;
        };
        let Some(v) = row.get(ci) else {
            continue;
        };
        conds.push(format!(
            "{} = {}",
            q(k),
            val_literal(v, column_type(app, schema, table, k).as_deref())
        ));
    }
    let clause = if conds.is_empty() {
        "1 = 1".to_string()
    } else {
        conds.join(" AND ")
    };
    (clause, keys, no_pk)
}

/// `Delete` / `Ctrl-D` — delete the focused row. The `DELETE … WHERE …` is built
/// from the primary key (or every column, with a warning) and always goes
/// through the red confirmation layer; nothing is deleted until Enter.
pub(crate) fn delete_row(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持删除行").into();
        return;
    }
    if readonly_conn_block(app) {
        return;
    }
    let Some(grid) = active_grid(app) else {
        return;
    };
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(row) = grid.rows.get(app.sel).cloned() else {
        app.status = t("没有可删除的行").into();
        return;
    };
    let (where_clause, keys, no_pk) = row_where_clause(app, &grid, &row, &ps.schema, &ps.table);
    let sql = format!(
        "DELETE FROM {}\nWHERE {};",
        table_ref(cfg.db_type, &ps.schema, &ps.table),
        where_clause
    );
    let mut reasons: Vec<String> = Vec::new();
    if no_pk {
        reasons.push(t("⚠ 未检测到主键：将按全部列匹配删除，请确认只命中这一行").into());
    } else {
        reasons.push(tf("将删除 1 行（主键 {}）", &[&(keys.join(", "))]));
    }
    reasons.push(t("DELETE 不可撤销，Enter 后立即执行").into());
    // The WHERE predicate itself is shown by the confirm layer's impact line.
    app.confirm = Some(Confirm {
        sql,
        reasons,
        refresh: true,
        clear_batch: false,
        conn: None,
        redis: None,
        mongo: None,
    });
    app.status = t("删除确认 · Enter 执行 · Esc 取消").into();
}

/// `e` — open a diff-style confirmation layer for the focused cell. The user
/// types the new value, sees old → new plus the WHERE clause, and only then is
/// the UPDATE sent (Enter). Esc cancels, `v` hands the SQL to the editor, `b`
/// queues it for one transactional batch commit.
pub(crate) fn edit_cell(app: &mut App) {
    if !in_table_data_view(app) {
        app.focus = Focus::Editor;
        return;
    }
    if readonly_conn_block(app) {
        return;
    }
    let Some(grid) = active_grid(app) else {
        return;
    };
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(row) = grid.rows.get(app.sel).cloned() else {
        return;
    };
    let Some(col) = grid.columns.get(app.col_cursor).cloned() else {
        return;
    };
    let Some(val) = row.get(app.col_cursor).cloned() else {
        return;
    };

    // Primary keys drive the WHERE clause; fall back to every column (with a
    // warning) when the table has none or its metadata is not loaded yet.
    let (where_clause, keys, no_pk) = row_where_clause(app, &grid, &row, &ps.schema, &ps.table);

    let dt = column_type(app, &ps.schema, &ps.table, &col);
    // A NULL cell opens with an empty box (the old value is shown above), so
    // there is no chance of the literal text "NULL" sneaking into the input.
    let initial = edit_prefill(&val);
    let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
    ta.set_placeholder_text(t("留空 = NULL · '文本' = 字符串"));
    ta.move_cursor(CursorMove::End);
    app.edit_dialog = Some(EditDialog {
        kind: EditKind::Update,
        cfg: Box::new(cfg),
        db: app.current_db(),
        schema: ps.schema.clone(),
        table: ps.table.clone(),
        column: col.clone(),
        data_type: dt,
        old: val,
        new_input: ta,
        where_clause,
        keys,
        no_pk,
        insert_sql: String::new(),
        insert_preview: Vec::new(),
    });
    app.status = tf(
        "编辑 {} → Enter 确认执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量",
        &[&(col)],
    );
}

/// `i` — open the diff layer with an `INSERT` template built from the table's
/// column list. Enter submits, `v` hands the SQL to the editor.
pub(crate) fn quick_insert(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持快速插入").into();
        return;
    }
    if readonly_conn_block(app) {
        return;
    }
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(meta) = app
        .table_meta
        .as_ref()
        .filter(|m| m.table == ps.table && m.schema == ps.schema)
        .cloned()
    else {
        app.status = t("表结构尚未加载，稍后重试").into();
        return;
    };
    let cols: Vec<&ColumnInfo> = meta
        .columns
        .iter()
        .filter(|c| !is_server_generated_column(c))
        .collect();
    if cols.is_empty() {
        app.status = t("没有可插入的列").into();
        return;
    }
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let col_list = cols
        .iter()
        .map(|c| q(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut preview: Vec<(String, String)> = Vec::new();
    let mut vals: Vec<String> = Vec::new();
    for c in &cols {
        let v = insert_placeholder(c);
        vals.push(v.clone());
        preview.push((fix_double_encoding(&c.name), v));
    }
    let sql = format!(
        "INSERT INTO {} ({})\nVALUES ({});",
        table_ref(cfg.db_type, &ps.schema, &ps.table),
        col_list,
        vals.join(", ")
    );
    app.edit_dialog = Some(EditDialog {
        kind: EditKind::Insert,
        cfg: Box::new(cfg),
        db: app.current_db(),
        schema: ps.schema.clone(),
        table: ps.table.clone(),
        column: String::new(),
        data_type: None,
        old: Val::Null,
        new_input: TextArea::default(),
        where_clause: String::new(),
        keys: Vec::new(),
        no_pk: false,
        insert_sql: sql,
        insert_preview: preview,
    });
    app.status = tf(
        "插入 {} → Enter 确认执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量",
        &[&(ps.table)],
    );
}

/// Keys for the diff-style edit confirmation layer. UPDATE has a live text
/// input, so its commands use Ctrl combos (plain letters must reach the input).
pub(crate) fn edit_dialog_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut d) = app.edit_dialog.take() else {
        return;
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let plain = k.modifiers.is_empty();
    let insert = d.kind == EditKind::Insert;
    let to_editor =
        ctrl && k.code == KeyCode::Char('v') || (insert && plain && k.code == KeyCode::Char('v'));
    let to_batch =
        ctrl && k.code == KeyCode::Char('t') || (insert && plain && k.code == KeyCode::Char('b'));
    if k.code == KeyCode::Esc {
        app.flash(t("已取消编辑").into());
    } else if k.code == KeyCode::Enter {
        submit_edit_sql(app, tx, d.sql());
    } else if to_editor {
        let sql = d.sql();
        app.set_editor_text(&sql);
        app.focus = Focus::Editor;
        app.status = t("已转入编辑器微调 · Ctrl-J 执行").into();
    } else if to_batch {
        let sql = d.sql();
        app.batch.push(sql);
        app.status = tf(
            "已加入批量队列（{} 条）· Ctrl-S 打包提交 · Ctrl-X 清空",
            &[&(app.batch.len())],
        );
    } else {
        if d.kind == EditKind::Update {
            d.new_input.input(k);
        }
        app.edit_dialog = Some(d);
    }
}

/// Send a generated write. It still passes the dangerous-statement gate so a
/// write that somehow lacks a bound WHERE gets a second confirmation.
pub(crate) fn submit_edit_sql(app: &mut App, tx: &Tx, sql: String) {
    if readonly_block(app, &sql) {
        return;
    }
    let mut reason = detect_danger(&sql);
    if reason.is_none() && one_line(&sql).to_ascii_lowercase().contains("where 1 = 1") {
        reason = Some(t("WHERE 恒真（1 = 1），会作用于整张表").into());
    }
    if let Some(reason) = reason {
        app.pending_run_origin = "editor";
        app.confirm = Some(Confirm {
            sql,
            reasons: vec![reason],
            refresh: true,
            clear_batch: false,
            conn: None,
            redis: None,
            mongo: None,
        });
        return;
    }
    app.push_history(&sql);
    app.pending_write = true;
    execute_sql(app, tx, sql, "editor");
}
