// dbxt — Terminal UI client built on DBX kernel (dbx-core + dbx-mcp LocalBackend)
// Apache-2.0. Reuses DBX connection storage (dbx.db), native drivers, SQL safety.
#![recursion_limit = "512"]

mod ui_text;

use ui_text::{t, tf};

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use dbx_core::models::connection::ConnectionConfig;
use dbx_core::query::QueryExecutionOptions;
use dbx_core::sql_dialect::{
    build_count_table_sql, build_table_data_select_sql_with_database, normalize_where_input,
    quote_table_identifier, TableDataSelectSqlOptions,
};
use dbx_core::types::{ColumnInfo, TableInfo};
use dbx_mcp::backend::{
    new_connection_config, parse_database_type, BatchStatementResult, DbxBackend, LocalBackend,
};
use dbx_mcp::paths::storage_db_path;
use futures::StreamExt;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;
use tui_textarea::{CursorMove, TextArea};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use uuid::Uuid;

type Tx = tokio::sync::mpsc::UnboundedSender<OpResult>;

/// Rows fetched per table-data page (one extra row is fetched to detect a next page).
const PAGE_SIZE: usize = 50;
/// Rows fetched for an arbitrary SQL statement on the first run.
const QUERY_MAX_ROWS: usize = 500;
/// How many extra rows each `Ctrl-N` "load more" step pulls.
const QUERY_MORE_STEP: usize = 500;
/// Hard ceiling for `Ctrl-N`; past this the user should refine the query.
const QUERY_MAX_ROWS_CAP: usize = 20_000;
/// Last-resort watchdog for a single backend call. The SQL path already asks the
/// driver for a 60 s statement timeout, but Redis / MongoDB / metadata calls
/// carry no timeout of their own: without this a dead server would leave the
/// spinner turning forever instead of surfacing an error. The per-op ceiling is
/// in [`Op::watchdog`].
const OP_WATCHDOG_FALLBACK: Duration = Duration::from_secs(60);
/// SQL may be a multi-statement script, so it gets a wider ceiling (each
/// statement is still bounded by the driver's own 60 s timeout).
const OP_WATCHDOG_SQL: Duration = Duration::from_secs(180);

// ─── pages & focus ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Browse,  // sidebar (db/tables/structure) + editor + results
    NewConn, // connection form
}

#[derive(Clone, Copy, PartialEq)]
enum Backend {
    Sql,
    Redis,
    Mongo,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Sidebar,
    Preview,
    Editor,
    CmdInput,
}

#[derive(Clone, Copy, PartialEq)]
enum LayoutMode {
    Narrow, // cols < 46  (phone portrait / tiny tmux)
    Mid,    // 46..99
    Wide,   // >= 100
}

fn layout_mode(cols: u16) -> LayoutMode {
    if cols < 50 {
        LayoutMode::Narrow
    } else if cols < 100 {
        LayoutMode::Mid
    } else {
        LayoutMode::Wide
    }
}

/// Effective collapse state of a pane. A manual per-pane override always wins;
/// otherwise the single `auto_collapse` master switch decides: on = unfocused
/// aux panes collapse to a one-line strip so the focused pane owns the space
/// (the old responsive behaviour); off = every pane stays expanded.
fn pane_eff_collapsed(app: &App, pane: usize) -> bool {
    resolve_collapse(
        app.auto_collapse,
        app.focus,
        pane,
        app.pane_override.get(pane).copied().flatten(),
    )
}

/// Pure form of the collapse rule, so it can be unit-tested without an `App`.
fn resolve_collapse(auto: bool, focus: Focus, pane: usize, manual: Option<bool>) -> bool {
    if let Some(v) = manual {
        return v;
    }
    if !auto {
        return false;
    }
    match pane {
        PANE_SIDEBAR => focus != Focus::Sidebar,
        PANE_EDITOR => !matches!(focus, Focus::Editor | Focus::CmdInput),
        _ => false,
    }
}

/// Ctrl-A — one master switch for the whole responsive-collapse behaviour.
/// On: unfocused aux panes collapse (the old behaviour). Off: everything stays
/// expanded. Per-pane overrides are cleared so the two modes never mix.
fn toggle_auto_collapse(app: &mut App) {
    app.auto_collapse = !app.auto_collapse;
    app.pane_override = [None; 3];
    app.status = if app.auto_collapse {
        t("自动折叠：开 · 非焦点栏收起（Ctrl-A 关闭）").into()
    } else {
        t("自动折叠：关 · 所有栏展开（Ctrl-A 开启）").into()
    };
}

fn pane_name(pane: usize) -> &'static str {
    match pane {
        PANE_SIDEBAR => t("侧栏"),
        PANE_EDITOR => t("编辑器"),
        _ => t("结果区"),
    }
}

fn toggle_pane_collapse(app: &mut App) {
    let pane = match app.focus {
        Focus::Sidebar => PANE_SIDEBAR,
        Focus::Editor | Focus::CmdInput => PANE_EDITOR,
        Focus::Preview => PANE_RESULTS,
    };
    let cur = pane_eff_collapsed(app, pane);
    app.pane_override[pane] = Some(!cur);
    app.status = if cur {
        tf("已展开{}", &[&(pane_name(pane))])
    } else {
        tf("已收起{}", &[&(pane_name(pane))])
    };
}

fn cycle_focus(app: &mut App, forward: bool) {
    let order: Vec<Focus> = if app.backend_kind == Backend::Sql {
        vec![Focus::Sidebar, Focus::Editor, Focus::Preview]
    } else {
        vec![Focus::Sidebar, Focus::Editor, Focus::CmdInput, Focus::Preview]
    };
    let next = match order.iter().position(|f| *f == app.focus) {
        Some(i) if forward => (i + 1) % order.len(),
        Some(i) => (i + order.len() - 1) % order.len(),
        None => 0,
    };
    app.focus = order[next];
}

/// Which kind of content the results pane currently shows.
#[derive(Clone, Copy, PartialEq)]
enum GridKind {
    Query,     // arbitrary SQL result
    TableData, // paginated SELECT * of a table
    Columns,   // table structure (field list)
}

#[derive(Clone, Copy, PartialEq)]
enum StructView {
    Fields,
    Ddl,
}

// ─── cell values ─────────────────────────────────────────────────────────────

/// A result cell. NULL is kept distinct from the empty string so the grid can
/// render them differently.
#[derive(Clone, PartialEq)]
enum Val {
    Null,
    Text(String),
}

impl Val {
    fn text(&self) -> &str {
        match self {
            Val::Null => "",
            Val::Text(s) => s,
        }
    }
    #[allow(dead_code)]
    fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }}

fn value_to_val(v: &serde_json::Value) -> Val {
    match v {
        serde_json::Value::Null => Val::Null,
        serde_json::Value::String(s) => Val::Text(sanitize_cell(s)),
        serde_json::Value::Number(n) => Val::Text(n.to_string()),
        serde_json::Value::Bool(b) => Val::Text(b.to_string()),
        other => Val::Text(sanitize_cell(&other.to_string())),
    }
}

/// Collapse control characters so a value never breaks the one-line grid layout.
fn sanitize_cell(s: &str) -> String {
    if !s.chars().any(|c| c == '\n' || c == '\r' || c == '\t') {
        return s.to_string();
    }
    s.chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect()
}

// ─── result grid ─────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Grid {
    columns: Vec<String>,
    rows: Vec<Vec<Val>>,
    note: String,
}

impl Grid {
    fn from_query(columns: Vec<String>, rows: &[Vec<serde_json::Value>], note: String) -> Self {
        Self {
            columns,
            rows: rows
                .iter()
                .map(|row| row.iter().map(value_to_val).collect())
                .collect(),
            note,
        }
    }
}

/// One statement inside a multi-statement script run.
#[derive(Clone)]
struct StmtOutcome {
    sql: String,
    grid: Grid,
    error: Option<String>,
    affected: u64,
    ms: u128,
}

#[derive(Clone)]
struct ScriptView {
    outcomes: Vec<StmtOutcome>,
    sel: usize,
    drilled: Option<usize>,
}

/// One saved query result the user can flip back to with `[` / `]` (DBX keeps
/// a result tab per run; this is the TUI equivalent for query results).
#[derive(Clone)]
struct ResultTab {
    /// Short label (the first line of the SQL, trimmed).
    title: String,
    grid: Option<Grid>,
    /// Unfiltered grid, so the column-visibility filter can be re-applied.
    grid_full: Option<Grid>,
    script: Option<ScriptView>,
    kind: GridKind,
    sel: usize,
    col_offset: usize,
    col_cursor: usize,
}

#[derive(Clone)]
struct PageState {
    table: String,
    table_type: Option<String>,
    page: usize,
    page_size: usize,
    total: Option<u64>,
    has_next: bool,
    /// Active WHERE predicate (without the `WHERE` keyword); empty = no filter.
    filter: String,
    /// Active ORDER BY expression (without the `ORDER BY` keyword).
    order_by: Option<String>,
}

/// Column metadata for the table currently open in the data browser. Used to
/// build `UPDATE`/`INSERT` templates (primary-key detection, value typing).
#[derive(Clone)]
struct TableMeta {
    table: String,
    columns: Vec<ColumnInfo>,
}

/// One paginated table-data request (first load, page turn, filter or sort).
struct TableDataReq {
    cfg: Box<ConnectionConfig>,
    db: String,
    table: String,
    table_type: Option<String>,
    page: usize,
    page_size: usize,
    filter: String,
    order_by: Option<String>,
    /// Reuse a session-cached total instead of running COUNT(*) again.
    known_total: Option<u64>,
    /// Monotonic request id; a reply whose id is not the latest is discarded.
    gen: u64,
}

#[derive(Clone)]
struct Confirm {
    sql: String,
    reasons: Vec<String>,
    /// True when a successful run should refresh the current table page in place
    /// (row edits, deletes, queued batches) instead of replacing the grid.
    refresh: bool,
    /// True when accepting the confirmation should also drain the queued batch.
    clear_batch: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Update,
    Insert,
}

/// A diff-style confirmation layer for a generated write. UPDATE edits let the
/// new value be typed inline; INSERT shows the row that is about to be added.
#[derive(Clone)]
struct EditDialog {
    kind: EditKind,
    cfg: Box<ConnectionConfig>,
    db: String,
    table: String,
    // UPDATE fields
    column: String,
    data_type: Option<String>,
    old: Val,
    new_input: TextArea<'static>,
    where_clause: String,
    keys: Vec<String>,
    no_pk: bool,
    // INSERT fields
    insert_sql: String,
    insert_preview: Vec<(String, String)>,
}

/// One logical line of a modal text popup together with the style its value
/// deserves (NULL → grey italic, empty string → grey, ordinary text → plain).
#[derive(Clone)]
struct PopupLine {
    text: String,
    style: Style,
}

/// A modal showing one cell's full, untruncated value.
#[derive(Clone)]
struct CellPopup {
    title: String,
    lines: Vec<PopupLine>,
    scroll: u16,
}

/// A modal showing every column of the focused row, one per line.
#[derive(Clone)]
struct RowPopup {
    title: String,
    lines: Vec<PopupLine>,
    scroll: u16,
}

/// SQL prefix-completion popup in the editor (Ctrl-Space). Tab / Enter accept,
/// Esc cancels; typing keeps refining the candidate list.
#[derive(Clone)]
struct CompletionItem {
    text: String,
    /// `T` table, `C` column, `K` keyword — shown in the popup.
    kind: char,
}

#[derive(Clone)]
struct Completion {
    items: Vec<CompletionItem>,
    sel: usize,
    /// Characters immediately before the cursor that are replaced when a
    /// candidate is accepted (the fragment after the last `.` when qualified).
    replace: usize,
}

/// The context the cursor sits in, used to order / restrict candidates.
#[derive(Clone, PartialEq, Debug)]
enum CompCtx {
    /// After `table.` — only that table's columns.
    Qualified(String),
    /// After `FROM ` / `JOIN ` / `INTO ` — tables first.
    TableList,
    /// After `WHERE ` / `ON ` / `SET ` … — columns first.
    Column,
    /// Anything else — the historical columns → tables → keywords order.
    Any,
}

// ─── persistent TUI config (~/.config/dbxt/tui.json) ─────────────────────────

/// Per `(database, table)` preferences restored when the table is reopened.
#[derive(Clone, Default, PartialEq)]
struct TablePrefs {
    /// Columns hidden for this table (DBX column-visibility equivalent).
    hidden: HashSet<String>,
    /// Compact-column choice; `None` = follow the global default.
    compact: Option<bool>,
    /// Last ORDER BY expression (without the `ORDER BY` keyword).
    order_by: Option<String>,
}

/// The whole on-disk config. Parsing is deliberately forgiving: a missing file,
/// an unknown version, a truncated body or a single malformed table entry all
/// fall back to defaults instead of failing to start.
#[derive(Clone, Default)]
struct TuiConfig {
    /// Global compact-column default, used when a table has no stored choice.
    compact: Option<bool>,
    tables: HashMap<(String, String), TablePrefs>,
    /// Whether this session changed the global compact default. Untouched
    /// globals are left to whatever another session last wrote.
    dirty_global: bool,
    /// `(database, table)` entries this session actually changed. Saving merges
    /// only these into the on-disk file, so two dbxt sessions (or a hand-edit)
    /// no longer clobber each other's tables; an entry reset to defaults is
    /// removed instead of silently surviving.
    dirty: HashSet<(String, String)>,
}

impl TuiConfig {
    /// Read `path`, tolerating every kind of corruption (returns defaults).
    fn load(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let mut cfg = Self {
            compact: v.get("compact").and_then(|b| b.as_bool()),
            ..Self::default()
        };
        if let Some(tables) = v.get("tables").and_then(|t| t.as_object()) {
            for (db, by_table) in tables {
                let Some(by_table) = by_table.as_object() else {
                    continue;
                };
                for (table, prefs) in by_table {
                    let Some(prefs) = prefs.as_object() else {
                        continue;
                    };
                    let hidden = prefs
                        .get("hidden")
                        .and_then(|h| h.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let compact = prefs.get("compact").and_then(|b| b.as_bool());
                    let order_by = prefs
                        .get("order_by")
                        .and_then(|o| o.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string);
                    cfg.tables.insert(
                        (db.clone(), table.clone()),
                        TablePrefs {
                            hidden,
                            compact,
                            order_by,
                        },
                    );
                }
            }
        }
        cfg
    }

    /// Write the config back, merging with whatever is on disk so two dbxt
    /// sessions (or an external editor) do not clobber each other: only the
    /// `(database, table)` entries this session actually changed are applied,
    /// and a cleared entry is removed. Best-effort: a read-only config dir must
    /// never interrupt the TUI.
    fn save(&self, path: &std::path::Path) {
        // Start from the current on-disk state so another session's tables are
        // preserved; a missing / corrupt file simply means we start from scratch.
        let mut merged = TuiConfig::load(path);
        if self.dirty_global {
            merged.compact = self.compact;
        }
        for key in &self.dirty {
            let all_default = self
                .tables
                .get(key)
                .map(|p| p.hidden.is_empty() && p.compact.is_none() && p.order_by.is_none())
                .unwrap_or(true);
            if all_default {
                merged.tables.remove(key);
            } else if let Some(prefs) = self.tables.get(key) {
                merged.tables.insert(key.clone(), prefs.clone());
            }
        }
        merged.write(path);
    }

    /// Serialize `self` (the already-merged state) to `path`, creating the parent
    /// directory. Best-effort and silent on failure.
    fn write(&self, path: &std::path::Path) {
        let mut tables = serde_json::Map::new();
        let mut keys: Vec<&(String, String)> = self.tables.keys().collect();
        keys.sort();
        for (db, table) in keys {
            let prefs = &self.tables[&(db.clone(), table.clone())];
            if prefs.hidden.is_empty() && prefs.compact.is_none() && prefs.order_by.is_none() {
                continue;
            }
            let mut hidden: Vec<&String> = prefs.hidden.iter().collect();
            hidden.sort();
            let mut entry = serde_json::Map::new();
            entry.insert(
                "hidden".into(),
                serde_json::Value::Array(
                    hidden
                        .iter()
                        .map(|c| serde_json::Value::String((*c).clone()))
                        .collect(),
                ),
            );
            if let Some(c) = prefs.compact {
                entry.insert("compact".into(), serde_json::Value::Bool(c));
            }
            if let Some(o) = &prefs.order_by {
                entry.insert("order_by".into(), serde_json::Value::String(o.clone()));
            }
            let by_table = tables
                .entry(db.clone())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(map) = by_table.as_object_mut() {
                map.insert(table.clone(), serde_json::Value::Object(entry));
            }
        }
        let mut root = serde_json::Map::new();
        root.insert("version".into(), serde_json::Value::from(1));
        if let Some(c) = self.compact {
            root.insert("compact".into(), serde_json::Value::Bool(c));
        }
        root.insert("tables".into(), serde_json::Value::Object(tables));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::Value::Object(root).to_string();
        // Atomic-ish: write a sibling temp file then rename so a crash never
        // leaves a half-written config behind. The pid keeps two concurrent
        // dbxt processes from fighting over the same temp path.
        let tmp = path.with_extension(format!("{}.tmp",  std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn table(&self, db: &str, table: &str) -> Option<&TablePrefs> {
        self.tables.get(&(db.to_string(), table.to_string()))
    }

    /// Mutable access that records the entry as changed by this session.
    fn entry(&mut self, db: &str, table: &str) -> &mut TablePrefs {
        let key = (db.to_string(), table.to_string());
        self.dirty.insert(key.clone());
        self.tables.entry(key).or_default()
    }

    /// Set the global compact default and mark it changed by this session.
    fn set_compact(&mut self, value: Option<bool>) {
        self.compact = value;
        self.dirty_global = true;
    }
}

/// Resolve the config file path. `DBXT_CONFIG` overrides it (tests / portable
/// setups), `DBXT_NO_PERSIST=1` disables persistence entirely.
fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DBXT_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("DBXT_NO_PERSIST").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("dbxt").join("tui.json"))
}

// ─── text helpers ────────────────────────────────────────────────────────────

fn disp_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `max` display columns, appending `…` when content was dropped.
fn truncate_disp(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if disp_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reverse CP1252→UTF-8 double-encoding in an identifier, for display only.
///
/// A MySQL client that writes through the wrong connection charset
/// (latin1/CP1252) stores each byte of the correct UTF-8 sequence as a separate
/// CP1252 character. dbx-core already reverses this for cell values and table
/// comments (`fix_potential_double_encoding` in `db/db/mysql.rs`) but not for
/// table / database / column names, so dbxt applies the same reversal when
/// *rendering* identifiers. The raw name is always what is sent to the server,
/// and correctly stored CJK names (chars > U+00FF) pass through untouched.
fn fix_double_encoding(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let byte = match c as u32 {
            0x20AC => 0x80,
            0x201A => 0x82,
            0x0192 => 0x83,
            0x201E => 0x84,
            0x2026 => 0x85,
            0x2020 => 0x86,
            0x2021 => 0x87,
            0x02C6 => 0x88,
            0x2030 => 0x89,
            0x0160 => 0x8A,
            0x2039 => 0x8B,
            0x0152 => 0x8C,
            0x017D => 0x8E,
            0x2018 => 0x91,
            0x2019 => 0x92,
            0x201C => 0x93,
            0x201D => 0x94,
            0x2022 => 0x95,
            0x2013 => 0x96,
            0x2014 => 0x97,
            0x02DC => 0x98,
            0x2122 => 0x99,
            0x0161 => 0x9A,
            0x203A => 0x9B,
            0x0153 => 0x9C,
            0x017E => 0x9E,
            0x0178 => 0x9F,
            v if v <= 0xFF => v as u8,
            _ => return s.to_string(),
        };
        bytes.push(byte);
    }
    match String::from_utf8(bytes) {
        Ok(decoded) if decoded.chars().any(|c| c > '\u{00FF}') => decoded,
        _ => s.to_string(),
    }
}

/// Hard-wrap text to `width` display columns, returning physical lines.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut w = 0usize;
        for c in line.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
            if w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Wrap a multi-line SQL statement for display, dropping blank lines so the
/// preview stays compact.
fn wrap_sql_lines(sql: &str, width: usize) -> Vec<String> {
    wrap_text(sql, width)
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .collect()
}

// ─── dangerous-statement detection ───────────────────────────────────────────

/// Strip SQL string literals and comments so keyword scans cannot be fooled by
/// `'where'` inside a literal or a commented-out clause.
fn strip_sql_noise(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' | '"' | '`' => {
                let quote = c;
                out.push(' ');
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' && quote != '`' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if i + 1 < chars.len() && chars[i + 1] == '-' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                out.push(' ');
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
                out.push(' ');
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn has_keyword(cleaned_lower: &str, keyword: &str) -> bool {
    cleaned_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == keyword)
}

/// Returns a human-readable reason when a statement is destructive enough to
/// deserve a confirmation prompt before it runs.
fn detect_danger(statement: &str) -> Option<String> {
    let cleaned = strip_sql_noise(statement);
    let lower = cleaned.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let first = lower
        .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .find(|w| !w.is_empty())
        .unwrap_or("");
    match first {
        "drop" => Some(t("DROP 会永久删除对象").to_string()),
        "truncate" => Some(t("TRUNCATE 会清空整张表且不可回滚").to_string()),
        "update" | "delete" => {
            if !has_keyword(&lower, "where") {
                Some(tf("{} 没有 WHERE 子句，会作用于整张表", &[&(first.to_ascii_uppercase())]))
            } else {
                None
            }
        }
        // A common-table-expression statement can hide a destructive DELETE
        // (`WITH x AS (...) DELETE FROM t`), which has no leading DELETE keyword.
        "with" if has_keyword(&lower, "delete") && !has_keyword(&lower, "where") => {
            Some(t("DELETE 没有 WHERE 子句，会作用于整张表").to_string())
        }
        _ => None,
    }
}

// ─── async ops ───────────────────────────────────────────────────────────────

enum Op {
    ListConnections,
    Databases(Box<ConnectionConfig>),
    ListTables(Box<ConnectionConfig>, String),
    Columns(Box<ConnectionConfig>, String, String),
    Ddl(Box<ConnectionConfig>, String, String),
    TableData(Box<TableDataReq>),
    TableColumns(Box<ConnectionConfig>, String, String),
    Query(Box<ConnectionConfig>, String, String, usize),
    Redis(Box<ConnectionConfig>, u32, String),
    Mongo(Box<ConnectionConfig>, String, String),
    History(Box<ConnectionConfig>),
    Snippets(Box<ConnectionConfig>),
    /// Save the editor's SQL into DBX's `saved_sql_files` (query favourites).
    SaveSnippet(Box<ConnectionConfig>, String, String),
    DatabasesRefresh(Box<ConnectionConfig>),
    AddConn(Box<ConnectionConfig>),
}

impl Op {
    /// Watchdog for this call. SQL statements already carry a 60 s driver
    /// statement timeout; the ceiling here is only the last resort against a
    /// server that never answers, so it is generous for a (possibly
    /// multi-statement) script and tighter for calls that should be instant.
    fn watchdog(&self) -> Duration {
        match self {
            Op::Query(..) => OP_WATCHDOG_SQL,
            _ => OP_WATCHDOG_FALLBACK,
        }
    }
}

enum OpResult {
    Connections(Vec<ConnectionConfig>),
    Databases {
        databases: Vec<String>,
        /// Set when the driver could not enumerate databases but the
        /// connection's configured database is still usable — surfaced so the
        /// failure is never silent.
        warning: Option<String>,
    },
    Tables(Vec<TableInfo>),
    Columns {
        table: String,
        columns: Vec<ColumnInfo>,
    },
    Ddl {
        table: String,
        text: String,
    },
    TableData {
        grid: Box<Grid>,
        total: Option<u64>,
        has_next: bool,
        page: usize,
        table: String,
        table_type: Option<String>,
        filter: String,
        order_by: Option<String>,
        gen: u64,
    },
    TableColumns {
        table: String,
        columns: Vec<ColumnInfo>,
    },
    Query(Box<dbx_core::db::QueryResult>, String, usize),
    Script(Vec<StmtOutcome>),
    Redis(String),
    Mongo(String),
    History(Vec<String>),
    Snippets(Vec<(String, String)>),
    SnippetSaved(String),
    DatabasesRefresh(Vec<String>),
    Added(String),
    Error(String),
}

fn note_of(r: &dbx_core::db::QueryResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !r.columns.is_empty() {
        parts.push(tf("{} 行", &[&(r.rows.len())]));
    } else {
        // DML / DDL: report the affected-row count even when it is zero.
        parts.push(tf("影响 {} 行", &[&(r.affected_rows)]));
    }
    if r.truncated {
        parts.push(t("已截断").into());
    }
    parts.push(format!("{}ms",  r.execution_time_ms));
    parts.join(" · ")
}

/// UTC timestamp in the RFC3339 shape DBX writes into `saved_sql_files`
/// (`2026-06-27T00:00:00Z`). Avoids pulling in a date crate for one string.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn stmt_outcome(sql: String, b: BatchStatementResult) -> StmtOutcome {
    let BatchStatementResult {
        result,
        execution_error,
        error_message,
        ..
    } = b;
    let error = if execution_error {
        error_message.or_else(|| {
            result
                .rows
                .first()
                .and_then(|row| row.first())
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
    } else {
        None
    };
    let affected = result.affected_rows;
    let ms = result.execution_time_ms;
    let note = if error.is_some() {
        format!("{ms}ms")
    } else {
        note_of(&result)
    };
    let grid = Grid::from_query(result.columns, &result.rows, note);
    StmtOutcome {
        sql,
        grid,
        error,
        affected,
        ms,
    }
}

async fn run_op(backend: &LocalBackend, op: Op) -> OpResult {
    match op {
        Op::ListConnections => match backend.load_connections().await {
            Ok(cs) => OpResult::Connections(cs),
            Err(e) => OpResult::Error(format!("load connections: {e}")),
        },
        Op::Databases(cfg) => match backend.list_databases(&cfg).await {
            Ok(dbs) if !dbs.is_empty() => OpResult::Databases {
                databases: dbs,
                warning: None,
            },
            // A backend that legitimately exposes no database list (e.g. SQLite)
            // still connects using the configured database.
            Ok(_) => OpResult::Databases {
                databases: vec![cfg.database.clone().unwrap_or_default()],
                warning: None,
            },
            Err(e) => OpResult::Databases {
                databases: vec![cfg.database.clone().unwrap_or_default()],
                warning: Some(match cfg.database.as_deref() {
                    Some(db) if !db.is_empty() => {
                        tf("无法列举数据库（{}），仅使用配置库 {}", &[&(e), &(db)])
                    }
                    _ => tf("无法列举数据库（{}），将使用连接默认库", &[&(e)]),
                }),
            },
        },
        Op::ListTables(cfg, db) => match backend.list_tables(&cfg, &db, "").await {
            Ok(t) => OpResult::Tables(t),
            Err(e) => OpResult::Error(format!("list tables: {e}")),
        },
        Op::Columns(cfg, db, table) => match backend.get_columns(&cfg, &db, "", &table).await {
            Ok(c) => OpResult::Columns { table, columns: c },
            Err(e) => OpResult::Error(format!("columns: {e}")),
        },
        Op::Ddl(cfg, db, table) => {
            match dbx_core::schema::get_table_ddl_core(backend.state().as_ref(), &cfg.id, &db, "", &table, None)
                .await
            {
                Ok(ddl) => OpResult::Ddl { table, text: ddl },
                Err(e) => OpResult::Ddl {
                    table,
                    text: tf("-- 无法获取 DDL: {}", &[&(e)]),
                },
            }
        }
        Op::TableData(req) => {
            let TableDataReq {
                cfg,
                db,
                table,
                table_type,
                page,
                page_size,
                filter,
                order_by,
                known_total,
                gen,
            } = *req;
            // A user filter may be typed with a leading WHERE; strip it so it can
            // be embedded as a predicate.
            let filter = normalize_where_input(Some(&filter));
            let options = TableDataSelectSqlOptions {
                database_type: Some(cfg.db_type),
                table_name: table.clone(),
                table_type: table_type.clone(),
                limit: Some(page_size + 1),
                offset: Some(page * page_size),
                where_input: (!filter.is_empty()).then(|| filter.clone()),
                order_by: order_by.clone(),
                ..Default::default()
            };
            let sql = build_table_data_select_sql_with_database(options, false);
            match backend
                .execute_query(&cfg, &db, &sql, Some(page_size + 1), Some(60))
                .await
            {
                Ok(r) => {
                    let columns = r.columns;
                    let mut rows = r.rows;
                    let ms = r.execution_time_ms;
                    let has_next = rows.len() > page_size;
                    rows.truncate(page_size);
                    let grid = Grid::from_query(columns, &rows, format!("{ms}ms"));
                    // COUNT(*) is a full scan on large tables; reuse the session
                    // cache and only run it when the caller has no cached total.
                    let total = match known_total {
                        Some(t) => Some(t),
                        None => {
                            let base = build_count_table_sql(Some(cfg.db_type), None, &table);
                            let count_sql = if filter.is_empty() {
                                base
                            } else {
                                format!("{base} WHERE ({filter})")
                            };
                            match backend
                                .execute_query(&cfg, &db, &count_sql, Some(1), Some(15))
                                .await
                            {
                                Ok(c) => c
                                    .rows
                                    .first()
                                    .and_then(|row| row.first())
                                    .and_then(|v| match v {
                                        serde_json::Value::Number(n) => n.as_u64(),
                                        serde_json::Value::String(s) => s.parse().ok(),
                                        _ => None,
                                    }),
                                Err(_) => None,
                            }
                        }
                    };
                    OpResult::TableData {
                        grid: Box::new(grid),
                        total,
                        has_next,
                        page,
                        table,
                        table_type,
                        filter,
                        order_by,
                        gen,
                    }
                }
                Err(e) => OpResult::Error(format!("table data: {e}")),
            }
        }
        Op::TableColumns(cfg, db, table) => match backend.get_columns(&cfg, &db, "", &table).await {
            Ok(columns) => OpResult::TableColumns { table, columns },
            Err(e) => OpResult::Error(format!("table columns: {e}")),
        },
        Op::Query(cfg, db, sql, cap) => {
            let cap = cap.max(1);
            let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
            if statements.len() > 1 {
                let options = QueryExecutionOptions {
                    max_rows: Some(cap),
                    timeout_secs: Some(60),
                    ..Default::default()
                };
                match backend.execute_batch(&cfg, &db, None, &sql, options).await {
                    Ok(results) => {
                        let mut outcomes: Vec<StmtOutcome> = Vec::new();
                        for (idx, r) in results.into_iter().enumerate() {
                            let text = statements
                                .get(idx)
                                .cloned()
                                .unwrap_or_else(|| format!("-- statement {}",  idx + 1));
                            outcomes.push(stmt_outcome(text, r));
                        }
                        OpResult::Script(outcomes)
                    }
                    Err(e) => OpResult::Error(format!("script: {e}")),
                }
            } else {
                match backend
                    .execute_query(&cfg, &db, &sql, Some(cap), Some(60))
                    .await
                {
                    Ok(r) => OpResult::Query(Box::new(r), sql, cap),
                    Err(e) => OpResult::Error(format!("query: {e}")),
                }
            }
        }
        Op::Redis(cfg, db, cmd) => {
            match backend.execute_redis_command(&cfg, db, &cmd, true).await {
                // skip_safety_check = true: this is an interactive human console (like the DBX
                // desktop Redis console, which defaults `blockDangerousRedisCommands` to false).
                // Without it, dbx-core's allowlist blocks ordinary commands such as KEYS.
                Ok(r) => OpResult::Redis(match serde_json::to_string_pretty(&r.value) {
                    Ok(s) => s,
                    Err(_) => format!("{:?}",  r.value),
                }),
                Err(e) => OpResult::Error(format!("redis: {e}")),
            }
        }
        Op::Mongo(cfg, db, source) => match dbx_core::mongo_shell::parse(&source) {
            Ok(cmd) => match backend.execute_mongo_command(&cfg, &db, &cmd).await {
                Ok(r) => {
                    let mut rows = String::new();
                    for row in r.rows.iter().take(50) {
                        let line: Vec<String> = row.iter().map(value_to_val).map(|v| v.text().to_string()).collect();
                        rows.push_str(&line.join("  "));
                        rows.push('\n');
                    }
                    OpResult::Mongo(if rows.is_empty() { note_of(&r) } else { rows })
                }
                Err(e) => OpResult::Error(format!("mongo: {e}")),
            },
            Err(e) => OpResult::Error(tf("mongo parse: {} (例: db.col.find({{}}))", &[&(e)])),
        },
        Op::History(cfg) => {
            match backend
                .state()
                .storage
                .load_history_entries(300, 0, Some("query".to_string()))
                .await
            {
                Ok(entries) => {
                    let mut seen: Vec<String> = Vec::new();
                    for e in entries {
                        if e.connection_id != cfg.id {
                            continue;
                        }
                        let sql = e.sql.trim().to_string();
                        if sql.is_empty() || seen.contains(&sql) {
                            continue;
                        }
                        seen.push(sql);
                    }
                    seen.reverse(); // oldest first, so ↑ walks backwards through time
                    OpResult::History(seen)
                }
                Err(_) => OpResult::History(Vec::new()),
            }
        }
        Op::Snippets(cfg) => match backend.state().storage.load_saved_sql_library().await {
            Ok(lib) => {
                let folder_name = |id: &Option<String>| -> Option<String> {
                    let id = id.as_deref()?;
                    lib.folders
                        .iter()
                        .find(|f| f.id == id)
                        .map(|f| f.name.clone())
                };
                let mut items: Vec<(String, String)> = lib
                    .files
                    .into_iter()
                    // Only this connection's snippets, plus unscoped ones, so the
                    // overlay never mixes another database's SQL.
                    .filter(|f| f.connection_id.is_empty() || f.connection_id == cfg.id)
                    .map(|f| {
                        let label = match folder_name(&f.folder_id) {
                            Some(folder) if !folder.is_empty() => format!("{folder}/{}",  f.name),
                            _ => f.name.clone(),
                        };
                        (label, f.sql)
                    })
                    .collect();
                items.sort_by_key(|a| a.0.to_lowercase());
                OpResult::Snippets(items)
            }
            Err(e) => OpResult::Error(format!("snippets: {e}")),
        },
        Op::SaveSnippet(cfg, name, sql) => {
            let now = now_iso8601();
            let file = dbx_core::saved_sql::SavedSqlFile {
                id: Uuid::new_v4().to_string(),
                connection_id: cfg.id.clone(),
                folder_id: None,
                name: name.clone(),
                database: cfg.database.clone().unwrap_or_default(),
                catalog: None,
                schema: None,
                sql,
                sql_loaded: true,
                order_index: 0,
                open_count: 0,
                opened_at: None,
                created_at: now.clone(),
                updated_at: now,
            };
            match backend.state().storage.save_saved_sql_file(&file).await {
                Ok(()) => OpResult::SnippetSaved(name),
                Err(e) => OpResult::Error(format!("save snippet: {e}")),
            }
        }
        Op::DatabasesRefresh(cfg) => match backend.list_databases(&cfg).await {
            Ok(dbs) => OpResult::DatabasesRefresh(dbs),
            Err(e) => OpResult::Error(format!("databases: {e}")),
        },
        Op::AddConn(cfg) => match backend.add_connection_for_mcp(*cfg).await {
            Ok(saved) => OpResult::Added(tf("已保存: {} ({})", &[&(saved.name), &(saved.db_type.as_str())])),
            Err(e) => OpResult::Error(format!("save: {e}")),
        },
    }
}

fn spawn_op(backend: &Arc<LocalBackend>, tx: &Tx, op: Op) {
    let backend = backend.clone();
    let tx = tx.clone();
    let limit = op.watchdog();
    tokio::spawn(async move {
        // The watchdog is the last resort: a server that accepts the socket but
        // never answers must surface an error, not a spinner that never stops.
        let res = match tokio::time::timeout(limit, run_op(&backend, op)).await {
            Ok(r) => r,
            Err(_) => OpResult::Error(tf("操作超时（{}s）· 服务器无响应或网络中断，请检查连接后用 d 重连", &[&(limit.as_secs())])),
        };
        let _ = tx.send(res);
    });
}

// ─── app state ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ConnForm {
    name: String,
    db_type: String,
    host: String,
    port: String,
    username: String,
    password: String,
    database: String,
    ssl: bool,
    field: usize, // 0..8; 7=ssl 8=save
    editing: bool,
    err: String,
}

impl Default for ConnForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            db_type: "mysql".into(),
            host: String::new(),
            port: String::new(),
            username: String::new(),
            password: String::new(),
            database: String::new(),
            ssl: false,
            field: 0,
            editing: false,
            err: String::new(),
        }
    }
}

fn form_fields() -> [&'static str; 9] {
    [
        "name", "db_type", "host", "port", "username", "password", "database", "ssl", t("保存"),
    ]
}

#[derive(Default, Clone, Copy)]
struct Rects {
    sidebar: Rect,
    editor: Rect,
    cmd: Rect,
    results: Rect,
    picker: Rect,
    picker_visible: bool,
    db_picker: Rect,
    db_picker_visible: bool,
    /// Clickable horizontal scrollbar track (bottom border of the result grid).
    hbar: Rect,
    hbar_visible: bool,
    /// Tap targets for the `◀` / `▶` pan buttons drawn at either end of the bar.
    hbar_prev: Rect,
    hbar_next: Rect,
}

// ─── touch / swipe gesture layer ─────────────────────────────────────────────

/// How a horizontal swipe is recognised, from `DBXT_DRAG_PAN`:
///
/// * `button` (default) — a held left button that moves (`MouseEventKind::Drag`),
///   which is how phone terminals encode a left/right swipe when they do not
///   emit a horizontal wheel at all;
/// * `any` — also treat bare motion (`MouseEventKind::Moved`) as a swipe, for
///   terminals that report a touch drag without a button; a desktop mouse also
///   sends `Moved` constantly, so this is opt-in only;
/// * `off` — no swipe handling (wheel and keys only).
#[derive(Clone, Copy, PartialEq, Debug)]
enum DragPan {
    Off,
    Button,
    Any,
}

impl DragPan {
    fn from_env() -> Self {
        Self::parse(&std::env::var("DBXT_DRAG_PAN").unwrap_or_default())
    }

    fn parse(v: &str) -> Self {
        match v.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "no" | "none" | "false" => DragPan::Off,
            "any" | "all" | "motion" | "moved" | "touch" => DragPan::Any,
            _ => DragPan::Button,
        }
    }
}

/// Finger columns of travel that make up one pan step (2:1 keeps a slow swipe
/// moving without making a fast flick jump the whole table away).
const DRAG_COLS_PER_STEP: i32 = 2;
/// A gesture that travelled this far is a swipe, not a tap.
const DRAG_TAP_SLOP: i32 = 2;
/// Cap on the columns one (possibly coalesced) drag event may pan.
const DRAG_MAX_STEPS: i32 = 4;

/// Turn finger travel into whole pan steps, carrying the remainder so a slow
/// swipe still moves the window instead of being rounded away.
fn pan_steps(accum: &mut i32, dx: i32) -> i32 {
    let cap = DRAG_MAX_STEPS * DRAG_COLS_PER_STEP;
    *accum = (*accum + dx).clamp(-cap, cap);
    let steps = *accum / DRAG_COLS_PER_STEP;
    *accum -= steps * DRAG_COLS_PER_STEP;
    steps
}

/// State machine that turns "button held + moved horizontally" into column pans.
/// A touch left/right swipe reaches us as `Drag(Left)` (button-event tracking) or
/// `Moved` (any-event tracking); a horizontal wheel (`ScrollLeft`/`ScrollRight`)
/// is a different, much rarer encoding, so both paths exist.
#[derive(Default, Clone, Copy)]
struct PanGesture {
    /// position of the previous mouse event, for the travel delta
    last: Option<(u16, u16)>,
    /// button currently held (a `Down` was seen)
    held: Option<MouseButton>,
    /// travel not yet converted into pan steps
    accum: i32,
    /// the gesture has travelled far enough to be a swipe, not a tap
    moved: bool,
    /// the terminal has sent at least one `Up`; only then may a tap be deferred
    saw_up: bool,
}

impl PanGesture {
    /// Feed one mouse event.
    ///
    /// `None` means "not part of a swipe, handle it normally"; `Some(steps)` means
    /// the event belongs to a swipe and the caller should swallow it, panning the
    /// column window by `steps` columns (`0` = swipe, but no step completed yet).
    fn feed(&mut self, kind: MouseEventKind, col: u16, row: u16, mode: DragPan) -> Option<i32> {
        let prev = self.last.replace((col, row));
        match kind {
            MouseEventKind::Down(b) => {
                self.held = Some(b);
                self.accum = 0;
                self.moved = false;
                None
            }
            MouseEventKind::Up(_) => {
                self.held = None;
                self.accum = 0;
                self.moved = false;
                self.saw_up = true;
                None
            }
            MouseEventKind::Drag(b) => {
                if mode == DragPan::Off || b != MouseButton::Left {
                    return None;
                }
                match self.held {
                    Some(MouseButton::Left) => {}
                    // another button owns this gesture (e.g. desktop text selection)
                    Some(_) => return None,
                    // Some terminals drop the press and only report the drag; start
                    // the gesture from the first drag event in that case.
                    None => {
                        self.held = Some(MouseButton::Left);
                        self.accum = 0;
                        self.moved = false;
                    }
                }
                self.travel(prev, col, row)
            }
            MouseEventKind::Moved => {
                let touching = match mode {
                    DragPan::Off => return None,
                    DragPan::Any => true,
                    DragPan::Button => self.held == Some(MouseButton::Left),
                };
                if !touching {
                    return None;
                }
                self.travel(prev, col, row)
            }
            _ => None,
        }
    }

    fn travel(&mut self, prev: Option<(u16, u16)>, col: u16, row: u16) -> Option<i32> {
        let (pc, pr) = prev?;
        let dx = col as i32 - pc as i32;
        let dy = row as i32 - pr as i32;
        if dx.abs() >= DRAG_TAP_SLOP || dy.abs() >= DRAG_TAP_SLOP {
            // The finger travelled: whatever happens next, this was not a tap.
            self.moved = true;
        }
        if dy.abs() > dx.abs() {
            // Vertical-dominant travel is not a horizontal pan: rows keep moving
            // through the wheel, which every terminal reports.
            self.accum = 0;
            return Some(0);
        }
        Some(pan_steps(&mut self.accum, dx))
    }

    /// True once the gesture travelled far enough that it must not also click.
    fn is_swipe(&self) -> bool {
        self.moved
    }

    /// A tap may only be deferred to its `Up` on a terminal that sends `Up`
    /// events; otherwise the press itself has to click or tapping would break.
    fn can_defer_tap(&self) -> bool {
        self.saw_up
    }
}

/// Panes that can be collapsed in the responsive layout.
const PANE_SIDEBAR: usize = 0;
const PANE_EDITOR: usize = 1;
const PANE_RESULTS: usize = 2;

struct App {
    backend: Arc<LocalBackend>,
    page: Page,
    focus: Focus,
    quit: bool,

    connections: Vec<ConnectionConfig>,
    conn_list: ListState,
    picker_open: bool,

    selected: Option<ConnectionConfig>,
    databases: Vec<String>,
    db_index: usize,

    tables: Vec<TableInfo>,
    table_list: ListState,
    /// Unfiltered table list; `tables` is this list with the `/` filter applied.
    tables_all: Vec<TableInfo>,
    /// Active sidebar table-name filter (`/`, filter-as-you-type).
    table_filter: String,
    table_prompt: Option<TextArea<'static>>,

    // table structure
    columns: Vec<ColumnInfo>,
    ddl: Option<String>,
    struct_view: StructView,
    ddl_scroll: u16,

    editor: TextArea<'static>,
    history: Vec<String>,
    history_idx: Option<usize>,
    history_draft: String,

    // results
    grid: Option<Grid>,
    grid_kind: GridKind,
    page_state: Option<PageState>,
    script: Option<ScriptView>,
    sel: usize,       // cursor row inside the current page / result set
    col_offset: usize, // leftmost column of the scrollable window
    col_cursor: usize, // focused column (cell cursor)
    vis_cols: usize,   // columns currently visible (set while rendering)
    /// Width cap actually used for the last render (compact mode aware).
    grid_max_cell: usize,
    freeze_first: bool, // pin the first data column (row-number gutter is always pinned)
    cell_popup: Option<CellPopup>,
    row_popup: Option<RowPopup>,

    // ── mobile efficiency ──
    /// Compact column-width mode (`None` = automatic for a narrow terminal).
    compact: Option<bool>,
    /// Column names hidden for this browsing session (Ctrl-Shift-H).
    col_hidden: HashSet<String>,
    col_picker_open: bool,
    col_picker_list: ListState,
    /// The unfiltered grid backing the filtered `grid` (needed to re-show a
    /// hidden column without re-querying).
    grid_full: Option<Grid>,
    /// The five most recently browsed `(database, table)` pairs.
    recent_tables: Vec<(String, String)>,
    recent_open: bool,
    recent_list: ListState,
    /// A table to open as soon as the (new) table list arrives.
    pending_open_table: Option<String>,
    /// SQL prefix-completion popup in the editor (Ctrl-Space).
    completion: Option<Completion>,

    // ── persistent preferences (db.table granularity) ──
    /// Column visibility / compact / sort restored per `(database, table)`.
    config: TuiConfig,
    config_path: Option<PathBuf>,

    // ── result-grid search (`/` in the results pane) ──
    /// The modal input while `/` is being typed (filter-as-you-type).
    result_filter: Option<TextArea<'static>>,
    /// Active search needle; non-empty hides every non-matching row.
    result_needle: String,
    /// Displayed row index → row index in the unfiltered grid (row filter map).
    result_rows: Vec<usize>,
    /// Last SQL sent to the backend, used to guess a table for `y`.
    last_sql: Option<String>,

    // WHERE filter prompt (modal text input)
    filter_prompt: Option<TextArea<'static>>,

    // cell edit dialog: diff-style confirmation before any write is sent
    edit_dialog: Option<EditDialog>,
    // a write is in flight; on success refresh the current page instead of
    // replacing the grid with the DML result
    pending_write: bool,
    // success message kept until the refreshed page lands so it is not lost
    pending_write_msg: Option<String>,
    // queued edits for one transactional batch commit (Ctrl-S)
    batch: Vec<String>,
    // manual per-pane collapse override (None = follow `auto_collapse`)
    pane_override: [Option<bool>; 3],
    // master switch for responsive collapse: on = unfocused panes collapse
    auto_collapse: bool,

    // help overlay
    help_open: bool,
    help_scroll: u16,

    // touch / terminal fallbacks
    //   pan_mode: vertical wheel pans columns instead of rows (for phone terminals
    //   that never emit a horizontal wheel for a left/right swipe).
    pan_mode: bool,
    //   drag_pan: how a swipe is recognised (see `DragPan`).
    drag_pan: DragPan,
    //   gesture: swipe state for the drag → column-pan path.
    gesture: PanGesture,
    //   pending_tap: a results-pane click waiting for its `Up`, so the press that
    //   starts a swipe does not also select a row / jump the scrollbar.
    pending_tap: Option<(u16, u16)>,
    //   When `DBXT_EVENT_TRACE` is set, mouse/resize events are appended here so a
    //   user can report exactly what their terminal sends. Keystrokes are never
    //   traced (a password field would leak).
    trace_path: Option<PathBuf>,
    last_event: Option<String>,
    //   `DBXT_MOUSE_DEBUG`: same log plus a live on-screen event readout, so a
    //   phone user can see what a swipe is encoded as without leaving the TUI.
    mouse_debug: bool,
    mouse_log: VecDeque<String>,

    // successive query results, switchable with `[` / `]`
    result_tabs: Vec<ResultTab>,
    result_tab: usize,
    // the last query that hit the row cap, for `Ctrl-N` "load more"
    query_more: Option<(String, usize)>,

    // saved-SQL snippet overlay (DBX's `saved_sql_files`)
    snippet_open: bool,
    snippet_list: ListState,
    snippets: Vec<(String, String)>,
    /// Name prompt shown when saving the editor's SQL as a DBX favourite.
    snippet_name: Option<TextArea<'static>>,

    // column metadata for the table currently open in the data browser
    table_meta: Option<TableMeta>,
    // session cache of COUNT(*) totals, keyed by db/table/filter
    count_cache: HashMap<String, u64>,

    // pagination hand-off between key handling and the async page load
    pending_sel: Option<usize>,
    // focus to apply when the next page arrives (None = do not steal focus)
    pending_focus: Option<Focus>,
    page_pending: bool,
    // monotonically increasing id of the latest table-data request
    page_gen: u64,

    // database switcher overlay
    db_picker_open: bool,
    db_list: ListState,
    pending_table: Option<String>,

    // grid geometry captured while rendering, used to map clicks back to cells
    grid_gutter: u16,
    grid_frozen: usize,
    grid_widths: Vec<usize>,
    /// width available to the scrollable column window, captured while rendering.
    /// `pan_columns` recomputes the visible-column count from it so the cell
    /// cursor lands inside the window the renderer will actually draw (a stale
    /// count would let `window_for_cursor` undo the pan).
    grid_avail: usize,

    confirm: Option<Confirm>,

    loading: bool,
    /// Backend calls currently in flight. The spinner only stops when this hits
    /// zero, so a fast secondary result (e.g. the history fetch) cannot make a
    /// slow primary one (the table list) look finished.
    pending_ops: usize,
    /// When the oldest in-flight call started, shown as elapsed seconds so a slow
    /// query is visibly progressing rather than apparently hung.
    loading_since: Option<Instant>,
    spinner: usize,
    status: String,

    backend_kind: Backend,
    cmd_input: TextArea<'static>,
    cmd_output: Vec<String>,
    redis_db: u32,

    form: ConnForm,

    layout_mode: LayoutMode,
    term_h: u16,
    rects: Rects,
}

impl App {
    fn selected_name(&self) -> String {
        self.selected
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }
    /// Queue a backend call, keeping the spinner up until *every* in-flight call
    /// has answered.
    fn spawn(&mut self, tx: &Tx, op: Op) {
        self.pending_ops = self.pending_ops.saturating_add(1);
        if self.pending_ops == 1 {
            self.loading_since = Some(Instant::now());
        }
        self.loading = true;
        spawn_op(&self.backend, tx, op);
    }
    fn current_db(&self) -> String {
        self.databases
            .get(self.db_index)
            .cloned()
            .unwrap_or_default()
    }
    fn set_placeholder(&mut self) {
        let t = match self.backend_kind {
            Backend::Redis => tf("redis 命令… (db={}) · Ctrl-L 切换", &[&(self.redis_db)]),
            Backend::Mongo => tf("mongo shell… (db={}) · Ctrl-L 切换", &[&(self.current_db())]),
            Backend::Sql => String::new(),
        };
        self.cmd_input.set_placeholder_text(t);
    }
    fn set_editor_text(&mut self, text: &str) {
        let mut ta = TextArea::from(text.split('\n'));
        ta.set_placeholder_text(t("SQL … (Ctrl-J / F5 执行 · ↑ 历史)"));
        ta.move_cursor(CursorMove::Bottom);
        ta.move_cursor(CursorMove::End);
        self.editor = ta;
    }
    fn editor_sql(&self) -> String {
        self.editor.lines().join("\n")
    }
    fn push_history(&mut self, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        self.history_idx = None;
        if self.history.last().map(|s| s.as_str()) == Some(sql) {
            return;
        }
        self.history.push(sql.to_string());
        if self.history.len() > 500 {
            self.history.remove(0);
        }
    }
    fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.history_idx {
            None => {
                self.history_draft = self.editor_sql();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        self.history_idx = Some(next);
        let text = self.history[next].clone();
        self.set_editor_text(&text);
        true
    }
    fn history_next(&mut self) -> bool {
        let Some(i) = self.history_idx else {
            return false;
        };
        if i + 1 >= self.history.len() {
            self.history_idx = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_editor_text(&draft);
            return true;
        }
        self.history_idx = Some(i + 1);
        let text = self.history[i + 1].clone();
        self.set_editor_text(&text);
        true
    }
    fn selected_table(&self) -> Option<&TableInfo> {
        let idx = self.table_list.selected()?;
        self.tables.get(idx)
    }
}

// ─── main ────────────────────────────────────────────────────────────────────

/// `VAR=1` (or `true`) → the default path under the temp dir, `VAR=<path>` → that
/// path, unset/empty → `None`.
fn env_log_path(var: &str, default_name: &str) -> Option<PathBuf> {
    let v = std::env::var_os(var).filter(|v| !v.is_empty())?;
    let s = v.to_string_lossy().to_string();
    if s == "1" || s.eq_ignore_ascii_case("true") {
        Some(std::env::temp_dir().join(default_name))
    } else {
        Some(PathBuf::from(s))
    }
}

/// Version-selection rule, split out from [`dbxt_version`] so the compile-time
/// injection can be unit-tested: an injected release version wins, an empty one
/// is ignored, and everything else falls back to Cargo.toml.
fn pick_version<'a>(injected: Option<&'a str>, fallback: &'a str) -> &'a str {
    match injected {
        Some(v) if !v.is_empty() => v,
        _ => fallback,
    }
}

/// The version this binary reports.
///
/// Release artifacts get the git tag injected through `DBXT_VERSION` at compile
/// time (see the `build` job in `.github/workflows/release.yml`), so their
/// `--version` matches the tag even though Cargo.toml is not bumped for every
/// release. A plain `cargo build` sees no such variable and falls back to
/// `CARGO_PKG_VERSION`.
fn dbxt_version() -> &'static str {
    pick_version(option_env!("DBXT_VERSION"), env!("CARGO_PKG_VERSION"))
}

/// The exact `--version` line. Kept in one place so the format is tested once.
fn version_line() -> String {
    format!("dbxt {}", dbxt_version())
}

/// `dbxt --help`: a short usage summary. The full manual lives in the README.
///
/// Kept as a plain string (rather than a series of `println!`) so the exact
/// text is unit-testable and so the caller can route it through
/// [`write_stdout`], which tolerates a closed pipe.
fn help_text() -> String {
    format!(
        "dbxt {} — {}\n\n{}: dbxt [DBX_STORE]\n\n{}:\n{}\n\n{}:\n{}\n{}\n\n{}: https://github.com/vst93/dbxt\n",
        dbxt_version(),
        t("DBX 的终端界面"),
        t("用法"),
        t("参数"),
        t("  DBX_STORE  dbx.db 文件或其所在目录（默认：DBX_DATA_DIR 或平台默认位置）"),
        t("选项"),
        t("  -h, --help     显示本帮助"),
        t("  -V, --version  显示版本"),
        t("文档"),
    )
}

/// Write a block of text to stdout, exiting quietly when the reader has gone
/// away (`dbxt --help | head -1`).
///
/// Rust ignores `SIGPIPE` at startup, so a closed pipe surfaces as an `EPIPE`
/// write error instead of a signal. The two conventional fixes are (a) restore
/// `SIG_DFL` so the process dies by signal, or (b) treat `BrokenPipe` as a
/// normal, silent exit. We pick (b): it keeps a *meaningful, zero* exit status
/// for `set -euo pipefail` callers such as `cmd/install.sh`, needs no `unsafe`
/// signal handling, and touches only the non-TUI output paths — the TUI itself
/// is never affected. Any other write error is a genuine failure and
/// propagates, so the process still exits non-zero.
fn write_stdout(text: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => Err(e.into()),
    }
}

/// Best-effort stderr line that never panics (a closed stderr is ignored).
fn write_stderr(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

#[tokio::main]
async fn main() -> Result<()> {
    // Resolve the UI language once, before any text is drawn.
    ui_text::set_lang(ui_text::detect_lang());
    // `--version` / `--help` answer before the TUI is initialised, so they work
    // over a pipe (the install scripts query `--version`) and without a terminal.
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                write_stdout(&format!("{}\n", version_line()))?;
                return Ok(());
            }
            "-h" | "--help" => {
                write_stdout(&help_text())?;
                return Ok(());
            }
            // An unknown option is a usage error (exit 2, like cmd/install.sh):
            // previously `dbxt --foo` was taken as a store path, created a file
            // literally named `--foo`, and then failed inside the TUI.
            s if s.len() > 1 && s.starts_with('-') => {
                write_stderr(&format!(
                    "{}\n{}: dbxt [DBX_STORE]  (-h/--help)\n",
                    tf("未知选项: {}", &[&s]),
                    t("用法"),
                ));
                std::process::exit(2);
            }
            _ => {}
        }
    }
    // The TUI needs a real terminal on stdout; without one ratatui's init()
    // panics (exit 101). Report it cleanly *before* touching the store, so a
    // non-interactive caller gets a sensible non-zero exit code and no side
    // effects (no store file created).
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "{}",
            t("stdout 不是终端，无法启动 TUI（--help / --version 可在管道中使用）")
        );
    }

    // The positional argument is the `dbx.db` file itself. A directory is also
    // accepted (and joined with `dbx.db`) so the historical documented usage
    // keeps working.
    let mut db_path: PathBuf = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => storage_db_path().map_err(|e| anyhow::anyhow!(e))?,
    };
    if db_path.is_dir() {
        db_path = db_path.join("dbx.db");
    }
    if !db_path.exists() {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let backend = Arc::new(LocalBackend::open(&db_path).await.map_err(|e| {
        anyhow::anyhow!("{}", tf("打开 DBX 存储文件失败 ({}): {}\n(可用 DBX_DATA_DIR 指定目录，或把 dbx.db 文件路径作为第一个位置参数传入)", &[&format!("{:?}", db_path), &(e)]))
    })?);

    let terminal = ratatui::init();
    // ratatui 0.29's init() does not enable mouse capture; do it explicitly so the
    // touch (Down) / wheel (Scroll) layer receives events. crossterm's command
    // enables normal (1000), button-event (1002) and any-event (1003) tracking plus
    // SGR encoding (1006), so press, release, `Drag` and bare `Moved` all reach us —
    // the drag path a phone's horizontal swipe needs is therefore live.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run_app(terminal, backend).await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

async fn run_app(mut terminal: ratatui::DefaultTerminal, backend: Arc<LocalBackend>) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(180));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // `DBXT_EVENT_TRACE=<path>` (or `=1` for the default path) records mouse and
    // resize events so a user can tell us exactly what their phone terminal sends.
    // `DBXT_MOUSE_DEBUG=1` does the same, defaults the log to the temp dir, and adds
    // a live on-screen readout.
    let mouse_debug = std::env::var_os("DBXT_MOUSE_DEBUG").is_some_and(|v| !v.is_empty());
    let trace_path = env_log_path("DBXT_EVENT_TRACE", "dbxt-events.log").or_else(|| {
        mouse_debug.then(|| env_log_path("DBXT_MOUSE_DEBUG", "dbxt-mouse.log")).flatten()
    });

    let config_path = config_path();
    let config = config_path
        .as_deref()
        .map(TuiConfig::load)
        .unwrap_or_default();
    let config_compact = config.compact;

    let mut app = App {
        backend: backend.clone(),
        page: Page::Browse,
        focus: Focus::Sidebar,
        quit: false,
        connections: Vec::new(),
        conn_list: ListState::default(),
        picker_open: true,
        selected: None,
        databases: Vec::new(),
        db_index: 0,
        tables: Vec::new(),
        table_list: ListState::default(),
        tables_all: Vec::new(),
        table_filter: String::new(),
        table_prompt: None,
        columns: Vec::new(),
        ddl: None,
        struct_view: StructView::Fields,
        ddl_scroll: 0,
        editor: TextArea::default(),
        history: Vec::new(),
        history_idx: None,
        history_draft: String::new(),
        grid: None,
        grid_kind: GridKind::Query,
        page_state: None,
        script: None,
        sel: 0,
        col_offset: 0,
        col_cursor: 0,
        vis_cols: 0,
        grid_max_cell: 44,
        freeze_first: true,
        cell_popup: None,
        row_popup: None,
        compact: config_compact,
        col_hidden: HashSet::new(),
        col_picker_open: false,
        col_picker_list: ListState::default(),
        grid_full: None,
        recent_tables: Vec::new(),
        recent_open: false,
        recent_list: ListState::default(),
        pending_open_table: None,
        completion: None,
        config,
        config_path,
        result_filter: None,
        result_needle: String::new(),
        result_rows: Vec::new(),
        last_sql: None,
        filter_prompt: None,
        edit_dialog: None,
        pending_write: false,
        pending_write_msg: None,
        batch: Vec::new(),
        pane_override: [None; 3],
        auto_collapse: false,
        help_open: false,
        help_scroll: 0,
        pan_mode: false,
        drag_pan: DragPan::from_env(),
        gesture: PanGesture::default(),
        pending_tap: None,
        mouse_debug,
        mouse_log: VecDeque::new(),
        trace_path,
        last_event: None,
        result_tabs: Vec::new(),
        result_tab: 0,
        query_more: None,
        snippet_open: false,
        snippet_list: ListState::default(),
        snippets: Vec::new(),
        snippet_name: None,
        table_meta: None,
        count_cache: HashMap::new(),
        pending_sel: None,
        pending_focus: None,
        page_pending: false,
        page_gen: 0,
        db_picker_open: false,
        db_list: ListState::default(),
        pending_table: None,
        grid_gutter: 0,
        grid_frozen: 0,
        grid_widths: Vec::new(),
        grid_avail: 0,
        confirm: None,
        loading: true,
        // The initial `ListConnections` below is the one call not spawned through
        // `App::spawn`, so it is pre-counted here.
        pending_ops: 1,
        loading_since: Some(Instant::now()),
        spinner: 0,
        status: t("加载连接…").into(),
        backend_kind: Backend::Sql,
        cmd_input: TextArea::default(),
        cmd_output: Vec::new(),
        redis_db: 0,
        form: ConnForm::default(),
        layout_mode: LayoutMode::Mid,
        term_h: 0,
        rects: Rects::default(),
    };
    app.editor.set_placeholder_text(t("SQL … (Ctrl-J / F5 执行 · ↑ 历史)"));
    app.set_placeholder();

    // Pre-counted by `pending_ops: 1` in the initializer above.
    spawn_op(&backend, &tx, Op::ListConnections);

    while !app.quit {
        terminal.draw(|f| ui(f, &mut app))?;

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
                        handle_event(&mut app, &tx, ev);
                        while let Ok(res) = rx.try_recv() {
                            apply_op_result(&mut app, res, &tx);
                        }
                    }
                    Some(Err(_)) | None => app.quit = true,
                }
            }
            Some(res) = rx.recv() => {
                apply_op_result(&mut app, res, &tx);
            }
            _ = ticker.tick() => {
                if app.loading {
                    app.spinner = app.spinner.wrapping_add(1);
                }
            }
        }
    }
    Ok(())
}

fn apply_op_result(app: &mut App, res: OpResult, tx: &Tx) {
    // Only stop the spinner once every in-flight call has answered.
    app.pending_ops = app.pending_ops.saturating_sub(1);
    if app.pending_ops == 0 {
        app.loading = false;
        app.loading_since = None;
    }
    match res {
        OpResult::Connections(cs) => {
            let n = cs.len();
            app.connections = cs;
            if !app.connections.is_empty() && app.conn_list.selected().is_none() {
                app.conn_list.select(Some(0));
            }
            app.picker_open = app.selected.is_none();
            app.status = tf("{} 个连接 · ↑↓+Enter 选择 · c 新建", &[&(n)]);
        }
        OpResult::Databases { databases: dbs, warning } => {
            let configured = app.selected.as_ref().and_then(|c| c.database.clone());
            app.databases = dbs;
            app.db_index = configured
                .as_deref()
                .and_then(|db| app.databases.iter().position(|d| d == db))
                .unwrap_or(0);
            app.clear_grid();
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.cell_popup = None;
            app.row_popup = None;
            app.filter_prompt = None;
            app.table_meta = None;
            app.pending_sel = None;
            app.pending_focus = None;
            app.page_pending = false;
            app.set_placeholder();
            // auto-load tables for the selected database
            if let Some(cfg) = app.selected.clone() {
                let db = app.current_db();
                app.status = if db.is_empty() {
                    tf("加载 {} 表…", &[&(cfg.name)])
                } else {
                    tf("加载 {} 表…", &[&(db)])
                };
                app.spawn(tx, Op::ListTables(Box::new(cfg.clone()), db));
                app.spawn(tx, Op::History(Box::new(cfg)));
            }
            // A failed `list_databases` is not fatal (the configured database is
            // still used) but it must not be swallowed either.
            if let Some(w) = warning {
                app.status = format!("⚠ {w}");
            }
        }
        OpResult::Tables(ts) => {
            app.tables_all = ts;
            apply_table_filter(app);
            let n = app.tables_all.len();
            // Keep the previously selected table across a database switch when the
            // new database also has a table with the same name; otherwise go to top.
            let wanted = app.pending_table.take();
            if let Some(name) = wanted.as_deref() {
                if let Some(pos) = app.tables.iter().position(|t| t.name == name) {
                    app.table_list.select(Some(pos));
                }
            }
            app.columns.clear();
            app.ddl = None;
            // The browsed table's column metadata may belong to another database.
            app.table_meta = None;
            // A recent-table jump that had to switch database first: open the
            // requested table now that the list has arrived.
            if let Some(name) = app.pending_open_table.take() {
                if let Some(pos) = app.tables.iter().position(|t| t.name == name) {
                    app.table_list.select(Some(pos));
                    open_table_data(app, tx);
                    return;
                }
                app.status = tf("✗ 未找到表 {}", &[&(name)]);
                return;
            }
            app.status = if app.table_filter.is_empty() {
                tf("{} 个表/视图 · Enter 数据 · r 结构 · / 过滤 · Tab 编辑SQL", &[&(n)])
            } else {
                tf("过滤「{}」· {}/{} 个表 · Esc 清除", &[&(app.table_filter), &(app.tables.len()), &(n)])
            };
        }
        OpResult::Columns { table, columns: cols } => {
            // Ignore a late result for a table the user has already navigated away from.
            if app.selected_table().map(|t| t.name.clone()).as_deref() != Some(table.as_str()) {
                return;
            }
            let n = cols.len();
            let grid = columns_grid(&cols);
            app.columns = cols;
            app.grid_kind = GridKind::Columns;
            app.set_grid(grid);
            app.struct_view = StructView::Fields;
            app.page_state = None;
            app.script = None;
            app.sel = 0;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.cell_popup = None;
            app.focus = Focus::Preview;
            app.status = tf("{} 结构 · {} 字段 · t 切换 DDL · Esc 返回", &[&(table), &(n)]);
        }
        OpResult::Ddl { table, text } => {
            if app.selected_table().map(|t| t.name.clone()).as_deref() == Some(table.as_str()) {
                app.ddl = Some(text);
                app.ddl_scroll = 0;
            }
        }
        OpResult::TableData {
            grid,
            total,
            has_next,
            page,
            table,
            table_type,
            filter,
            order_by,
            gen,
        } => {
            // Discard any reply that is not for the latest request: a slow page
            // load must not clobber a newer filter / sort / table view.
            if gen != app.page_gen {
                return;
            }
            app.page_pending = false;
            let rows = grid.rows.len();
            if let Some(t) = total {
                app.count_cache
                    .insert(count_cache_key(&app.current_db(), &table, &filter), t);
            }
            app.grid_kind = GridKind::TableData;
            app.set_grid(*grid);
            app.page_state = Some(PageState {
                table: table.clone(),
                table_type,
                page,
                page_size: PAGE_SIZE,
                total,
                has_next,
                filter,
                order_by,
            });
            app.script = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            // `pending_sel` carries the cursor across a page turn: `Some(0)` when the
            // cursor ran off the bottom, `Some(usize::MAX)` off the top, or the
            // relative index preserved by an explicit page turn.
            app.sel = app
                .pending_sel
                .take()
                .map(|s| s.min(rows.saturating_sub(1)))
                .unwrap_or(0);
            app.cell_popup = None;
            app.row_popup = None;
            // Keep the horizontal window across pages; only clamp the cell cursor.
            let ncols = app.grid.as_ref().map(|g| g.columns.len()).unwrap_or(0);
            app.col_cursor = app.col_cursor.min(ncols.saturating_sub(1));
            app.col_offset = app.col_offset.min(ncols.saturating_sub(1));
            // Do not steal focus on a background page load: only move it when the
            // request asked for it (opening a table from the sidebar) and the user
            // has not already moved focus somewhere else.
            if let Some(f) = app.pending_focus.take() {
                if app.focus == Focus::Sidebar {
                    app.focus = f;
                }
            }
            let total_txt = total
                .map(|t| tf("共 {} 行", &[&(t)]))
                .unwrap_or_else(|| t("总数未知").into());
            let ps = app.page_state.as_ref().unwrap();
            let extra = page_state_extra(ps);
            app.status = tf("{}.{} · 第 {} 页 · {} 行 · {}{}", &[&(app.current_db()), &(table), &(page + 1), &(rows), &(total_txt), &(extra)]);
            if let Some(msg) = app.pending_write_msg.take() {
                app.status = tf("{} · 已刷新（第 {} 页）", &[&(msg), &(page + 1)]);
            }
        }
        OpResult::TableColumns { table, columns } => {
            // Only keep metadata that belongs to the table on screen.
            let active = app.page_state.as_ref().map(|p| p.table.as_str()) == Some(table.as_str())
                || app.selected_table().map(|t| t.name.as_str()) == Some(table.as_str());
            if active {
                app.table_meta = Some(TableMeta { table, columns });
            }
        }
        OpResult::Query(r, sql, cap) => {
            // Remember the SQL so `y` can guess a table name for a query result.
            app.last_sql = Some(sql.clone());
            // A fresh run starts a new result, so drop any previous row search
            // (a `Ctrl-N` load-more keeps it, since it is the same result).
            if cap <= QUERY_MAX_ROWS {
                app.result_needle.clear();
                app.result_filter = None;
            }
            // A statement that returned no columns is a write/DDL, and one that
            // reports affected rows (e.g. `INSERT … RETURNING`) changed data too:
            // any cached COUNT(*) may be stale now.
            let is_write = r.columns.is_empty() || r.affected_rows > 0;
            if is_write {
                app.count_cache.clear();
            }
            // A write launched from the edit dialog refreshes the current page
            // instead of replacing the grid with the DML result.
            if app.pending_write && is_write {
                app.pending_write = false;
                let affected = r.affected_rows;
                let note = format!("{}ms",  r.execution_time_ms);
                if app.page_state.is_some() && app.grid_kind == GridKind::TableData {
                    let sel = app.sel;
                    let ps = app.page_state.clone().unwrap();
                    let msg = tf("✓ 影响 {} 行 · {}", &[&(affected), &(note)]);
                    app.pending_write_msg = Some(msg.clone());
                    reload_table_view(app, tx, ps.filter.clone(), ps.order_by.clone(), ps.page);
                    app.pending_sel = Some(sel);
                    app.status = tf("{} · 已刷新当前页", &[&(msg)]);
                } else {
                    app.status = tf("✓ 影响 {} 行 · {}", &[&(affected), &(note)]);
                }
                return;
            }
            app.pending_write = false;
            let note = note_of(&r);
            let truncated = r.truncated;
            let grid = Grid::from_query(r.columns.clone(), &r.rows, note.clone());
            app.status = format!("{} · {} · {}",  app.selected_name(),  grid.rows.len(),  note);
            // Keep every query result as a tab so consecutive SELECTs can be flipped
            // with `[` / `]` instead of overwriting each other. A `Ctrl-N` "load more"
            // (a cap above the default) replaces the active tab instead of spawning
            // one per step.
            let title = query_tab_title(&sql);
            if cap > QUERY_MAX_ROWS {
                replace_result_tab(app, title, Some(grid), None, GridKind::Query);
            } else {
                push_result_tab(app, title, Some(grid), None, GridKind::Query);
            }
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.focus = Focus::Preview;
            // A truncated result can be extended with Ctrl-N.
            app.query_more = if truncated { Some((sql, cap)) } else { None };
        }
        OpResult::Script(outcomes) => {
            app.count_cache.clear();
            // A script result replaces any grid on screen; a result-row search
            // (which only applies to a data grid) must not leak into it.
            app.result_needle.clear();
            app.result_filter = None;
            let n = outcomes.len();
            let errors = outcomes.iter().filter(|o| o.error.is_some()).count();
            let affected: u64 = outcomes.iter().map(|o| o.affected).sum();
            let was_batch = app.pending_write;
            app.pending_write = false;
            app.query_more = None;
            let script = ScriptView {
                outcomes,
                sel: 0,
                drilled: None,
            };
            push_result_tab(app, tf("脚本 {} 条", &[&(n)]), None, Some(script), GridKind::Query);
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.focus = Focus::Preview;
            if was_batch {
                app.status = if errors == 0 {
                    tf("✓ 批量提交成功 · {} 条语句 · 影响 {} 行", &[&(n), &(affected)])
                } else {
                    tf("✗ 批量提交失败 · {} 错误 · 影响 {} 行（事务可能已回滚）· Enter 看详情", &[&(errors), &(affected)])
                };
            } else {
                app.status =
                    tf("脚本 · {} 条语句 · 影响 {} 行 · {} 错误 · Enter 看结果", &[&(n), &(affected), &(errors)]);
            }
        }
        OpResult::Redis(s) => {
            app.cmd_output.push(s);
            trim_output(&mut app.cmd_output);
        }
        OpResult::Mongo(s) => {
            app.cmd_output.push(s);
            trim_output(&mut app.cmd_output);
        }
        OpResult::History(items) => {
            if app.history.is_empty() {
                app.history = items;
            } else {
                let mut merged = items;
                for h in app.history.clone() {
                    if !merged.contains(&h) {
                        merged.push(h);
                    }
                }
                app.history = merged;
            }
            app.history_idx = None;
        }
        OpResult::Snippets(items) => {
            let n = items.len();
            app.snippets = items;
            app.snippet_open = true;
            app.snippet_list
                .select(if n == 0 { None } else { Some(0) });
            // A just-saved confirmation must survive the refresh that follows it.
            if !app.status.starts_with('✓') {
                app.status = if n == 0 {
                    t("没有保存的 SQL 片段（可在 DBX 桌面端保存后复用）").into()
                } else {
                    tf("{} 个 SQL 片段 · Enter 插入编辑器 · s 收藏当前 SQL · r 刷新 · Esc 关闭", &[&(n)])
                };
            }
        }
        OpResult::SnippetSaved(name) => {
            app.status = tf("✓ 已收藏 SQL 片段「{}」（DBX saved_sql_files）", &[&(name)]);
            // Refresh the list so the new favourite is visible immediately.
            if let Some(cfg) = app.selected.clone() {
                app.spawn(tx, Op::Snippets(Box::new(cfg)));
            }
        }
        OpResult::DatabasesRefresh(dbs) => {
            if dbs.is_empty() {
                app.status = t("未发现数据库").into();
                return;
            }
            let current = app.current_db();
            app.databases = dbs;
            if let Some(i) = app.databases.iter().position(|d| d == &current) {
                app.db_index = i;
            }
            app.db_index = app.db_index.min(app.databases.len().saturating_sub(1));
            if app.db_picker_open {
                let n = db_entries(app).len();
                let cur = db_current_index(app).min(n.saturating_sub(1));
                app.db_list.select(if n == 0 { None } else { Some(cur) });
            }
            app.status = tf("已刷新 {} 个数据库", &[&(app.databases.len())]);
        }
        OpResult::Added(msg) => {
            app.status = format!("✓ {msg}");
            app.page = Page::Browse;
            app.focus = Focus::Sidebar;
            app.form = ConnForm::default();
            app.selected = None;
            app.picker_open = true;
            app.loading = true;
            app.spawn(tx, Op::ListConnections);
        }
        OpResult::Error(e) => {
            app.page_pending = false;
            app.pending_sel = None;
            app.pending_focus = None;
            app.pending_write = false;
            app.pending_write_msg = None;
            app.status = format!("✗ {e}");
        }
    }
}

fn trim_output(v: &mut Vec<String>) {
    while v.len() > 400 {
        v.remove(0);
    }
}

/// Session cache key for a COUNT(*) total. Sorting does not affect the count, so
/// it is intentionally not part of the key; the WHERE filter is.
fn count_cache_key(db: &str, table: &str, filter: &str) -> String {
    format!("{db}\u{1}{table}\u{1}{filter}")
}

/// Human-readable `· 过滤: … · 排序: …` suffix for the status line and grid title.
fn page_state_extra(ps: &PageState) -> String {
    let mut s = String::new();
    if !ps.filter.trim().is_empty() {
        s.push_str(&tf(" · 过滤: {}", &[&(truncate_disp(&one_line(&ps.filter), 48))]));
    }
    if let Some(o) = ps.order_by.as_deref().filter(|o| !o.trim().is_empty()) {
        s.push_str(&tf(" · 排序: {}", &[&(truncate_disp(o, 32))]));
    }
    s
}

fn columns_grid(cols: &[ColumnInfo]) -> Grid {
    let columns = [t("字段"), t("类型"), t("键"), t("可空"), t("默认值"), t("注释")]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = cols
        .iter()
        .map(|c| {
            let key = if c.is_primary_key {
                "PK"
            } else if c.is_unique {
                "UQ"
            } else {
                ""
            };
            vec![
                Val::Text(fix_double_encoding(&c.name)),
                Val::Text(c.data_type.clone()),
                Val::Text(key.to_string()),
                Val::Text(if c.is_nullable { "Y" } else { "N" }.to_string()),
                c.column_default
                    .clone()
                    .map(Val::Text)
                    .unwrap_or_else(|| Val::Text(String::new())),
                Val::Text(c.comment.clone().unwrap_or_default()),
            ]
        })
        .collect();
    Grid {
        columns,
        rows,
        note: tf("{} 字段", &[&(cols.len())]),
    }
}

// ─── input handling ──────────────────────────────────────────────────────────

fn handle_event(app: &mut App, tx: &Tx, ev: Event) {
    trace_event(app, &ev);
    match ev {
        Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            key(app, tx, k)
        }
        Event::Paste(s) => match app.focus {
            Focus::Editor => {
                app.editor.insert_str(s);
            }
            Focus::CmdInput => {
                app.cmd_input.insert_str(s);
            }
            _ => {}
        },
        Event::Mouse(m) => mouse(app, tx, m),
        _ => {}
    }
}

fn key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // global: quit. Ctrl-Shift-C is a *view* toggle (compact columns), so the
    // quit must not swallow it on terminals that report Shift as a modifier.
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::SHIFT)
        && k.code == KeyCode::Char('c')
    {
        app.quit = true;
        return;
    }

    // confirmation overlay swallows everything else
    if app.confirm.is_some() {
        confirm_key(app, tx, k);
        return;
    }

    // diff-style edit confirmation layer is modal too
    if app.edit_dialog.is_some() {
        edit_dialog_key(app, tx, k);
        return;
    }

    // global: cycle backend line sql → redis → mongo
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('l') {
        app.backend_kind = match app.backend_kind {
            Backend::Sql => Backend::Redis,
            Backend::Redis => Backend::Mongo,
            Backend::Mongo => Backend::Sql,
        };
        app.cmd_input = TextArea::default();
        app.clear_grid();
        app.script = None;
        app.ddl = None;
        app.page_state = None;
        app.struct_view = StructView::Fields;
        app.col_offset = 0;
        app.col_cursor = 0;
        app.cell_popup = None;
        app.row_popup = None;
        app.filter_prompt = None;
        app.help_open = false;
        app.db_picker_open = false;
        app.set_placeholder();
        return;
    }

    match app.page {
        Page::NewConn => form_key(app, tx, k),
        Page::Browse => browse_key(app, tx, k),
    }
}

fn confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(c) = app.confirm.take() {
                app.push_history(&c.sql);
                if c.clear_batch {
                    app.batch.clear();
                }
                // Row edits / deletes / batches refresh the current page on success.
                if c.refresh {
                    app.pending_write = true;
                }
                execute_sql(app, tx, c.sql);
            }
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm = None;
            app.pending_write = false;
            app.status = t("已取消").into();
        }
        _ => {}
    }
}

fn browse_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // Overlays are modal, most-specific first. Esc always closes the current one.
    if app.help_open {
        help_key(app, k);
        return;
    }
    if app.filter_prompt.is_some() {
        filter_prompt_key(app, tx, k);
        return;
    }
    if app.row_popup.is_some() {
        popup_key(app, k, PopupTarget::Row);
        return;
    }
    if app.cell_popup.is_some() {
        popup_key(app, k, PopupTarget::Cell);
        return;
    }

    // Result-row search prompt (`/` in the results pane) is modal while typing.
    if app.result_filter.is_some() {
        result_filter_key(app, k);
        return;
    }

    // SQL completion popup (editor): must be handled before the global Tab
    // handler, otherwise Tab would switch panes instead of accepting.
    if app.completion.is_some() {
        completion_key(app, k);
        return;
    }

    // Saved-SQL snippet overlay (Ctrl-O) is modal; the name prompt is on top.
    if app.snippet_name.is_some() {
        snippet_name_key(app, tx, k);
        return;
    }
    if app.snippet_open {
        snippet_key(app, tx, k);
        return;
    }

    // Database switcher overlay (`d`) is modal.
    if app.db_picker_open {
        db_picker_key(app, tx, k);
        return;
    }

    // Sidebar table filter (`/`) is modal while it is being typed.
    if app.table_prompt.is_some() {
        table_filter_key(app, k);
        return;
    }

    // Recent-table overlay (Ctrl-Shift-R) is modal.
    if app.recent_open {
        recent_key(app, tx, k);
        return;
    }

    // Column-visibility overlay (Ctrl-Shift-H) is modal.
    if app.col_picker_open {
        col_picker_key(app, k);
        return;
    }

    // Ctrl-Shift view controls (mobile efficiency). Handled before the pane
    // handlers so a Shift is never dropped by the Ctrl-letter blocks below.
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.modifiers.contains(KeyModifiers::SHIFT) {
        match k.code {
            KeyCode::Char('c') | KeyCode::Char('C') => {
                toggle_compact(app);
                return;
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                open_col_picker(app);
                return;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                open_recent_tables(app);
                return;
            }
            _ => {}
        }
    }

    // Help works from anywhere except the text inputs (where `?` is a character).
    if k.code == KeyCode::Char('?')
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        open_help(app);
        return;
    }

    // run from anywhere (browse page)
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('j'))
        || k.code == KeyCode::F(5)
    {
        run_current(app, tx);
        return;
    }

    // Ctrl-O: DBX's saved SQL snippets, insertable into the editor. Allowed from
    // the editor too (that is where the snippet lands).
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && k.code == KeyCode::Char('o')
        && app.selected.is_some()
    {
        open_snippets(app, tx);
        return;
    }

    // Ctrl-P: run the editor's SQL through the dialect's EXPLAIN.
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && k.code == KeyCode::Char('p')
        && app.selected.is_some()
        && app.backend_kind == Backend::Sql
    {
        explain_current(app, tx);
        return;
    }

    // `d` opens the database list from any non-text area: one uniform gesture for
    // SQL databases, MongoDB databases and Redis logical DBs.
    if k.code == KeyCode::Char('d')
        && k.modifiers.is_empty()
        && app.selected.is_some()
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        open_db_picker(app);
        return;
    }

    // transactional batch queue: Ctrl-S commits, Ctrl-X discards
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('s') {
        commit_batch(app);
        return;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('x') {
        if app.batch.is_empty() {
            app.status = t("批量队列为空（编辑时按 Ctrl-T 加入）").into();
        } else {
            let n = app.batch.len();
            app.batch.clear();
            app.status = tf("已清空批量队列（{} 条）", &[&(n)]);
        }
        return;
    }

    // responsive layout: Alt-1/2/3 focus a pane and reset the collapse overrides.
    // Alt-C / Alt-H / Alt-R are the mobile-efficiency view commands (compact
    // columns / column visibility / recent tables): an Alt combo is reported
    // distinctly by every terminal, unlike Ctrl-Shift-X which tmux and legacy
    // terminals fold back into Ctrl-X.
    if k.modifiers.contains(KeyModifiers::ALT) {
        match k.code {
            KeyCode::Char('1') => {
                app.focus = Focus::Sidebar;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Char('2') => {
                app.focus = Focus::Editor;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Char('3') => {
                app.focus = Focus::Preview;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                toggle_compact(app);
                return;
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                open_col_picker(app);
                return;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                open_recent_tables(app);
                return;
            }
            _ => {}
        }
    }

    // Shift-← / Shift-→ pan the column window from any pane — the keyboard twin of
    // a horizontal swipe, for terminals that report neither a horizontal wheel nor
    // a drag. Holding the key repeats (the terminal auto-repeats), so a long press
    // scrolls continuously. The text inputs keep Shift-←/→ for selection.
    if k.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(k.code, KeyCode::Left | KeyCode::Right)
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        pan_columns(app, if k.code == KeyCode::Left { -1 } else { 1 });
        return;
    }

    // Tab / Shift-Tab cycle panes; B toggles the focused pane's collapse state.
    if k.code == KeyCode::Tab && k.modifiers.is_empty() {
        cycle_focus(app, true);
        return;
    }
    if k.code == KeyCode::BackTab {
        cycle_focus(app, false);
        return;
    }

    // Layout toggles are Ctrl-combos so no bare uppercase key is needed, and they
    // are kept out of the text inputs so typing is never hijacked.
    //   Ctrl-A = auto-collapse master switch
    //   Ctrl-W = collapse / expand just the focused pane
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        match k.code {
            KeyCode::Char('a') => {
                toggle_auto_collapse(app);
                return;
            }
            KeyCode::Char('w') => {
                toggle_pane_collapse(app);
                return;
            }
            // Ctrl-G: fallback for phone terminals that never send a horizontal
            // wheel — the vertical wheel (the one gesture every terminal has)
            // pans columns instead of rows.
            KeyCode::Char('g') => {
                app.pan_mode = !app.pan_mode;
                app.status = if app.pan_mode {
                    t("横滚 开 · 滚轮横滚列").into()
                } else {
                    t("横滚 关 · 滚轮纵向").into()
                };
                return;
            }
            _ => {}
        }
    }

    match app.focus {
        Focus::Sidebar => sidebar_key(app, tx, k),
        Focus::Editor => editor_key(app, tx, k),
        Focus::CmdInput => cmd_input_key(app, tx, k),
        Focus::Preview => preview_key(app, tx, k),
    }
}

// ── database switcher overlay ──

/// Entries shown by the `d` overlay. Redis exposes its 16 logical databases;
/// everything else lists the server's schemas/databases.
fn db_entries(app: &App) -> Vec<String> {
    if app.backend_kind == Backend::Redis {
        (0..16).map(|i| format!("db{i}")).collect()
    } else {
        app.databases.clone()
    }
}

fn db_current_index(app: &App) -> usize {
    if app.backend_kind == Backend::Redis {
        app.redis_db as usize
    } else {
        app.db_index
    }
}

fn open_db_picker(app: &mut App) {
    let n = db_entries(app).len();
    if n == 0 {
        app.status = t("没有可切换的数据库").into();
        return;
    }
    app.db_picker_open = true;
    let cur = db_current_index(app).min(n - 1);
    app.db_list.select(Some(cur));
}

fn db_picker_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = db_entries(app).len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.db_picker_open = false;
        }
        KeyCode::Char('d') if k.modifiers.is_empty() => {
            app.db_picker_open = false;
        }
        KeyCode::Char('r') if k.modifiers.is_empty() => {
            // Refresh the list in place, keeping the overlay open.
            if app.backend_kind == Backend::Redis {
                app.status = t("Redis 固定 16 个逻辑库").into();
            } else if let Some(cfg) = app.selected.clone() {
                app.status = t("刷新数据库列表…").into();
                app.spawn(tx, Op::DatabasesRefresh(Box::new(cfg)));
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app.db_list.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
                app.db_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .db_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.db_list.select(Some(i));
            }
        }
        KeyCode::Home => {
            if n > 0 {
                app.db_list.select(Some(0));
            }
        }
        KeyCode::End => {
            if n > 0 {
                app.db_list.select(Some(n - 1));
            }
        }
        KeyCode::Enter => {
            if let Some(i) = app.db_list.selected() {
                db_picker_apply(app, tx, i);
            }
        }
        _ => {}
    }
}

fn db_picker_apply(app: &mut App, tx: &Tx, idx: usize) {
    app.db_picker_open = false;
    match app.backend_kind {
        Backend::Redis => {
            app.redis_db = idx as u32;
            app.set_placeholder();
            app.status = format!("redis db → {idx}");
        }
        _ => {
            if idx < app.databases.len() {
                app.db_index = idx;
                let db = app.current_db();
                app.status = tf("切换数据库 → {}", &[&(db)]);
                reload_tables(app, tx);
            }
        }
    }
}

// ── sidebar: connection picker or table browser ──

fn sidebar_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // no connection selected yet → picker mode
    if app.selected.is_none() {
        match k.code {
            KeyCode::Char('c') => {
                app.page = Page::NewConn;
                app.form = ConnForm::default();
            }
            // Duplicate the highlighted connection into the form.
            KeyCode::Char('p') => duplicate_connection(app),
            KeyCode::Char('q') => {
                app.picker_open = !app.picker_open;
            }
            KeyCode::Tab => {
                app.focus = Focus::Editor;
            }
            KeyCode::Up => {
                let n = app.connections.len();
                if n > 0 {
                    let i = app
                        .conn_list
                        .selected()
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    app.conn_list.select(Some(i));
                }
            }
            KeyCode::Down => {
                let n = app.connections.len();
                if n > 0 {
                    let i = app
                        .conn_list
                        .selected()
                        .map(|i| (i + 1).min(n - 1))
                        .unwrap_or(0);
                    app.conn_list.select(Some(i));
                }
            }
            KeyCode::Enter => {
                connect_selected(app, tx);
            }
            _ => {}
        }
        return;
    }

    // connection selected → table browser
    match k.code {
        KeyCode::Tab => app.focus = Focus::Editor,
        KeyCode::Char('c') => {
            app.page = Page::NewConn;
            app.form = ConnForm::default();
        }
        // Duplicate the current connection into the form (new id on save).
        KeyCode::Char('p') => duplicate_connection(app),
        KeyCode::Char('o') => {
            // back to connection picker
            app.selected = None;
            app.tables.clear();
            app.tables_all.clear();
            app.table_filter.clear();
            app.columns.clear();
            app.databases.clear();
            app.clear_grid();
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.cell_popup = None;
            app.picker_open = true;
        }
        KeyCode::Char('r') => load_structure(app, tx),
        // `/` — filter-as-you-type over the table list (vim-style), the fast way
        // to reach a table when the sidebar is long.
        KeyCode::Char('/') => open_table_filter(app),
        // `t` — jump straight to one of the last five browsed tables.
        KeyCode::Char('t') => open_recent_tables(app),
        KeyCode::Up => {
            let n = app.tables.len();
            if n > 0 {
                let i = app
                    .table_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.table_list.select(Some(i));
            }
        }
        KeyCode::Down => {
            let n = app.tables.len();
            if n > 0 {
                let i = app
                    .table_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.table_list.select(Some(i));
            }
        }
        KeyCode::Enter => open_table_data(app, tx),
        // ←/→ (and h/l) stay as a fast shortcut; `d` is the discoverable list.
        KeyCode::Left | KeyCode::Char('h') => cycle_db(app, tx, false),
        KeyCode::Right | KeyCode::Char('l') => cycle_db(app, tx, true),
        _ => {}
    }
}

fn load_structure(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        app.status = t("先选中一张表").into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.loading = true;
    app.status = tf("加载 {} 结构…", &[&(table)]);
    let db = app.current_db();
    app.spawn(
        tx,
        Op::Columns(Box::new(cfg.clone()), db.clone(), table.clone()),
    );
    app.spawn(tx, Op::Ddl(Box::new(cfg), db, table));
}

fn open_table_data(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| (t.name.clone(), t.table_type.clone())) else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.clear_grid();
    app.script = None;
    app.ddl = None;
    app.struct_view = StructView::Fields;
    app.col_offset = 0;
    app.col_cursor = 0;
    app.cell_popup = None;
    app.row_popup = None;
    app.sel = 0;
    app.pending_sel = Some(0);
    app.pending_focus = Some(Focus::Preview);
    app.page_pending = true;
    app.table_meta = None;
    app.result_needle.clear();
    app.result_filter = None;
    remember_recent_table(app, &app.current_db(), &table.0);
    // Restore this table's persisted preferences (db.table granularity).
    let db = app.current_db();
    let prefs = app.config.table(&db, &table.0).cloned().unwrap_or_default();
    app.col_hidden = prefs.hidden;
    // Fall back to the *global* default (not whatever the previously opened table
    // happened to use), so a table with no stored choice is not contaminated.
    app.compact = prefs.compact.or(app.config.compact);
    let order_by = prefs.order_by;
    app.page_state = Some(PageState {
        table: table.0.clone(),
        table_type: Some(table.1.clone()),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        has_next: false,
        filter: String::new(),
        order_by: order_by.clone(),
    });
    app.loading = true;
    app.status = tf("加载 {}.{} 数据…", &[&(app.current_db()), &(table.0)]);
    // Column metadata powers the `e`/`i` templates (primary-key detection).
    app.spawn(
        tx,
        Op::TableColumns(Box::new(cfg.clone()), app.current_db(), table.0.clone()),
    );
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &table.0, ""))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            table: table.0,
            table_type: Some(table.1),
            page: 0,
            page_size: PAGE_SIZE,
            filter: String::new(),
            order_by,
            known_total: known,
            gen,
        })),
    );
}

/// Fetch `page`, moving the cursor to `pending_sel` when the result lands.
/// Returns false when a page load is already in flight, so a held-down key cannot
/// stack duplicate queries.
fn goto_page(app: &mut App, tx: &Tx, page: usize, pending_sel: Option<usize>) -> bool {
    if app.page_pending {
        return false;
    }
    let Some(ps) = app.page_state.clone() else {
        return false;
    };
    let Some(cfg) = app.selected.clone() else {
        return false;
    };
    app.page_pending = true;
    app.pending_sel = pending_sel;
    app.loading = true;
    app.status = tf("加载 {} 第 {} 页…", &[&(ps.table), &(page + 1)]);
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &ps.table, &ps.filter))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            table: ps.table.clone(),
            table_type: ps.table_type.clone(),
            page,
            page_size: ps.page_size,
            filter: ps.filter.clone(),
            order_by: ps.order_by.clone(),
            known_total: known,
            gen,
        })),
    );
    true
}

/// Re-run the current table view from page 0 with a new filter / sort. The
/// focus is intentionally left where it is (background refresh).
fn reload_table_view(app: &mut App, tx: &Tx, filter: String, order_by: Option<String>, page: usize) {
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.page_state = Some(PageState {
        page,
        total: None,
        has_next: false,
        filter: filter.clone(),
        order_by: order_by.clone(),
        ..ps.clone()
    });
    app.page_pending = true;
    app.pending_sel = Some(0);
    app.loading = true;
    app.status = tf("加载 {} 第 {} 页…", &[&(ps.table), &(page + 1)]);
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &ps.table, &filter))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            table: ps.table,
            table_type: ps.table_type,
            page,
            page_size: ps.page_size,
            filter,
            order_by,
            known_total: known,
            gen,
        })),
    );
}

/// Rows the results pane can show at once (header + borders excluded).
fn viewport_rows(app: &App) -> usize {
    app.rects.results.height.saturating_sub(3).max(1) as usize
}

/// One-row cursor move, flipping the page when the cursor runs off an edge so
/// browsing is continuous (no "turn page, then hunt for the row").
fn cursor_step(app: &mut App, tx: &Tx, dir: i32) -> bool {
    let n = result_row_count(app);
    if n == 0 {
        return false;
    }
    if dir > 0 {
        if app.sel + 1 < n {
            app.sel += 1;
            return true;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.has_next {
                return goto_page(app, tx, ps.page + 1, Some(0));
            }
        }
        false
    } else {
        if app.sel > 0 {
            app.sel -= 1;
            return true;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.page > 0 {
                // usize::MAX is clamped to the last row when the page arrives.
                return goto_page(app, tx, ps.page - 1, Some(usize::MAX));
            }
        }
        false
    }
}

fn move_cursor(app: &mut App, tx: &Tx, delta: i32) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            let n = s.outcomes.len();
            if n == 0 {
                return;
            }
            let next = (s.sel as i32 + delta).clamp(0, n as i32 - 1);
            s.sel = next as usize;
            return;
        }
    }
    let steps = delta.unsigned_abs() as usize;
    let dir = if delta < 0 { -1 } else { 1 };
    for _ in 0..steps {
        if !cursor_step(app, tx, dir) {
            break;
        }
    }
}

/// Screen-at-a-time scroll that carries over the page boundary.
fn screen_move(app: &mut App, tx: &Tx, dir: i32) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return;
    }
    let screen = viewport_rows(app);
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            let n = s.outcomes.len();
            if n == 0 {
                return;
            }
            let next = (s.sel as i32 + dir * screen as i32).clamp(0, n as i32 - 1);
            s.sel = next as usize;
            return;
        }
    }
    let n = result_row_count(app);
    if n == 0 {
        return;
    }
    if dir > 0 {
        if app.sel + screen < n {
            app.sel = (app.sel + screen).min(n - 1);
            return;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.has_next {
                let carry = (app.sel + screen).saturating_sub(n);
                let target = carry.min(ps.page_size.saturating_sub(1));
                goto_page(app, tx, ps.page + 1, Some(target));
                return;
            }
        }
        app.sel = n - 1;
    } else {
        if app.sel >= screen {
            app.sel -= screen;
            return;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.page > 0 {
                let carry = screen - app.sel;
                let target = ps.page_size.saturating_sub(carry);
                goto_page(app, tx, ps.page - 1, Some(target));
                return;
            }
        }
        app.sel = 0;
    }
}

/// Explicit page turn (`n`/`p`/Ctrl-F/Ctrl-B): keep the cursor at the same
/// relative row so the view does not jump back to the top.
fn page_turn(app: &mut App, tx: &Tx, forward: bool) {
    let Some(ps) = app.page_state.clone() else {
        screen_move(app, tx, if forward { 1 } else { -1 });
        return;
    };
    if forward && !ps.has_next {
        app.status = t("已经是最后一页").into();
        return;
    }
    if !forward && ps.page == 0 {
        app.status = t("已经是第一页").into();
        return;
    }
    let target = if forward { ps.page + 1 } else { ps.page - 1 };
    goto_page(app, tx, target, Some(app.sel));
}

fn reload_tables(app: &mut App, tx: &Tx) {
    if let Some(cfg) = app.selected.clone() {
        // Remember the current table so a same-named table can be re-selected in
        // the new database.
        app.pending_table = app.selected_table().map(|t| t.name.clone());
        app.tables.clear();
        app.tables_all.clear();
        app.columns.clear();
        app.ddl = None;
        app.clear_grid();
        app.script = None;
        app.page_state = None;
        app.col_offset = 0;
        app.col_cursor = 0;
        app.cell_popup = None;
        app.row_popup = None;
        app.filter_prompt = None;
        app.table_meta = None;
        app.pending_sel = None;
        app.pending_focus = None;
        app.page_pending = false;
        app.loading = true;
        let db = app.current_db();
        app.status = tf("切换到 {} …", &[&(db)]);
        app.set_placeholder();
        app.spawn(tx, Op::ListTables(Box::new(cfg), db));
    }
}

fn cycle_db(app: &mut App, tx: &Tx, forward: bool) {
    let len = app.databases.len();
    if len <= 1 {
        return;
    }
    app.db_index = if forward {
        (app.db_index + 1) % len
    } else {
        (app.db_index + len - 1) % len
    };
    reload_tables(app, tx);
}

// ── result column window ──

fn max_cell_width(mode: LayoutMode) -> usize {
    match mode {
        LayoutMode::Narrow => 18,
        LayoutMode::Mid => 28,
        LayoutMode::Wide => 44,
    }
}

const MIN_CELL_WIDTH: usize = 6;

/// Narrowest a column may get in the compact (mobile) column-width mode. Below
/// this a value is no longer identifiable at a glance.
const COMPACT_MIN_CELL: usize = 6;
/// Widest a column gets in compact mode when the pane is too small for every
/// column to share the space equally. Keeps a phone table from showing two
/// half-screen columns.
const COMPACT_MAX_CELL: usize = 8;

/// Is the compact column-width mode active? `None` means "auto": on for a
/// narrow (phone) terminal, off otherwise. The toggle stores an explicit
/// on/off so a user can override the automatic choice in either direction.
fn compact_active(compact: Option<bool>, mode: LayoutMode) -> bool {
    compact.unwrap_or(mode == LayoutMode::Narrow)
}

/// Width cap for one grid column in compact mode.
///
/// The pane is shared equally among all columns, so a table whose columns are
/// not too numerous fits completely and needs no horizontal scrolling at all.
/// When even `COMPACT_MIN_CELL` per column does not fit, the cap stays at the
/// minimum and the grid scrolls as before.
fn compact_max_cell(inner_w: usize, gutter: usize, ncols: usize, base: usize) -> usize {
    if ncols == 0 {
        return COMPACT_MAX_CELL.min(base);
    }
    let avail = inner_w.saturating_sub(gutter);
    let per = avail.saturating_sub(ncols.saturating_sub(1)) / ncols;
    if per >= COMPACT_MAX_CELL {
        // Room to spare: let a wide terminal use its normal content width.
        base.min(per.max(COMPACT_MIN_CELL))
    } else {
        per.clamp(COMPACT_MIN_CELL, COMPACT_MAX_CELL).min(base)
    }
}

/// Effective per-column width cap for the current frame.
fn grid_max_cell(app: &App, ncols: usize, inner_w: usize, gutter: u16) -> usize {
    let base = max_cell_width(app.layout_mode);
    if compact_active(app.compact, app.layout_mode) {
        compact_max_cell(inner_w, gutter as usize, ncols, base)
    } else {
        base
    }
}

/// Human label for the compact mode, used in the status bar and messages.
fn compact_label(app: &App) -> String {
    let on = compact_active(app.compact, app.layout_mode);
    let auto = if app.compact.is_none() { t("自动") } else { t("手动") };
    tf("紧凑列 {}{}", &[&(if on { t("开") } else { t("关") }), &(auto)])
}

/// Drop every column the user hid with Ctrl-Shift-H. At least one column always
/// survives so a grid can never render as nothing (DBX does the same).
fn filter_grid(grid: &Grid, hidden: &HashSet<String>) -> Grid {
    if hidden.is_empty() {
        return grid.clone();
    }
    let mut keep: Vec<usize> = (0..grid.columns.len())
        .filter(|&i| !hidden.contains(grid.columns[i].as_str()))
        .collect();
    if keep.is_empty() {
        keep.push(0);
    }
    if keep.len() == grid.columns.len() {
        return grid.clone();
    }
    Grid {
        columns: keep.iter().map(|&i| grid.columns[i].clone()).collect(),
        rows: grid
            .rows
            .iter()
            .map(|r| {
                keep.iter()
                    .map(|&i| r.get(i).cloned().unwrap_or(Val::Null))
                    .collect()
            })
            .collect(),
        note: grid.note.clone(),
    }
}

/// True when any cell of `row` contains `needle` (already lower-cased). NULL is
/// matched as the text `null` so `/null` finds real NULLs.
fn row_matches(row: &[Val], needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    row.iter().any(|v| {
        let s = match v {
            Val::Null => "null",
            Val::Text(s) => s.as_str(),
        };
        s.to_lowercase().contains(needle)
    })
}

/// Keep only the rows matching the active result-row search. An empty needle (or
/// one that is all whitespace) returns the grid unchanged, so this is a no-op
/// when no search is active.
fn apply_row_search(grid: Grid, needle: &str) -> Grid {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return grid;
    }
    let rows = grid
        .rows
        .into_iter()
        .filter(|r| row_matches(r, &needle))
        .collect();
    Grid {
        columns: grid.columns,
        rows,
        note: grid.note,
    }
}

/// Natural width of one grid column: the widest of its header and cells,
/// clamped to `[MIN_CELL_WIDTH, max_cell]`.
fn natural_width(grid: &Grid, ci: usize, max_cell: usize) -> usize {
    let mut w = disp_width(grid.columns.get(ci).map(String::as_str).unwrap_or(""));
    for row in &grid.rows {
        if let Some(v) = row.get(ci) {
            let cw = disp_width(v.text());
            if cw > w {
                w = cw;
            }
        }
    }
    w.clamp(MIN_CELL_WIDTH, max_cell)
}

/// How many columns starting at `off` fit in `avail` display columns using their
/// natural widths. Content-sized columns keep a narrow `id` narrow instead of
/// stretching it to fill the pane.
fn visible_cols(grid: &Grid, off: usize, avail: usize, max_cell: usize) -> usize {
    let n = grid.columns.len();
    if n == 0 || off >= n {
        return 0;
    }
    let mut used = 0usize;
    let mut count = 0usize;
    for ci in off..n {
        let w = natural_width(grid, ci, max_cell);
        let add = w + if count > 0 { 1 } else { 0 };
        if count > 0 && used + add > avail {
            break;
        }
        used += add;
        count += 1;
        if used >= avail {
            break;
        }
    }
    count.max(1)
}

/// Move the focused cell one column left/right. The visible window follows the
/// cursor, which is what makes horizontal browsing feel like scrolling a table.
/// Works for table data, query results and drilled script results alike.
fn move_col_cursor(app: &mut App, delta: i32) {
    let Some(grid) = active_grid(app) else {
        return;
    };
    let n = grid.columns.len();
    if n == 0 {
        return;
    }
    let next = (app.col_cursor as i32 + delta).clamp(0, n as i32 - 1);
    app.col_cursor = next as usize;
}

/// The grid the cell cursor currently operates on: a drilled script result
/// takes precedence over the top-level grid (which is empty while a script is
/// shown). Hidden columns and the active result-row search are applied, so the
/// row / column cursor and CSV export see exactly what is on screen.
fn active_grid(app: &App) -> Option<Grid> {
    if let Some(s) = &app.script {
        if let Some(i) = s.drilled {
            let full = s.outcomes.get(i)?.grid.clone();
            let cols = filter_grid(&full, &app.col_hidden);
            return Some(apply_row_search(cols, &app.result_needle));
        }
    }
    app.grid.clone()
}

/// The grid as it was fetched, before the session column filter. Used by the row
/// detail popup, which must show every column even the hidden ones.
fn full_grid(app: &App) -> Option<Grid> {
    if let Some(s) = &app.script {
        if let Some(i) = s.drilled {
            return s.outcomes.get(i).map(|o| o.grid.clone());
        }
    }
    app.grid_full.clone()
}

/// True when the results pane is showing a browsable table (not a query result,
/// structure list or script).
fn in_table_data_view(app: &App) -> bool {
    app.script.is_none() && app.grid_kind == GridKind::TableData && app.page_state.is_some()
}

/// Absolute row number (1-based) of the cursor across all pages.
fn abs_row(page: usize, page_size: usize, sel: usize) -> usize {
    page * page_size + sel + 1
}

/// Total page count for a known row total (at least one page).
fn page_count(total: u64, page_size: usize) -> usize {
    if page_size == 0 {
        return 1;
    }
    ((total as usize).div_ceil(page_size)).max(1)
}

/// How many leading data columns to pin. The row-number gutter is always pinned;
/// the first data column is pinned only when the toggle is on, the grid is wide
/// enough to still scroll, and there is room for at least one more column.
fn effective_frozen(
    freeze_first: bool,
    grid: &Grid,
    gutter: usize,
    inner_w: usize,
    max_cell: usize,
) -> usize {
    if !freeze_first {
        return 0;
    }
    let n = grid.columns.len();
    if n < 3 {
        return 0;
    }
    let w0 = natural_width(grid, 0, max_cell);
    if gutter + 1 + w0 + 1 + MIN_CELL_WIDTH <= inner_w {
        1
    } else {
        0
    }
}

/// The scrollable window `(off, visible)` that keeps `cursor` on screen, starting
/// from the previous window origin `start` and never scrolling into the frozen
/// prefix.
fn window_for_cursor(
    grid: &Grid,
    cursor: usize,
    start: usize,
    avail: usize,
    max_cell: usize,
    frozen: usize,
) -> (usize, usize) {
    let n = grid.columns.len();
    if n == 0 {
        return (0, 0);
    }
    let mut off = start.max(frozen).min(n - 1);
    let mut visible = visible_cols(grid, off, avail, max_cell).max(1);
    if cursor < frozen {
        return (off, visible);
    }
    let mut guard = 0usize;
    while cursor >= off + visible && off + visible < n && guard <= n {
        off += 1;
        visible = visible_cols(grid, off, avail, max_cell).max(1);
        guard += 1;
    }
    if cursor < off {
        off = cursor;
        visible = visible_cols(grid, off, avail, max_cell).max(1);
    }
    (off, visible)
}

// ── mouse / touch (touch tap = Mouse Down, wheel = Scroll) ──

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn connect_selected(app: &mut App, tx: &Tx) {
    if let Some(idx) = app.conn_list.selected() {
        if let Some(cfg) = app.connections.get(idx).cloned() {
            app.selected = Some(cfg.clone());
            app.picker_open = false;
            app.clear_grid();
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.cell_popup = None;
            app.cmd_output.clear();
            app.set_placeholder();
            app.loading = true;
            app.status = tf("连接 {} ({})…", &[&(cfg.name), &(cfg.db_type.as_str())]);
            app.spawn(tx, Op::Databases(Box::new(cfg)));
        }
    }
}

fn result_row_count(app: &App) -> usize {
    if let Some(s) = &app.script {
        if s.drilled.is_none() {
            return s.outcomes.len();
        }
        return active_grid(app).map(|g| g.rows.len()).unwrap_or(0);
    }
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return 0;
    }
    app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0)
}

fn scroll(app: &mut App, tx: &Tx, delta: i32) {
    // wheel never steals keyboard focus
    match app.focus {
        Focus::Sidebar => {
            if app.selected.is_none() {
                let n = app.connections.len();
                if n > 0 {
                    let cur = app.conn_list.selected().unwrap_or(0) as i32;
                    let next = (cur + delta).clamp(0, n as i32 - 1);
                    app.conn_list.select(Some(next as usize));
                }
            } else {
                let n = app.tables.len();
                if n > 0 {
                    let cur = app.table_list.selected().unwrap_or(0) as i32;
                    let next = (cur + delta).clamp(0, n as i32 - 1);
                    app.table_list.select(Some(next as usize));
                }
            }
        }
        Focus::Preview => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                let d = app.ddl_scroll as i32 + delta;
                app.ddl_scroll = d.max(0) as u16;
            } else {
                move_cursor(app, tx, delta);
            }
        }
        Focus::Editor => {
            // let tui-textarea scroll itself
            let code = if delta < 0 {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            app.editor.input(KeyEvent::new(code, KeyModifiers::NONE));
        }
        Focus::CmdInput => {}
    }
}

fn mouse(app: &mut App, tx: &Tx, m: MouseEvent) {
    let r = app.rects;
    // Forensics first: every event the terminal actually delivered is recorded,
    // including the ones an overlay swallows, so `DBXT_MOUSE_DEBUG` shows the
    // truth rather than what we happened to act on.
    if app.mouse_debug {
        let desc = describe_mouse(&m);
        app.mouse_log.push_back(desc);
        while app.mouse_log.len() > MOUSE_DEBUG_LINES {
            app.mouse_log.pop_front();
        }
    }
    // Overlays own the wheel while they are open; scrolling the grid underneath a
    // modal would be invisible and confusing.
    let overlay_open = app.confirm.is_some()
        || app.edit_dialog.is_some()
        || app.cell_popup.is_some()
        || app.row_popup.is_some()
        || app.filter_prompt.is_some()
        || app.db_picker_open
        || app.snippet_open
        || app.col_picker_open
        || app.recent_open
        || app.table_prompt.is_some()
        || app.help_open;
    match m.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = m.kind == MouseEventKind::ScrollUp;
            if let Some(p) = app.cell_popup.as_mut() {
                p.scroll = if up {
                    p.scroll.saturating_sub(1)
                } else {
                    p.scroll.saturating_add(1)
                };
                return;
            }
            if let Some(p) = app.row_popup.as_mut() {
                p.scroll = if up {
                    p.scroll.saturating_sub(1)
                } else {
                    p.scroll.saturating_add(1)
                };
                return;
            }
            if app.help_open {
                app.help_scroll = if up {
                    app.help_scroll.saturating_sub(1)
                } else {
                    app.help_scroll.saturating_add(1)
                };
                return;
            }
            if overlay_open {
                return;
            }
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if overlay_open => return,
        _ => {}
    }

    // ── swipe layer: a held button moving horizontally pans the columns ──
    //
    // A phone terminal that never emits a horizontal wheel encodes a left/right
    // swipe as `Drag(Left)` (button-event tracking) or `Moved` (any-event
    // tracking) — a gesture the wheel-only code could not see at all. Panning
    // ignores focus, exactly like the horizontal wheel.
    if !overlay_open {
        // A tap is only confirmed on its `Up`, so the press that starts a swipe
        // does not also select a row / jump the scrollbar. A gesture that turned
        // into a swipe drops the pending tap instead.
        if let MouseEventKind::Up(MouseButton::Left) = m.kind {
            let swipe = app.gesture.is_swipe();
            let _ = app.gesture.feed(m.kind, m.column, m.row, app.drag_pan);
            let tap = app.pending_tap.take();
            if let Some((cx, cy)) = tap {
                if !swipe {
                    result_click(app, cx, cy);
                }
            }
            return;
        }
        if let Some(steps) = app.gesture.feed(m.kind, m.column, m.row, app.drag_pan) {
            if steps != 0 {
                pan_columns(app, steps);
            }
            if app.gesture.is_swipe() {
                app.pending_tap = None;
            }
            return;
        }
    }

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.confirm.is_some()
                || app.edit_dialog.is_some()
                || app.snippet_open
                || app.col_picker_open
                || app.recent_open
                || app.table_prompt.is_some()
            {
                return;
            }
            if app.cell_popup.is_some() {
                return;
            }
            if r.db_picker_visible && rect_contains(r.db_picker, m.column, m.row) {
                let row_index = m.row as i32 - r.db_picker.y as i32 - 1;
                if row_index >= 0 {
                    let idx = row_index as usize;
                    if idx < db_entries(app).len() {
                        if app.db_list.selected() == Some(idx) {
                            db_picker_apply(app, tx, idx);
                        } else {
                            app.db_list.select(Some(idx));
                        }
                    }
                }
                return;
            }
            // hit-test order: picker > cmd > editor > results > sidebar
            if r.picker_visible && rect_contains(r.picker, m.column, m.row) {
                let row_index = m.row as i32 - r.picker.y as i32 - 1;
                if row_index >= 0 {
                    let idx = row_index as usize;
                    if idx < app.connections.len() {
                        if app.conn_list.selected() == Some(idx) {
                            connect_selected(app, tx);
                        } else {
                            app.conn_list.select(Some(idx));
                        }
                    }
                }
                return;
            }
            if rect_contains(r.cmd, m.column, m.row) {
                app.focus = Focus::CmdInput;
                return;
            }
            if rect_contains(r.editor, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_EDITOR) {
                    app.pane_override[PANE_EDITOR] = Some(false);
                }
                app.focus = Focus::Editor;
                return;
            }
            if rect_contains(r.results, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_RESULTS) {
                    app.pane_override[PANE_RESULTS] = Some(false);
                    app.focus = Focus::Preview;
                    return;
                }
                app.focus = Focus::Preview;
                // A touch swipe starts with the same press as a tap, so defer the
                // click to the matching `Up` and drop it when the gesture becomes a
                // swipe. Terminals that never send `Up` keep press-to-click
                // (`can_defer_tap`), so tapping can never regress.
                if app.gesture.can_defer_tap() {
                    app.pending_tap = Some((m.column, m.row));
                } else {
                    result_click(app, m.column, m.row);
                }
                return;
            }
            if rect_contains(r.sidebar, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_SIDEBAR) {
                    app.pane_override[PANE_SIDEBAR] = Some(false);
                    app.focus = Focus::Sidebar;
                    return;
                }
                app.focus = Focus::Sidebar;
                if app.selected.is_some() {
                    sidebar_click(app, tx, m.column, m.row);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if wheel_pans_columns(app, &m) {
                pan_columns(app, -1);
            } else {
                scroll(app, tx, -1);
            }
        }
        MouseEventKind::ScrollDown => {
            if wheel_pans_columns(app, &m) {
                pan_columns(app, 1);
            } else {
                scroll(app, tx, 1);
            }
        }
        // A touch screen's left/right swipe arrives as a horizontal wheel. Pan the
        // columns regardless of which pane has focus, so a swipe works even after
        // tapping the sidebar or the editor; when every column already fits the
        // event is simply ignored.
        MouseEventKind::ScrollLeft => {
            pan_columns(app, -1);
        }
        MouseEventKind::ScrollRight => {
            pan_columns(app, 1);
        }
        _ => {}
    }
}

/// Map a click inside the results pane back to a column index using the geometry
/// captured during the last render.
fn col_at_x(app: &App, rel_x: i32) -> Option<usize> {
    if rel_x < 0 {
        return None;
    }
    let mut x = app.grid_gutter as i32;
    if rel_x < x {
        return None; // row-number gutter
    }
    for ci in 0..app.grid_frozen {
        x += 1; // column spacing
        let w = app.grid_widths.get(ci).copied().unwrap_or(MIN_CELL_WIDTH) as i32;
        if rel_x < x + w {
            return Some(ci);
        }
        x += w;
    }
    x += 1; // gap between the pinned block and the scrollable window
    for k in 0..app.vis_cols {
        let ci = app.col_offset + k;
        let w = app.grid_widths.get(ci).copied().unwrap_or(MIN_CELL_WIDTH) as i32;
        if rel_x < x + w {
            return Some(ci);
        }
        x += w + 1;
    }
    None
}

fn result_click(app: &mut App, x: u16, y: u16) {
    if hbar_click(app, x, y) {
        return;
    }
    let area = app.rects.results;
    let rel = y as i32 - area.y as i32 - 2; // skip border + header row
    let rel_x = x as i32 - area.x as i32 - 1;
    if rel < 0 {
        return;
    }
    if app.grid_kind != GridKind::Columns {
        if let Some(ci) = col_at_x(app, rel_x) {
            app.col_cursor = ci;
        }
    }
    let h = (area.height as usize).saturating_sub(3).max(1);
    if rel as usize >= h {
        return;
    }
    let n = result_row_count(app);
    if n == 0 {
        return;
    }
    let start = app.sel.saturating_sub(h / 2).min(n.saturating_sub(h.min(n)));
    let idx = start + rel as usize;
    if idx >= n {
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            if s.sel == idx {
                drill_script(app, idx);
            } else {
                s.sel = idx;
            }
            return;
        }
    }
    app.sel = idx;
}

/// Clicking the horizontal progress bar jumps the column window to the clicked
/// position. Returns true when the click was on the bar (and handled).
fn hbar_click(app: &mut App, x: u16, y: u16) -> bool {
    if !app.rects.hbar_visible {
        return false;
    }
    // The `◀` / `▶` end buttons pan one window — the touch-friendly control for
    // phones whose terminal never sends a horizontal wheel.
    if rect_contains(app.rects.hbar_prev, x, y) {
        let step = app.vis_cols.max(1) as i32;
        pan_columns(app, -step);
        return true;
    }
    if rect_contains(app.rects.hbar_next, x, y) {
        let step = app.vis_cols.max(1) as i32;
        pan_columns(app, step);
        return true;
    }
    let r = app.rects.hbar;
    if r.width == 0 || y != r.y || x < r.x || x >= r.x + r.width {
        return false;
    }
    let Some(grid) = active_grid(app) else {
        return true;
    };
    let ncols = grid.columns.len();
    if ncols == 0 {
        return true;
    }
    let frozen = app.grid_frozen;
    let total = ncols.saturating_sub(frozen).max(1);
    let rel = (x - r.x) as usize;
    let frac = if r.width > 1 {
        rel as f64 / (r.width - 1) as f64
    } else {
        0.0
    };
    let target = frozen + (frac * (total.saturating_sub(1)) as f64).round() as usize;
    app.col_cursor = target.min(ncols - 1);
    app.col_offset = app.col_cursor;
    true
}

fn sidebar_click(app: &mut App, tx: &Tx, x: u16, y: u16) {
    let area = app.rects.sidebar;
    let rel = y as i32 - area.y as i32 - 1; // skip top border
    if rel < 0 {
        return;
    }
    // row 0 = connection header, then optional database selector row and the
    // table-filter row (both only when the connection is open).
    let header = 1
        + if sidebar_db_row(app) { 1 } else { 0 }
        + if app.tables_all.is_empty() { 0 } else { 1 };
    if rel == 1 && sidebar_db_row(app) {
        // clicking the database row opens the switcher
        open_db_picker(app);
        return;
    }
    let table_row = rel - header;
    if table_row < 0 {
        return;
    }
    let _ = x;
    let cap = (area.height as usize)
        .saturating_sub(
            2 + if sidebar_db_row(app) { 1 } else { 0 }
                + if app.tables_all.is_empty() { 0 } else { 1 },
        )
        .max(1);
    let sel = app.table_list.selected();
    let start = sel
        .unwrap_or(0)
        .saturating_sub(cap / 2)
        .min(app.tables.len().saturating_sub(cap.min(app.tables.len())));
    let idx = start + table_row as usize;
    if idx >= app.tables.len() {
        return;
    }
    if app.table_list.selected() == Some(idx) {
        open_table_data(app, tx);
    } else {
        app.table_list.select(Some(idx));
    }
}

/// Whether the sidebar shows a database row under the connection header.
fn sidebar_db_row(app: &App) -> bool {
    app.selected.is_some() && !app.databases.is_empty()
}

fn sidebar_db_label(app: &App) -> String {
    if app.backend_kind == Backend::Redis {
        tf("redis db {} · d 切换", &[&(app.redis_db)])
    } else {
        tf("{} · d 切换", &[&(fix_double_encoding(&app.current_db()))])
    }
}

// ── editor / cmd input / preview ──

fn editor_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The completion popup owns the keyboard while it is open: Tab / Enter
    // accept, Esc cancels, arrows move, anything else keeps typing (and refines
    // the candidate list).
    if app.completion.is_some() {
        completion_key(app, k);
        return;
    }
    match (k.modifiers, k.code) {
        (m, KeyCode::Char('j')) if m.contains(KeyModifiers::CONTROL) => run_current(app, tx),
        // Ctrl-Space (some terminals send NUL): table / column / keyword prefix
        // completion at the cursor.
        (m, KeyCode::Char(' ')) if m.contains(KeyModifiers::CONTROL) => open_completion(app),
        (m, KeyCode::Null) if m.contains(KeyModifiers::CONTROL) || m.is_empty() => {
            open_completion(app)
        }
        (KeyModifiers::NONE, KeyCode::F(5)) => run_current(app, tx),
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.focus = if app.backend_kind == Backend::Sql {
                Focus::Preview
            } else {
                Focus::CmdInput
            }
        }
        (KeyModifiers::NONE, KeyCode::Esc) => app.focus = Focus::Sidebar,
        // shell-style history recall: ↑ on the first line walks back in time
        (KeyModifiers::NONE, KeyCode::Up) if app.editor.cursor().0 == 0 => {
            if !app.history_prev() {
                app.editor.input(k);
            }
        }
        (KeyModifiers::NONE, KeyCode::Down)
            if app.history_idx.is_some() && app.editor.cursor().0 + 1 == app.editor.lines().len() =>
        {
            if !app.history_next() {
                app.editor.input(k);
            }
        }
        _ => {
            app.editor.input(k);
        }
    }
}

fn cmd_input_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match (k.modifiers, k.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => run_cmd_line(app, tx),
        (KeyModifiers::NONE, KeyCode::Esc) => app.focus = Focus::Editor,
        (KeyModifiers::NONE, KeyCode::Tab) => app.focus = Focus::Preview,
        (KeyModifiers::NONE, KeyCode::Char('[')) => {
            app.redis_db = app.redis_db.saturating_sub(1);
            app.set_placeholder();
        }
        (KeyModifiers::NONE, KeyCode::Char(']')) => {
            app.redis_db = app.redis_db.saturating_add(1);
            app.set_placeholder();
        }
        _ => {
            app.cmd_input.input(k);
        }
    }
}

fn preview_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // Every results-pane command that needs a modifier is a Ctrl combo, so no
    // bare uppercase letter is required. Handle them here and swallow any other
    // Ctrl combo so it can never fall through to a plain-key action.
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('f') => page_turn(app, tx, true),
            KeyCode::Char('b') => page_turn(app, tx, false),
            KeyCode::Char('e') => app.focus = Focus::Editor,
            KeyCode::Char('k') => sort_column(app, tx, true),
            KeyCode::Char('r') => clear_filter(app, tx),
            KeyCode::Char('d') => delete_row(app),
            // Ctrl-Y: export the focused grid to CSV; Ctrl-N: load more rows when
            // the previous query hit the row cap.
            KeyCode::Char('y') => export_csv(app),
            KeyCode::Char('n') => load_more_rows(app, tx),
            _ => {}
        }
        return;
    }
    let screen = viewport_rows(app) as u16;
    let ddl = app.struct_view == StructView::Ddl && app.ddl.is_some();
    // Esc clears an active result search before it does anything else. This
    // applies to the top-level grid and to a drilled script result; only the
    // script *list* has no search to clear.
    if k.code == KeyCode::Esc
        && !app.result_needle.is_empty()
        && !ddl
        && app.script.as_ref().is_none_or(|s| s.drilled.is_some())
    {
        app.result_needle.clear();
        app.rebuild_view();
        app.sel = 0;
        app.status = t("已清除结果搜索").into();
        return;
    }
    match k.code {
        KeyCode::Esc => {
            if let Some(s) = &mut app.script {
                if s.drilled.is_some() {
                    s.drilled = None;
                    app.sel = 0;
                    app.col_offset = 0;
                    app.col_cursor = 0;
                    // A result search does not apply to the statement list.
                    app.result_needle.clear();
                    app.result_filter = None;
                    return;
                }
            }
            if ddl {
                app.struct_view = StructView::Fields;
                return;
            }
            app.show_first_grid();
            app.focus = Focus::Sidebar;
        }
        KeyCode::Char('e') => edit_cell(app),
        KeyCode::Char('i') => quick_insert(app),
        KeyCode::Char('o') => open_row_popup(app),
        // Result tabs: flip between successive query results.
        KeyCode::Char('[') => switch_result_tab(app, -1),
        KeyCode::Char(']') => switch_result_tab(app, 1),
        KeyCode::Char('t') => {
            if app.ddl.is_some() {
                app.struct_view = match app.struct_view {
                    StructView::Fields => StructView::Ddl,
                    StructView::Ddl => StructView::Fields,
                };
                app.ddl_scroll = 0;
            }
        }
        KeyCode::Char('s') => sort_column(app, tx, false),
        KeyCode::Char('f') => open_filter_prompt(app),
        // `/` searches the visible result rows (filter-as-you-type).
        KeyCode::Char('/') => open_result_filter(app),
        // `y` copies the focused row as an INSERT statement (OSC 52 + file).
        KeyCode::Char('y') => copy_row_sql(app),
        // Bare-key aliases for the two view commands (mobile reachability).
        KeyCode::Char('w') => toggle_compact(app),
        KeyCode::Char('c') => open_col_picker(app),
        // Delete the focused row: builds a bound `DELETE … WHERE …` and routes it
        // through the same red confirmation layer as every other write.
        KeyCode::Delete => delete_row(app),
        KeyCode::Char('z') => {
            app.freeze_first = !app.freeze_first;
            app.status = if app.freeze_first {
                t("首列已钉住 · z 取消").into()
            } else {
                t("首列已取消钉住 · z 钉住").into()
            };
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(1);
            } else {
                move_cursor(app, tx, -1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_add(1);
            } else {
                move_cursor(app, tx, 1);
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if !ddl {
                move_col_cursor(app, -1);
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if !ddl {
                move_col_cursor(app, 1);
            }
        }
        KeyCode::PageUp => {
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(screen);
            } else {
                screen_move(app, tx, -1);
            }
        }
        KeyCode::PageDown => {
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_add(screen);
            } else {
                screen_move(app, tx, 1);
            }
        }
        KeyCode::Home => {
            if !ddl {
                app.sel = 0;
            }
        }
        KeyCode::End => {
            if !ddl {
                let n = result_row_count(app);
                if n > 0 {
                    app.sel = n - 1;
                }
            }
        }
        KeyCode::Char('n') => {
            if app.result_needle.trim().is_empty() {
                page_turn(app, tx, true);
            } else {
                search_move(app, 1);
            }
        }
        KeyCode::Char('N') => {
            if app.result_needle.trim().is_empty() {
                app.status = t("先按 / 搜索结果，再用 n/N 跳转命中").into();
            } else {
                search_move(app, -1);
            }
        }
        KeyCode::Char('p') => page_turn(app, tx, false),
        KeyCode::Enter => {
            if let Some(s) = &app.script {
                if s.drilled.is_none() {
                    let idx = s.sel;
                    drill_script(app, idx);
                    return;
                }
            }
            // Row-expand mode: in compact (mobile) mode Enter opens the whole row
            // as a vertical column=value list — the narrow-screen replacement for
            // reading a truncated cell.
            if compact_active(app.compact, app.layout_mode) {
                open_row_popup(app);
            } else {
                open_cell_popup(app);
            }
        }
        // `v`: full cell value. Kept alongside Enter so the cell popup stays
        // reachable when Enter means "expand the row" in compact mode.
        KeyCode::Char('v') => open_cell_popup(app),
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum PopupTarget {
    Cell,
    Row,
}

/// Shared key handling for the scrollable text popups (cell value / row detail).
/// Esc (and q / Enter) close the current popup.
fn popup_key(app: &mut App, k: KeyEvent, target: PopupTarget) {
    if matches!(k.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
        match target {
            PopupTarget::Cell => app.cell_popup = None,
            PopupTarget::Row => app.row_popup = None,
        }
        return;
    }
    let delta: i32 = match k.code {
        KeyCode::Up | KeyCode::Char('k') => -1,
        KeyCode::Down | KeyCode::Char('j') => 1,
        KeyCode::PageUp => -5,
        KeyCode::PageDown => 5,
        _ => 0,
    };
    if delta == 0 {
        return;
    }
    match target {
        PopupTarget::Cell => {
            if let Some(p) = &mut app.cell_popup {
                p.scroll = (p.scroll as i32 + delta).max(0) as u16;
            }
        }
        PopupTarget::Row => {
            if let Some(p) = &mut app.row_popup {
                p.scroll = (p.scroll as i32 + delta).max(0) as u16;
            }
        }
    }
}

fn open_help(app: &mut App) {
    app.help_open = true;
    app.help_scroll = 0;
}

fn help_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.help_open = false,
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.help_scroll = app.help_scroll.saturating_add(1)
        }
        KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(8),
        KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(8),
        _ => {}
    }
}

fn filter_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let filter = app
                .filter_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.filter_prompt = None;
            let order_by = app.page_state.as_ref().and_then(|p| p.order_by.clone());
            if filter.is_empty() {
                app.status = t("过滤已清除").into();
            } else {
                app.status = tf("过滤: {}", &[&(filter)]);
            }
            reload_table_view(app, tx, filter, order_by, 0);
        }
        KeyCode::Esc => {
            app.filter_prompt = None;
            app.status = t("已取消过滤").into();
        }
        _ => {
            if let Some(t) = &mut app.filter_prompt {
                t.input(k);
            }
        }
    }
}

// ── result-grid search (`/` in the results pane) ──

/// `/` in the results pane: filter visible rows as you type. The current needle
/// is loaded for editing, so `/` again refines an existing search.
fn open_result_filter(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图不支持搜索").into();
        return;
    }
    // The script *list* has no data grid to search; once a statement is drilled
    // into, its result is a normal grid and search applies.
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表不支持搜索（先 Enter 进入某条语句的结果）").into();
        return;
    }
    if active_grid(app).is_none() {
        app.status = t("没有可搜索的结果").into();
        return;
    }
    let mut ta = TextArea::from([app.result_needle.clone()]);
    ta.set_placeholder_text(t("搜索本页结果行…"));
    ta.move_cursor(CursorMove::End);
    app.result_filter = Some(ta);
}

/// Filter-as-you-type handler for the result search prompt.
fn result_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.result_filter = None;
            let n = result_row_count(app);
            app.status = if app.result_needle.trim().is_empty() {
                t("结果搜索已清除").into()
            } else {
                tf("搜索「{}」· {} 行命中 · n/N 跳转 · Esc 清除", &[&(app.result_needle), &(n)])
            };
        }
        KeyCode::Esc => {
            app.result_filter = None;
            app.result_needle.clear();
            app.rebuild_view();
            app.sel = 0;
            app.status = t("已清除结果搜索").into();
        }
        _ => {
            if let Some(t) = &mut app.result_filter {
                t.input(k);
            }
            app.result_needle = app
                .result_filter
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.rebuild_view();
            app.sel = 0;
            let n = result_row_count(app);
            app.status = if app.result_needle.trim().is_empty() {
                t("输入以搜索结果行…").into()
            } else {
                tf("搜索「{}」· {} 行命中", &[&(app.result_needle), &(n)])
            };
        }
    }
}

/// `n` / `N` while a result search is active: cycle through the matching rows
/// (the filter already hides every non-match, so every visible row is a hit).
fn search_move(app: &mut App, dir: i32) {
    let n = result_row_count(app);
    if n == 0 {
        app.status = tf("搜索「{}」· 0 行命中", &[&(app.result_needle)]);
        return;
    }
    if dir > 0 {
        app.sel = (app.sel + 1) % n;
    } else {
        app.sel = (app.sel + n - 1) % n;
    }
    app.status = tf("搜索「{}」· 命中 {}/{}", &[&(app.result_needle), &(app.sel + 1), &(n)]);
}

// ── mobile efficiency: compact columns / column visibility / recents / filter ──

/// Ctrl-Shift-C — toggle the compact column-width mode. The first press always
/// flips whatever the current (possibly automatic) state is, so the user sees an
/// immediate change on any screen size.
fn toggle_compact(app: &mut App) {
    let now = compact_active(app.compact, app.layout_mode);
    app.compact = Some(!now);
    let on = compact_active(app.compact, app.layout_mode);
    app.status = if on {
        tf("{} · 列宽≤{} 自适应，尽量一屏放下（Alt-C / w 关闭）", &[&(compact_label(app)), &(COMPACT_MAX_CELL)])
    } else {
        tf("{} · 列宽按内容（Alt-C / w 开启）", &[&(compact_label(app))])
    };
    // Persist the choice: as the global default and, when a table is open, for
    // that exact `database.table` so reopening it restores the mode.
    app.config.set_compact(app.compact);
    if let Some(ps) = app.page_state.clone() {
        let db = app.current_db();
        app.config.entry(&db, &ps.table).compact = app.compact;
    }
    app.persist();
}

/// Ctrl-Shift-H — open the column-visibility picker for the grid on screen.
fn open_col_picker(app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        app.status = t("没有可选择的列（先打开一张表或执行查询）").into();
        return;
    };
    if grid.columns.is_empty() || app.grid_kind == GridKind::Columns {
        app.status = t("当前视图不支持列选择").into();
        return;
    }
    app.col_picker_open = true;
    app.col_picker_list.select(Some(0));
}

fn col_picker_key(app: &mut App, k: KeyEvent) {
    let n = app
        .grid_full
        .as_ref()
        .map(|g| g.columns.len())
        .unwrap_or(0);
    match k.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            app.col_picker_open = false;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app
                    .col_picker_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.col_picker_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .col_picker_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.col_picker_list.select(Some(i));
            }
        }
        KeyCode::Char(' ') => toggle_col_visible(app),
        // `a` shows every column again, `x` narrows to just the first.
        KeyCode::Char('a') => {
            app.col_hidden.clear();
            app.reapply_col_filter();
            app.persist_cols();
            app.status = t("已显示全部列（已记住）").into();
        }
        KeyCode::Char('x') => {
            if let Some(grid) = app.grid_full.clone() {
                app.col_hidden = grid.columns.iter().skip(1).cloned().collect();
                app.reapply_col_filter();
                app.persist_cols();
                app.status = t("仅保留第一列（已记住）").into();
            }
        }
        _ => {}
    }
}

/// Space in the column picker: hide / show the highlighted column. The last
/// visible column can never be hidden.
fn toggle_col_visible(app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        return;
    };
    let Some(i) = app.col_picker_list.selected() else {
        return;
    };
    let Some(name) = grid.columns.get(i).cloned() else {
        return;
    };
    if app.col_hidden.remove(&name) {
        app.reapply_col_filter();
        app.persist_cols();
        app.status = tf("显示列 {} · 已记住", &[&(name)]);
    } else {
        let visible = grid
            .columns
            .iter()
            .filter(|c| !app.col_hidden.contains(c.as_str()))
            .count();
        if visible <= 1 {
            app.status = t("至少保留一列").into();
            return;
        }
        app.col_hidden.insert(name.clone());
        app.reapply_col_filter();
        app.persist_cols();
        app.status = tf("隐藏列 {} · 已记住", &[&(name)]);
    }
}

/// Ctrl-Shift-R — jump straight to one of the last five browsed tables.
fn open_recent_tables(app: &mut App) {
    if app.recent_tables.is_empty() {
        app.status = t("还没有浏览过表").into();
        return;
    }
    app.recent_open = true;
    app.recent_list.select(Some(0));
}

fn recent_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = app.recent_tables.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.recent_open = false,
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app
                    .recent_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.recent_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .recent_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.recent_list.select(Some(i));
            }
        }
        KeyCode::Enter => {
            if let Some(i) = app.recent_list.selected() {
                open_recent(app, tx, i);
            }
        }
        _ => {}
    }
}

/// Remember a table at the head of the recents list (max 5, unique).
fn remember_recent_table(app: &mut App, db: &str, table: &str) {
    let entry = (db.to_string(), table.to_string());
    app.recent_tables.retain(|e| e != &entry);
    app.recent_tables.insert(0, entry);
    app.recent_tables.truncate(5);
}

fn open_recent(app: &mut App, tx: &Tx, idx: usize) {
    let Some((db, table)) = app.recent_tables.get(idx).cloned() else {
        return;
    };
    app.recent_open = false;
    // Another database: switch first and let the table-list reply open the table.
    if db != app.current_db() {
        let Some(pos) = app.databases.iter().position(|d| *d == db) else {
            app.status = tf("✗ 数据库 {} 不在当前连接中", &[&(db)]);
            return;
        };
        app.db_index = pos;
        app.pending_open_table = Some(table.clone());
        app.pending_table = None;
        app.status = tf("切换到 {} 并打开 {} …", &[&(db), &(table)]);
        reload_tables(app, tx);
        return;
    }
    if let Some(pos) = app.tables.iter().position(|t| t.name == table) {
        app.table_list.select(Some(pos));
        open_table_data(app, tx);
    } else {
        app.status = tf("✗ 未找到表 {}（可能被过滤或已删除）", &[&(table)]);
    }
}

// ── sidebar table filter (`/`, filter-as-you-type) ──

/// Recompute the visible table list from `tables_all` + `table_filter`, keeping
/// the previously selected table selected when it still matches.
fn apply_table_filter(app: &mut App) {
    let prev = app.selected_table().map(|t| t.name.clone());
    let needle = app.table_filter.trim().to_lowercase();
    app.tables = if needle.is_empty() {
        app.tables_all.clone()
    } else {
        app.tables_all
            .iter()
            .filter(|t| t.name.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    };
    let n = app.tables.len();
    if n == 0 {
        app.table_list.select(None);
        return;
    }
    let sel = prev
        .and_then(|p| app.tables.iter().position(|t| t.name == p))
        .unwrap_or(0)
        .min(n - 1);
    app.table_list.select(Some(sel));
}

fn open_table_filter(app: &mut App) {
    if app.tables_all.is_empty() {
        app.status = t("还没有表可过滤").into();
        return;
    }
    let mut ta = TextArea::from([app.table_filter.clone()]);
    ta.move_cursor(CursorMove::End);
    app.table_prompt = Some(ta);
}

fn table_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.table_filter = app
                .table_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.table_prompt = None;
            apply_table_filter(app);
            let (n, total) = (app.tables.len(), app.tables_all.len());
            app.status = if app.table_filter.is_empty() {
                tf("{} 个表/视图", &[&(total)])
            } else {
                tf("过滤「{}」· {}/{} 个表 · Esc 清除", &[&(app.table_filter), &(n), &(total)])
            };
        }
        KeyCode::Esc => {
            app.table_prompt = None;
            app.table_filter.clear();
            apply_table_filter(app);
            app.status = tf("已清除表过滤 · {} 个表/视图", &[&(app.tables.len())]);
        }
        _ => {
            if let Some(t) = app.table_prompt.as_mut() {
                t.input(k);
            }
            app.table_filter = app
                .table_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            apply_table_filter(app);
        }
    }
}

// ── SQL prefix completion (Ctrl-Space) ──

/// Keywords offered alongside table / column names. Small on purpose: a TUI
/// completion is a shortcut for long identifiers, not a SQL parser.
const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET",
    "INSERT INTO", "UPDATE", "DELETE FROM", "SET", "VALUES", "JOIN", "LEFT JOIN",
    "INNER JOIN", "ON", "AS", "AND", "OR", "NOT", "NULL", "IS NULL", "LIKE", "IN",
    "BETWEEN", "DISTINCT", "COUNT", "SUM", "AVG", "MIN", "MAX", "CASE", "WHEN",
    "THEN", "ELSE", "END", "ASC", "DESC", "CREATE TABLE", "ALTER TABLE", "DROP TABLE",
    "UNION", "UNION ALL", "EXPLAIN", "WITH",
];

/// The identifier fragment ending at the cursor, and how many characters it is.
fn word_before_cursor(ta: &TextArea) -> (usize, String) {
    let (row, col) = ta.cursor();
    let line = ta.lines().get(row).cloned().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let end = col.min(chars.len());
    let mut start = end;
    while start > 0 {
        let c = chars[start - 1];
        if c.is_alphanumeric() || c == '_' || c == '.' {
            start -= 1;
        } else {
            break;
        }
    }
    (end - start, chars[start..end].iter().collect())
}

/// Everything on the editor's lines up to (not including) the cursor, joined by
/// newlines. Used to look at the keyword that precedes the fragment.
fn text_before_cursor(ta: &TextArea) -> String {
    let (row, col) = ta.cursor();
    let lines = ta.lines();
    let mut out = String::new();
    for l in lines.iter().take(row) {
        out.push_str(l);
        out.push('\n');
    }
    if let Some(line) = lines.get(row) {
        out.extend(line.chars().take(col));
    }
    out
}

/// Read the (possibly quoted, possibly `schema.`) identifier that precedes the
/// final `.` at the cursor — the qualifier of a `qualifier.partial` form.
/// Returns `None` when the cursor is not after such a form.
fn qualifier_before_cursor(ta: &TextArea) -> Option<String> {
    let (row, col) = ta.cursor();
    let line = ta.lines().get(row).cloned().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    // Step back over the fragment being completed to the dot.
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        i -= 1;
    }
    if i == 0 || chars[i - 1] != '.' {
        return None;
    }
    i -= 1; // the dot
    let end = i;
    if end == 0 {
        return None;
    }
    // A quoted qualifier (`` `t` ``, `"t"`, `[t]`) is read back to its opener.
    let close = chars[end - 1];
    let open = match close {
        '`' => Some('`'),
        '"' => Some('"'),
        ']' => Some('['),
        _ => None,
    };
    if let Some(open) = open {
        let mut j = end - 1;
        while j > 0 {
            j -= 1;
            if chars[j] == open {
                return Some(chars[j + 1..end - 1].iter().collect());
            }
        }
        return None;
    }
    // A bare identifier, keeping only the last dot-separated segment.
    let mut start = end;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// Work out what the cursor is completing: the context and the fragment that
/// would be replaced (after the last `.` for a qualified name).
fn completion_context(ta: &TextArea) -> (CompCtx, String) {
    let (n, word) = word_before_cursor(ta);
    if let Some(dot) = word.rfind('.') {
        let partial = word[dot + 1..].to_string();
        if let Some(qual) = qualifier_before_cursor(ta) {
            return (CompCtx::Qualified(qual), partial);
        }
        return (CompCtx::Any, partial);
    }
    // Drop the fragment being completed before looking at the preceding keyword.
    let before = text_before_cursor(ta);
    let keep = before.chars().count().saturating_sub(n);
    let before: String = before.chars().take(keep).collect();
    let head = before.trim_end();
    let last = head
        .split(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ';')
        .rfind(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_uppercase();
    let ctx = match last.as_str() {
        "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE" => CompCtx::TableList,
        "WHERE" | "ON" | "SET" | "BY" | "HAVING" | "SELECT" | "AND" | "OR" => CompCtx::Column,
        _ if head.ends_with('(') => CompCtx::Column,
        _ => CompCtx::Any,
    };
    (ctx, word)
}

/// Column names known for the connection: the browsed table's metadata first,
/// then whatever columns the current result grid carries.
fn column_names(app: &App) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(meta) = &app.table_meta {
        for c in &meta.columns {
            if !out.contains(&c.name) {
                out.push(c.name.clone());
            }
        }
    }
    if let Some(grid) = full_grid(app) {
        for c in &grid.columns {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
    }
    out
}

fn push_item(
    out: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    text: &str,
    kind: char,
    needle: &str,
) {
    let lower = text.to_lowercase();
    if !seen.insert(lower.clone()) {
        return;
    }
    if needle.is_empty() || lower.starts_with(needle) {
        out.push(CompletionItem {
            text: text.to_string(),
            kind,
        });
    }
}

fn push_names(
    out: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    names: &[String],
    kind: char,
    needle: &str,
) {
    for n in names {
        push_item(out, seen, n, kind, needle);
    }
}

/// Candidate list for the fragment before the cursor, ordered by context:
/// `table.` → that table's columns only; after `FROM`/`JOIN` → tables first;
/// after `WHERE`/`ON` → columns first; otherwise columns → tables → keywords.
/// Matching is case-insensitive.
fn completion_candidates(app: &App, ctx: &CompCtx, partial: &str) -> Vec<CompletionItem> {
    let needle = partial.to_lowercase();
    let mut out: Vec<CompletionItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let cols = column_names(app);
    let tables: Vec<String> = app.tables_all.iter().map(|t| t.name.clone()).collect();
    let keywords: Vec<String> = SQL_KEYWORDS.iter().map(|k| (*k).to_string()).collect();
    match ctx {
        CompCtx::Qualified(q) => {
            // Match the qualifier case-insensitively, so `USERS.` still offers
            // the columns of `users`.
            let qcols: Vec<String> = if let Some(meta) = app
                .table_meta
                .as_ref()
                .filter(|m| m.table.eq_ignore_ascii_case(q))
            {
                meta.columns.iter().map(|c| c.name.clone()).collect()
            } else if app
                .page_state
                .as_ref()
                .is_some_and(|p| p.table.eq_ignore_ascii_case(q))
            {
                full_grid(app).map(|g| g.columns).unwrap_or_default()
            } else {
                cols.clone()
            };
            push_names(&mut out, &mut seen, &qcols, 'C', &needle);
        }
        CompCtx::TableList => {
            push_names(&mut out, &mut seen, &tables, 'T', &needle);
            push_names(&mut out, &mut seen, &cols, 'C', &needle);
            push_names(&mut out, &mut seen, &keywords, 'K', &needle);
        }
        CompCtx::Column => {
            push_names(&mut out, &mut seen, &cols, 'C', &needle);
            push_names(&mut out, &mut seen, &tables, 'T', &needle);
            push_names(&mut out, &mut seen, &keywords, 'K', &needle);
        }
        CompCtx::Any => {
            push_names(&mut out, &mut seen, &cols, 'C', &needle);
            push_names(&mut out, &mut seen, &tables, 'T', &needle);
            push_names(&mut out, &mut seen, &keywords, 'K', &needle);
        }
    }
    out.truncate(8);
    out
}

fn open_completion(app: &mut App) {
    let (ctx, partial) = completion_context(&app.editor);
    let items = completion_candidates(app, &ctx, &partial);
    if items.is_empty() {
        app.status = tf("无可补全项（前缀「{}」）", &[&(partial)]);
        return;
    }
    app.completion = Some(Completion {
        items,
        sel: 0,
        replace: partial.chars().count(),
    });
}

/// Recompute the candidate list after the user typed another character.
fn refresh_completion(app: &mut App) {
    let (ctx, partial) = completion_context(&app.editor);
    let items = completion_candidates(app, &ctx, &partial);
    if items.is_empty() {
        app.completion = None;
        return;
    }
    let sel = app
        .completion
        .as_ref()
        .map(|c| c.sel)
        .unwrap_or(0)
        .min(items.len() - 1);
    app.completion = Some(Completion {
        items,
        sel,
        replace: partial.chars().count(),
    });
}

fn accept_completion(app: &mut App) {
    let Some(c) = app.completion.clone() else {
        return;
    };
    let Some(item) = c.items.get(c.sel).cloned() else {
        app.completion = None;
        return;
    };
    let back = c.replace;
    if back > 0 {
        // `delete_str` deletes *forward* from the cursor, so step back to the
        // start of the fragment first.
        let (row, col) = app.editor.cursor();
        app.editor
            .move_cursor(CursorMove::Jump(row as u16, col.saturating_sub(back) as u16));
        app.editor.delete_str(back);
    }
    app.editor.insert_str(&item.text);
    app.completion = None;
}

fn completion_key(app: &mut App, k: KeyEvent) {
    let n = app.completion.as_ref().map(|c| c.items.len()).unwrap_or(0);
    match k.code {
        KeyCode::Esc => app.completion = None,
        KeyCode::Up => {
            if let Some(c) = app.completion.as_mut() {
                c.sel = c.sel.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(c) = app.completion.as_mut() {
                c.sel = (c.sel + 1).min(n.saturating_sub(1));
            }
        }
        KeyCode::Tab | KeyCode::Enter => accept_completion(app),
        _ => {
            app.editor.input(k);
            refresh_completion(app);
        }
    }
}

/// 1-based absolute row number of the cursor across all pages. The result-row
/// search keeps a display→source map, so the reported number is the source row.
fn cursor_abs_row(app: &App) -> usize {
    let src = app.full_row_index().unwrap_or(app.sel);
    match &app.page_state {
        Some(ps) => abs_row(ps.page, ps.page_size, src),
        None => src + 1,
    }
}

/// Show the focused cell's full value in a modal (truncated cells stay readable).
fn open_cell_popup(app: &mut App) {
    let Some(grid) = active_grid(app) else {
        return;
    };
    let Some(row) = grid.rows.get(app.sel) else {
        return;
    };
    let Some(v) = row.get(app.col_cursor) else {
        return;
    };
    let col = grid.columns.get(app.col_cursor).cloned().unwrap_or_default();
    let (text, style) = value_display(v);
    let title = tf("{} · 第 {} 行 · {} 字符", &[&(fix_double_encoding(&col)), &(cursor_abs_row(app)), &(text.chars().count())]);
    app.cell_popup = Some(CellPopup {
        title,
        lines: vec![PopupLine { text, style }],
        scroll: 0,
    });
}

/// Open the focused row as a vertical `column = value` list. Uses the unfiltered
/// grid so a column hidden with Ctrl-Shift-H is still readable here.
fn open_row_popup(app: &mut App) {
    let Some(grid) = full_grid(app) else {
        return;
    };
    let idx = app.full_row_index().unwrap_or(app.sel);
    let Some(row) = grid.rows.get(idx) else {
        return;
    };
    let mut lines: Vec<PopupLine> = Vec::new();
    for (ci, col) in grid.columns.iter().enumerate() {
        let (shown, style) = match row.get(ci) {
            Some(Val::Null) | None => ("NULL".to_string(), null_style()),
            Some(Val::Text(s)) if s.is_empty() => ("''".to_string(), empty_string_style()),
            Some(Val::Text(s)) => (s.clone(), Style::default()),
        };
        lines.push(PopupLine {
            text: format!("{} = {}",  fix_double_encoding(col),  shown),
            style,
        });
    }
    let title = tf("第 {} 行 · {} 列", &[&(cursor_abs_row(app)), &(grid.columns.len())]);
    app.row_popup = Some(RowPopup {
        title,
        lines,
        scroll: 0,
    });
}

// ── edit / insert templates ──

/// Escape a value as a standard SQL string literal (quote doubled, backslash
/// escaped). Fine for MySQL's default mode and standard SQL alike.
fn sql_literal(s: &str) -> String {
    format!("'{}'",  s.replace('\\', "\\\\").replace('\'', "''"))
}

fn is_numeric_type(t: &str) -> bool {
    let lower = t.trim().to_ascii_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("");
    matches!(
        base,
        "int" | "integer"
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

/// Column type from the browsed table's metadata, when available.
fn column_type(app: &App, table: &str, col: &str) -> Option<String> {
    let meta = app.table_meta.as_ref()?;
    if meta.table != table {
        return None;
    }
    meta.columns
        .iter()
        .find(|c| c.name == col)
        .map(|c| c.data_type.clone())
}

/// Render a cell value as a SQL literal, keeping numeric columns unquoted when
/// the value really is a number.
fn val_literal(v: &Val, data_type: Option<&str>) -> String {
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
fn is_binary_type(t: &str) -> bool {
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

/// `X'0A1B'` — the portable SQL hex literal, used for binary columns so a copied
/// row round-trips instead of being mangled by string escaping.
fn hex_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2 + 3);
    out.push_str("X'");
    for b in s.as_bytes() {
        out.push_str(&format!("{b:02X}"));
    }
    out.push('\'');
    out
}

/// Literal used by the copy-row-as-INSERT action: binary columns become hex,
/// everything else follows the edit layer's rules.
fn insert_literal(v: &Val, data_type: Option<&str>) -> String {
    if let (Val::Text(s), Some(dt)) = (v, data_type) {
        if is_binary_type(dt) {
            return hex_literal(s);
        }
    }
    val_literal(v, data_type)
}

/// Build `INSERT INTO t (cols…) VALUES (vals…)` for one row of `grid`.
fn build_insert_sql(
    cfg: &ConnectionConfig,
    table: &str,
    grid: &Grid,
    row: &[Val],
    app: &App,
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
        .map(|(ci, c)| {
            let v = row.get(ci).cloned().unwrap_or(Val::Null);
            insert_literal(&v, column_type(app, table, c).as_deref())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {} ({})\nVALUES ({});",  q(table),  cols,  vals)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Read the (possibly quoted, possibly `schema.`) identifier at the start of
/// `s`, returning its last dot-separated segment.
fn read_ident(s: &str) -> Option<String> {
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
                let end = rest.bytes().position(|b| !is_ident_byte(b)).unwrap_or(rest.len());
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
fn guess_table_from_sql(sql: &str) -> Option<String> {
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

fn base64_encode(data: &[u8]) -> String {
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
fn clipboard_file_path() -> Option<PathBuf> {
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
fn clipboard_copy(text: &str) -> Option<PathBuf> {
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

/// `y` in the results pane: copy the focused row as an `INSERT` statement.
fn copy_row_sql(app: &mut App) {
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
    let table = if let Some(ps) = &app.page_state {
        Some(ps.table.clone())
    } else if let Some(s) = &app.script {
        s.drilled
            .and_then(|i| s.outcomes.get(i))
            .and_then(|o| guess_table_from_sql(&o.sql))
    } else {
        app.last_sql.as_deref().and_then(guess_table_from_sql)
    };
    let Some(table) = table else {
        app.status = t("无法从当前结果确定表名（仅表格浏览与含 FROM 的查询支持 y）").into();
        return;
    };
    let sql = build_insert_sql(&cfg, &table, &full, &row, app);
    let n = sql.chars().count();
    match clipboard_copy(&sql) {
        Some(p) => {
            app.status = tf("✓ 已复制 INSERT（{} 字符）· OSC52 剪贴板 · 兜底 {}", &[&(n), &(p.display())])
        }
        None => app.status = tf("✓ 已复制 INSERT（{} 字符）· OSC52 剪贴板", &[&(n)]),
    }
}

/// New value typed in the edit dialog → SQL literal. A blank box (or `NULL`,
/// any case) means SQL NULL; `''` means the empty string; `'text'` is taken as
/// a literal string; anything else is coerced like a cell value (numbers stay
/// bare for numeric columns, text gets quoted).
fn new_value_literal(input: &str, data_type: Option<&str>) -> String {
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
fn strip_string_literal(s: &str) -> Option<String> {
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
fn edit_prefill(v: &Val) -> String {
    match v {
        Val::Null => String::new(),
        Val::Text(s) => {
            if new_value_literal(s, None) == sql_literal(s) {
                s.clone()
            } else {
                format!("'{}'",  s.replace('\'', "''"))
            }
        }
    }
}

fn build_update_sql(
    cfg: &ConnectionConfig,
    table: &str,
    column: &str,
    data_type: Option<&str>,
    input: &str,
    where_clause: &str,
) -> String {
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    format!(
        "UPDATE {}\nSET {} = {}\nWHERE {};", 
        q(table), 
        q(column), 
        new_value_literal(input, data_type), 
        where_clause
    )
}

impl EditDialog {
    fn update_sql(&self) -> String {
        build_update_sql(
            &self.cfg,
            &self.table,
            &self.column,
            self.data_type.as_deref(),
            &self.new_input.lines().join(" "),
            &self.where_clause,
        )
    }
    fn sql(&self) -> String {
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
fn row_where_clause(
    app: &App,
    grid: &Grid,
    row: &[Val],
    table: &str,
) -> (String, Vec<String>, bool) {
    let (keys, no_pk) = match app.table_meta.as_ref().filter(|m| m.table == table) {
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
            val_literal(v, column_type(app, table, k).as_deref())
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
fn delete_row(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持删除行").into();
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
    let (where_clause, keys, no_pk) = row_where_clause(app, &grid, &row, &ps.table);
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let sql = format!("DELETE FROM {}\nWHERE {};",  q(&ps.table),  where_clause);
    let mut reasons: Vec<String> = Vec::new();
    if no_pk {
        reasons.push(t("⚠ 未检测到主键：将按全部列匹配删除，请确认只命中这一行").into());
    } else {
        reasons.push(tf("将删除 1 行（主键 {}）", &[&(keys.join(", "))]));
    }
    reasons.push(t("DELETE 不可撤销，Enter 后立即执行").into());
    reasons.push(format!("WHERE {where_clause}"));
    app.confirm = Some(Confirm {
        sql,
        reasons,
        refresh: true,
        clear_batch: false,
    });
    app.status = t("删除确认 · Enter 执行 · Esc 取消").into();
}

/// `e` — open a diff-style confirmation layer for the focused cell. The user
/// types the new value, sees old → new plus the WHERE clause, and only then is
/// the UPDATE sent (Enter). Esc cancels, `v` hands the SQL to the editor, `b`
/// queues it for one transactional batch commit.
fn edit_cell(app: &mut App) {
    if !in_table_data_view(app) {
        app.focus = Focus::Editor;
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
    let (where_clause, keys, no_pk) = row_where_clause(app, &grid, &row, &ps.table);

    let dt = column_type(app, &ps.table, &col);
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
    app.status = tf("编辑 {} → Enter 确认执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量", &[&(col)]);
}

/// `i` — open the diff layer with an `INSERT` template built from the table's
/// column list. Enter submits, `v` hands the SQL to the editor.
fn quick_insert(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持快速插入").into();
        return;
    }
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(meta) = app.table_meta.as_ref().filter(|m| m.table == ps.table).cloned() else {
        app.status = t("表结构尚未加载，稍后重试").into();
        return;
    };
    let cols: Vec<&ColumnInfo> = meta
        .columns
        .iter()
        .filter(|c| {
            !c.extra
                .as_deref()
                .map(|e| e.to_ascii_lowercase().contains("auto_increment"))
                .unwrap_or(false)
        })
        .collect();
    if cols.is_empty() {
        app.status = t("没有可插入的列").into();
        return;
    }
    let q = |name: &str| quote_table_identifier(Some(cfg.db_type), name);
    let col_list = cols.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", ");
    let mut preview: Vec<(String, String)> = Vec::new();
    let mut vals: Vec<String> = Vec::new();
    for c in &cols {
        let v = if is_numeric_type(&c.data_type) {
            "0".to_string()
        } else if c.is_nullable {
            "NULL".to_string()
        } else {
            "''".to_string()
        };
        vals.push(v.clone());
        preview.push((fix_double_encoding(&c.name), v));
    }
    let sql = format!(
        "INSERT INTO {} ({})\nVALUES ({});", 
        q(&ps.table), 
        col_list, 
        vals.join(", ")
    );
    app.edit_dialog = Some(EditDialog {
        kind: EditKind::Insert,
        cfg: Box::new(cfg),
        db: app.current_db(),
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
    app.status = tf("插入 {} → Enter 确认执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量", &[&(ps.table)]);
}

/// Keys for the diff-style edit confirmation layer. UPDATE has a live text
/// input, so its commands use Ctrl combos (plain letters must reach the input).
fn edit_dialog_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut d) = app.edit_dialog.take() else {
        return;
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let plain = k.modifiers.is_empty();
    let insert = d.kind == EditKind::Insert;
    let to_editor = ctrl && k.code == KeyCode::Char('v') || (insert && plain && k.code == KeyCode::Char('v'));
    let to_batch = ctrl && k.code == KeyCode::Char('t') || (insert && plain && k.code == KeyCode::Char('b'));
    if k.code == KeyCode::Esc {
        app.status = t("已取消编辑").into();
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
        app.status = tf("已加入批量队列（{} 条）· Ctrl-S 打包提交 · Ctrl-X 清空", &[&(app.batch.len())]);
    } else {
        if d.kind == EditKind::Update {
            d.new_input.input(k);
        }
        app.edit_dialog = Some(d);
    }
}

/// Send a generated write. It still passes the dangerous-statement gate so a
/// write that somehow lacks a bound WHERE gets a second confirmation.
fn submit_edit_sql(app: &mut App, tx: &Tx, sql: String) {
    let mut reason = detect_danger(&sql);
    if reason.is_none() && one_line(&sql).to_ascii_lowercase().contains("where 1 = 1") {
        reason = Some(t("WHERE 恒真（1 = 1），会作用于整张表").into());
    }
    if let Some(reason) = reason {
        app.confirm = Some(Confirm {
            sql,
            reasons: vec![reason],
            refresh: true,
            clear_batch: false,
        });
        return;
    }
    app.push_history(&sql);
    app.pending_write = true;
    execute_sql(app, tx, sql);
}

/// Ctrl-S — package every queued edit into one transaction and ask for
/// confirmation. The full `BEGIN … COMMIT` script is shown in the red layer;
/// nothing runs until Enter (Esc keeps the queue intact).
fn commit_batch(app: &mut App) {
    if app.batch.is_empty() {
        app.status = t("批量队列为空（编辑时按 Ctrl-T 加入）").into();
        return;
    }
    let n = app.batch.len();
    let mut script = String::from("BEGIN;\n");
    for s in &app.batch {
        let s = s.trim().trim_end_matches(';');
        script.push_str(s);
        script.push_str(";\n");
    }
    script.push_str("COMMIT;");
    app.confirm = Some(Confirm {
        sql: script,
        reasons: vec![
            tf("批量事务：{} 条修改将在同一个 BEGIN … COMMIT 中执行", &[&(n)]),
            t("任一语句失败则整体回滚；Enter 后立即执行").into(),
        ],
        refresh: true,
        clear_batch: true,
    });
    app.status = tf("批量提交确认（{} 条）· Enter 执行 · Esc 取消", &[&(n)]);
}


// ── filter / sort ──

fn open_filter_prompt(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持过滤").into();
        return;
    }
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    // Prefill with the focused column so a filter is one `f` away; an existing
    // filter is loaded for editing instead.
    let initial = if ps.filter.trim().is_empty() {
        match (active_grid(app), app.selected.as_ref()) {
            (Some(grid), Some(cfg)) => match grid.columns.get(app.col_cursor) {
                Some(col) => format!("{} = ",  quote_table_identifier(Some(cfg.db_type), col)),
                None => String::new(),
            },
            _ => String::new(),
        }
    } else {
        ps.filter.clone()
    };
    let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
    ta.set_placeholder_text(t("例: city = 'Beijing'（留空回车 = 清除）"));
    ta.move_cursor(CursorMove::End);
    app.filter_prompt = Some(ta);
}

/// Ctrl-R — drop the active filter and reload the first page.
fn clear_filter(app: &mut App, tx: &Tx) {
    let has_filter = app
        .page_state
        .as_ref()
        .map(|p| !p.filter.trim().is_empty())
        .unwrap_or(false);
    if !has_filter {
        app.status = t("当前无过滤条件").into();
        return;
    }
    let order_by = app.page_state.as_ref().and_then(|p| p.order_by.clone());
    reload_table_view(app, tx, String::new(), order_by, 0);
    app.status = t("过滤已清除").into();
}

/// `s` — sort by the focused column, toggling ASC ↔ DESC.
/// Parse a generated ORDER BY expression into `(column, desc)` keys.
fn parse_order_by(order_by: Option<&str>) -> Vec<(String, bool)> {
    let Some(o) = order_by else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in o.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, desc) = if let Some(n) = part
            .strip_suffix(" DESC")
            .or_else(|| part.strip_suffix(" desc"))
        {
            (n.trim(), true)
        } else if let Some(n) = part
            .strip_suffix(" ASC")
            .or_else(|| part.strip_suffix(" asc"))
        {
            (n.trim(), false)
        } else {
            (part, false)
        };
        out.push((unquote_ident(name), desc));
    }
    out
}

/// Strip the dialect quoting from a single identifier.
fn unquote_ident(s: &str) -> String {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        let (a, b) = (chars[0], chars[chars.len() - 1]);
        if (a == '`' && b == '`') || (a == '"' && b == '"') || (a == '[' && b == ']') {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    s.to_string()
}

fn build_order_by(cfg: &ConnectionConfig, keys: &[(String, bool)]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let q = |n: &str| quote_table_identifier(Some(cfg.db_type), n);
    Some(
        keys.iter()
            .map(|(c, d)| format!("{} {}",  q(c),  if *d { "DESC" } else { "ASC" }))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn sort_column(app: &mut App, tx: &Tx, append: bool) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持排序").into();
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
    let Some(col) = grid.columns.get(app.col_cursor).cloned() else {
        return;
    };
    let mut keys = parse_order_by(ps.order_by.as_deref());
    let existing = keys.iter().position(|(c, _)| c == &col);
    if append {
        match existing {
            Some(i) => keys[i].1 = !keys[i].1,
            None => keys.push((col.clone(), false)),
        }
    } else {
        // Single-key sort: toggle direction when this column is already the
        // only sort key, otherwise replace the sort with this column ascending.
        let dir = match keys.first() {
            Some((c, d)) if c == &col && keys.len() == 1 => !*d,
            _ => false,
        };
        keys = vec![(col.clone(), dir)];
    }
    let next = build_order_by(&cfg, &keys);
    let dir = if keys.first().map(|(_, d)| *d).unwrap_or(false) {
        t("降序")
    } else {
        t("升序")
    };
    // Persist the sort for this table so reopening it restores the order.
    let db = app.current_db();
    app.config.entry(&db, &ps.table).order_by = next.clone();
    app.persist();
    reload_table_view(app, tx, ps.filter.clone(), next, 0);
    app.status = tf("按 {} {}{}", &[&(col), &(dir), &(if append { t("（附加排序键）") } else { "" })]);
}

fn drill_script(app: &mut App, idx: usize) {
    if let Some(s) = &mut app.script {
        if idx < s.outcomes.len() {
            s.drilled = Some(idx);
            app.sel = 0;
            app.col_offset = 0;
            app.col_cursor = 0;
        }
    }
}

fn run_current(app: &mut App, tx: &Tx) {
    match app.backend_kind {
        Backend::Sql => run_sql(app, tx),
        Backend::Redis | Backend::Mongo => run_cmd_line(app, tx),
    }
}

fn run_sql(app: &mut App, tx: &Tx) {
    let sql = app.editor_sql();
    let sql = sql.trim().to_string();
    if sql.is_empty() {
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    // Danger check runs per statement so `UPDATE a; DELETE FROM b;` is caught too.
    let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
    let mut reasons: Vec<String> = Vec::new();
    for st in &statements {
        if let Some(r) = detect_danger(st) {
            if !reasons.contains(&r) {
                reasons.push(r);
            }
        }
    }
    if !reasons.is_empty() {
        app.confirm = Some(Confirm {
            sql,
            reasons,
            refresh: false,
            clear_batch: false,
        });
        return;
    }
    app.push_history(&sql);
    execute_sql(app, tx, sql);
}

fn execute_sql(app: &mut App, tx: &Tx, sql: String) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.loading = true;
    app.status = t("执行中…").into();
    let db = app.current_db();
    app.spawn(
        tx,
        Op::Query(Box::new(cfg), db, sql, QUERY_MAX_ROWS),
    );
}

/// `Ctrl-N`: re-run the last query with a larger row cap when it was truncated.
fn load_more_rows(app: &mut App, tx: &Tx) {
    let Some((sql, cap)) = app.query_more.clone() else {
        app.status = t("没有可加载的更多结果（结果未截断）").into();
        return;
    };
    if cap >= QUERY_MAX_ROWS_CAP {
        app.status = tf("已达上限 {} 行，请用 WHERE / LIMIT 缩小查询", &[&(QUERY_MAX_ROWS_CAP)]);
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let next = (cap + QUERY_MORE_STEP).min(QUERY_MAX_ROWS_CAP);
    app.loading = true;
    app.status = tf("加载更多… (上限 {} 行)", &[&(next)]);
    let db = app.current_db();
    app.spawn(tx, Op::Query(Box::new(cfg), db, sql, next));
}

fn run_cmd_line(app: &mut App, tx: &Tx) {
    let cmd = app.cmd_input.lines().join(" ").trim().to_string();
    if cmd.is_empty() {
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.cmd_input = TextArea::default();
    app.set_placeholder();
    app.loading = true;
    match app.backend_kind {
        Backend::Redis => {
            app.cmd_output
                .push(format!("redis[{}]> {cmd}",  app.redis_db));
            app.spawn(
                tx,
                Op::Redis(Box::new(cfg), app.redis_db, cmd),
            );
        }
        Backend::Mongo => {
            // `use dbname` switches the mongo database locally. The selected
            // database (not a separate field) is what gets passed to every
            // command, so the shell prompt and the executed op can never drift.
            if let Some(db) = cmd.strip_prefix("use ") {
                let db = db.trim().trim_end_matches(';').trim().to_string();
                if db.is_empty() {
                    app.status = t("✗ use 需要数据库名").into();
                    app.loading = false;
                    return;
                }
                app.select_database(&db);
                app.cmd_output.push(format!("switched to db {db}"));
                app.set_placeholder();
                app.loading = false;
                reload_tables(app, tx);
                return;
            }
            app.cmd_output
                .push(format!("mongo({})> {cmd}",  app.current_db()));
            app.spawn(
                tx,
                Op::Mongo(Box::new(cfg), app.current_db(), cmd),
            );
        }
        Backend::Sql => run_sql(app, tx),
    }
}

// ── new connection form ──

fn form_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let f = &mut app.form;
    if f.editing {
        match k.code {
            KeyCode::Enter | KeyCode::Esc => {
                if k.code == KeyCode::Esc {
                    // cancel: clear current field
                    match f.field {
                        0 => f.name.clear(),
                        1 => f.db_type = "mysql".into(),
                        2 => f.host.clear(),
                        3 => f.port.clear(),
                        4 => f.username.clear(),
                        5 => f.password.clear(),
                        6 => f.database.clear(),
                        _ => {}
                    }
                }
                f.editing = false;
            }
            KeyCode::Backspace => match f.field {
                0 => {
                    f.name.pop();
                }
                1 => {
                    f.db_type.pop();
                }
                2 => {
                    f.host.pop();
                }
                3 => {
                    f.port.pop();
                }
                4 => {
                    f.username.pop();
                }
                5 => {
                    f.password.pop();
                }
                6 => {
                    f.database.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match f.field {
                0 => f.name.push(c),
                1 => f.db_type.push(c.to_ascii_lowercase()),
                2 => f.host.push(c),
                3 if c.is_ascii_digit() => f.port.push(c),
                4 => f.username.push(c),
                5 => f.password.push(c),
                6 => f.database.push(c),
                _ => {}
            },
            _ => {}
        }
        return;
    }
    match k.code {
        KeyCode::Esc => {
            app.page = Page::Browse;
            app.focus = Focus::Sidebar;
        }
        KeyCode::Up => f.field = (f.field + form_fields().len() - 1) % form_fields().len(),
        KeyCode::Down | KeyCode::Tab => f.field = (f.field + 1) % form_fields().len(),
        KeyCode::Enter => {
            if f.field == 7 {
                f.ssl = !f.ssl;
            } else if f.field == 8 {
                save_form(app, tx);
            } else {
                f.editing = true;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            f.field = (f.field + form_fields().len() - 1) % form_fields().len()
        }
        KeyCode::Right | KeyCode::Char('l') => f.field = (f.field + 1) % form_fields().len(),
        _ => {}
    }
}

fn save_form(app: &mut App, tx: &Tx) {
    let f = app.form.clone();
    if f.name.trim().is_empty() || f.host.trim().is_empty() {
        app.form.err = t("name / host 必填").into();
        return;
    }
    let Ok(db_type) = parse_database_type(&f.db_type) else {
        app.form.err = tf("未知类型: {} (mysql / postgres / redis / mongodb …)", &[&(f.db_type)]);
        return;
    };
    let port = f
        .port
        .trim()
        .parse::<u16>()
        .ok()
        .or_else(|| dbx_core::database_manifest::default_port(&db_type))
        .unwrap_or(0);
    let cfg = match new_connection_config(
        Uuid::new_v4().to_string(),
        f.name.trim().to_string(),
        db_type,
        f.host.trim().to_string(),
        port,
        f.username.trim().to_string(),
        f.password.clone(),
        if f.database.trim().is_empty() {
            None
        } else {
            Some(f.database.trim().to_string())
        },
        f.ssl,
        None,
    ) {
        Ok(c) => c,
        Err(e) => {
            app.form.err = e;
            return;
        }
    };
    app.form.err.clear();
    app.loading = true;
    app.status = t("保存连接…").into();
    app.spawn(tx, Op::AddConn(Box::new(cfg)));
}

impl App {
    /// Select `db` in the database list, appending it when it is not present
    /// (MongoDB `use <db>` on a database with no collections yet).
    fn select_database(&mut self, db: &str) {
        match self.databases.iter().position(|d| d == db) {
            Some(i) => self.db_index = i,
            None => {
                self.databases.push(db.to_string());
                self.db_index = self.databases.len() - 1;
            }
        }
    }
    /// When leaving the DDL sub-view with Esc, make sure a field list is visible.
    fn show_first_grid(&mut self) {
        self.struct_view = StructView::Fields;
    }
}

// ─── DBX-parity feature helpers ──────────────────────────────────────────────

/// Parse a `#rrggbb` / `rrggbb` connection colour into a terminal colour.
fn parse_hex_color(s: &str) -> Option<Color> {
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
fn db_type_color(db_type: &str) -> Color {
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
fn connection_color(cfg: &ConnectionConfig) -> Color {
    cfg.color
        .as_deref()
        .and_then(parse_hex_color)
        .unwrap_or_else(|| db_type_color(cfg.db_type.as_str()))
}

/// Build the EXPLAIN statement for the current dialect. `None` when the engine
/// has no single-statement EXPLAIN (SQL Server / Oracle need a session toggle).
fn explain_sql_for(db_type: &str, sql: &str) -> Option<String> {
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
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"",  s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Serialise a grid to CSV. NULL becomes an empty field (the same convention
/// DBX's own CSV export uses).
fn grid_to_csv(grid: &Grid) -> String {
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

/// True when the active grid has columns hidden to the right, i.e. horizontal
/// panning would actually change what is on screen.
fn has_h_scroll(app: &App) -> bool {
    if app.grid_kind == GridKind::Columns {
        return false;
    }
    let Some(grid) = active_grid(app) else {
        return false;
    };
    let n = grid.columns.len();
    if n == 0 {
        return false;
    }
    n > app.grid_frozen + visible_now(app, &grid, app.col_offset)
}

/// How many columns fit starting at `off`, using the geometry the last render
/// captured. Exact rather than remembered, so a pan can place the cell cursor
/// where the renderer will actually keep the window.
fn visible_now(app: &App, grid: &Grid, off: usize) -> usize {
    visible_cols(
        grid,
        off,
        app.grid_avail.max(MIN_CELL_WIDTH),
        app.grid_max_cell.max(MIN_CELL_WIDTH),
    )
    .max(1)
}

/// Pure core of `pan_columns`: move the window by `delta` columns and place the
/// cell cursor inside it. `vis` is the number of columns that fit at the new
/// origin, so the result is exactly what `window_for_cursor` will keep.
fn pan_window(
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

/// Pan the visible column *window* by `delta` columns and pull the cell cursor
/// along so it never leaves the screen.
///
/// The window itself moves: a wheel notch, a swipe step or a `◀`/`▶` tap has to
/// change what is on screen immediately. (Moving only the cursor, as the first
/// implementation did, made a horizontal swipe look dead until the cursor had
/// walked past the right edge of the window.)
/// Returns true when a grid with a horizontal overflow handled it.
fn pan_columns(app: &mut App, delta: i32) -> bool {
    if !has_h_scroll(app) {
        return false;
    }
    let Some(grid) = active_grid(app) else {
        return false;
    };
    let n = grid.columns.len();
    let min_off = app.grid_frozen.min(n - 1);
    let target = (app.col_offset as i32 + delta).clamp(min_off as i32, n as i32 - 1) as usize;
    let vis = visible_now(app, &grid, target);
    let (off, cursor) = pan_window(
        n,
        app.grid_frozen,
        app.col_offset,
        app.col_cursor,
        vis,
        delta,
    );
    app.col_offset = off;
    app.col_cursor = cursor;
    true
}

/// Does this wheel event mean "pan columns" rather than "scroll rows"?
///
/// A terminal is only required to put a modifier into a wheel event's SGR button
/// byte if it chooses to; many PC terminals and tmux never set the SHIFT bit for
/// Shift+wheel (and some swallow Shift+wheel for their own horizontal scroll), so
/// the app cannot rely on SHIFT alone. ALT and CONTROL are reported far more
/// reliably, and `Ctrl-G` pan mode works regardless of what the terminal sends.
fn wheel_wants_pan(
    focus: Focus,
    mods: KeyModifiers,
    pan_mode: bool,
    has_h_scroll: bool,
) -> bool {
    let modifier_pan = mods.intersects(
        KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
    );
    focus == Focus::Preview && (pan_mode || modifier_pan) && has_h_scroll
}

/// Does this wheel event mean "pan columns" rather than "scroll rows"?
fn wheel_pans_columns(app: &App, m: &MouseEvent) -> bool {
    wheel_wants_pan(app.focus, m.modifiers, app.pan_mode, has_h_scroll(app))
}

/// How many recent mouse events the `DBXT_MOUSE_DEBUG` overlay keeps.
const MOUSE_DEBUG_LINES: usize = 6;

/// The exact wire encoding that would produce this event, so a user can report
/// (or replay) the bytes their terminal sends. crossterm parses both the modern
/// SGR encoding and the legacy X10 one, so both are printed.
fn mouse_wire_hint(m: &MouseEvent) -> String {
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
            cb,  m.column,  m.row
        ));
    }
    out
}

/// One line describing a mouse event, with the encoding it arrived in.
fn describe_mouse(m: &MouseEvent) -> String {
    let mods = if m.modifiers.is_empty() {
        String::new()
    } else {
        format!(" mods={:?}",  m.modifiers)
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
fn describe_event(ev: &Event) -> Option<String> {
    match ev {
        Event::Mouse(m) => Some(format!("Mouse {}",  describe_mouse(m))),
        Event::Resize(w, h) => Some(format!("Resize {w}x{h}")),
        _ => None,
    }
}

/// Short one-line form used by the status bar, so a long wire encoding never
/// crowds out the page / row / column readout next to it.
fn describe_event_short(ev: &Event) -> Option<String> {
    match ev {
        Event::Mouse(m) => {
            let mods = if m.modifiers.is_empty() {
                String::new()
            } else {
                format!(" mods={:?}",  m.modifiers)
            };
            Some(format!("Mouse {:?} @({},{}){}",  m.kind,  m.column,  m.row,  mods))
        }
        Event::Resize(w, h) => Some(format!("Resize {w}x{h}")),
        _ => None,
    }
}

/// Append a mouse/resize event to the trace file and remember it for the status
/// bar. Only mouse and resize events are traced: never keystrokes, so a password
/// typed into the connection form can never be written to disk.
fn trace_event(app: &mut App, ev: &Event) {
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
fn query_tab_title(sql: &str) -> String {
    let first = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    truncate_disp(first, 40)
}

impl App {
    /// Store a freshly fetched grid: the unfiltered original is kept so the
    /// column-visibility filter can be re-applied later, and the display grid is
    /// the filtered view (hidden columns + the result-row search). The structure
    /// field list is never filtered.
    fn set_grid(&mut self, grid: Grid) {
        self.grid_full = Some(grid);
        self.rebuild_view();
    }

    fn clear_grid(&mut self) {
        self.grid = None;
        self.grid_full = None;
        self.result_rows.clear();
    }

    /// Re-apply the session column selection to the grid on screen.
    fn reapply_col_filter(&mut self) {
        self.rebuild_view();
    }

    /// Recompute the displayed grid from `grid_full` by applying the hidden-column
    /// set and the result-row search, and rebuild the display→source row map that
    /// the popups and `y` (copy as INSERT) rely on.
    fn rebuild_view(&mut self) {
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
        let needle = self.result_needle.trim().to_lowercase();
        if needle.is_empty() || self.grid_kind == GridKind::Columns {
            self.result_rows = (0..cols.rows.len()).collect();
            self.grid = Some(cols);
            return;
        }
        let mut rows = Vec::new();
        let mut map = Vec::new();
        for (i, r) in cols.rows.iter().enumerate() {
            if row_matches(r, &needle) {
                rows.push(r.clone());
                map.push(i);
            }
        }
        self.grid = Some(Grid {
            columns: cols.columns,
            rows,
            note: cols.note,
        });
        self.result_rows = map;
    }

    /// Index of the focused display row in the *unfiltered* grid. The result-row
    /// search keeps a display→source map, so this is the identity when no row
    /// filter is active. Handles both the top-level grid and a drilled script
    /// result.
    fn full_row_index(&self) -> Option<usize> {
        if let Some(s) = &self.script {
            let i = s.drilled?;
            let full = &s.outcomes.get(i)?.grid;
            let needle = self.result_needle.trim().to_lowercase();
            if needle.is_empty() {
                return (self.sel < full.rows.len()).then_some(self.sel);
            }
            // filter_grid only drops columns, so row indices still line up with
            // `full.rows`; the map translates the searched display row back.
            let cols = filter_grid(full, &self.col_hidden);
            let map: Vec<usize> = cols
                .rows
                .iter()
                .enumerate()
                .filter(|(_, r)| row_matches(r, &needle))
                .map(|(i, _)| i)
                .collect();
            return map.get(self.sel).copied();
        }
        if self.result_rows.is_empty() {
            return None;
        }
        self.result_rows.get(self.sel).copied()
    }

    /// Persist the in-memory config (best-effort, silent on failure).
    fn persist(&self) {
        if let Some(p) = &self.config_path {
            self.config.save(p);
        }
    }

    /// Save the current table's hidden-column set under its `(database, table)`.
    fn persist_cols(&mut self) {
        let Some(ps) = self.page_state.clone() else {
            return;
        };
        let db = self.current_db();
        self.config.entry(&db, &ps.table).hidden = self.col_hidden.clone();
        self.persist();
    }

    /// Copy the on-screen query result back into its tab before leaving it.
    fn save_result_tab(&mut self) {
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
        }
    }

    /// Show the active tab's stored view.
    fn restore_result_tab(&mut self) {
        let Some(tab) = self.result_tabs.get(self.result_tab).cloned() else {
            return;
        };
        self.grid_full = tab.grid_full.clone();
        self.script = tab.script;
        self.grid_kind = tab.kind;
        if self.script.is_some() {
            // A result search belongs to a data grid, not the script list.
            self.result_needle.clear();
            self.result_filter = None;
        }
        self.rebuild_view();
        self.sel = tab.sel.min(self.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0).saturating_sub(1));
        self.col_offset = tab.col_offset;
        self.col_cursor = tab.col_cursor;
        self.page_state = None;
        self.cell_popup = None;
        self.row_popup = None;
    }
}

/// Record a fresh query result as a new tab and show it.
fn push_result_tab(
    app: &mut App,
    title: String,
    grid: Option<Grid>,
    script: Option<ScriptView>,
    kind: GridKind,
) {
    app.save_result_tab();
    app.result_tabs.push(ResultTab {
        title,
        grid: grid
            .as_ref()
            .map(|g| if kind == GridKind::Columns { g.clone() } else { filter_grid(g, &app.col_hidden) }),
        grid_full: grid.clone(),
        script: script.clone(),
        kind,
        sel: 0,
        col_offset: 0,
        col_cursor: 0,
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
    app.page_state = None;
    app.cell_popup = None;
    app.row_popup = None;
}

/// Show a result in place of the active tab (used by Ctrl-N "load more").
fn replace_result_tab(
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
    app.result_tabs[idx] = ResultTab {
        title,
        grid: grid
            .as_ref()
            .map(|g| if kind == GridKind::Columns { g.clone() } else { filter_grid(g, &app.col_hidden) }),
        grid_full: grid.clone(),
        script: script.clone(),
        kind,
        sel: 0,
        col_offset: 0,
        col_cursor: 0,
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
    app.page_state = None;
    app.cell_popup = None;
    app.row_popup = None;
}

/// `[` / `]`: flip between the query results kept in this session.
fn switch_result_tab(app: &mut App, delta: i32) {
    if app.result_tabs.len() < 2 {
        app.status = t("仅 1 个结果标签").into();
        return;
    }
    app.save_result_tab();
    let n = app.result_tabs.len() as i32;
    app.result_tab = (app.result_tab as i32 + delta).rem_euclid(n) as usize;
    app.restore_result_tab();
    app.status = tf("结果 {}/{}", &[&(app.result_tab + 1), &(app.result_tabs.len())]);
}

/// `Ctrl-P`: run the editor's SQL through the dialect's EXPLAIN.
fn explain_current(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let sql = app.editor_sql();
    match explain_sql_for(cfg.db_type.as_str(), &sql) {
        Some(explain) => {
            app.loading = true;
            app.status = format!("{} EXPLAIN…",  cfg.db_type.as_str());
            let db = app.current_db();
            app.spawn(
                tx,
                Op::Query(Box::new(cfg), db, explain, QUERY_MAX_ROWS),
            );
        }
        None => {
            app.status = tf("{} 不支持单语句 EXPLAIN（请手动执行）", &[&(cfg.db_type.as_str())]);
        }
    }
}

/// `Ctrl-Y`: export the focused result grid to a CSV file under `$HOME`.
fn export_csv(app: &mut App) {
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可导出的结果").into();
        return;
    };
    if grid.columns.is_empty() {
        app.status = t("没有可导出的列").into();
        return;
    }
    let csv = grid_to_csv(&grid);
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let raw_label = match app.grid_kind {
        GridKind::TableData => app
            .page_state
            .as_ref()
            .map(|p| p.table.clone())
            .unwrap_or_else(|| "table".into()),
        _ => "query".into(),
    };
    let label: String = raw_label
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ' ' { '_' } else { c })
        .collect();
    let path = dir.join(format!("dbxt-export-{label}-{ts}.csv"));
    match std::fs::write(&path, csv.as_bytes()) {
        Ok(_) => {
            let rows = grid.rows.len();
            app.status = tf("✓ 已导出 {} 行 → {}", &[&(rows), &(path.display())]);
        }
        Err(e) => app.status = tf("✗ 导出失败: {}", &[&(e)]),
    }
}

/// `Ctrl-O`: open the saved-SQL snippet overlay for the current connection.
fn open_snippets(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("先选择连接").into();
        return;
    };
    app.loading = true;
    app.status = t("加载 SQL 片段…").into();
    app.spawn(tx, Op::Snippets(Box::new(cfg)));
}

fn snippet_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = app.snippets.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.snippet_open = false,
        KeyCode::Up => {
            let i = app
                .snippet_list
                .selected()
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0);
            if n > 0 {
                app.snippet_list.select(Some(i));
            }
        }
        KeyCode::Down => {
            let i = app
                .snippet_list
                .selected()
                .map(|i| (i + 1).min(n.saturating_sub(1)))
                .unwrap_or(0);
            if n > 0 {
                app.snippet_list.select(Some(i));
            }
        }
        KeyCode::Char('r') if k.modifiers.is_empty() => open_snippets(app, tx),
        // `s`: save the editor's SQL as a new DBX favourite.
        KeyCode::Char('s') if k.modifiers.is_empty() => open_snippet_name(app),
        KeyCode::Enter => {
            if let Some(i) = app.snippet_list.selected() {
                if let Some((name, sql)) = app.snippets.get(i).cloned() {
                    let existing = app.editor_sql();
                    let merged = if existing.trim().is_empty() {
                        sql
                    } else {
                        format!("{}\n{sql}",  existing.trim_end())
                    };
                    app.set_editor_text(&merged);
                    app.snippet_open = false;
                    app.focus = Focus::Editor;
                    app.status = tf("✓ 已插入 {}", &[&(name)]);
                }
            }
        }
        _ => {}
    }
}

/// Name prompt for "save the current SQL as a favourite".
fn open_snippet_name(app: &mut App) {
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

fn snippet_name_key(app: &mut App, tx: &Tx, k: KeyEvent) {
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
            app.spawn(
                tx,
                Op::SaveSnippet(Box::new(cfg), name, sql),
            );
        }
        KeyCode::Esc => {
            app.snippet_name = None;
            app.status = t("已取消收藏").into();
        }
        _ => {
            if let Some(t) = app.snippet_name.as_mut() {
                t.input(k);
            }
        }
    }
}

/// `p` in the connection picker: pre-fill the form with a copy of a connection.
fn duplicate_connection(app: &mut App) {
    let Some(idx) = app.conn_list.selected() else {
        return;
    };
    let Some(cfg) = app.connections.get(idx).cloned() else {
        return;
    };
    app.form = ConnForm {
        name: tf("{} (副本)", &[&(cfg.name)]),
        db_type: cfg.db_type.as_str().to_string(),
        host: cfg.host.clone(),
        port: if cfg.port == 0 {
            String::new()
        } else {
            cfg.port.to_string()
        },
        username: cfg.username.clone(),
        password: cfg.password.clone(),
        database: cfg.database.clone().unwrap_or_default(),
        ssl: cfg.ssl,
        field: 0,
        editing: false,
        err: String::new(),
    };
    app.page = Page::NewConn;
    app.status = tf("复制连接 {} · 改参数后 Enter 保存", &[&(cfg.name)]);
}

// ─── rendering ───────────────────────────────────────────────────────────────

fn ui(f: &mut Frame, app: &mut App) {
    let (w, h) = (f.area().width, f.area().height);
    app.layout_mode = layout_mode(w);
    app.term_h = h;
    app.rects = Rects::default();

    let header_h = if h < 14 { 0 } else { 1 };
    let status_h = if h < 10 { 0 } else { 1 };
    let footer_h = if h < 12 { 0 } else { 1 };

    let chunks = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(3),
        Constraint::Length(status_h),
        Constraint::Length(footer_h),
    ])
    .split(f.area());

    if header_h > 0 {
        render_header(f, chunks[0], app);
    }
    match app.page {
        Page::Browse => render_browse(f, chunks[1], app),
        Page::NewConn => render_form(f, chunks[1], app),
    }
    if status_h > 0 {
        render_status(f, chunks[2], app);
    }
    if footer_h > 0 {
        render_footer(f, chunks[3], app);
    }

    if app.page == Page::Browse && app.picker_open && app.selected.is_none() {
        render_conn_picker(f, f.area(), app);
    }
    if app.db_picker_open {
        render_db_picker(f, f.area(), app);
    }
    if app.snippet_open {
        render_snippets(f, f.area(), app);
    }
    if app.recent_open {
        render_recent_tables(f, f.area(), app);
    }
    if app.col_picker_open {
        render_col_picker(f, f.area(), app);
    }
    if app.table_prompt.is_some() {
        render_table_filter(f, f.area(), app);
    }
    if app.result_filter.is_some() {
        render_result_filter(f, f.area(), app);
    }
    if app.snippet_name.is_some() {
        render_snippet_name(f, f.area(), app);
    }
    if let Some(confirm) = app.confirm.clone() {
        render_confirm(f, f.area(), &confirm);
    }
    if let Some(popup) = app.cell_popup.clone() {
        render_text_popup(f, f.area(), &popup.title, &popup.lines, popup.scroll);
    }
    if let Some(popup) = app.row_popup.clone() {
        render_text_popup(f, f.area(), &popup.title, &popup.lines, popup.scroll);
    }
    if app.filter_prompt.is_some() {
        render_filter_prompt(f, f.area(), app);
    }
    if app.edit_dialog.is_some() {
        render_edit_dialog(f, f.area(), app);
    }
    if app.help_open {
        render_help(f, f.area(), app);
    }
    // The completion popup sits just under the editor, over whatever is below.
    if app.completion.is_some() {
        render_completion(f, app);
    }
    if app.mouse_debug {
        render_mouse_debug(f, f.area(), app);
    }
}

/// Live mouse-event readout for `DBXT_MOUSE_DEBUG`: a small floating panel drawn
/// last, over the normal UI, so a phone user can swipe and read what their
/// terminal encoded it as without the layout changing underneath.
fn render_mouse_debug(f: &mut Frame, area: Rect, app: &App) {
    if app.mouse_log.is_empty() || area.width < 30 || area.height < 8 {
        return;
    }
    let w = area.width.saturating_sub(4).min(78);
    let inner_w = w.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .mouse_log
        .iter()
        .map(|l| Line::from(truncate_disp(l, inner_w)))
        .collect();
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) - 2,
        y: area.y + 1,
        width: w,
        height: h,
    };
    let title = tf(" 鼠标事件 DBXT_MOUSE_DEBUG · 横滑={} ", &[&format!("{:?}", app.drag_pan)]);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(Span::styled(title, Style::default().fg(Color::Magenta))),
        ),
        rect,
    );
}

fn fit_status(msg: &str, width: usize) -> String {
    let n = disp_width(msg);
    if n <= width {
        return msg.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if msg.starts_with('✗') || msg.starts_with('✓') {
        // 错误 / 成功确认：关键信息在前，保留头部，尾部截断
        truncate_disp(msg, width)
    } else {
        // 普通消息：进度类根因常在尾部，保留尾部。
        // Keep the last `width - 1` *display cells* (not chars) so a CJK
        // message is not over-skipped into an empty `…` on a narrow screen.
        let budget = width - 1;
        let mut tail = String::new();
        let mut used = 0usize;
        for c in msg.chars().rev() {
            let cw = disp_width(&c.to_string());
            if used + cw > budget {
                break;
            }
            used += cw;
            tail.insert(0, c);
        }
        format!("…{tail}")
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let conn = app
        .selected
        .as_ref()
        .map(|c| format!("{} ({})",  c.name,  c.db_type.as_str()))
        .unwrap_or_else(|| t("未连接").into());
    let db = if app.selected.is_some() && !app.current_db().is_empty() {
        format!(" · db:{}",  fix_double_encoding(&app.current_db()))
    } else {
        String::new()
    };
    let mode = match app.backend_kind {
        Backend::Sql => "",
        Backend::Redis => " · redis",
        Backend::Mongo => " · mongo",
    };
    let spinner = if app.loading {
        // Show elapsed seconds once a call is slow enough to be worth noticing,
        // so a long query reads as "working" rather than "hung".
        let secs = app
            .loading_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if secs >= 3 {
            format!(" {} {}s",  spinner_frame(app.spinner),  secs)
        } else {
            format!(" {}",  spinner_frame(app.spinner))
        }
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(
            " dbxt ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(conn, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(db, Style::default().fg(Color::Cyan)),
        Span::styled(mode, Style::default().fg(Color::Magenta)),
        Span::styled(spinner, Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn spinner_frame(i: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[i % FRAMES.len()]
}

/// Right-hand section of the status bar: context about the current result set.
fn context_info(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Mobile efficiency markers go first: on a phone the status bar is narrow,
    // and whether the wide table now fits is the single most useful fact.
    let mut fits: Option<usize> = None;
    if let Some(grid) = &app.grid {
        if app.grid_kind != GridKind::Columns && !grid.columns.is_empty() {
            let ncols = grid.columns.len();
            if app.grid_frozen + app.vis_cols.max(1) >= ncols {
                fits = Some(ncols);
            }
        }
    }
    if let Some(ncols) = fits {
        parts.push(tf("全部 {} 列已适配", &[&(ncols)]));
    }
    if compact_active(app.compact, app.layout_mode) {
        parts.push(if app.compact.is_none() {
            t("紧凑列").into()
        } else {
            t("紧凑列 手动").into()
        });
    }
    if !app.col_hidden.is_empty() {
        parts.push(tf("隐藏列 {}", &[&(app.col_hidden.len())]));
    }
    // Touch fallback: vertical wheel pans columns (Ctrl-G).
    if app.pan_mode {
        parts.push(t("横滚 开").into());
    }
    match app.focus {
        Focus::Sidebar => parts.push(t("焦点 侧栏").into()),
        Focus::Editor => parts.push(t("焦点 SQL").into()),
        Focus::CmdInput => parts.push(t("焦点 命令").into()),
        Focus::Preview => parts.push(t("焦点 结果").into()),
    }
    // Small marker for the responsive-collapse master switch (Ctrl-A).
    parts.push(if app.auto_collapse {
        t("自动折叠 开").into()
    } else {
        t("自动折叠 关").into()
    });
    // Result tabs (queries only).
    if app.result_tabs.len() > 1 && app.grid_kind != GridKind::TableData {
        parts.push(tf("结果 {}/{}", &[&(app.result_tab + 1), &(app.result_tabs.len())]));
    }
    // Live event readout while `DBXT_EVENT_TRACE` is set, so a user can report
    // exactly which events their terminal sends for a swipe. Pushed last so the
    // page / row / column readout survives the status-bar truncation (the event
    // text is the first thing that can go); with the `DBXT_MOUSE_DEBUG` panel on
    // screen the status bar keeps its width instead.
    let event_part = if app.trace_path.is_some() && !app.mouse_debug {
        app.last_event.as_ref().map(|ev| tf("事件 {}", &[&(ev)]))
    } else {
        None
    };
    if let Some(ps) = &app.page_state {
        let pages = ps
            .total
            .map(|t| page_count(t, ps.page_size).to_string())
            .unwrap_or_else(|| "?".into());
        parts.push(tf("第 {}/{} 页", &[&(ps.page + 1), &(pages)]));
    }
    let n = result_row_count(app);
    if n > 0 {
        let cur = cursor_abs_row(app);
        let total_abs = app
            .page_state
            .as_ref()
            .and_then(|ps| ps.total)
            .map(|t| t as usize)
            .unwrap_or(n);
        parts.push(tf("行 {}/{}", &[&(cur), &(total_abs)]));
    }
    // Horizontal position is always visible for data grids so the user can tell
    // at a glance that more columns exist off-screen. With a pinned prefix the
    // label reads `列 1|3-8/21` (pinned | scrolled).
    if let Some(grid) = &app.grid {
        if app.grid_kind != GridKind::Columns && !grid.columns.is_empty() {
            let ncols = grid.columns.len();
            let off = app.col_offset.min(ncols - 1);
            let vis = app.vis_cols.clamp(1, ncols - off);
            let pin = match app.grid_frozen {
                0 => String::new(),
                1 => "1|".to_string(),
                f => format!("1-{f}|"),
            };
            // When every column is on screen the user needs to know there is
            // nothing left to scroll to — the whole point of compact mode. That
            // marker was already pushed at the front of the list.
            if app.grid_frozen + vis < ncols {
                parts.push(tf("列 {}{}-{}/{}", &[&(pin), &(off + 1), &(off + vis), &(ncols)]));
            }
        }
    }
    if !app.batch.is_empty() {
        parts.push(tf("批量 {} 待提交", &[&(app.batch.len())]));
    }
    if let Some(ev) = event_part {
        parts.push(ev);
    }
    parts.join(" · ")
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let right = context_info(app);
    // While events are being traced the context block carries one extra field
    // (the last mouse/resize event), so let it use more of the line — otherwise
    // the row/column readout next to it would be the first thing cut off.
    let cap = if app.trace_path.is_some() && !app.mouse_debug {
        area.width.saturating_sub(24).max(area.width / 2)
    } else {
        area.width / 2
    };
    let right_w = (disp_width(&right) as u16 + 2).min(cap);
    let chunks = Layout::horizontal([Constraint::Min(10), Constraint::Length(right_w)]).split(area);
    let style = if app.status.starts_with('✗') {
        Style::default().fg(Color::Red)
    } else if app.status.starts_with('✓') {
        Style::default().fg(Color::Green)
    } else if app.status.starts_with('⚠') || app.loading {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let msg = fit_status(&app.status, chunks[0].width as usize);
    f.render_widget(Paragraph::new(msg).style(style), chunks[0]);
    f.render_widget(
        Paragraph::new(truncate_disp(&right, chunks[1].width as usize))
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

/// One footer hint: the keycap plus what it does.
type Hint = (&'static str, &'static str);

/// Which surface currently owns the keyboard, from the footer's point of view.
/// Overlays take precedence over the page, exactly like the key router.
#[derive(Clone, Copy, PartialEq, Debug)]
enum FooterView {
    Help,
    TablePrompt,
    ResultFilter,
    Recent,
    ColPicker,
    Completion,
    SnippetName,
    Snippets,
    FilterPrompt,
    Popup,
    EditDialog,
    DbPicker,
    Confirm,
    ConnPicker,
    NewConn,
    Browse,
}

/// Everything the footer needs to know, so the group choice can be unit-tested
/// without building a whole `App`.
#[derive(Clone, Copy)]
struct FooterCtx {
    view: FooterView,
    focus: Focus,
    has_connection: bool,
}

fn footer_ctx(app: &App) -> FooterCtx {
    let view = if app.help_open {
        FooterView::Help
    } else if app.table_prompt.is_some() {
        FooterView::TablePrompt
    } else if app.result_filter.is_some() {
        FooterView::ResultFilter
    } else if app.recent_open {
        FooterView::Recent
    } else if app.col_picker_open {
        FooterView::ColPicker
    } else if app.completion.is_some() {
        FooterView::Completion
    } else if app.snippet_name.is_some() {
        FooterView::SnippetName
    } else if app.snippet_open {
        FooterView::Snippets
    } else if app.filter_prompt.is_some() {
        FooterView::FilterPrompt
    } else if app.row_popup.is_some() || app.cell_popup.is_some() {
        FooterView::Popup
    } else if app.edit_dialog.is_some() {
        FooterView::EditDialog
    } else if app.db_picker_open {
        FooterView::DbPicker
    } else if app.confirm.is_some() {
        FooterView::Confirm
    } else if app.page == Page::NewConn {
        FooterView::NewConn
    } else if app.picker_open && app.selected.is_none() {
        FooterView::ConnPicker
    } else {
        FooterView::Browse
    };
    FooterCtx {
        view,
        focus: app.focus,
        has_connection: app.selected.is_some(),
    }
}

/// Build the footer hint list for a context, most relevant first. The last entry
/// is always the pinned `?` help key, so the escape hatch can never be dropped;
/// rendering trims lower-priority hints when the line is narrow.
fn footer_hints_ctx(ctx: FooterCtx) -> Vec<Hint> {
    let mut v: Vec<Hint> = match ctx.view {
        FooterView::Help => vec![("↑↓", t("滚动")), ("Esc", t("关闭"))],
        FooterView::TablePrompt | FooterView::ResultFilter => {
            vec![("Enter", t("保留")), ("Esc", t("清除"))]
        }
        FooterView::Recent => vec![
            ("↑↓", t("选择")),
            ("Enter", t("直达")),
            ("Esc", t("关闭")),
        ],
        FooterView::ColPicker => vec![
            ("Space", t("勾选")),
            ("a", t("全选")),
            ("x", t("仅首列")),
            ("Esc", t("关闭")),
        ],
        FooterView::Completion => vec![
            ("↑↓", t("选择")),
            ("Tab/Enter", t("上屏")),
            ("Esc", t("取消")),
        ],
        FooterView::SnippetName => vec![("Enter", t("保存")), ("Esc", t("取消"))],
        FooterView::Snippets => vec![
            ("↑↓", t("选择")),
            ("Enter", t("插入编辑器")),
            ("s", t("收藏")),
            ("r", t("刷新")),
            ("Esc", t("关闭")),
        ],
        FooterView::FilterPrompt => {
            vec![("Enter", t("应用")), ("Esc", t("取消")), ("⏎", t("清除"))]
        }
        FooterView::Popup => vec![("↑↓", t("滚动")), ("Esc/Enter", t("关闭"))],
        FooterView::EditDialog => vec![
            ("Enter", t("执行")),
            ("Esc", t("取消")),
            ("Ctrl-V", t("转编辑器")),
            ("Ctrl-T", t("加入批量")),
        ],
        FooterView::DbPicker => vec![
            ("↑↓", t("选择")),
            ("Enter", t("切换")),
            ("r", t("刷新")),
            ("Esc", t("关闭")),
        ],
        FooterView::Confirm => vec![("Enter/y", t("执行")), ("Esc/n", t("取消"))],
        FooterView::ConnPicker => vec![
            ("↑↓", t("选择连接")),
            ("Enter", t("连接")),
            ("c", t("新建")),
            ("p", t("复制")),
            ("q", t("显隐")),
        ],
        FooterView::NewConn => vec![
            ("↑↓", t("字段")),
            ("Enter", t("编辑/保存")),
            ("Esc", t("返回")),
            ("ssl", t("切换")),
        ],
        FooterView::Browse => match ctx.focus {
            Focus::Sidebar if !ctx.has_connection => vec![
                ("↑↓", t("选择连接")),
                ("Enter", t("连接")),
                ("c", t("新建")),
                ("p", t("复制")),
                ("q", t("显隐")),
            ],
            Focus::Sidebar => vec![
                ("↑↓", t("表")),
                ("/", t("过滤")),
                ("Enter", t("浏览")),
                ("r", t("结构")),
                ("d", t("切库")),
                ("Tab", t("SQL")),
            ],
            Focus::Editor => vec![
                ("Ctrl-J", t("运行")),
                ("Ctrl-Space", t("补全")),
                ("Enter", t("换行")),
                ("↑↓", t("历史")),
                ("Tab", t("下一区")),
                ("Esc", t("侧栏")),
            ],
            Focus::CmdInput => vec![
                ("Enter", t("执行")),
                ("[ ]", t("切库")),
                ("Ctrl-L", t("换模式")),
                ("Esc", t("编辑器")),
            ],
            Focus::Preview => vec![
                ("↑↓", t("行")),
                ("←→", t("列")),
                ("Enter", t("详情")),
                ("e", t("编辑")),
                ("i", t("插入")),
                ("Del", t("删行")),
                ("y", t("复制INSERT")),
                ("f", t("过滤")),
                ("/", t("搜索")),
            ],
        },
    };
    // The help key is the one hint that is never dropped.
    v.push(("?", t("帮助")));
    v
}

/// Build the footer hint list for the current app state.
fn footer_hints(app: &App) -> Vec<Hint> {
    footer_hints_ctx(footer_ctx(app))
}

fn hint_width(h: &Hint) -> usize {
    disp_width(h.0) + 1 + disp_width(h.1)
}

/// Choose which leading hints fit in `width` while always keeping the pinned
/// final hint (`?` help). Returns the chosen leading hints and whether anything
/// was dropped (so the caller can draw the `…` marker).
fn footer_select<'a>(hints: &'a [Hint], width: usize) -> (Vec<&'a Hint>, bool) {
    let (help, lead) = hints.split_last().expect("footer always has a hint");
    // Reserve the help hint plus two " · " separators and the `…` marker.
    let mut budget = width.saturating_sub(hint_width(help) + 5);
    let mut chosen: Vec<&'a Hint> = Vec::new();
    let mut dropped = false;
    for h in lead {
        let w = hint_width(h);
        if w + 3 <= budget {
            budget -= w + 3;
            chosen.push(h);
        } else {
            dropped = true;
            break;
        }
    }
    (chosen, dropped)
}

#[cfg(test)]
/// Display width of the rendered footer line for a chosen set, used by tests.
/// Mirrors exactly what [`render_footer`] draws: the chosen hints, an optional
/// `…` marker, the pinned help hint, and one `" · "` separator per gap.
fn footer_line_width(chosen: &[&Hint], help: &Hint, dropped: bool) -> usize {
    let mut w: usize = chosen.iter().map(|h| hint_width(h)).sum();
    w += hint_width(help);
    let mut tokens = chosen.len() + 1; // chosen hints + help
    if dropped {
        tokens += 1;
        w += 1; // the "…" glyph
    }
    w + tokens.saturating_sub(1) * 3
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints = footer_hints(app);
    let (chosen, dropped) = footer_select(&hints, area.width as usize);
    let help = hints.last().expect("footer always has a hint");
    let mut spans: Vec<Span> = Vec::new();
    let sep = Span::styled(" · ", Style::default().fg(Color::DarkGray));
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::DarkGray);
    for h in &chosen {
        if !spans.is_empty() {
            spans.push(sep.clone());
        }
        spans.push(Span::styled(h.0, key_style));
        spans.push(Span::styled(format!(" {}", h.1), desc_style));
    }
    if dropped {
        if !spans.is_empty() {
            spans.push(sep.clone());
        }
        spans.push(Span::styled("…", desc_style));
    }
    if !spans.is_empty() {
        spans.push(sep);
    }
    spans.push(Span::styled(help.0, key_style));
    spans.push(Span::styled(format!(" {}", help.1), desc_style));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_browse(f: &mut Frame, area: Rect, app: &mut App) {
    let mode = app.layout_mode;
    let has_cmd = app.backend_kind != Backend::Sql;
    let sidebar_collapsed = pane_eff_collapsed(app, PANE_SIDEBAR);
    let editor_collapsed = pane_eff_collapsed(app, PANE_EDITOR);
    let results_collapsed = pane_eff_collapsed(app, PANE_RESULTS);

    // Stacked layout when the terminal is narrow or the sidebar is collapsed:
    // the sidebar becomes a one-line strip above the editor / results column.
    if mode == LayoutMode::Narrow || sidebar_collapsed {
        let sidebar_h = if sidebar_collapsed {
            1
        } else {
            7
        };
        let v = Layout::vertical([Constraint::Length(sidebar_h), Constraint::Min(4)]).split(area);
        app.rects.sidebar = v[0];
        if sidebar_collapsed {
            render_sidebar_strip(f, v[0], app);
        } else {
            render_sidebar(f, v[0], app);
        }
        render_main_area(f, v[1], app, editor_collapsed, results_collapsed, has_cmd);
    } else {
        let sidebar_w = if mode == LayoutMode::Wide { 28 } else { 22 };
        let hz =
            Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(20)]).split(area);
        app.rects.sidebar = hz[0];
        render_sidebar(f, hz[0], app);
        render_main_area(f, hz[1], app, editor_collapsed, results_collapsed, has_cmd);
    }
}

fn render_main_area(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    editor_collapsed: bool,
    results_collapsed: bool,
    has_cmd: bool,
) {
    let mode = app.layout_mode;
    let cmd_h = if has_cmd { 3 } else { 0 };
    let base_editor_h = if mode == LayoutMode::Narrow { 3 } else { 5 };
    // Narrow + Editor 焦点：同帧把编辑器 3→6 行（小屏 height<14 或放不下时不扩）
    let expand_editor = mode == LayoutMode::Narrow
        && app.focus == Focus::Editor
        && app.term_h >= 14
        && area.height as usize >= 6 + cmd_h as usize + 5;
    let editor_h = if editor_collapsed {
        1
    } else if expand_editor {
        6
    } else {
        base_editor_h
    };
    let results_c = if results_collapsed {
        Constraint::Length(1)
    } else {
        Constraint::Min(5)
    };
    let main_chunks = Layout::vertical([
        Constraint::Length(editor_h),
        Constraint::Length(cmd_h),
        results_c,
    ])
    .split(area);
    app.rects.editor = main_chunks[0];
    app.rects.cmd = if has_cmd {
        main_chunks[1]
    } else {
        Rect::default()
    };

    if editor_collapsed {
        render_editor_strip(f, main_chunks[0], app);
    } else {
        let focused = app.focus == Focus::Editor;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" SQL ")
            .border_set(border::ROUNDED)
            .border_style(border_style(focused));
        app.editor.set_block(block);
        f.render_widget(&app.editor, main_chunks[0]);
    }

    if has_cmd {
        let title = match app.backend_kind {
            Backend::Redis => format!(" redis[{}] ",  app.redis_db),
            Backend::Mongo => format!(" mongo({}) ",  app.current_db()),
            Backend::Sql => " cmd ".into(),
        };
        let cfocused = app.focus == Focus::CmdInput;
        let b = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Magenta)))
            .border_set(border::ROUNDED)
            .border_style(border_style(cfocused));
        app.cmd_input.set_block(b);
        f.render_widget(&app.cmd_input, main_chunks[1]);
    }

    let res_area = main_chunks[2];
    app.rects.results = res_area;
    if results_collapsed {
        render_results_strip(f, res_area, app);
    } else {
        render_results_pane(f, res_area, app);
    }
}

/// One-line summary shown in place of the sidebar when it is collapsed.
fn render_sidebar_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Sidebar;
    let mut text = String::new();
    if let Some(c) = &app.selected {
        text.push_str(&tf("▸ {} · {} 表", &[&(truncate_disp(&c.name, 16)), &(app.tables.len())]));
        let db = app.current_db();
        if !db.is_empty() {
            text.push_str(&format!(" · {}",  fix_double_encoding(&db)));
        }
        if let Some(t) = app.selected_table() {
            text.push_str(&format!(" · {}",  fix_double_encoding(&t.name)));
        }
    } else {
        text.push_str(t("▸ 未连接"));
    }
    text.push_str(t(" · 点击/Ctrl-W 展开"));
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize)).style(style),
        area,
    );
}

/// One-line summary shown in place of the SQL editor when it is collapsed.
fn render_editor_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Editor;
    let sql = app.editor_sql();
    let first = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let text = if first.is_empty() {
        t("SQL ▸ 空 · 点击/Tab 展开").to_string()
    } else {
        tf("SQL ▸ {} · 点击/Tab 展开", &[&(one_line(first))])
    };
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize)).style(style),
        area,
    );
}

/// One-line summary shown in place of the results pane when it is collapsed.
fn render_results_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let n = result_row_count(app);
    let text = tf("结果 ▸ {} 行 · 点击/Ctrl-W 展开", &[&(n)]);
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize))
            .style(Style::default().fg(Color::Gray)),
        area,
    );
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_results_pane(f: &mut Frame, area: Rect, app: &mut App) {
    if let Some(s) = app.script.clone() {
        if let Some(i) = s.drilled {
            let o = &s.outcomes[i];
            let title = tf(" {}语句 {} 结果 · {} · Esc 返回脚本 ", &[&(search_marker(app)), &(i + 1), &(if o.grid.note.is_empty() {
                    "".to_string()
                } else {
                    o.grid.note.clone()
                })]);
            if let Some(grid) = active_grid(app) {
                render_grid(f, area, app, &grid, GridKind::Query, &title);
            }
        } else {
            render_script_list(f, area, app, &s);
        }
        return;
    }
    if app.struct_view == StructView::Ddl {
        if let Some(ddl) = app.ddl.clone() {
            render_ddl(f, area, app, &ddl);
            return;
        }
    }
    if app.grid.is_some() {
        let title = grid_title(app);
        let grid = app.grid.clone().unwrap();
        let kind = app.grid_kind;
        render_grid(f, area, app, &grid, kind, &title);
        return;
    }
    if app.backend_kind != Backend::Sql && !app.cmd_output.is_empty() {
        render_console(f, area, app);
        return;
    }
    let hint = if app.selected.is_none() {
        if app.picker_open {
            ""
        } else {
            t("q 显示连接列表")
        }
    } else if app.tables.is_empty() {
        t("无表 · Tab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行")
    } else {
        t("↑↓ 选表 · Enter 浏览数据 · r 表结构\nTab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行")
    };
    f.render_widget(
        Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t(" 结果 "))
                    .border_set(border::ROUNDED)
                    .border_style(border_style(app.focus == Focus::Preview)),
            ),
        area,
    );
}

/// Leading marker for an active result search, so the indicator stays visible
/// even when a narrow pane clips the rest of the title.
fn search_marker(app: &App) -> String {
    if app.result_needle.trim().is_empty() {
        String::new()
    } else {
        tf("🔍「{}」{} 命中 · ", &[&(app.result_needle), &(result_row_count(app))])
    }
}

fn grid_title(app: &App) -> String {
    match app.grid_kind {
        GridKind::TableData => {
            let Some(ps) = &app.page_state else {
                return t(" 结果 ").into();
            };
            let rows = app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0);
            let offset = ps.page * ps.page_size;
            let total = ps
                .total
                .map(|t| tf("共 {} 行", &[&(t)]))
                .unwrap_or_else(|| t("总数未知").into());
            let more = if ps.has_next { t(" · n 下一页") } else { "" };
            tf(" {}{}.{} · 第 {} 页 · {}–{} / {} · {}{}{} ", &[&(search_marker(app)), &(fix_double_encoding(&app.current_db())), &(fix_double_encoding(&ps.table)), &(ps.page + 1), &(if rows == 0 { 0 } else { offset + 1 }), &(offset + rows), &(total), &(app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default()), &(more), &(page_state_extra(ps))])
        }
        GridKind::Columns => {
            let table = app
                .selected_table()
                .map(|t| fix_double_encoding(&t.name))
                .unwrap_or_default();
            tf(" 表结构 · {} · {} · t 查看 DDL ", &[&(table), &(app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default())])
        }
        GridKind::Query => {
            let note = app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default();
            if app.result_tabs.len() > 1 {
                let title = app
                    .result_tabs
                    .get(app.result_tab)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                tf(" {}结果 {}/{} · {} · {} · [ ] 切换 ", &[&(search_marker(app)), &(app.result_tab + 1), &(app.result_tabs.len()), &(note), &(truncate_disp(&title, 20))])
            } else {
                tf(" {}结果 · {} ", &[&(search_marker(app)), &(note)])
            }
        }
    }
}

fn render_grid(f: &mut Frame, area: Rect, app: &mut App, grid: &Grid, kind: GridKind, title: &str) {
    let focused = app.focus == Focus::Preview;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_set(border::ROUNDED)
        .border_style(border_style(focused));

    if grid.columns.is_empty() {
        let body = if grid.note.is_empty() {
            "OK".to_string()
        } else {
            grid.note.clone()
        };
        f.render_widget(
            Paragraph::new(body)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }

    let total_rows = grid.rows.len();
    let gutter = ((total_rows + 1).to_string().len()).max(2) as u16;

    // Fixed layout for the structure field list; windowed layout for data grids.
    if kind == GridKind::Columns {
        render_columns_grid(f, area, app, grid, block, gutter);
        return;
    }

    let inner_w = area.width.saturating_sub(2) as usize;
    let ncols = grid.columns.len();
    // Highlight cells that match the active result search (`/` in the results).
    let needle_lc = app.result_needle.trim().to_lowercase();
    let needle = if needle_lc.is_empty() {
        None
    } else {
        Some(needle_lc.as_str())
    };
    // Compact (mobile) mode shares the pane among all columns so a wide table can
    // fit without horizontal scrolling; otherwise each column keeps its natural
    // content width, capped per layout.
    let max_cell = grid_max_cell(app, ncols, inner_w, gutter);
    app.grid_max_cell = max_cell;
    let widths: Vec<usize> = (0..ncols)
        .map(|ci| natural_width(grid, ci, max_cell))
        .collect();
    let frozen = effective_frozen(app.freeze_first, grid, gutter as usize, inner_w, max_cell);
    let left_w: usize = gutter as usize
        + if frozen > 0 {
            frozen + widths[..frozen].iter().sum::<usize>()
        } else {
            0
        };
    const GAP: usize = 1;
    let avail = inner_w.saturating_sub(left_w + GAP).max(MIN_CELL_WIDTH);
    app.grid_avail = avail;
    let (off, visible) = window_for_cursor(
        grid,
        app.col_cursor,
        app.col_offset,
        avail,
        max_cell,
        frozen,
    );
    app.col_offset = off;
    app.vis_cols = visible;
    app.grid_gutter = gutter;
    app.grid_frozen = frozen;
    app.grid_widths = widths.clone();

    // Sort / filter marks only make sense for a browsed table.
    let (sort_keys, filter_text) = if kind == GridKind::TableData {
        match &app.page_state {
            Some(ps) => (parse_order_by(ps.order_by.as_deref()), ps.filter.clone()),
            None => (Vec::new(), String::new()),
        }
    } else {
        (Vec::new(), String::new())
    };
    let sort_of = |name: &str| {
        sort_keys
            .iter()
            .position(|(c, _)| c == name)
            .map(|i| (sort_keys[i].1, i + 1))
    };
    let filt_of = |name: &str| !filter_text.is_empty() && filter_mentions(&filter_text, name);

    let h = (area.height as usize).saturating_sub(3).max(1);
    let nrows = grid.rows.len();
    let start = app
        .sel
        .saturating_sub(h / 2)
        .min(nrows.saturating_sub(h.min(nrows)));
    let sel = app.sel;
    let cc = app.col_cursor;

    let inner = block.inner(area);
    f.render_widget(block, area);

    // ── pinned block: row-number gutter + optionally the first data column ──
    let mut left_widths: Vec<usize> = vec![gutter as usize];
    left_widths.extend(widths[..frozen].iter().copied());
    let mut lheader: Vec<Cell> = vec![gutter_header_cell()];
    for (ci, w) in widths.iter().enumerate().take(frozen) {
        let name = &grid.columns[ci];
        lheader.push(col_header_cell(
            &fix_double_encoding(name),
            *w,
            ci == cc,
            sort_of(name),
            filt_of(name),
        ));
    }
    let mut lrows: Vec<Row> = Vec::new();
    for (i, row) in grid.rows.iter().enumerate().skip(start).take(h) {
        let mut cells: Vec<Cell> = vec![gutter_cell(i, i == sel)];
        for (ci, w) in widths.iter().enumerate().take(frozen) {
            cells.push(match row.get(ci) {
                Some(v) => cell_widget_hl(v, *w, i == sel && ci == cc, needle),
                None => Cell::from(""),
            });
        }
        let mut r = Row::new(cells);
        if i == sel {
            r = r.style(highlight_style());
        }
        lrows.push(r);
    }
    let left_area = Rect {
        x: inner.x,
        y: inner.y,
        width: (left_w.min(inner.width as usize)) as u16,
        height: inner.height,
    };
    let ltable = Table::new(
        lrows,
        left_widths
            .iter()
            .map(|w| Constraint::Length(*w as u16))
            .collect::<Vec<_>>(),
    )
    .header(Row::new(lheader))
    .column_spacing(1);
    f.render_widget(ltable, left_area);

    // ── scrollable window ──
    if visible > 0 && (inner.width as usize) > left_w + GAP {
        let right_x = inner.x + (left_w + GAP) as u16;
        let right_w = (inner.x + inner.width).saturating_sub(right_x);
        if right_w > 0 {
            let right_area = Rect {
                x: right_x,
                y: inner.y,
                width: right_w,
                height: inner.height,
            };
            let mut rheader: Vec<Cell> = Vec::new();
            for (ci, w) in widths.iter().enumerate().skip(off).take(visible) {
                let name = &grid.columns[ci];
                rheader.push(col_header_cell(
                    &fix_double_encoding(name),
                    *w,
                    ci == cc,
                    sort_of(name),
                    filt_of(name),
                ));
            }
            let mut rrows: Vec<Row> = Vec::new();
            for (i, row) in grid.rows.iter().enumerate().skip(start).take(h) {
                let mut cells: Vec<Cell> = Vec::new();
                for (ci, w) in widths.iter().enumerate().skip(off).take(visible) {
                    cells.push(match row.get(ci) {
                        Some(v) => cell_widget_hl(v, *w, i == sel && ci == cc, needle),
                        None => Cell::from(""),
                    });
                }
                let mut r = Row::new(cells);
                if i == sel {
                    r = r.style(highlight_style());
                }
                rrows.push(r);
            }
            let rtable = Table::new(
                rrows,
                (off..off + visible)
                    .map(|ci| Constraint::Length(widths[ci] as u16))
                    .collect::<Vec<_>>(),
            )
            .header(Row::new(rheader))
            .column_spacing(1);
            f.render_widget(rtable, right_area);
        }
    }

    // ── horizontal scroll progress bar (drawn on the bottom border) ──
    app.rects.hbar_visible = false;
    app.rects.hbar_prev = Rect::default();
    app.rects.hbar_next = Rect::default();
    let scrollable_total = ncols.saturating_sub(frozen);
    if visible > 0 && scrollable_total > visible && inner_w >= 16 {
        let win_start = off.saturating_sub(frozen);
        let pin = match frozen {
            0 => String::new(),
            1 => "1|".to_string(),
            f => format!("1-{f}|"),
        };
        let label = tf("列 {}{}-{}/{}", &[&(pin), &(off + 1), &(off + visible), &(ncols)]);
        // One cell at each end is a tap target for panning a whole window — the
        // touch-friendly control for phones whose terminal sends no h-wheel.
        let track = inner_w - 2;
        let label_w = disp_width(&label);
        let bar_len = if track > label_w + 6 {
            track - label_w - 1
        } else {
            track
        };
        let (ts, tl) = scrollbar_geom(scrollable_total, win_start, visible, bar_len);
        let tl = tl.max(1).min(bar_len);
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            "◀",
            Style::default().fg(Color::LightGreen),
        ));
        // A half-height bar (lower block) instead of a full `█` keeps the
        // indicator visually thin on the bottom border.
        if ts > 0 {
            spans.push(Span::styled(
                "▁".repeat(ts),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            "▄".repeat(tl),
            Style::default().fg(Color::LightGreen),
        ));
        let after = bar_len.saturating_sub(ts + tl);
        if after > 0 {
            spans.push(Span::styled(
                "▁".repeat(after),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if bar_len < track {
            let pad = track - bar_len - label_w;
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            spans.push(Span::styled(label, Style::default().fg(Color::Gray)));
        }
        spans.push(Span::styled(
            "▶",
            Style::default().fg(Color::LightGreen),
        ));
        let bar_area = Rect {
            x: inner.x,
            y: area.y + area.height - 1,
            width: inner_w as u16,
            height: 1,
        };
        f.render_widget(Paragraph::new(Line::from(spans)), bar_area);
        app.rects.hbar_prev = Rect {
            x: inner.x,
            y: bar_area.y,
            width: 1,
            height: 1,
        };
        app.rects.hbar_next = Rect {
            x: inner.x + inner_w as u16 - 1,
            y: bar_area.y,
            width: 1,
            height: 1,
        };
        app.rects.hbar = Rect {
            x: inner.x + 1,
            y: bar_area.y,
            width: bar_len as u16,
            height: 1,
        };
        app.rects.hbar_visible = bar_len > 0;
    }

    // ── vertical position indicator (drawn on the right border) ──
    let track_h = inner.height as usize;
    if track_h >= 3 {
        let win = h.min(nrows.saturating_sub(start)).max(1);
        let (v_total, v_start) = match (&app.page_state, kind) {
            (Some(ps), GridKind::TableData) => match ps.total {
                Some(t) if (t as usize) > win => (t as usize, ps.page * ps.page_size + start),
                _ => (nrows, start),
            },
            _ => (nrows, start),
        };
        if v_total > win {
            let lines = vbar_lines(v_total, v_start, win, track_h);
            let v_area = Rect {
                x: area.x + area.width - 1,
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            f.render_widget(Paragraph::new(lines), v_area);
        }
    }
}

/// Thumb geometry for a scrollbar track: `(thumb_start, thumb_len)` within
/// `track` cells for a window of `win_len` at `win_start` out of `total` items.
fn scrollbar_geom(total: usize, win_start: usize, win_len: usize, track: usize) -> (usize, usize) {
    if total == 0 || track == 0 {
        return (0, 0);
    }
    let win_len = win_len.clamp(1, total);
    if win_len >= total {
        return (0, track);
    }
    let thumb_len = ((win_len * track) / total).max(1).min(track);
    let max_start = total - win_len;
    let travel = track - thumb_len;
    let start = (win_start.min(max_start) * travel)
        .checked_div(max_start)
        .unwrap_or(0);
    (start.min(travel), thumb_len)
}

fn vbar_lines(total: usize, start: usize, win: usize, height: usize) -> Vec<Line<'static>> {
    let (ts, tl) = scrollbar_geom(total, start, win, height);
    let tl = tl.max(1).min(height);
    (0..height)
        .map(|i| {
            if i >= ts && i < ts + tl {
                // Half-width block: a thin vertical thumb on the right border.
                Line::from(Span::styled(
                    "▐",
                    Style::default().fg(Color::LightGreen),
                ))
            } else {
                Line::from(Span::styled("│", Style::default().fg(Color::DarkGray)))
            }
        })
        .collect()
}

/// True when `col` appears as a whole identifier inside the filter expression
/// (quote characters are ignored).
fn filter_mentions(filter: &str, col: &str) -> bool {
    if filter.trim().is_empty() || col.is_empty() {
        return false;
    }
    let cleaned: String = filter
        .chars()
        .filter(|c| !matches!(c, '`' | '"' | '[' | ']'))
        .collect();
    let hay = cleaned.to_ascii_lowercase();
    let needle = col.to_ascii_lowercase();
    let (hb, nb) = (hay.as_bytes(), needle.as_bytes());
    let (n, m) = (hb.len(), nb.len());
    if m == 0 || m > n {
        return false;
    }
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + m <= n {
        if &hb[i..i + m] == nb {
            let before_ok = i == 0 || !ident(hb[i - 1]);
            let after_ok = i + m == n || !ident(hb[i + m]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Table-structure field list: fixed percentage widths, but still shows the cell
/// cursor and focused-cell highlight for consistency with data grids.
fn render_columns_grid(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    grid: &Grid,
    block: Block,
    gutter: u16,
) {
    let widths = [
        Constraint::Length(gutter),
        Constraint::Percentage(22),
        Constraint::Percentage(20),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Percentage(22),
        Constraint::Percentage(26),
    ];
    let cc = app.col_cursor;
    let mut header = vec![gutter_header_cell()];
    header.extend(grid.columns.iter().enumerate().map(|(ci, c)| {
        let shown = fix_double_encoding(c);
        col_header_cell(&shown, disp_width(&shown), ci == cc, None, false)
    }));
    let rows: Vec<Row> = grid
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut cells = vec![gutter_cell(i, i == app.sel)];
            cells.extend(row.iter().enumerate().map(|(ci, v)| {
                cell_widget_hl(v, 40, i == app.sel && ci == cc, None)
            }));
            let mut r = Row::new(cells);
            if i == app.sel {
                r = r.style(highlight_style());
            }
            r
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(Row::new(header))
        .column_spacing(1)
        .block(block);
    f.render_widget(table, area);
}

fn gutter_header_cell() -> Cell<'static> {
    Cell::from(Span::styled(
        "#",
        Style::default().fg(Color::DarkGray),
    ))
}

fn gutter_cell(i: usize, selected: bool) -> Cell<'static> {
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Cell::from(Span::styled(format!("{}",  i + 1), style))
}

fn col_header_cell(
    name: &str,
    w: usize,
    current: bool,
    sort: Option<(bool, usize)>,
    filtered: bool,
) -> Cell<'static> {
    let mut suffix = String::new();
    if let Some((desc, rank)) = sort {
        suffix.push(' ');
        suffix.push(if desc { '▼' } else { '▲' });
        if rank > 1 {
            suffix.push_str(&rank.to_string());
        }
    }
    if filtered {
        suffix.push_str(" ⚑");
    }
    let sw = disp_width(&suffix);
    let text = if w > sw {
        format!("{}{}",  truncate_disp(name, w - sw),  suffix)
    } else {
        truncate_disp(suffix.trim_start(), w)
    };
    let style = if current {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if sort.is_some() {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if filtered {
        Style::default()
            .fg(Color::LightMagenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    Cell::from(Span::styled(text, style))
}

fn highlight_style() -> Style {
    Style::default()
        .bg(Color::Rgb(38, 48, 38))
        .add_modifier(Modifier::BOLD)
}

fn focused_cell_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::LightCyan)
        .add_modifier(Modifier::BOLD)
}

/// Italic is a nicety, not a requirement: some terminals ignore it, some render
/// it as reverse video. The grey foreground alone still separates a real NULL
/// from ordinary text, so an unsupported italic degrades to grey-only. Set
/// `DBXT_NO_ITALIC=1` to force that fallback (or when the font's italic is hard
/// to read on a light theme).
fn italic_supported() -> bool {
    match std::env::var("DBXT_NO_ITALIC") {
        Ok(v) => {
            let v = v.trim();
            !(v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        }
        Err(_) => true,
    }
}

/// The style that marks a real SQL NULL: grey, italic when the terminal can.
fn null_style() -> Style {
    let mut style = Style::default().fg(Color::DarkGray);
    if italic_supported() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// The style for an empty string — grey, never italic, and always drawn as
/// `''` so it can never be mistaken for NULL.
fn empty_string_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Text and style a value should be drawn with, shared by the grid, the cell /
/// row modals and the edit dialog so every surface tells the same story:
/// `NULL` = grey italic, `''` = grey, anything else = plain. A literal string
/// `"NULL"` stays plain, which is exactly how it stays distinct from the real
/// thing.
fn value_display(v: &Val) -> (String, Style) {
    match v {
        Val::Null => ("NULL".to_string(), null_style()),
        Val::Text(s) if s.is_empty() => ("''".to_string(), empty_string_style()),
        Val::Text(s) => (s.clone(), Style::default()),
    }
}

/// Style for a cell whose value matches the active result search.
fn search_hit_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// Render one cell, optionally marking it as the focused cell or highlighting a
/// search hit.
fn cell_widget_hl(v: &Val, w: usize, focused: bool, needle: Option<&str>) -> Cell<'static> {
    if focused {
        let (text, _) = value_display(v);
        return Cell::from(Span::styled(truncate_disp(&text, w), focused_cell_style()));
    }
    let hit = needle.is_some_and(|n| {
        let s = match v {
            Val::Null => "null",
            Val::Text(s) => s.as_str(),
        };
        !n.is_empty() && s.to_lowercase().contains(n)
    });
    if hit {
        let (text, _) = value_display(v);
        return Cell::from(Span::styled(truncate_disp(&text, w), search_hit_style()));
    }
    match v {
        Val::Null => Cell::from(Span::styled("NULL", null_style())),
        Val::Text(s) if s.is_empty() => Cell::from(Span::styled("''", empty_string_style())),
        Val::Text(s) => Cell::from(Span::raw(truncate_disp(s, w))),
    }
}

fn render_script_list(f: &mut Frame, area: Rect, app: &mut App, script: &ScriptView) {
    let focused = app.focus == Focus::Preview;
    let errors = script.outcomes.iter().filter(|o| o.error.is_some()).count();
    let affected: u64 = script.outcomes.iter().map(|o| o.affected).sum();
    let title = tf(" 脚本 · {} 条语句 · 影响 {} 行 · {} 错误 · Enter 查看结果 ", &[&(script.outcomes.len()), &(affected), &(errors)]);
    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(18),
        Constraint::Length(9),
    ];
    let rows: Vec<Row> = script
        .outcomes
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let status = match &o.error {
                Some(e) => format!("✗ {}",  truncate_disp(&one_line(e), 16)),
                None if !o.grid.columns.is_empty() => tf("{} 行", &[&(o.grid.rows.len())]),
                None => tf("影响 {} 行", &[&(o.affected)]),
            };
            let style = if o.error.is_some() {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let mut r = Row::new(vec![
                Cell::from(Span::styled(
                    format!("{}",  i + 1),
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::raw(truncate_disp(&one_line(&o.sql), 80))),
                Cell::from(Span::styled(status, style)),
                Cell::from(Span::styled(
                    format!("{}ms",  o.ms),
                    Style::default().fg(Color::DarkGray),
                )),
            ]);
            if i == script.sel {
                r = r.style(highlight_style());
            }
            r
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["#", t("语句"), t("结果"), t("耗时")]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .column_spacing(1)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        );
    f.render_widget(table, area);
}

fn render_ddl(f: &mut Frame, area: Rect, app: &mut App, ddl: &str) {
    let focused = app.focus == Focus::Preview;
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<String> = wrap_text(ddl, inner_w);
    let total = lines.len();
    let max_scroll = total.saturating_sub(inner_h) as u16;
    if app.ddl_scroll > max_scroll {
        app.ddl_scroll = max_scroll;
    }
    let table = app
        .selected_table()
        .map(|t| t.name.clone())
        .unwrap_or_default();
    let body: Vec<Line> = lines
        .iter()
        .map(|l| {
            let trimmed = l.trim_start();
            let indent = l.len() - trimmed.len();
            let style = if trimmed.starts_with("--") {
                Style::default().fg(Color::DarkGray)
            } else if is_ddl_keyword_line(trimmed) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![Span::raw(" ".repeat(indent)), Span::styled(trimmed.to_string(), style)])
        })
        .collect();
    let title = tf(" 表结构 (DDL) · {} · {}/{} 行 · t 返回字段 ", &[&(table), &((app.ddl_scroll as usize + inner_h).min(total)), &(total)]);
    f.render_widget(
        Paragraph::new(body)
            .scroll((app.ddl_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_set(border::ROUNDED)
                    .border_style(border_style(focused)),
            ),
        area,
    );
}

fn is_ddl_keyword_line(s: &str) -> bool {
    let up = s.to_ascii_uppercase();
    up.starts_with("CREATE ")
        || up.starts_with("PRIMARY KEY")
        || up.starts_with("UNIQUE KEY")
        || up.starts_with("KEY ")
        || up.starts_with("CONSTRAINT")
        || up.starts_with("FOREIGN KEY")
        || up.starts_with(")")
        || up.starts_with("ENGINE")
        || up.starts_with("DEFAULT CHARSET")
}

fn render_console(f: &mut Frame, area: Rect, app: &App) {
    // Console log: keep the newest output visible by scrolling to the bottom.
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .cmd_output
        .iter()
        .flat_map(|entry| entry.lines().map(Line::raw))
        .collect();
    let rows: usize = lines
        .iter()
        .map(|l| {
            let w = l.width();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_w)
            }
        })
        .sum();
    let scroll_y = rows.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t(" 输出 "))
                    .border_set(border::ROUNDED)
                    .border_style(border_style(app.focus == Focus::Preview)),
            ),
        area,
    );
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Sidebar;
    let mut lines: Vec<Line> = Vec::new();

    if let Some(c) = &app.selected {
        let conn_color = connection_color(c);
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(conn_color)),
            Span::styled(
                c.name.clone(),
                Style::default()
                    .fg(conn_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        // database row: always visible when known, opens the `d` switcher on click
        if sidebar_db_row(app) {
            let label = sidebar_db_label(app);
            let w = (area.width as usize).saturating_sub(4).max(6);
            lines.push(Line::from(vec![
                Span::styled("▤ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    truncate_disp(&label, w),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        // table-name filter row: shows the active `/` filter, or the hint.
        let filter_rows = if app.tables_all.is_empty() {
            0
        } else {
            1
        };
        if filter_rows > 0 {
            let w = (area.width as usize).saturating_sub(4).max(6);
            let (mark, text, style) = if app.table_filter.is_empty() {
                (
                    "/ ",
                    t("/ 过滤表名").to_string(),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                (
                    "▸ ",
                    format!("/{} · {}/{}",  app.table_filter,  app.tables.len(),  app.tables_all.len()),
                    Style::default().fg(Color::Yellow),
                )
            };
            lines.push(Line::from(vec![
                Span::styled(mark, Style::default().fg(Color::Yellow)),
                Span::styled(truncate_disp(&text, w), style),
            ]));
        }

        let cap = (area.height as usize)
            .saturating_sub(2 + if sidebar_db_row(app) { 1 } else { 0 } + filter_rows)
            .max(1);
        let sel = app.table_list.selected();
        let start = sel
            .unwrap_or(0)
            .saturating_sub(cap / 2)
            .min(app.tables.len().saturating_sub(cap.min(app.tables.len())));
        for (i, t) in app.tables.iter().enumerate().skip(start).take(cap) {
            let marker = if sel == Some(i) { "▸ " } else { "  " };
            let view = if t.table_type.eq_ignore_ascii_case("VIEW") {
                "~"
            } else {
                ""
            };
            let style = if sel == Some(i) {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if view == "~" {
                Style::default().fg(Color::Blue)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{marker}{}{view}",  fix_double_encoding(&t.name)),
                style,
            )));
        }

        let title = if app.table_filter.is_empty() {
            format!(" {} ({}) ",  c.name,  app.tables_all.len())
        } else {
            tf(" {} 表 {}/{} ", &[&(c.name), &(app.tables.len()), &(app.tables_all.len())])
        };
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_set(border::ROUNDED)
                    .border_style(border_style(focused)),
            ),
            area,
        );
    } else {
        // no connection selected: sidebar is just a placeholder (picker overlay does the job)
        let hint = if app.picker_open {
            ""
        } else {
            t("q 显示\n连接列表")
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(t(" 连接 "))
                        .border_set(border::ROUNDED)
                        .border_style(border_style(focused)),
                ),
            area,
        );
    }
}

fn render_form(f: &mut Frame, area: Rect, app: &mut App) {
    let form = app.form.clone();
    let box_w = if app.layout_mode == LayoutMode::Narrow {
        area.width.saturating_sub(2)
    } else {
        52.min(area.width.saturating_sub(4))
    };
    let box_h = (form_fields().len() as u16 + 6).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(box_w)) / 2;
    let y = area.y + (area.height.saturating_sub(box_h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: box_w,
        height: box_h,
    };

    let mut lines: Vec<Line> = Vec::new();
    let vals = [
        form.name.clone(),
        form.db_type.clone(),
        form.host.clone(),
        form.port.clone(),
        form.username.clone(),
        "*".repeat(form.password.chars().count()),
        form.database.clone(),
        if form.ssl { "y".into() } else { "n".into() },
        String::new(),
    ];
    for (i, label) in form_fields().iter().enumerate() {
        let active = i == form.field;
        let value = if i == 8 {
            t("↵ 保存连接").to_string()
        } else if form.editing && active {
            format!("{}▏",  vals[i])
        } else {
            vals[i].clone()
        };
        let marker = if active { "▸ " } else { "  " };
        let style = if active {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{label:10} {value}"),
            style,
        )));
    }
    if !form.err.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("✗ {}",  form.err),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(Span::styled(
        "types: mysql postgres sqlite redis mongodb clickhouse sqlserver …",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" 新建连接 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Clear, box_area);
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}

fn render_conn_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        56
    });
    let h = (app.connections.len() as u16 + 2).clamp(3, area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + 1;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    app.rects.picker = box_area;
    app.rects.picker_visible = true;

    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = app
        .connections
        .iter()
        .map(|c| {
            let w = (box_area.width as usize).saturating_sub(16);
            let color = connection_color(c);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:11}",  truncate_disp(c.db_type.as_str(), 11)),
                    Style::default().fg(color),
                ),
                Span::raw(" "),
                Span::styled(
                    truncate_disp(&c.name, w),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t(" 连接 · ↑↓ Enter · c 新建 · p 复制 · q 隐藏 "))
                .border_set(border::ROUNDED),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.conn_list);
}

fn render_db_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let entries = db_entries(app);
    let cur = db_current_index(app);
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        46
    });
    let h = (entries.len() as u16 + 2).clamp(3, area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    app.rects.db_picker = box_area;
    app.rects.db_picker_visible = true;
    f.render_widget(Clear, box_area);

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == cur {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mark = if i == cur { "● " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(
                    truncate_disp(
                        &fix_double_encoding(name),
                        (box_area.width as usize).saturating_sub(6),
                    ),
                    style,
                ),
            ]))
        })
        .collect();
    let title = match app.backend_kind {
        Backend::Redis => t(" Redis db · ↑↓ Enter · Esc 关 "),
        _ => t(" 数据库 · ↑↓ Enter · Esc 关 "),
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.db_list);
}

/// Saved-SQL snippet overlay (DBX's `saved_sql_files`). Enter inserts into the
/// editor; `r` reloads from the shared DBX store.
fn render_snippets(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        60
    });
    let h = (app.snippets.len() as u16 + 2).clamp(3, area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = app
        .snippets
        .iter()
        .map(|(name, sql)| {
            let head = sql
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:20}",  truncate_disp(name, 20)),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    truncate_disp(head, (box_area.width as usize).saturating_sub(24)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(tf(" SQL 片段 · {} 个 · Enter 插入 · r 刷新 · Esc 关 ", &[&(app.snippets.len())]))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.snippet_list);
}

/// Ctrl-Shift-H column-visibility overlay: space toggles the highlighted column,
/// `a` shows all, `x` keeps only the first. Changes apply live behind the popup.
fn render_col_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        return;
    };
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        52
    });
    let h = (grid.columns.len() as u16 + 2).clamp(3, area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = grid
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let shown = !app.col_hidden.contains(name.as_str());
            let mark = if shown { "[x] " } else { "[ ] " };
            let style = if shown {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(
                    truncate_disp(
                        &fix_double_encoding(name),
                        (box_area.width as usize).saturating_sub(6),
                    ),
                    style,
                ),
                Span::styled(
                    format!("  {}",  i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let visible = grid
        .columns
        .iter()
        .filter(|c| !app.col_hidden.contains(c.as_str()))
        .count();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(tf(" 列显示 {}/{} · 空格勾选 · a 全选 · x 仅首列 · Esc 关 ", &[&(visible), &(grid.columns.len())]))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.col_picker_list);
}

/// Ctrl-Shift-R recent-table overlay: Enter jumps straight to the table.
fn render_recent_tables(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        54
    });
    let h = (app.recent_tables.len() as u16 + 2).clamp(3, area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let cur_db = app.current_db();
    let items: Vec<ListItem> = app
        .recent_tables
        .iter()
        .map(|(db, table)| {
            let here = *db == cur_db;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if here { "● " } else { "○ " },
                    Style::default().fg(if here { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(
                    fix_double_encoding(table),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}",  fix_double_encoding(db)),
                    Style::default().fg(Color::Cyan),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t(" 最近表 · ↑↓ Enter 直达 · Esc 关 "))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.recent_list);
}

/// The `/` table-name filter prompt, drawn as a one-line box at the bottom.
fn render_table_filter(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.saturating_sub(4).max(20).min(area.width);
    let h = 3.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    // Sit just above the footer line so the input is not clipped by it.
    let y = area.y + area.height.saturating_sub(h + 1);
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(tf(" 过滤表名 {}/{} · Enter 保留 · Esc 清除 ", &[&(app.tables.len()), &(app.tables_all.len())]))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.table_prompt.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// Result-row search prompt (`/` in the results pane), styled like the table
/// filter so both filter-as-you-type flows feel identical.
fn render_result_filter(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.saturating_sub(4).max(20).min(area.width);
    let h = 3.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 1);
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let hits = result_row_count(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(tf(" 搜索结果 {} 行命中 · Enter 保留 · Esc 清除 ", &[&(hits)]))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.result_filter.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// SQL prefix-completion popup, anchored just under the editor.
fn render_completion(f: &mut Frame, app: &App) {
    let Some(c) = app.completion.clone() else {
        return;
    };
    let screen = f.area();
    let ed = app.rects.editor;
    let w = 40.min(screen.width.saturating_sub(2)).max(12);
    let h = (c.items.len() as u16 + 2).min(screen.height.saturating_sub(1));
    let x = (ed.x + 2).min(screen.x + screen.width.saturating_sub(w));
    let mut y = ed.y + ed.height;
    if y + h > screen.y + screen.height {
        y = screen.y + screen.height - h;
    }
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let lines: Vec<Line> = c
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == c.sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let tag = item.kind.to_string();
            let room = (w as usize).saturating_sub(6);
            Line::from(vec![
                Span::styled(format!("{:<room$}",  truncate_disp(&item.text, room)), style),
                Span::styled(format!("[{tag}]"), style.fg(Color::DarkGray)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(t(" 补全 · T表 C列 K关键字 · Tab 上屏 · ↑↓ · Esc "))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

/// Name prompt for saving the editor's SQL into DBX's favourites.
fn render_snippet_name(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.saturating_sub(4).max(24).min(area.width);
    let h = 3.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 1);
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" 收藏为 SQL 片段（DBX saved_sql_files）· Enter 保存 · Esc 取消 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.snippet_name.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// Shared scrollable text popup used for both cell values and row details.
fn render_text_popup(f: &mut Frame, area: Rect, title: &str, lines: &[PopupLine], scroll: u16) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(88)
        }
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;
    // Wrap each logical line on its own so the style that marks NULL / ''
    // survives across physical rows.
    let body: Vec<Line> = lines
        .iter()
        .flat_map(|pl| {
            let style = pl.style;
            wrap_text(&pl.text, inner_w)
                .into_iter()
                .map(move |t| Line::from(Span::styled(t, style)))
        })
        .collect();
    let total = body.len();
    let max_h = area.height.saturating_sub(4).max(3);
    let h = ((total as u16) + 2).min(max_h);
    let inner_h = h.saturating_sub(2) as usize;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let max_scroll = total.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    let title = tf(" {} · {}/{} · Esc 关闭 ", &[&(title), &((scroll as usize + inner_h).min(total)), &(total)]);
    f.render_widget(
        Paragraph::new(body).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

fn render_edit_dialog(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(d) = app.edit_dialog.clone() else {
        return;
    };
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 34 {
            area.width
        } else {
            avail.min(78)
        }
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;

    match d.kind {
        EditKind::Update => {
            let (old, old_style) = match &d.old {
                Val::Null => ("NULL".to_string(), null_style()),
                Val::Text(s) if s.is_empty() => ("''".to_string(), empty_string_style()),
                Val::Text(s) => (s.clone(), Style::default().fg(Color::Red)),
            };
            let mut header_lines: Vec<Line> = Vec::new();
            header_lines.push(Line::from(vec![
                Span::styled(t("列   "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    d.column.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {}", 
                        d.data_type.clone().unwrap_or_else(|| "?".into())
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            header_lines.push(Line::from(vec![
                Span::styled(t("旧值 "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_disp(&one_line(&old), inner_w.saturating_sub(6)),
                    old_style,
                ),
            ]));
            header_lines.push(Line::from(vec![
                Span::styled("WHERE ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_disp(&one_line(&d.where_clause), inner_w.saturating_sub(6)),
                    Style::default().fg(Color::Gray),
                ),
            ]));
            if d.no_pk {
                header_lines.push(Line::from(Span::styled(
                    t("⚠ 未检测到主键：WHERE 用全部列匹配，请确认条件唯一"),
                    Style::default().fg(Color::Yellow),
                )));
            } else {
                header_lines.push(Line::from(vec![
                    Span::styled(t("主键 "), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        truncate_disp(&d.keys.join(", "), inner_w.saturating_sub(6)),
                        Style::default().fg(Color::Cyan),
                    ),
                ]));
            }
            // The full statement is always shown: nothing is executed from a
            // summary alone.
            let sql_lines = wrap_sql_lines(&d.sql(), inner_w.saturating_sub(2));
            header_lines.push(Line::from(""));
            header_lines.push(Line::from(Span::styled(
                t("生成的 SQL（Enter 执行）"),
                Style::default().fg(Color::DarkGray),
            )));
            for l in &sql_lines {
                header_lines.push(Line::from(Span::styled(
                    l.clone(),
                    Style::default().fg(Color::White),
                )));
            }
            let header_h = header_lines.len() as u16;
            let h = (header_h + 3 + 1 + 2).min(area.height);
            let x = area.x + (area.width.saturating_sub(w)) / 2;
            let y = area.y + (area.height.saturating_sub(h)) / 2;
            let box_area = Rect {
                x,
                y,
                width: w,
                height: h,
            };
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(" ✎ 编辑 {}.{} ", &[&(d.db), &(d.table)]),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Yellow));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            // Keep room for the value input + hint even on a short terminal.
            let max_header = inner.height.saturating_sub(3 + 1) as usize;
            let shown: Vec<Line> = header_lines
                .iter()
                .take(max_header)
                .cloned()
                .collect();
            let shown_h = shown.len() as u16;
            let hdr_area = Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: shown_h.min(inner.height),
            };
            f.render_widget(Paragraph::new(shown), hdr_area);
            let ta_y = inner.y + shown_h;
            let ta_h = 3.min((inner.y + inner.height).saturating_sub(ta_y));
            if ta_h > 0 {
                let ta_area = Rect {
                    x: inner.x,
                    y: ta_y,
                    width: inner.width,
                    height: ta_h,
                };
                if let Some(dd) = app.edit_dialog.as_mut() {
                    let b = Block::default()
                        .borders(Borders::ALL)
                        .title(t(" 新值 · Enter 执行 "))
                        .border_set(border::ROUNDED)
                        .border_style(Style::default().fg(Color::Green));
                    dd.new_input.set_block(b);
                    f.render_widget(&dd.new_input, ta_area);
                }
            }
            let hint_y = ta_y + ta_h;
            if hint_y < inner.y + inner.height {
                let hint_area = Rect {
                    x: inner.x,
                    y: hint_y,
                    width: inner.width,
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new(truncate_disp(
                        t("Enter 执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量"),
                        inner_w,
                    ))
                    .style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }
        EditKind::Insert => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(Span::styled(
                tf("新增一行到 {}.{}", &[&(d.db), &(d.table)]),
                Style::default().fg(Color::Cyan),
            )));
            for (col, val) in d.insert_preview.iter().take(10) {
                lines.push(Line::from(vec![
                    Span::styled(
                        truncate_disp(col, inner_w.saturating_sub(14)),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::raw(" = "),
                    Span::styled(
                        truncate_disp(val, 12),
                        if val == "NULL" {
                            null_style()
                        } else if val == "''" {
                            empty_string_style()
                        } else {
                            Style::default().fg(Color::Green)
                        },
                    ),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t("生成的 SQL（Enter 执行）"),
                Style::default().fg(Color::DarkGray),
            )));
            for l in wrap_sql_lines(&d.sql(), inner_w.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(l, Style::default().fg(Color::White))));
            }
            lines.push(Line::from(Span::styled(
                t("Enter 执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量"),
                Style::default().fg(Color::DarkGray),
            )));
            let h = (lines.len() as u16 + 2).min(area.height);
            let x = area.x + (area.width.saturating_sub(w)) / 2;
            let y = area.y + (area.height.saturating_sub(h)) / 2;
            let box_area = Rect {
                x,
                y,
                width: w,
                height: h,
            };
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(" ➕ 插入 {}.{} ", &[&(d.db), &(d.table)]),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Green));
            f.render_widget(Paragraph::new(lines).block(block), box_area);
        }
    }
}

fn render_filter_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(74)
        }
    };
    let h = 7.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" WHERE 过滤 · Enter 应用 · Esc 取消 · 留空清除 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    // Leave two lines at the bottom for the syntax quick-reference.
    let hint_h = 2u16.min(inner.height.saturating_sub(1));
    let ta_h = inner.height.saturating_sub(hint_h).max(1);
    let ta_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: ta_h,
    };
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + ta_h,
        width: inner.width,
        height: hint_h,
    };
    if let Some(ta) = app.filter_prompt.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, ta_area);
    }
    if hint_h > 0 {
        let hints = vec![
            Line::from(Span::styled(
                t("语法: = != <> > < >= <= LIKE IN BETWEEN IS NULL · AND/OR · 字符串单引号"),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                t("MySQL 反引号 `col` · PG 双引号 \"col\"（区分大小写）"),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hints), hint_area);
    }
}

/// The `?` shortcut cheat-sheet, generated from the same list the README table
/// mirrors.
const HELP_ROWS: &[(&str, &str)] = &[
    ("— 全局 —", ""),
    ("Ctrl-C", "退出"),
    ("Ctrl-L", "切换命令模式 SQL → Redis → MongoDB"),
    ("F5 / Ctrl-J", "执行当前 SQL"),
    ("Tab / Shift-Tab", "循环切换区域（侧栏 → 编辑器 → 结果）"),
    ("Alt-1 / 2 / 3", "直接聚焦 侧栏 / 编辑器 / 结果"),
    ("Ctrl-A", "自动折叠 开 / 关（开=非焦点栏收起）"),
    ("Ctrl-W", "收起 / 展开当前焦点区域"),
    ("Ctrl-G", "横滚模式（触屏兜底：滚轮/上下滑 = 横滚列）"),
    ("Alt-C / w", "紧凑列宽：窄屏自动共享列宽，宽表尽量一屏放下"),
    ("Alt-H / c", "列显隐：空格勾选显示的列（按 库.表 记住，跨会话）"),
    ("Alt-R / t", "最近浏览的 5 张表，Enter 直达（侧栏 t）"),
    ("Shift+← →", "列窗口横滚一列（任意区域，按住连滚）"),
    ("Ctrl-O", "SQL 片段收藏（DBX saved_sql_files）"),
    ("Ctrl-P", "EXPLAIN 当前 SQL（SQL 后端）"),
    ("?", "本帮助"),
    ("DBXT_MOUSE_DEBUG=1", "启动时显示鼠标事件浮层（滑动无效时排查终端编码）"),
    ("— 显示约定 —", ""),
    ("NULL", "真正的 SQL NULL：灰色斜体（终端不支持斜体时仅灰色）"),
    ("''", "空字符串：灰色，带引号的空串，不会与 NULL 混淆"),
    ("DBXT_NO_ITALIC=1", "强制 NULL 仅用灰色，不依赖终端斜体"),
    ("— 连接选择 —", ""),
    ("↑ ↓ / Enter", "选择 / 连接"),
    ("c", "新建连接"),
    ("p", "复制连接（预填表单）"),
    ("q", "折叠 / 展开连接列表"),
    ("— 侧栏 —", ""),
    ("↑ ↓", "移动表列表"),
    ("/", "过滤表名（输入即筛选，Enter 保留，Esc 清除）"),
    ("t", "最近表浮层（Enter 直达）"),
    ("Enter", "浏览表数据"),
    ("r", "表结构（字段 + DDL）"),
    ("d", "数据库列表（浮层内 r 刷新）"),
    ("← →", "切换数据库（快捷）"),
    ("o", "返回连接选择"),
    ("c", "新建连接"),
    ("p", "复制连接（预填表单）"),
    ("— 结果（表格浏览）—", ""),
    ("↑ ↓ / j k", "行光标（到边自动翻页）"),
    ("PgUp / PgDn", "整屏滚动，跨页衔接"),
    ("n / p", "下一页 / 上一页"),
    ("Ctrl-F / Ctrl-B", "下一页 / 上一页"),
    ("← → / h l", "单元格光标（列窗口跟随）"),
    ("Home / End", "首行 / 末行"),
    ("Ctrl-E", "聚焦 SQL 编辑器"),
    (
        "Shift/Alt/Ctrl+滚轮 · 横滑",
        "横向滚动列（触屏左右滑动 / 拖动）；PC 终端若 Shift+滚轮 无效，用 Ctrl+滚轮 或 Ctrl-G",
    ),
    ("Shift+← →", "横滚列一列（任意区域，按住连滚）"),
    ("Ctrl-G", "横滚模式：纵向滚轮/上下滑改为横滚列"),
    ("◀ ▶（底部）", "点击向左/右翻一屏列（触屏可用）"),
    ("底部进度条", "当前列窗口位置 · 点击可跳转"),
    ("[ ]", "切换本次会话的结果标签"),
    ("Ctrl-Y", "导出当前结果为 CSV（$HOME）"),
    ("y", "复制当前行为 INSERT 语句（OSC52 + 文件兜底）"),
    ("/", "搜索结果行（输入即筛选，Enter 保留，Esc 清除）"),
    ("n / Shift-N", "搜索命中时：下 / 上一个命中（否则 n 翻页）"),
    ("Ctrl-N", "结果被截断时加载更多行"),
    ("Enter", "整行详情（紧凑列模式）/ 完整单元格"),
    ("v", "完整单元格（任意模式）"),
    ("o", "整行详情（纵向，含隐藏列）"),
    ("e", "编辑单元格 → diff 确认后执行"),
    ("i", "快速插入 → diff 确认后执行"),
    ("Delete / Ctrl-D", "删除当前行 → 确认后执行"),
    ("f", "WHERE 过滤（预填当前列）"),
    ("Ctrl-R", "清除过滤"),
    ("s", "按当前列升 / 降序"),
    ("Ctrl-K", "附加排序键（多列排序）"),
    ("z", "钉住 / 取消首列"),
    ("w / Alt-C", "紧凑列宽 开 / 关（窄屏默认自动开，按 库.表 记住）"),
    ("c / Alt-H", "列显隐浮层（空格勾选 / a 全选 / x 仅首列，按 库.表 记住）"),
    ("Alt-R", "最近表直达浮层"),
    ("t", "字段 ↔ DDL（表结构）"),
    ("Esc", "收起结果 / 关闭浮层"),
    ("— 编辑确认层 —", ""),
    ("Enter", "执行（UPDATE / INSERT，SQL 全文可见）"),
    ("Esc", "取消编辑"),
    ("Ctrl-V", "将生成的 SQL 转入编辑器微调"),
    ("Ctrl-T", "加入批量队列（Ctrl-S 打包事务提交）"),
    ("插入层 v / b", "转编辑器 / 加入批量（等价 Ctrl-V / Ctrl-T）"),
    ("Ctrl-S / Ctrl-X", "提交 / 清空批量队列"),
    ("— 编辑器 / 命令 —", ""),
    ("Ctrl-Space", "SQL 前缀补全（表名 T / 列名 C / 关键字 K，Tab 上屏）"),
    ("补全上下文", "表名. 后只补该表列名；FROM/JOIN 后优先表名；WHERE/ON 后优先列名"),
    ("↑ ↓", "历史（首行 / 末行）"),
    ("Esc", "回到侧栏"),
    ("[ ]", "Redis 逻辑库"),
    ("use <db>", "MongoDB 切库"),
    ("— 危险操作 / 删除确认 —", ""),
    ("Enter / y", "执行（SQL 全文可见）"),
    ("Esc / n", "取消"),
    ("— SQL 片段（Ctrl-O）—", ""),
    ("↑ ↓ / Enter", "选择 / 插入到编辑器"),
    ("s", "把编辑器里的 SQL 收藏为片段（写入 DBX saved_sql_files）"),
    ("r / Esc", "刷新 / 关闭"),
];

fn render_help(f: &mut Frame, area: Rect, app: &mut App) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 30 {
            area.width
        } else {
            avail.min(64)
        }
    };
    let max_h = area.height.saturating_sub(2).max(3);
    let h = (HELP_ROWS.len() as u16 + 2).min(max_h);
    let inner_h = h.saturating_sub(2) as usize;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let total = HELP_ROWS.len();
    let max_scroll = total.saturating_sub(inner_h) as u16;
    let scroll = app.help_scroll.min(max_scroll);
    let key_w = 16usize.min(w.saturating_sub(6) as usize);
    let lines: Vec<Line> = HELP_ROWS
        .iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::from(Span::styled(
                    t(k),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{:<key_w$}", k),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(t(d)),
                ])
            }
        })
        .collect();
    let title = tf(" 快捷键 · {}/{} · ↑↓ 滚动 · Esc 关闭 ", &[&((scroll as usize + inner_h).min(total)), &(total)]);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

fn render_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 30 {
            area.width
        } else {
            avail.min(72)
        }
    };
    let inner_w = w.saturating_sub(4) as usize;
    // Keep the statement's own line structure so a multi-line UPDATE / DELETE /
    // BEGIN … COMMIT stays readable; nothing is run from a summary alone.
    let sql_lines = wrap_sql_lines(&confirm.sql, inner_w);
    let max_h = area.height.saturating_sub(2) as usize;
    // reasons + blank + SQL + blank + hint, plus the two border rows
    let needed = confirm.reasons.len() + sql_lines.len() + 5;
    let h = needed.min(max_h).max(3) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);

    let mut lines: Vec<Line> = Vec::new();
    for r in &confirm.reasons {
        lines.push(Line::from(Span::styled(
            format!("⚠ {r}"),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    let sql_room = (h as usize).saturating_sub(confirm.reasons.len() + 5);
    let truncated = sql_lines.len() > sql_room;
    let shown_sql = if truncated {
        sql_room.saturating_sub(1)
    } else {
        sql_room
    };
    for l in sql_lines.iter().take(shown_sql) {
        lines.push(Line::from(Span::styled(
            l.clone(),
            Style::default().fg(Color::White),
        )));
    }
    if truncated {
        lines.push(Line::from(Span::styled(
            t("…（语句过长，已截断显示）"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t("Enter/y 执行   Esc/n 取消"),
        Style::default().fg(Color::Yellow),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            t(" ⚠ 危险操作确认 "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_is_untouched() {
        assert_eq!(fit_status("ok", 10), "ok");
    }

    #[test]
    fn error_keeps_head() {
        let msg = "✗ query: Server error: `ERROR 1146 (42S02): Table 'mysql.users' doesn't exist` SQL text omitted from user-facing error; enable debug SQL diagnostics to inspect the original statement.";
        let out = fit_status(msg, 30);
        assert!(out.starts_with("✗ query: Server error:"));
        assert!(out.ends_with('…'));
        assert_eq!(disp_width(&out), 30);
    }

    #[test]
    fn normal_message_keeps_tail() {
        let msg = "loading tables for database mydb ... done";
        let out = fit_status(msg, 12);
        assert!(out.starts_with('…'));
        assert!(out.ends_with("done"));
        assert_eq!(disp_width(&out), 12);
    }

    #[test]
    fn normal_cjk_message_keeps_a_visible_tail() {
        // Display width != char count here: the old skip-by-chars logic dropped
        // the whole message (just `…`) on a narrow screen.
        let msg = "脚本列表不支持搜索（先 Enter 进入某条语句的结果）";
        let out = fit_status(msg, 21);
        assert!(out.starts_with('…'));
        assert!(disp_width(&out) <= 21);
        assert!(out.ends_with('）'));
        // The tail must actually carry content, not be an empty `…`.
        assert!(disp_width(&out) > 1);
    }

    #[test]
    fn multibyte_is_char_safe() {
        let msg = "✗ 错误：表不存在，这是一段很长的中文诊断信息";
        let out = fit_status(msg, 8);
        assert!(out.starts_with('✗'));
        assert!(disp_width(&out) <= 8);
    }

    #[test]
    fn truncate_is_display_width_aware() {
        assert_eq!(truncate_disp("abcdef", 4), "abc…");
        assert_eq!(truncate_disp("abc", 4), "abc");
        // CJK characters are two columns wide
        assert_eq!(truncate_disp("中文字符", 5), "中文…");
    }

    #[test]
    fn danger_detection_flags_unbounded_dml() {
        assert!(detect_danger("UPDATE users SET a = 1").is_some());
        assert!(detect_danger("DELETE FROM users").is_some());
        assert!(detect_danger("DROP TABLE users").is_some());
        assert!(detect_danger("TRUNCATE TABLE users").is_some());
    }

    #[test]
    fn danger_detection_allows_bounded_and_reads() {
        assert!(detect_danger("UPDATE users SET a = 1 WHERE id = 2").is_none());
        assert!(detect_danger("DELETE FROM users WHERE id = 2").is_none());
        assert!(detect_danger("SELECT * FROM users").is_none());
        assert!(detect_danger("INSERT INTO users VALUES (1)").is_none());
    }

    #[test]
    fn danger_detection_ignores_literals_and_comments() {
        assert!(detect_danger("DELETE FROM t WHERE name = 'where'").is_none());
        // A commented-out WHERE must not count as a real clause.
        assert!(detect_danger("DELETE FROM t -- WHERE id = 1\n").is_some());
        assert!(detect_danger("UPDATE t SET a = 'DROP TABLE x'").is_some());
    }

    #[test]
    fn danger_detection_sees_cte_delete() {
        assert!(detect_danger("WITH x AS (SELECT id FROM t) DELETE FROM t").is_some());
        assert!(detect_danger("WITH x AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM x)").is_none());
    }

    #[test]
    fn natural_width_is_content_sized() {
        let grid = Grid {
            columns: vec!["id".into(), "description".into()],
            rows: vec![
                vec![Val::Text("1".into()), Val::Text("a longer value".into())],
                vec![Val::Text("22".into()), Val::Null],
            ],
            note: String::new(),
        };
        assert_eq!(natural_width(&grid, 0, 44), MIN_CELL_WIDTH);
        assert_eq!(natural_width(&grid, 1, 44), 14);
        assert_eq!(natural_width(&grid, 1, 10), 10);
    }

    #[test]
    fn visible_cols_fits_content_widths() {
        let grid = Grid {
            columns: vec!["a".into(), "b".into(), "c".into()],
            rows: vec![vec![
                Val::Text("1234567890".into()),
                Val::Text("1234567890".into()),
                Val::Text("1234567890".into()),
            ]],
            note: String::new(),
        };
        // 10-wide columns + 1 space each: two fit in 21, three need 32
        assert_eq!(visible_cols(&grid, 0, 21, 44), 2);
        assert_eq!(visible_cols(&grid, 0, 32, 44), 3);
        assert_eq!(visible_cols(&grid, 2, 32, 44), 1);
    }

    #[test]
    fn wrap_text_splits_on_width() {
        let lines = wrap_text("abcdefghij", 4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn strip_sql_noise_removes_literals_and_comments() {
        let cleaned = strip_sql_noise("SELECT 'a''b', \"c\", `d` -- trailing\n/* block */ FROM t");
        assert!(!cleaned.contains('a'));
        assert!(cleaned.contains("FROM t"));
        assert!(!cleaned.contains("trailing"));
        assert!(!cleaned.contains("block"));
    }

    #[test]
    fn keyword_matching_is_word_based() {
        assert!(has_keyword("delete from t where x=1", "where"));
        assert!(!has_keyword("select * from somewhere", "where"));
        assert!(!has_keyword("update t set a='nowhere'", "where"));
    }

    // ── swipe / drag → column pan ──

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn pan_steps_carries_the_remainder_and_caps_a_jump() {
        let mut a = 0;
        // half a step is kept, not rounded away
        assert_eq!(pan_steps(&mut a, 1), 0);
        assert_eq!(pan_steps(&mut a, 1), 1);
        assert_eq!(a, 0);
        assert_eq!(pan_steps(&mut a, -1), 0);
        assert_eq!(pan_steps(&mut a, -1), -1);
        // a coalesced jump (one event carrying the whole swipe) is capped
        let mut b = 0;
        assert_eq!(pan_steps(&mut b, 100), DRAG_MAX_STEPS);
        assert_eq!(b, 0);
        assert_eq!(pan_steps(&mut b, -100), -DRAG_MAX_STEPS);
    }

    #[test]
    fn drag_left_pans_and_becomes_a_swipe() {
        let mut g = PanGesture::default();
        // the press itself is still handled as a potential tap
        assert_eq!(
            g.feed(MouseEventKind::Down(MouseButton::Left), 10, 5, DragPan::Button),
            None
        );
        assert!(!g.is_swipe());
        // one column of travel: not enough for a step, and not yet a swipe
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 11, 5, DragPan::Button),
            Some(0)
        );
        assert!(!g.is_swipe());
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 13, 5, DragPan::Button),
            Some(1)
        );
        assert!(g.is_swipe(), "2 columns of travel is a swipe, not a tap");
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 15, 5, DragPan::Button),
            Some(1)
        );
        // releasing swallows nothing else and re-arms the tap logic
        assert_eq!(
            g.feed(MouseEventKind::Up(MouseButton::Left), 15, 5, DragPan::Button),
            None
        );
        assert!(!g.is_swipe());
    }

    #[test]
    fn a_drag_leftward_pans_the_other_way() {
        let mut g = PanGesture::default();
        g.feed(MouseEventKind::Down(MouseButton::Left), 20, 5, DragPan::Button);
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 16, 5, DragPan::Button),
            Some(-2)
        );
    }

    #[test]
    fn vertical_travel_is_neither_a_pan_nor_a_tap() {
        let mut g = PanGesture::default();
        g.feed(MouseEventKind::Down(MouseButton::Left), 10, 5, DragPan::Button);
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 10, 9, DragPan::Button),
            Some(0)
        );
        assert!(g.is_swipe(), "a vertical drag must not fire the deferred tap");
    }

    #[test]
    fn bare_motion_is_a_swipe_only_when_opted_in() {
        // A desktop mouse sends `Moved` all the time, so it must be inert by
        // default (otherwise moving the mouse would scroll the table).
        let mut g = PanGesture::default();
        assert_eq!(g.feed(MouseEventKind::Moved, 10, 5, DragPan::Button), None);
        assert_eq!(g.feed(MouseEventKind::Moved, 14, 5, DragPan::Button), None);
        assert!(!g.is_swipe());
        // ... unless the user asked for it (touch terminals that never send a press)
        let mut h = PanGesture::default();
        assert_eq!(h.feed(MouseEventKind::Moved, 10, 5, DragPan::Any), None);
        assert_eq!(h.feed(MouseEventKind::Moved, 12, 5, DragPan::Any), Some(1));
        // but a held left button also qualifies in the default mode
        let mut i = PanGesture::default();
        i.feed(MouseEventKind::Down(MouseButton::Left), 10, 5, DragPan::Button);
        assert_eq!(i.feed(MouseEventKind::Moved, 12, 5, DragPan::Button), Some(1));
    }

    #[test]
    fn other_buttons_and_off_mode_pass_through() {
        let mut g = PanGesture::default();
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 10, 5, DragPan::Off),
            None
        );
        assert_eq!(
            g.feed(MouseEventKind::Moved, 14, 5, DragPan::Off),
            None
        );
        // a right-button drag (text selection on a desktop) is not a swipe
        let mut h = PanGesture::default();
        assert_eq!(
            h.feed(MouseEventKind::Down(MouseButton::Right), 10, 5, DragPan::Button),
            None
        );
        assert_eq!(
            h.feed(MouseEventKind::Drag(MouseButton::Right), 14, 5, DragPan::Button),
            None
        );
        assert_eq!(
            h.feed(MouseEventKind::Drag(MouseButton::Left), 18, 5, DragPan::Button),
            None
        );
    }

    #[test]
    fn a_drag_without_a_press_still_starts_a_gesture() {
        // Some terminals (and tmux forwarding) report the drag but drop the press.
        // The first event only establishes the reference position, so it is not
        // swallowed (it cannot be turned into travel yet) ...
        let mut g = PanGesture::default();
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 10, 5, DragPan::Button),
            None
        );
        // ... and every later drag pans from it.
        assert_eq!(
            g.feed(MouseEventKind::Drag(MouseButton::Left), 14, 5, DragPan::Button),
            Some(2)
        );
    }

    #[test]
    fn taps_are_deferred_only_once_a_release_was_seen() {
        let mut g = PanGesture::default();
        assert!(!g.can_defer_tap(), "press-to-click until an Up is proven");
        g.feed(MouseEventKind::Up(MouseButton::Left), 10, 5, DragPan::Button);
        assert!(g.can_defer_tap());
    }

    #[test]
    fn drag_pan_mode_parses_its_env_values() {
        assert_eq!(DragPan::parse(""), DragPan::Button);
        assert_eq!(DragPan::parse("1"), DragPan::Button);
        assert_eq!(DragPan::parse(" Button "), DragPan::Button);
        assert_eq!(DragPan::parse("OFF"), DragPan::Off);
        assert_eq!(DragPan::parse("none"), DragPan::Off);
        assert_eq!(DragPan::parse("any"), DragPan::Any);
        assert_eq!(DragPan::parse("Moved"), DragPan::Any);
    }

    #[test]
    fn pan_window_moves_the_window_and_keeps_the_cursor_inside() {
        // window 4..6 (2 columns wide), cursor parked on the right edge
        let (off, cursor) = pan_window(10, 0, 4, 5, 2, 1);
        assert_eq!((off, cursor), (5, 5));
        // panning left past the cursor pulls it back to the new window's right edge
        let (off, cursor) = pan_window(10, 0, 5, 5, 2, -3);
        assert_eq!((off, cursor), (2, 3));
        // the window never scrolls into the pinned prefix
        let (off, _) = pan_window(10, 1, 1, 1, 2, -5);
        assert_eq!(off, 1);
        // a cursor inside the pinned prefix stays there
        let (off, cursor) = pan_window(10, 1, 1, 0, 2, 3);
        assert_eq!((off, cursor), (4, 0));
        // the right edge stops at the last column
        let (off, cursor) = pan_window(10, 0, 7, 8, 2, 9);
        assert_eq!((off, cursor), (9, 9));
        // an empty grid is a no-op
        assert_eq!(pan_window(0, 0, 0, 0, 2, 1), (0, 0));
    }

    /// The bug that made a swipe look dead: the cursor was clamped with a stale
    /// visible-column count, so `window_for_cursor` on the next render pulled the
    /// window straight back to where it started.
    #[test]
    fn pan_places_the_cursor_where_window_for_cursor_keeps_the_window() {
        let grid = ten_col_grid();
        let (avail, max_cell, frozen) = (21, 44, 1);
        // walk the window right one column at a time with the cursor parked on the
        // right edge (exactly what a swipe leaves behind)
        let mut off = 1;
        let mut cursor = 1;
        for _ in 0..8 {
            let target = (off + 1).min(grid.columns.len() - 1);
            let vis = visible_cols(&grid, target, avail, max_cell).max(1);
            let (next_off, next_cursor) = pan_window(grid.columns.len(), frozen, off, cursor, vis, 1);
            off = next_off;
            cursor = next_cursor;
            let vis = visible_cols(&grid, off, avail, max_cell).max(1);
            assert_eq!(
                window_for_cursor(&grid, cursor, off, avail, max_cell, frozen),
                (off, vis),
                "render must keep the window the pan chose (off={off}, cursor={cursor})"
            );
        }
        assert_eq!(off, 9);
    }

    #[test]
    fn wire_hint_shows_the_exact_encoding() {
        let left = MouseEvent {
            kind: MouseEventKind::ScrollLeft,
            column: 11,
            row: 4,
            modifiers: KeyModifiers::SHIFT,
        };
        let hint = mouse_wire_hint(&left);
        assert!(hint.contains("<70;12;5M"), "{hint}"); // 66 + 4 for shift
        assert!(hint.contains("X10"), "{hint}");
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0);
        assert!(mouse_wire_hint(&drag).contains("<32;1;1M"));
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 2, 3);
        assert!(mouse_wire_hint(&up).contains("<3;3;4m"), "SGR release uses a lowercase m");
        assert!(describe_mouse(&drag).contains("Drag(Left)"));
    }

    #[test]
    fn values_keep_null_and_empty_distinct() {
        assert!(value_to_val(&serde_json::Value::Null).is_null());
        assert_eq!(value_to_val(&serde_json::json!("")).text(), "");
        assert_eq!(value_to_val(&serde_json::json!(42)).text(), "42");
    }

    #[test]
    fn null_and_empty_string_render_distinctly() {
        let (null_text, null) = value_display(&Val::Null);
        assert_eq!(null_text, "NULL");
        assert_eq!(null.fg, Some(Color::DarkGray));
        assert_eq!(
            null.add_modifier.contains(Modifier::ITALIC),
            italic_supported(),
            "NULL is italic only when the terminal supports it"
        );

        let (empty_text, empty) = value_display(&Val::Text(String::new()));
        assert_eq!(empty_text, "''");
        assert_eq!(empty.fg, Some(Color::DarkGray));
        assert!(!empty.add_modifier.contains(Modifier::ITALIC));

        // A literal string "NULL" stays plain — that is what tells it apart
        // from the real thing, which is grey (and italic when possible).
        let (literal_text, literal) = value_display(&Val::Text("NULL".into()));
        assert_eq!(literal_text, "NULL");
        assert_eq!(literal.fg, None);
        assert_ne!(literal, null);
    }

    /// Render one row of grid cells exactly like the results pane does, into a
    /// headless buffer, so the NULL / `''` contract can be checked without a
    /// real terminal.
    fn capture_grid_cells(rows: &[Vec<Val>], width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        let ncols = rows.first().map(Vec::len).unwrap_or(0);
        let rendered: Vec<Row> = rows
            .iter()
            .map(|row| {
                Row::new(
                    row.iter()
                        .map(|v| cell_widget_hl(v, 8, false, None))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let table = Table::new(
            rendered,
            (0..ncols).map(|_| Constraint::Length(8)).collect::<Vec<_>>(),
        )
        .column_spacing(1);
        term.draw(|f| f.render_widget(table, f.area())).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn null_empty_and_literal_null_stay_apart_at_both_capture_sizes() {
        // Three columns, each a different case: a NULL-only column, an
        // empty-string-only column, and a mixed column that holds both a real
        // NULL and the literal text "NULL".
        let rows = vec![
            vec![
                Val::Null,
                Val::Text(String::new()),
                Val::Text("NULL".into()),
            ],
            vec![Val::Null, Val::Text(String::new()), Val::Null],
            vec![Val::Null, Val::Text(String::new()), Val::Text("x".into())],
        ];
        // The two capture sizes the R12 acceptance pass uses: a phone-ish 42×22
        // and a desktop 110×30.
        for (w, h) in [(42u16, 22u16), (110, 30)] {
            let buf = capture_grid_cells(&rows, w, h);
            let text_at = |x: u16, y: u16, n: u16| -> String {
                (0..n)
                    .map(|i| buf.cell((x + i, y)).unwrap().symbol())
                    .collect()
            };
            // 8-wide cells with a 1-column gutter: x = 0, 9, 18.
            for (r, row) in rows.iter().enumerate() {
                let y = r as u16;
                let null_cell = buf.cell((0u16, y)).unwrap();
                assert_eq!(text_at(0, y, 4), "NULL", "{w}x{h} r{r}");
                assert_eq!(null_cell.fg, Color::DarkGray, "{w}x{h} r{r}");
                assert_eq!(
                    null_cell.modifier.contains(Modifier::ITALIC),
                    italic_supported(),
                    "{w}x{h} r{r}"
                );

                let empty_cell = buf.cell((9u16, y)).unwrap();
                assert_eq!(text_at(9, y, 2), "''", "{w}x{h} r{r}");
                assert_eq!(empty_cell.fg, Color::DarkGray, "{w}x{h} r{r}");
                assert!(!empty_cell.modifier.contains(Modifier::ITALIC), "{w}x{h} r{r}");

                // The mixed column: the literal "NULL" is plain, the real
                // NULL is grey (and italic when supported).
                let expected = match &row[2] {
                    Val::Text(s) => s.clone(),
                    Val::Null => "NULL".to_string(),
                };
                let mixed = buf.cell((18u16, y)).unwrap();
                assert_eq!(text_at(18, y, expected.len() as u16), expected, "{w}x{h} r{r}");
                if row[2] == Val::Null {
                    assert_eq!(mixed.fg, Color::DarkGray, "{w}x{h} r{r}");
                    assert_eq!(
                        mixed.modifier.contains(Modifier::ITALIC),
                        italic_supported(),
                        "{w}x{h} r{r}"
                    );
                } else {
                    assert_ne!(mixed.fg, Color::DarkGray, "{w}x{h} r{r}");
                }
            }
        }
    }

    #[test]
    fn csv_export_keeps_null_and_empty_as_empty_fields() {
        let grid = Grid {
            columns: vec!["a".into(), "b".into(), "c".into()],
            rows: vec![vec![
                Val::Null,
                Val::Text(String::new()),
                Val::Text("NULL".into()),
            ]],
            note: String::new(),
        };
        // NULL and '' are both empty fields (RFC 4180), a literal "NULL" is not.
        assert_eq!(grid_to_csv(&grid), "a,b,c\n,,NULL\n");
    }

    fn ten_col_grid() -> Grid {
        let columns: Vec<String> = (0..10).map(|i| format!("c{i}")).collect();
        Grid {
            columns,
            rows: vec![vec![Val::Text("1234567890".into()); 10]],
            note: String::new(),
        }
    }

    #[test]
    fn abs_row_counts_across_pages() {
        // page 2 (0-based), 50/page, 5th row of the page → row 106
        assert_eq!(abs_row(2, 50, 5), 106);
        assert_eq!(abs_row(0, 50, 0), 1);
    }

    #[test]
    fn page_count_rounds_up_and_never_zero() {
        assert_eq!(page_count(400, 50), 8);
        assert_eq!(page_count(401, 50), 9);
        assert_eq!(page_count(0, 50), 1);
    }

    #[test]
    fn frozen_first_column_needs_room() {
        let grid = ten_col_grid();
        // 10-wide columns, gutter 2: 2+1+10+1+6 = 20 fits in 40, not in 15.
        assert_eq!(effective_frozen(true, &grid, 2, 40, 44), 1);
        assert_eq!(effective_frozen(true, &grid, 2, 15, 44), 0);
        assert_eq!(effective_frozen(false, &grid, 2, 40, 44), 0);
        // a two-column grid has nothing worth pinning
        let narrow = Grid {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Val::Text("1".into()), Val::Text("2".into())]],
            note: String::new(),
        };
        assert_eq!(effective_frozen(true, &narrow, 2, 40, 44), 0);
    }

    #[test]
    fn window_follows_cursor_to_the_right() {
        let grid = ten_col_grid();
        // 10-wide columns, 2 fit in 21; cursor on col 5 scrolls the window to 4..6
        assert_eq!(window_for_cursor(&grid, 0, 0, 21, 44, 0), (0, 2));
        assert_eq!(window_for_cursor(&grid, 5, 0, 21, 44, 0), (4, 2));
        // cursor to the left of the window pulls it back
        assert_eq!(window_for_cursor(&grid, 1, 4, 21, 44, 0), (1, 2));
    }

    #[test]
    fn window_never_scrolls_into_frozen_prefix() {
        let grid = ten_col_grid();
        // frozen=1: the scrollable window starts at index 1 even with cursor at 0
        assert_eq!(window_for_cursor(&grid, 0, 0, 21, 44, 1), (1, 2));
        // cursor past the frozen column scrolls the window while staying >= 1
        let (off, vis) = window_for_cursor(&grid, 7, 1, 21, 44, 1);
        assert!(off >= 1);
        assert!(off <= 7 && 7 < off + vis);
    }

    #[test]
    fn window_clamps_when_columns_are_narrow() {
        let grid = Grid {
            columns: vec!["a".into(), "b".into(), "c".into()],
            rows: vec![vec![Val::Text("x".into()); 3]],
            note: String::new(),
        };
        // avail 0 still shows one column
        assert_eq!(window_for_cursor(&grid, 0, 0, 0, 44, 0), (0, 1));
    }

    #[test]
    fn sql_literal_escapes_quotes_and_backslashes() {
        assert_eq!(sql_literal("O'Brien"), "'O''Brien'");
        assert_eq!(sql_literal("a\\b"), "'a\\\\b'");
        assert_eq!(sql_literal("plain"), "'plain'");
    }

    #[test]
    fn numeric_type_detection_ignores_length_params() {
        assert!(is_numeric_type("int"));
        assert!(is_numeric_type("BIGINT(20) UNSIGNED"));
        assert!(is_numeric_type("decimal(10, 2)"));
        assert!(is_numeric_type("double precision"));
        assert!(!is_numeric_type("varchar(50)"));
        assert!(!is_numeric_type("text"));
        assert!(!is_numeric_type("date"));
        assert!(!is_numeric_type("json"));
    }

    #[test]
    fn value_literal_types_numbers_but_quotes_text() {
        // NULL and empty string stay distinct
        assert_eq!(val_literal(&Val::Null, Some("int")), "NULL");
        assert_eq!(val_literal(&Val::Text(String::new()), Some("int")), "''");
        // numeric column + numeric value → unquoted
        assert_eq!(val_literal(&Val::Text("42".into()), Some("int")), "42");
        // same value in a text column → quoted
        assert_eq!(val_literal(&Val::Text("42".into()), Some("varchar(10)")), "'42'");
        // a non-numeric value in a numeric column must still be quoted
        assert_eq!(val_literal(&Val::Text("n/a".into()), Some("int")), "'n/a'");
        assert_eq!(
            val_literal(&Val::Text("true".into()), Some("bool")),
            "TRUE"
        );
    }

    #[test]
    fn count_cache_key_depends_on_filter_not_sort() {
        assert_eq!(count_cache_key("db", "t", ""), count_cache_key("db", "t", ""));
        assert_ne!(
            count_cache_key("db", "t", "a = 1"),
            count_cache_key("db", "t", "a = 2")
        );
        assert_ne!(count_cache_key("db1", "t", ""), count_cache_key("db2", "t", ""));
    }

    #[test]
    fn page_state_extra_reports_filter_and_sort() {
        let ps = PageState {
            table: "t".into(),
            table_type: None,
            page: 0,
            page_size: 50,
            total: None,
            has_next: false,
            filter: "city = 'Beijing'".into(),
            order_by: Some("`id` DESC".into()),
        };
        let extra = page_state_extra(&ps);
        assert!(extra.contains("过滤: city = 'Beijing'"));
        assert!(extra.contains("排序: `id` DESC"));
    }

    #[test]
    fn page_state_extra_is_empty_without_filter_or_sort() {
        let ps = PageState {
            table: "t".into(),
            table_type: None,
            page: 0,
            page_size: 50,
            total: None,
            has_next: false,
            filter: String::new(),
            order_by: None,
        };
        assert!(page_state_extra(&ps).is_empty());
    }

    #[test]
    fn double_encoding_is_reversed_for_display() {
        // "保留表" written through a CP1252 connection: the stored string is the
        // mojibake below (U+009D for byte 0x9D).
        let mojibake = "\u{e4}\u{bf}\u{9d}\u{e7}\u{2022}\u{2122}\u{e8}\u{a1}\u{a8}";
        assert_eq!(fix_double_encoding(mojibake), "保留表");
    }

    #[test]
    fn double_encoding_leaves_clean_names_alone() {
        // Correctly stored CJK contains chars > U+00FF and must pass through.
        assert_eq!(fix_double_encoding("保留表"), "保留表");
        assert_eq!(fix_double_encoding("users"), "users");
        // A Latin-1 name whose bytes are not valid UTF-8 is left untouched.
        assert_eq!(fix_double_encoding("café"), "café");
    }

    #[test]
    fn scrollbar_geometry_covers_full_track() {
        // Everything visible → the whole track is the thumb.
        assert_eq!(scrollbar_geom(10, 0, 10, 40), (0, 40));
        // Half the content visible: thumb is half, at the start / end.
        assert_eq!(scrollbar_geom(10, 0, 5, 40), (0, 20));
        assert_eq!(scrollbar_geom(10, 5, 5, 40), (20, 20));
        // Clamps beyond the ends.
        assert_eq!(scrollbar_geom(10, 99, 5, 40), (20, 20));
        assert_eq!(scrollbar_geom(0, 0, 1, 40), (0, 0));
        assert_eq!(scrollbar_geom(10, 0, 5, 0), (0, 0));
    }

    #[test]
    fn order_by_round_trips_through_parser() {
        let keys = parse_order_by(Some("`id` DESC, `name` ASC"));
        assert_eq!(
            keys,
            vec![("id".to_string(), true), ("name".to_string(), false)]
        );
        assert_eq!(parse_order_by(Some("\"a b\" DESC")), vec![("a b".into(), true)]);
        assert!(parse_order_by(None).is_empty());
    }

    #[test]
    fn identifier_unquoting_strips_dialect_quotes() {
        assert_eq!(unquote_ident("`id`"), "id");
        assert_eq!(unquote_ident("\"name\""), "name");
        assert_eq!(unquote_ident("[col]"), "col");
        assert_eq!(unquote_ident("plain"), "plain");
    }

    #[test]
    fn filter_mentions_matches_whole_identifiers() {
        assert!(filter_mentions("city = 'X' AND id > 3", "city"));
        assert!(filter_mentions("`city` = 'X'", "city"));
        assert!(filter_mentions("\"City\" = 'X'", "city"));
        // Substrings inside a longer identifier must not match.
        assert!(!filter_mentions("user_id = 3", "id"));
        assert!(!filter_mentions("id_card = '3'", "id"));
        assert!(!filter_mentions("", "id"));
    }

    #[test]
    fn new_value_literal_handles_null_empty_and_typing() {
        // A blank box (and an explicit NULL) both mean SQL NULL now.
        assert_eq!(new_value_literal("", Some("varchar(10)")), "NULL");
        assert_eq!(new_value_literal("   ", Some("varchar(10)")), "NULL");
        assert_eq!(new_value_literal("NULL", Some("int")), "NULL");
        assert_eq!(new_value_literal("null", Some("varchar(10)")), "NULL");
        // `''` is the empty string; quoted text is taken verbatim.
        assert_eq!(new_value_literal("''", Some("varchar(10)")), "''");
        assert_eq!(new_value_literal("'text'", Some("varchar(10)")), "'text'");
        assert_eq!(new_value_literal("'O''Brien'", Some("text")), "'O''Brien'");
        // Unquoted values are coerced by column type, as before.
        assert_eq!(new_value_literal("42", Some("int")), "42");
        assert_eq!(new_value_literal("42", Some("varchar(10)")), "'42'");
        assert_eq!(new_value_literal("O'Brien", Some("text")), "'O''Brien'");
    }

    #[test]
    fn edit_prefill_round_trips_through_new_value_literal() {
        let round = |v: Val, t: Option<&str>| new_value_literal(&edit_prefill(&v), t);
        // NULL opens blank and submits back as NULL.
        assert_eq!(edit_prefill(&Val::Null), "");
        assert_eq!(round(Val::Null, Some("varchar(10)")), "NULL");
        // The empty string opens as `''` and stays an empty string.
        assert_eq!(edit_prefill(&Val::Text(String::new())), "''");
        assert_eq!(round(Val::Text(String::new()), Some("varchar(10)")), "''");
        // A literal "NULL" is quoted so an unchanged submit cannot turn it into NULL.
        assert_eq!(edit_prefill(&Val::Text("NULL".into())), "'NULL'");
        assert_eq!(round(Val::Text("NULL".into()), Some("varchar(10)")), "'NULL'");
        // Ordinary values open verbatim.
        assert_eq!(edit_prefill(&Val::Text("42".into())), "42");
        assert_eq!(round(Val::Text("42".into()), Some("int")), "42");
        assert_eq!(round(Val::Text("42".into()), Some("varchar(10)")), "'42'");
        assert_eq!(round(Val::Text("O'Brien".into()), Some("text")), "'O''Brien'");
    }

    #[test]
    fn auto_collapse_switch_controls_unfocused_panes() {
        // Off → everything stays expanded, at any width.
        assert!(!resolve_collapse(false, Focus::Preview, PANE_SIDEBAR, None));
        assert!(!resolve_collapse(false, Focus::Editor, PANE_SIDEBAR, None));
        // On → unfocused aux panes collapse at every width.
        assert!(resolve_collapse(true, Focus::Preview, PANE_SIDEBAR, None));
        assert!(resolve_collapse(true, Focus::Sidebar, PANE_EDITOR, None));
        // The focused pane never collapses; the results pane is never auto-collapsed.
        assert!(!resolve_collapse(true, Focus::Sidebar, PANE_SIDEBAR, None));
        assert!(!resolve_collapse(true, Focus::Editor, PANE_EDITOR, None));
        assert!(!resolve_collapse(true, Focus::Sidebar, PANE_RESULTS, None));
        // A manual per-pane override always wins, even against the master switch.
        assert!(resolve_collapse(false, Focus::Sidebar, PANE_SIDEBAR, Some(true)));
        assert!(!resolve_collapse(true, Focus::Preview, PANE_SIDEBAR, Some(false)));
    }

    #[test]
    fn wrap_sql_lines_keeps_statement_shape() {
        let lines = wrap_sql_lines("UPDATE t\nSET a = 1\nWHERE id = 2;", 40);
        assert_eq!(lines, vec!["UPDATE t", "SET a = 1", "WHERE id = 2;"]);
        // blank lines are dropped so the preview stays compact
        assert_eq!(wrap_sql_lines("A\n\nB", 10), vec!["A", "B"]);
    }

    #[test]
    fn watchdog_tiers_are_bounded() {
        // Metadata / Redis / Mongo calls must never spin forever...
        assert_eq!(Op::ListConnections.watchdog(), OP_WATCHDOG_FALLBACK);
        // ...but a (possibly multi-statement) SQL script gets a wider ceiling.
        let cfg = new_connection_config(
            "t".into(),
            "t".into(),
            parse_database_type("sqlite").unwrap(),
            String::new(),
            0,
            String::new(),
            String::new(),
            None,
            false,
            None,
        )
        .unwrap();
        let q = Op::Query(
            Box::new(cfg),
            "db".into(),
            "SELECT 1".into(),
            QUERY_MAX_ROWS,
        );
        assert_eq!(q.watchdog(), OP_WATCHDOG_SQL);
        assert!(OP_WATCHDOG_FALLBACK < OP_WATCHDOG_SQL);
    }

    #[test]
    fn help_documents_core_bindings() {
        // Guard against the overlay drifting away from the real keymap: every
        // binding a user is likely to reach for must stay documented.
        let keys: Vec<&str> = HELP_ROWS.iter().map(|(k, _)| *k).collect();
        for needle in [
            "Home / End",
            "Ctrl-E",
            "Ctrl-O",
            "Ctrl-P",
            "Ctrl-N",
            "Ctrl-Y",
            "Ctrl-R",
            "Ctrl-K",
            "Ctrl-D",
            "Ctrl-V",
            "Ctrl-T",
            "Ctrl-Space",
        ] {
            assert!(
                keys.iter().any(|k| k.contains(needle)),
                "help is missing {needle}"
            );
        }
        // The connection-picker and editor Esc sections are documented too.
        assert!(keys.contains(&"— 连接选择 —"));
        assert!(HELP_ROWS.iter().any(|(_, d)| *d == "回到侧栏"));
    }

    #[test]
    fn help_has_no_bare_uppercase_shortcuts() {
        // Regression guard for the R8 keymap: every shortcut must be lowercase,
        // a named key, or a Ctrl/Alt/Shift/F-key combination — never a lone
        // uppercase letter the user has to reach with Shift.
        for (key, _) in HELP_ROWS {
            if key.starts_with('—') {
                continue;
            }
            for tok in key.split(['/', ' ', '+']).filter(|t| !t.is_empty()) {
                assert!(
                    !(tok.len() == 1 && tok.chars().all(|c| c.is_ascii_uppercase())),
                    "bare uppercase shortcut in help: {tok:?} ({key})"
                );
            }
        }
    }

    #[test]
    fn explain_uses_dialect_prefix() {
        assert_eq!(
            explain_sql_for("mysql", "SELECT 1;").as_deref(),
            Some("EXPLAIN SELECT 1")
        );
        assert_eq!(
            explain_sql_for("postgres", "select * from t").as_deref(),
            Some("EXPLAIN select * from t")
        );
        assert_eq!(
            explain_sql_for("sqlite", "SELECT 1").as_deref(),
            Some("EXPLAIN QUERY PLAN SELECT 1")
        );
        // Unknown / unsupported engines return None instead of a broken statement.
        assert!(explain_sql_for("sqlserver", "SELECT 1").is_none());
        assert!(explain_sql_for("oracle", "SELECT 1").is_none());
        assert!(explain_sql_for("mysql", "   ;").is_none());
    }

    #[test]
    fn csv_quoting_follows_rfc4180() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn grid_to_csv_keeps_null_empty() {
        let grid = Grid {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Val::Text("1".into()), Val::Null],
                vec![Val::Text("2".into()), Val::Text("a,b".into())],
            ],
            note: String::new(),
        };
        assert_eq!(grid_to_csv(&grid), "id,name\n1,\n2,\"a,b\"\n");
    }

    #[test]
    fn connection_colour_prefers_explicit_hex() {
        assert_eq!(parse_hex_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_hex_color("00ff00"), Some(Color::Rgb(0, 255, 0)));
        assert_eq!(parse_hex_color("not-a-colour"), None);
        assert_eq!(parse_hex_color("#fff"), None);
        // Families are distinct so a picker row is identifiable at a glance.
        assert_ne!(db_type_color("mysql"), db_type_color("redis"));
        assert_ne!(db_type_color("mongodb"), db_type_color("redis"));
    }

    #[test]
    fn query_tab_title_is_first_line() {
        assert_eq!(query_tab_title("\n\nSELECT 1\nFROM t"), "SELECT 1");
        assert_eq!(query_tab_title("   "), "");
    }

    // ── mobile efficiency: compact columns ──

    #[test]
    fn compact_mode_is_automatic_only_on_a_narrow_terminal() {
        assert!(compact_active(None, LayoutMode::Narrow));
        assert!(!compact_active(None, LayoutMode::Mid));
        assert!(!compact_active(None, LayoutMode::Wide));
        // An explicit choice always wins, in both directions.
        assert!(!compact_active(Some(false), LayoutMode::Narrow));
        assert!(compact_active(Some(true), LayoutMode::Wide));
    }

    #[test]
    fn compact_cap_shares_the_pane_so_all_columns_fit() {
        // 40 usable columns, 5 columns: each may be 7 wide, and 5*7+4 = 39 fits.
        let cap = compact_max_cell(42, 2, 5, 18);
        assert_eq!(cap, 7);
        assert!(5 * cap + 4 <= 40);
        // A wide pane with few columns keeps its normal content cap.
        assert_eq!(compact_max_cell(120, 2, 2, 44), 44);
        // One column on a narrow pane is capped at the base, not at 8.
        assert_eq!(compact_max_cell(42, 2, 1, 18), 18);
    }

    #[test]
    fn compact_cap_stays_readable_when_columns_cannot_all_fit() {
        // 20 columns on a phone: even 6 each cannot fit, so it scrolls but the
        // columns stay at the readable minimum instead of collapsing to 1-2 cells.
        let cap = compact_max_cell(42, 2, 20, 18);
        assert_eq!(cap, COMPACT_MIN_CELL);
        assert!(cap <= COMPACT_MAX_CELL);
        // The empty grid is harmless.
        assert_eq!(compact_max_cell(42, 2, 0, 18), COMPACT_MAX_CELL);
    }

    #[test]
    fn filter_grid_hides_columns_and_keeps_values_aligned() {
        let grid = Grid {
            columns: vec!["id".into(), "name".into(), "secret".into()],
            rows: vec![vec![
                Val::Text("1".into()),
                Val::Text("a".into()),
                Val::Text("s".into()),
            ]],
            note: String::new(),
        };
        let hidden: HashSet<String> = ["secret".to_string()].into_iter().collect();
        let out = filter_grid(&grid, &hidden);
        assert_eq!(out.columns, vec!["id", "name"]);
        assert_eq!(out.rows[0][0].text(), "1");
        assert_eq!(out.rows[0][1].text(), "a");
    }

    #[test]
    fn filter_grid_is_identity_without_hidden_columns() {
        let grid = ten_col_grid();
        let out = filter_grid(&grid, &HashSet::new());
        assert_eq!(out.columns.len(), 10);
        // Hiding every column still leaves one, so a grid never renders empty.
        let all: HashSet<String> = grid.columns.iter().cloned().collect();
        let out = filter_grid(&grid, &all);
        assert_eq!(out.columns, vec!["c0"]);
    }

    // ── SQL completion ──

    #[test]
    fn word_before_cursor_takes_the_identifier_tail() {
        let mut ta = TextArea::from(["select * from us"]);
        ta.move_cursor(CursorMove::End);
        let (n, w) = word_before_cursor(&ta);
        assert_eq!((n, w.as_str()), (2, "us"));
        // A dot is part of the word so `t.col` completes as one fragment.
        let mut ta = TextArea::from(["select t.co"]);
        ta.move_cursor(CursorMove::End);
        assert_eq!(word_before_cursor(&ta).1, "t.co");
        // Whitespace before the cursor yields an empty fragment (complete-all).
        let mut ta = TextArea::from(["select "]);
        ta.move_cursor(CursorMove::End);
        assert_eq!(word_before_cursor(&ta).1, "");
    }

    // ── DBX favourites write-back ──

    #[test]
    fn iso_timestamp_matches_the_dbx_shape() {
        let ts = now_iso8601();
        assert_eq!(ts.len(), 20, "{ts}");
        assert!(ts.ends_with('Z'), "{ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert!(ts[..4].chars().all(|c| c.is_ascii_digit()));
        let year: i32 = ts[..4].parse().unwrap();
        assert!((2024..2100).contains(&year), "{ts}");
    }

    #[test]
    fn accepting_a_completion_replaces_the_prefix() {
        let mut ta = TextArea::from(["select * from us"]);
        ta.move_cursor(CursorMove::End);
        let (n, _) = word_before_cursor(&ta);
        let (row, col) = ta.cursor();
        ta.move_cursor(CursorMove::Jump(row as u16, col.saturating_sub(n) as u16));
        ta.delete_str(n);
        ta.insert_str("users");
        assert_eq!(ta.lines(), ["select * from users"]);
    }

    // ── R13: copy row as INSERT ──

    #[test]
    fn insert_literal_keeps_null_empty_and_quotes_apart() {
        assert_eq!(insert_literal(&Val::Null, None), "NULL");
        assert_eq!(insert_literal(&Val::Text(String::new()), None), "''");
        assert_eq!(
            insert_literal(&Val::Text("O'Brien".into()), None),
            "'O''Brien'"
        );
        assert_eq!(
            insert_literal(&Val::Text("a\\b".into()), None),
            "'a\\\\b'"
        );
        // A numeric column keeps a real number bare.
        assert_eq!(
            insert_literal(&Val::Text("42".into()), Some("int")),
            "42"
        );
        // A binary column becomes a portable hex literal.
        assert_eq!(
            insert_literal(&Val::Text("\u{0}\u{1}A".into()), Some("varbinary(8)")),
            "X'000141'"
        );
        assert_eq!(insert_literal(&Val::Text("AB".into()), Some("bytea")), "X'4142'");
    }

    #[test]
    fn binary_type_detection_ignores_length_params() {
        assert!(is_binary_type("BLOB"));
        assert!(is_binary_type("varbinary(255)"));
        assert!(is_binary_type("bytea"));
        assert!(!is_binary_type("varchar(255)"));
        assert!(!is_binary_type("text"));
    }

    #[test]
    fn table_name_is_guessed_from_common_statements() {
        assert_eq!(guess_table_from_sql("select * from users"), Some("users".into()));
        assert_eq!(
            guess_table_from_sql("SELECT a FROM `shop`.`orders` WHERE x=1"),
            Some("orders".into())
        );
        assert_eq!(
            guess_table_from_sql("select * from (select 1)"),
            None
        );
        assert_eq!(
            guess_table_from_sql("update public.t set a=1"),
            Some("t".into())
        );
        assert_eq!(guess_table_from_sql("select 1"), None);
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn row_search_matches_any_cell_and_null_as_text() {
        let row = vec![Val::Text("Alice".into()), Val::Null, Val::Text("42".into())];
        assert!(row_matches(&row, "alice"));
        assert!(row_matches(&row, "null"));
        assert!(row_matches(&row, "4"));
        assert!(!row_matches(&row, "bob"));
        assert!(row_matches(&row, ""));
    }

    #[test]
    fn row_search_keeps_only_matching_rows_and_is_case_insensitive() {
        let grid = Grid {
            columns: vec!["name".into(), "city".into()],
            rows: vec![
                vec![Val::Text("Alice".into()), Val::Text("Beijing".into())],
                vec![Val::Text("Bob".into()), Val::Text("Shanghai".into())],
                vec![Val::Text("alice2".into()), Val::Null],
            ],
            note: String::new(),
        };
        // Empty / whitespace-only needles are a no-op.
        assert_eq!(apply_row_search(grid.clone(), "").rows.len(), 3);
        assert_eq!(apply_row_search(grid.clone(), "  ").rows.len(), 3);
        // Case-insensitive substring across any column.
        let hits = apply_row_search(grid.clone(), "ALICE");
        assert_eq!(hits.rows.len(), 2);
        assert!(matches!(&hits.rows[0][0], Val::Text(s) if s == "Alice"));
        assert!(matches!(&hits.rows[1][0], Val::Text(s) if s == "alice2"));
        // `null` finds real NULLs.
        assert_eq!(apply_row_search(grid.clone(), "null").rows.len(), 1);
        // No match yields an empty grid (but keeps the columns).
        let none = apply_row_search(grid, "zzz");
        assert!(none.rows.is_empty());
        assert_eq!(none.columns.len(), 2);
    }

    #[test]
    fn completion_candidates_are_tagged_deduped_and_case_insensitive() {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let names = vec!["id".to_string(), "ID".to_string(), "name".to_string()];
        push_names(&mut out, &mut seen, &names, 'C', "i");
        // `id` and `ID` collapse to one candidate (case-insensitive dedupe).
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "id");
        assert_eq!(out[0].kind, 'C');
        // A table candidate keeps its `T` tag and matches case-insensitively.
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        push_item(&mut out, &mut seen, "Users", 'T', "us");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Users");
        assert_eq!(out[0].kind, 'T');
    }

    // ── R13: persistent config ──

    #[test]
    fn config_round_trips_and_tolerates_corruption() {
        let path = std::env::temp_dir().join(format!("dbxt-test-{}.json", Uuid::new_v4()));
        let mut cfg = TuiConfig::default();
        cfg.set_compact(Some(true));
        let e = cfg.entry("shop", "orders");
        e.hidden = ["secret".to_string()].into_iter().collect();
        e.compact = Some(false);
        e.order_by = Some("\"id\" DESC".into());
        cfg.save(&path);
        let back = TuiConfig::load(&path);
        assert_eq!(back.compact, Some(true));
        let p = back.table("shop", "orders").unwrap();
        assert_eq!(p.hidden.len(), 1);
        assert!(p.hidden.contains("secret"));
        assert_eq!(p.compact, Some(false));
        assert_eq!(p.order_by.as_deref(), Some("\"id\" DESC"));

        // A truncated / garbage file falls back to defaults instead of failing.
        std::fs::write(&path, "{ not json").unwrap();
        let broken = TuiConfig::load(&path);
        assert!(broken.tables.is_empty());
        assert_eq!(broken.compact, None);
        // A missing file is fine too.
        let _ = std::fs::remove_file(&path);
        assert!(TuiConfig::load(&path).tables.is_empty());
    }

    #[test]
    fn config_tolerates_wrong_shapes() {
        let path = std::env::temp_dir().join(format!("dbxt-shape-{}.json", Uuid::new_v4()));
        // Valid JSON but every field has the wrong type: no panic, defaults only.
        std::fs::write(
            &path,
            r#"{"version":1,"compact":"yes","tables":{"db":"nope","db2":{"t":42},"db3":{"t":{"hidden":"x","compact":7,"order_by":false}}}}"#,
        )
        .unwrap();
        let cfg = TuiConfig::load(&path);
        assert_eq!(cfg.compact, None);
        // A wrong-shaped table map / entry is skipped, not fatal.
        assert!(cfg.table("db", "t").is_none());
        // A nested entry with every field the wrong type degrades to defaults.
        let p = cfg.table("db3", "t").expect("entry kept as defaults");
        assert!(p.hidden.is_empty() && p.compact.is_none() && p.order_by.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_save_merges_concurrent_sessions_and_clears_entries() {
        let path = std::env::temp_dir().join(format!("dbxt-merge-{}.json", Uuid::new_v4()));
        // Session A stores a sort for one table.
        let mut a = TuiConfig::default();
        a.entry("db", "a").order_by = Some("\"id\" ASC".into());
        a.save(&path);
        // Session B knows nothing about table `a` (older snapshot) and writes `b`.
        // Its save must not wipe A's entry.
        let mut b = TuiConfig::default();
        b.entry("db", "b").hidden = ["x".to_string()].into_iter().collect();
        b.save(&path);
        let after = TuiConfig::load(&path);
        assert!(after.table("db", "a").is_some(), "B must not clobber A");
        assert!(after.table("db", "b").is_some());
        // Resetting a table to defaults removes its stored entry instead of
        // silently keeping the stale one.
        let mut c = TuiConfig::default();
        c.entry("db", "a");
        c.save(&path);
        let cleared = TuiConfig::load(&path);
        assert!(cleared.table("db", "a").is_none());
        assert!(cleared.table("db", "b").is_some(), "unrelated entry survives");
        let _ = std::fs::remove_file(&path);
    }

    // ── R13: context-aware completion ──

    #[test]
    fn completion_context_follows_the_cursor() {
        let end = |s: &str| {
            let mut ta = TextArea::from([s]);
            ta.move_cursor(CursorMove::End);
            ta
        };
        assert_eq!(completion_context(&end("select * from us")).0, CompCtx::TableList);
        assert_eq!(completion_context(&end("select * from us")).1, "us");
        assert_eq!(
            completion_context(&end("select * from t left join ")).0,
            CompCtx::TableList
        );
        assert_eq!(completion_context(&end("select * from t where ")).0, CompCtx::Column);
        assert_eq!(completion_context(&end("select * from t on ")).0, CompCtx::Column);
        assert_eq!(completion_context(&end("select * ")).0, CompCtx::Any);
        let (ctx, partial) = completion_context(&end("select * from users.na"));
        assert_eq!(ctx, CompCtx::Qualified("users".into()));
        assert_eq!(partial, "na");
        // `db.table.` → the qualifier is the last segment only.
        assert_eq!(
            completion_context(&end("select * from shop.orders.")).0,
            CompCtx::Qualified("orders".into())
        );
        // Quoted / bracketed qualifiers are unquoted before matching.
        assert_eq!(
            completion_context(&end("select * from `users`.")).0,
            CompCtx::Qualified("users".into())
        );
        assert_eq!(
            completion_context(&end("select * from [users].")).0,
            CompCtx::Qualified("users".into())
        );
    }

    // ── R15: wheel modifier encodings, footer layout, bilingual UI ──

    #[test]
    fn every_wheel_modifier_encoding_pans_columns() {
        // The SGR button codes a terminal may send for a wheel event: 64/65 carry
        // no modifier bit; +4 shift, +8 alt, +16 ctrl. SHIFT is the one terminals
        // most often omit, so ALT and CTRL must pan as well.
        for (mods, pans) in [
            (KeyModifiers::NONE, false),
            (KeyModifiers::SHIFT, true),
            (KeyModifiers::ALT, true),
            (KeyModifiers::CONTROL, true),
        ] {
            assert_eq!(
                wheel_wants_pan(Focus::Preview, mods, false, true),
                pans,
                "modifiers {mods:?}"
            );
            // No horizontal overflow → never pan, modifier or not.
            assert!(!wheel_wants_pan(Focus::Preview, mods, false, false));
        }
        // Ctrl-G pan mode pans with an unmodified wheel; other panes never pan.
        assert!(wheel_wants_pan(Focus::Preview, KeyModifiers::NONE, true, true));
        assert!(!wheel_wants_pan(
            Focus::Editor,
            KeyModifiers::CONTROL,
            false,
            true
        ));
    }

    #[test]
    fn wire_hint_covers_all_wheel_encodings() {
        let mk = |code: u8| {
            let mut m = KeyModifiers::NONE;
            if code & 4 != 0 {
                m |= KeyModifiers::SHIFT;
            }
            if code & 8 != 0 {
                m |= KeyModifiers::ALT;
            }
            if code & 16 != 0 {
                m |= KeyModifiers::CONTROL;
            }
            MouseEvent {
                kind: if code & 1 == 0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                },
                column: 0,
                row: 0,
                modifiers: m,
            }
        };
        for code in [64u8, 65, 68, 69, 72, 73, 80, 81] {
            let hint = mouse_wire_hint(&mk(code));
            assert!(hint.contains(&format!("<{code};1;1M")), "code {code}: {hint}");
        }
    }

    #[test]
    fn footer_keeps_help_visible_and_fits() {
        let hints: Vec<Hint> = vec![
            ("↑↓", "row"),
            ("←→", "column"),
            ("Enter", "details"),
            ("e", "edit"),
            ("i", "insert"),
            ("Del", "delete row"),
            ("y", "copy INSERT"),
            ("f", "filter"),
            ("/", "search"),
            ("?", "help"),
        ];
        let help = *hints.last().unwrap();
        for width in [42usize, 60, 80, 110] {
            let (chosen, dropped) = footer_select(&hints, width);
            // `? 帮助` is pinned: always present and always last.
            assert_eq!(*hints.last().unwrap(), ("?", "help"));
            let line_w = footer_line_width(&chosen, &help, dropped);
            assert!(
                line_w <= width,
                "width {width}: line {line_w} chosen {chosen:?}"
            );
            if dropped {
                assert!(chosen.len() < hints.len() - 1);
            }
        }
        // Very narrow: only the help hint and the `…` marker survive.
        let (chosen, dropped) = footer_select(&hints, 10);
        assert!(chosen.is_empty());
        assert!(dropped);
        assert!(footer_line_width(&chosen, &help, dropped) <= 10);
    }

    #[test]
    fn footer_groups_follow_focus_and_overlays() {
        let keys = |view, focus, has_conn| -> Vec<&'static str> {
            footer_hints_ctx(FooterCtx {
                view,
                focus,
                has_connection: has_conn,
            })
            .iter()
            .map(|h| h.0)
            .collect()
        };
        // Overlays get their own group instead of the page's.
        assert_eq!(
            keys(FooterView::Confirm, Focus::Preview, true),
            vec!["Enter/y", "Esc/n", "?"]
        );
        assert_eq!(
            keys(FooterView::EditDialog, Focus::Preview, true),
            vec!["Enter", "Esc", "Ctrl-V", "Ctrl-T", "?"]
        );
        assert_eq!(
            keys(FooterView::Help, Focus::Preview, true),
            vec!["↑↓", "Esc", "?"]
        );
        // Each browse pane gets its own, most-relevant keys.
        let sidebar = keys(FooterView::Browse, Focus::Sidebar, true);
        assert!(sidebar.contains(&"/") && sidebar.contains(&"r") && sidebar.contains(&"Tab"));
        let editor = keys(FooterView::Browse, Focus::Editor, true);
        assert!(editor.contains(&"Ctrl-J") && editor.contains(&"Ctrl-Space"));
        let preview = keys(FooterView::Browse, Focus::Preview, true);
        assert!(preview.contains(&"↑↓") && preview.contains(&"e") && preview.contains(&"y"));
        // No connection yet → the picker group, never the table group.
        let no_conn = keys(FooterView::Browse, Focus::Sidebar, false);
        assert!(no_conn.contains(&"c") && !no_conn.contains(&"/"));
        // Every group ends with the pinned help key.
        for view in [
            FooterView::Help,
            FooterView::Confirm,
            FooterView::EditDialog,
            FooterView::Browse,
            FooterView::NewConn,
            FooterView::ColPicker,
            FooterView::Snippets,
        ] {
            let h = footer_hints_ctx(FooterCtx {
                view,
                focus: Focus::Preview,
                has_connection: true,
            });
            assert_eq!(h.last().unwrap().0, "?", "{view:?}");
        }
    }

    #[test]
    fn ui_switches_between_chinese_and_english() {
        use ui_text::{t_lang, tf_lang, Lang};
        // Confirm dialog, footer help, and a templated status message.
        assert_eq!(t_lang(" ⚠ 危险操作确认 ", Lang::Zh), " ⚠ 危险操作确认 ");
        assert_eq!(
            t_lang(" ⚠ 危险操作确认 ", Lang::En),
            " ⚠ Confirm dangerous operation "
        );
        assert_eq!(t_lang("帮助", Lang::Zh), "帮助");
        assert_eq!(t_lang("帮助", Lang::En), "Help");
        assert_eq!(t_lang("执行", Lang::En), "execute");
        assert_eq!(t_lang("取消", Lang::En), "cancel");
        assert_eq!(
            tf_lang("搜索「{}」· {} 行命中", &[&"x", &3], Lang::En),
            "Search \"x\" · 3 rows matched"
        );
        assert_eq!(
            tf_lang("搜索「{}」· {} 行命中", &[&"x", &3], Lang::Zh),
            "搜索「x」· 3 行命中"
        );
        // Escaped braces survive template substitution.
        assert_eq!(
            tf_lang("mongo parse: {} (例: db.col.find({{}}))", &[&"boom"], Lang::En),
            "mongo parse: boom (e.g. db.col.find({}))"
        );
    }

    #[test]
    fn language_detection_reads_the_environment() {
        use ui_text::{detect_lang_from, Lang};
        assert_eq!(detect_lang_from(None, Some("zh_CN.UTF-8")), Lang::Zh);
        assert_eq!(detect_lang_from(None, Some("en_US.UTF-8")), Lang::En);
        // DBXT_LANG wins over the locale.
        assert_eq!(detect_lang_from(Some("en"), Some("zh_CN.UTF-8")), Lang::En);
        assert_eq!(detect_lang_from(Some("zh"), Some("en_US.UTF-8")), Lang::Zh);
        // Nothing set → built-in default is Chinese.
        assert_eq!(detect_lang_from(None, None), Lang::Zh);
    }

    #[test]
    fn text_table_covers_every_key() {
        for k in ui_text::ALL_KEYS {
            let en = ui_text::t_lang(k, ui_text::Lang::En);
            assert_ne!(en, *k, "missing English translation for {k:?}");
            assert!(!en.is_empty(), "empty English for {k:?}");
            assert!(
                k.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "key without CJK: {k:?}"
            );
        }
        assert!(ui_text::ALL_KEYS.len() > 300);
    }

    #[test]
    fn version_picks_injected_then_cargo() {
        // An injected release tag wins over Cargo.toml...
        assert_eq!(pick_version(Some("0.0.1"), "0.1.0"), "0.0.1");
        // ...a missing or empty injection falls back to Cargo.toml.
        assert_eq!(pick_version(None, "0.1.0"), "0.1.0");
        assert_eq!(pick_version(Some(""), "0.1.0"), "0.1.0");
    }

    #[test]
    fn version_output_is_a_parseable_semver_line() {
        // cmd/install.sh reads this line, so the format is a contract: exactly
        // one `dbxt ` prefix followed by `x.y.z` (an optional pre-release
        // suffix such as `0.2.0-rc1` is allowed).
        let line = version_line();
        let ver = line
            .strip_prefix("dbxt ")
            .unwrap_or_else(|| panic!("unexpected --version format: {line:?}"));
        let core = ver.split(['-', '+']).next().unwrap();
        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(parts.len(), 3, "not x.y.z: {ver:?}");
        for p in parts {
            assert!(
                !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
                "non-numeric component in {ver:?}"
            );
        }
        // The reported value is the one actually compiled in.
        assert_eq!(ver, dbxt_version());
        assert!(!dbxt_version().is_empty());
    }

    #[test]
    fn help_text_matches_the_real_cli() {
        // The non-interactive `--help` is a contract too: it must name the
        // store argument, both options and the docs URL, and end in a newline
        // (it is written verbatim to stdout).
        let help = help_text();
        assert!(help.starts_with(&format!("dbxt {} — ", dbxt_version())));
        assert!(help.contains("dbxt [DBX_STORE]"));
        assert!(help.contains("DBX_STORE"));
        assert!(help.contains("-h, --help"));
        assert!(help.contains("-V, --version"));
        assert!(help.contains("https://github.com/vst93/dbxt"));
        assert!(help.ends_with('\n'));
        // Exactly the two long options the parser actually accepts.
        assert_eq!(help.matches("--").count(), 2, "unexpected --help drift: {help:?}");
    }

    #[test]
    fn unknown_option_and_no_tty_have_translations() {
        use ui_text::{t_lang, tf_lang, Lang};
        assert_eq!(
            tf_lang("未知选项: {}", &[&"--foo"], Lang::En),
            "Unknown option: --foo"
        );
        assert_eq!(t_lang("用法", Lang::En), "Usage");
        assert_eq!(
            t_lang(
                "stdout 不是终端，无法启动 TUI（--help / --version 可在管道中使用）",
                Lang::En
            ),
            "stdout is not a terminal, cannot start the TUI (--help / --version work over a pipe)"
        );
    }
}
