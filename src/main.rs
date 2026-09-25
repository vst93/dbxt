// dbxt — Terminal UI client built on DBX kernel (dbx-core + dbx-mcp LocalBackend)
// Apache-2.0. Reuses DBX connection storage (dbx.db), native drivers, SQL safety.
#![recursion_limit = "512"]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use dbx_core::models::connection::ConnectionConfig;
use dbx_core::query::QueryExecutionOptions;
use dbx_core::sql_dialect::{
    build_count_table_sql, build_table_data_select_sql_with_database, TableDataSelectSqlOptions,
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
/// Hard cap on rows returned for an arbitrary SQL statement.
const QUERY_MAX_ROWS: usize = 500;

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
    if cols < 46 {
        LayoutMode::Narrow
    } else if cols < 100 {
        LayoutMode::Mid
    } else {
        LayoutMode::Wide
    }
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

#[derive(Clone)]
struct PageState {
    table: String,
    page: usize,
    page_size: usize,
    total: Option<u64>,
    has_next: bool,
}

#[derive(Clone)]
struct Confirm {
    sql: String,
    reasons: Vec<String>,
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
        "drop" => Some("DROP 会永久删除对象".to_string()),
        "truncate" => Some("TRUNCATE 会清空整张表且不可回滚".to_string()),
        "update" | "delete" => {
            if !has_keyword(&lower, "where") {
                Some(format!(
                    "{} 没有 WHERE 子句，会作用于整张表",
                    first.to_ascii_uppercase()
                ))
            } else {
                None
            }
        }
        // A common-table-expression statement can hide a destructive DELETE
        // (`WITH x AS (...) DELETE FROM t`), which has no leading DELETE keyword.
        "with" if has_keyword(&lower, "delete") && !has_keyword(&lower, "where") => {
            Some("DELETE 没有 WHERE 子句，会作用于整张表".to_string())
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
    TableData(Box<ConnectionConfig>, String, String, Option<String>, usize, usize),
    Query(Box<ConnectionConfig>, String, String),
    Redis(Box<ConnectionConfig>, u32, String),
    Mongo(Box<ConnectionConfig>, String, String),
    History(Box<ConnectionConfig>),
    AddConn(Box<ConnectionConfig>),
}

enum OpResult {
    Connections(Vec<ConnectionConfig>),
    Databases(Vec<String>),
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
    },
    Query(Box<dbx_core::db::QueryResult>),
    Script(Vec<StmtOutcome>),
    Redis(String),
    Mongo(String),
    History(Vec<String>),
    Added(String),
    Error(String),
}

fn note_of(r: &dbx_core::db::QueryResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !r.columns.is_empty() {
        parts.push(format!("{} 行", r.rows.len()));
    } else {
        // DML / DDL: report the affected-row count even when it is zero.
        parts.push(format!("影响 {} 行", r.affected_rows));
    }
    if r.truncated {
        parts.push("已截断".into());
    }
    parts.push(format!("{}ms", r.execution_time_ms));
    parts.join(" · ")
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
            Ok(dbs) if !dbs.is_empty() => OpResult::Databases(dbs),
            Ok(_) | Err(_) => {
                if let Some(db) = &cfg.database {
                    OpResult::Databases(vec![db.clone()])
                } else {
                    OpResult::Databases(vec![String::new()])
                }
            }
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
                    text: format!("-- 无法获取 DDL: {e}"),
                },
            }
        }
        Op::TableData(cfg, db, table, table_type, page, page_size) => {
            let options = TableDataSelectSqlOptions {
                database_type: Some(cfg.db_type),
                table_name: table.clone(),
                table_type,
                limit: Some(page_size + 1),
                offset: Some(page * page_size),
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
                    let count_sql = build_count_table_sql(Some(cfg.db_type), None, &table);
                    let total = match backend
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
                    };
                    OpResult::TableData {
                        grid: Box::new(grid),
                        total,
                        has_next,
                        page,
                        table,
                    }
                }
                Err(e) => OpResult::Error(format!("table data: {e}")),
            }
        }
        Op::Query(cfg, db, sql) => {
            let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
            if statements.len() > 1 {
                let options = QueryExecutionOptions {
                    max_rows: Some(QUERY_MAX_ROWS),
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
                                .unwrap_or_else(|| format!("-- statement {}", idx + 1));
                            outcomes.push(stmt_outcome(text, r));
                        }
                        OpResult::Script(outcomes)
                    }
                    Err(e) => OpResult::Error(format!("script: {e}")),
                }
            } else {
                match backend
                    .execute_query(&cfg, &db, &sql, Some(QUERY_MAX_ROWS), Some(60))
                    .await
                {
                    Ok(r) => OpResult::Query(Box::new(r)),
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
                    Err(_) => format!("{:?}", r.value),
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
            Err(e) => OpResult::Error(format!("mongo parse: {e} (例: db.col.find({{}}))")),
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
        Op::AddConn(cfg) => match backend.add_connection_for_mcp(*cfg).await {
            Ok(saved) => OpResult::Added(format!(
                "已保存: {} ({})",
                saved.name,
                saved.db_type.as_str()
            )),
            Err(e) => OpResult::Error(format!("save: {e}")),
        },
    }
}

fn spawn_op(backend: &Arc<LocalBackend>, tx: &Tx, op: Op) {
    let backend = backend.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let res = run_op(&backend, op).await;
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

const FORM_FIELDS: [&str; 9] = [
    "name", "db_type", "host", "port", "username", "password", "database", "ssl", "保存",
];

#[derive(Default, Clone, Copy)]
struct Rects {
    sidebar: Rect,
    editor: Rect,
    cmd: Rect,
    results: Rect,
    picker: Rect,
    picker_visible: bool,
}

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
    sel: usize,
    col_offset: usize,

    confirm: Option<Confirm>,

    loading: bool,
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
    fn current_db(&self) -> String {
        self.databases
            .get(self.db_index)
            .cloned()
            .unwrap_or_default()
    }
    fn set_placeholder(&mut self) {
        let t = match self.backend_kind {
            Backend::Redis => format!("redis 命令… (db={}) · Ctrl-L 切换", self.redis_db),
            Backend::Mongo => format!("mongo shell… (db={}) · Ctrl-L 切换", self.current_db()),
            Backend::Sql => String::new(),
        };
        self.cmd_input.set_placeholder_text(t);
    }
    fn set_editor_text(&mut self, text: &str) {
        let mut ta = TextArea::from(text.split('\n'));
        ta.set_placeholder_text("SQL … (Ctrl-J / F5 执行 · ↑ 历史)");
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

#[tokio::main]
async fn main() -> Result<()> {
    let db_path: PathBuf = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => storage_db_path().map_err(|e| anyhow::anyhow!(e))?,
    };
    if !db_path.exists() {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let backend = Arc::new(LocalBackend::open(&db_path).await.map_err(|e| {
        anyhow::anyhow!(
            "打开 DBX 数据目录失败 ({db_path:?}): {e}\n(可用 DBX_DATA_DIR 或传入 dbx.db 目录参数)"
        )
    })?);

    let terminal = ratatui::init();
    // ratatui 0.29's init() does not enable mouse capture; do it explicitly so the
    // touch (Down) / wheel (Scroll) layer receives events.
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
        confirm: None,
        loading: false,
        spinner: 0,
        status: "加载连接…".into(),
        backend_kind: Backend::Sql,
        cmd_input: TextArea::default(),
        cmd_output: Vec::new(),
        redis_db: 0,
        form: ConnForm::default(),
        layout_mode: LayoutMode::Mid,
        term_h: 0,
        rects: Rects::default(),
    };
    app.editor.set_placeholder_text("SQL … (Ctrl-J / F5 执行 · ↑ 历史)");
    app.set_placeholder();

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
    app.loading = false;
    match res {
        OpResult::Connections(cs) => {
            let n = cs.len();
            app.connections = cs;
            if !app.connections.is_empty() && app.conn_list.selected().is_none() {
                app.conn_list.select(Some(0));
            }
            app.picker_open = app.selected.is_none();
            app.status = format!("{n} 个连接 · ↑↓+Enter 选择 · c 新建");
        }
        OpResult::Databases(dbs) => {
            let configured = app.selected.as_ref().and_then(|c| c.database.clone());
            app.databases = dbs;
            app.db_index = configured
                .as_deref()
                .and_then(|db| app.databases.iter().position(|d| d == db))
                .unwrap_or(0);
            app.grid = None;
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.set_placeholder();
            // auto-load tables for the selected database
            if let Some(cfg) = app.selected.clone() {
                let db = app.current_db();
                app.loading = true;
                app.status = if db.is_empty() {
                    format!("加载 {} 表…", cfg.name)
                } else {
                    format!("加载 {db} 表…")
                };
                spawn_op(&app.backend, tx, Op::ListTables(Box::new(cfg.clone()), db));
                spawn_op(&app.backend, tx, Op::History(Box::new(cfg)));
            }
        }
        OpResult::Tables(ts) => {
            let n = ts.len();
            app.tables = ts;
            app.table_list.select(if n == 0 { None } else { Some(0) });
            app.columns.clear();
            app.ddl = None;
            app.status = format!("{n} 个表/视图 · Enter 数据 · r 结构 · Tab 编辑SQL");
        }
        OpResult::Columns { table, columns: cols } => {
            // Ignore a late result for a table the user has already navigated away from.
            if app.selected_table().map(|t| t.name.clone()).as_deref() != Some(table.as_str()) {
                return;
            }
            let n = cols.len();
            let grid = columns_grid(&cols);
            app.columns = cols;
            app.grid = Some(grid);
            app.grid_kind = GridKind::Columns;
            app.struct_view = StructView::Fields;
            app.page_state = None;
            app.script = None;
            app.sel = 0;
            app.col_offset = 0;
            app.focus = Focus::Preview;
            app.status = format!("{table} 结构 · {n} 字段 · t 切换 DDL · Esc 返回");
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
        } => {
            let rows = grid.rows.len();
            app.grid = Some(*grid);
            app.grid_kind = GridKind::TableData;
            app.page_state = Some(PageState {
                table: table.clone(),
                page,
                page_size: PAGE_SIZE,
                total,
                has_next,
            });
            app.script = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.sel = 0;
            app.col_offset = 0;
            app.focus = Focus::Preview;
            let total_txt = total
                .map(|t| format!("共 {t} 行"))
                .unwrap_or_else(|| "总数未知".into());
            app.status = format!(
                "{}.{} · 第 {} 页 · {} 行 · {total_txt}",
                app.current_db(),
                table,
                page + 1,
                rows
            );
        }
        OpResult::Query(r) => {
            let note = note_of(&r);
            let grid = Grid::from_query(r.columns.clone(), &r.rows, note.clone());
            app.status = format!("{} · {} · {}", app.selected_name(), grid.rows.len(), note);
            app.grid = Some(grid);
            app.grid_kind = GridKind::Query;
            app.page_state = None;
            app.script = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.sel = 0;
            app.col_offset = 0;
            app.focus = Focus::Preview;
        }
        OpResult::Script(outcomes) => {
            let n = outcomes.len();
            let errors = outcomes.iter().filter(|o| o.error.is_some()).count();
            let affected: u64 = outcomes.iter().map(|o| o.affected).sum();
            app.script = Some(ScriptView {
                outcomes,
                sel: 0,
                drilled: None,
            });
            app.grid = None;
            app.page_state = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.sel = 0;
            app.col_offset = 0;
            app.focus = Focus::Preview;
            app.status = format!("脚本 · {n} 条语句 · 影响 {affected} 行 · {errors} 错误 · Enter 看结果");
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
        OpResult::Added(msg) => {
            app.status = format!("✓ {msg}");
            app.page = Page::Browse;
            app.focus = Focus::Sidebar;
            app.form = ConnForm::default();
            app.selected = None;
            app.picker_open = true;
            app.loading = true;
            spawn_op(&app.backend, tx, Op::ListConnections);
        }
        OpResult::Error(e) => {
            app.status = format!("✗ {e}");
        }
    }
}

fn trim_output(v: &mut Vec<String>) {
    while v.len() > 400 {
        v.remove(0);
    }
}

fn columns_grid(cols: &[ColumnInfo]) -> Grid {
    let columns = ["字段", "类型", "键", "可空", "默认值", "注释"]
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
                Val::Text(c.name.clone()),
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
        note: format!("{} 字段", cols.len()),
    }
}

// ─── input handling ──────────────────────────────────────────────────────────

fn handle_event(app: &mut App, tx: &Tx, ev: Event) {
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => key(app, tx, k),
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
    // global: quit
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }

    // confirmation overlay swallows everything else
    if app.confirm.is_some() {
        confirm_key(app, tx, k);
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
        app.grid = None;
        app.script = None;
        app.ddl = None;
        app.page_state = None;
        app.struct_view = StructView::Fields;
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
                execute_sql(app, tx, c.sql);
            }
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm = None;
            app.status = "已取消".into();
        }
        _ => {}
    }
}

fn browse_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // run from anywhere (browse page)
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('j'))
        || k.code == KeyCode::F(5)
    {
        run_current(app, tx);
        return;
    }

    // Preview 焦点：Left/Right 切库（与侧栏一致）；列窗口滚动只归 h/l 与鼠标
    if app.focus == Focus::Preview {
        match k.code {
            KeyCode::Left => {
                cycle_db(app, tx, false);
                return;
            }
            KeyCode::Right => {
                cycle_db(app, tx, true);
                return;
            }
            KeyCode::Char('h') => {
                move_col(app, -1);
                return;
            }
            KeyCode::Char('l') => {
                move_col(app, 1);
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

// ── sidebar: connection picker or table browser ──

fn sidebar_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // no connection selected yet → picker mode
    if app.selected.is_none() {
        match k.code {
            KeyCode::Char('c') => {
                app.page = Page::NewConn;
                app.form = ConnForm::default();
            }
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
        KeyCode::Char('o') => {
            // back to connection picker
            app.selected = None;
            app.tables.clear();
            app.columns.clear();
            app.databases.clear();
            app.grid = None;
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.picker_open = true;
        }
        KeyCode::Char('r') => load_structure(app, tx),
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
        KeyCode::Left | KeyCode::Char('h') => cycle_db(app, tx, false),
        KeyCode::Right | KeyCode::Char('l') => cycle_db(app, tx, true),
        _ => {}
    }
}

fn load_structure(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        app.status = "先选中一张表".into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.loading = true;
    app.status = format!("加载 {table} 结构…");
    let db = app.current_db();
    spawn_op(
        &app.backend,
        tx,
        Op::Columns(Box::new(cfg.clone()), db.clone(), table.clone()),
    );
    spawn_op(&app.backend, tx, Op::Ddl(Box::new(cfg), db, table));
}

fn open_table_data(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| (t.name.clone(), t.table_type.clone())) else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.grid = None;
    app.script = None;
    app.ddl = None;
    app.struct_view = StructView::Fields;
    app.col_offset = 0;
    app.sel = 0;
    app.page_state = Some(PageState {
        table: table.0.clone(),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        has_next: false,
    });
    app.loading = true;
    app.status = format!("加载 {}.{} 数据…", app.current_db(), table.0);
    spawn_op(
        &app.backend,
        tx,
        Op::TableData(
            Box::new(cfg),
            app.current_db(),
            table.0,
            Some(table.1),
            0,
            PAGE_SIZE,
        ),
    );
}

fn goto_page(app: &mut App, tx: &Tx, page: usize) {
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let table_type = app
        .tables
        .iter()
        .find(|t| t.name == ps.table)
        .map(|t| t.table_type.clone());
    app.loading = true;
    app.status = format!("加载 {} 第 {} 页…", ps.table, page + 1);
    spawn_op(
        &app.backend,
        tx,
        Op::TableData(
            Box::new(cfg),
            app.current_db(),
            ps.table.clone(),
            table_type,
            page,
            ps.page_size,
        ),
    );
}

fn reload_tables(app: &mut App, tx: &Tx) {
    if let Some(cfg) = app.selected.clone() {
        app.tables.clear();
        app.columns.clear();
        app.ddl = None;
        app.grid = None;
        app.script = None;
        app.page_state = None;
        app.col_offset = 0;
        app.loading = true;
        let db = app.current_db();
        app.status = format!("切换到 {db} …");
        app.set_placeholder();
        spawn_op(&app.backend, tx, Op::ListTables(Box::new(cfg), db));
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

fn move_col(app: &mut App, delta: i32) {
    // 表结构字段视图固定列，不参与横向滚动
    if app.grid_kind == GridKind::Columns {
        return;
    }
    let Some(grid) = app.grid.as_ref() else {
        return;
    };
    let n = grid.columns.len();
    if n == 0 {
        return;
    }
    let next = (app.col_offset.min(n - 1) as i32 + delta).clamp(0, n as i32 - 1);
    app.col_offset = next as usize;
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
            app.grid = None;
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.cmd_output.clear();
            app.set_placeholder();
            app.loading = true;
            app.status = format!("连接 {} ({})…", cfg.name, cfg.db_type.as_str());
            spawn_op(&app.backend, tx, Op::Databases(Box::new(cfg)));
        }
    }
}

fn result_row_count(app: &App) -> usize {
    if let Some(s) = &app.script {
        if s.drilled.is_none() {
            return s.outcomes.len();
        }
        return s.outcomes[s.drilled.unwrap_or(0)].grid.rows.len();
    }
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return 0;
    }
    app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0)
}

fn result_scroll(app: &mut App, delta: i32) {
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
    let n = result_row_count(app);
    if n == 0 {
        return;
    }
    let next = (app.sel as i32 + delta).clamp(0, n as i32 - 1);
    app.sel = next as usize;
}

fn scroll(app: &mut App, delta: i32) {
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
                result_scroll(app, delta);
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
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.confirm.is_some() {
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
                app.focus = Focus::Editor;
                return;
            }
            if rect_contains(r.results, m.column, m.row) {
                app.focus = Focus::Preview;
                result_click(app, m.row);
                return;
            }
            if rect_contains(r.sidebar, m.column, m.row) {
                app.focus = Focus::Sidebar;
                if app.selected.is_some() {
                    sidebar_click(app, tx, m.row);
                }
            }
        }
        MouseEventKind::ScrollUp => scroll(app, -1),
        MouseEventKind::ScrollDown => scroll(app, 1),
        MouseEventKind::ScrollLeft if app.focus == Focus::Preview => move_col(app, -1),
        MouseEventKind::ScrollRight if app.focus == Focus::Preview => move_col(app, 1),
        _ => {}
    }
}

fn result_click(app: &mut App, y: u16) {
    let area = app.rects.results;
    let rel = y as i32 - area.y as i32 - 2; // skip border + header row
    if rel < 0 {
        return;
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

fn sidebar_click(app: &mut App, tx: &Tx, y: u16) {
    let area = app.rects.sidebar;
    let rel = y as i32 - area.y as i32 - 1; // skip top border
    if rel < 0 {
        return;
    }
    // row 0 = connection header, then optional database selector row
    let header = 1 + if app.databases.len() > 1 { 1 } else { 0 };
    let table_row = rel - header;
    if table_row < 0 {
        return;
    }
    let cap = (area.height as usize)
        .saturating_sub(2 + if app.databases.len() > 1 { 1 } else { 0 })
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

// ── editor / cmd input / preview ──

fn editor_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match (k.modifiers, k.code) {
        (m, KeyCode::Char('j')) if m.contains(KeyModifiers::CONTROL) => run_current(app, tx),
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
    let n = result_row_count(app);
    let page = app
        .rects
        .results
        .height
        .saturating_sub(3)
        .max(1) as usize;
    match k.code {
        KeyCode::Esc => {
            if let Some(s) = &mut app.script {
                if s.drilled.is_some() {
                    s.drilled = None;
                    app.sel = 0;
                    app.col_offset = 0;
                    return;
                }
            }
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                app.struct_view = StructView::Fields;
                return;
            }
            app.show_first_grid();
            app.focus = Focus::Sidebar;
        }
        KeyCode::Char('e') | KeyCode::Char('E') => app.focus = Focus::Editor,
        KeyCode::Char('t') | KeyCode::Char('T') => {
            if app.ddl.is_some() {
                app.struct_view = match app.struct_view {
                    StructView::Fields => StructView::Ddl,
                    StructView::Ddl => StructView::Fields,
                };
                app.ddl_scroll = 0;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(1);
            } else {
                result_scroll(app, -1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                app.ddl_scroll = app.ddl_scroll.saturating_add(1);
            } else {
                result_scroll(app, 1);
            }
        }
        KeyCode::PageUp => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(page as u16);
            } else if app.sel == 0 {
                // at the top of a data page → step to the previous page
                match app.page_state.clone() {
                    Some(ps) if ps.page > 0 => goto_page(app, tx, ps.page - 1),
                    _ => {}
                }
            } else {
                result_scroll(app, -(page as i32));
            }
        }
        KeyCode::PageDown => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                app.ddl_scroll = app.ddl_scroll.saturating_add(page as u16);
            } else if n > 0 && app.sel + 1 >= n {
                // at the bottom of a data page → step to the next page
                match app.page_state.clone() {
                    Some(ps) if ps.has_next => goto_page(app, tx, ps.page + 1),
                    _ => {}
                }
            } else {
                result_scroll(app, page as i32);
            }
        }
        KeyCode::Char('n') => {
            if let Some(ps) = app.page_state.clone() {
                if ps.has_next {
                    goto_page(app, tx, ps.page + 1);
                } else {
                    app.status = "已经是最后一页".into();
                }
            }
        }
        KeyCode::Char('p') => {
            if let Some(ps) = app.page_state.clone() {
                if ps.page > 0 {
                    goto_page(app, tx, ps.page - 1);
                } else {
                    app.status = "已经是第一页".into();
                }
            }
        }
        KeyCode::Enter => {
            if let Some(s) = &app.script {
                if s.drilled.is_none() {
                    let idx = s.sel;
                    drill_script(app, idx);
                }
            }
        }
        _ => {
            let _ = n;
        }
    }
}

fn drill_script(app: &mut App, idx: usize) {
    if let Some(s) = &mut app.script {
        if idx < s.outcomes.len() {
            s.drilled = Some(idx);
            app.sel = 0;
            app.col_offset = 0;
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
        app.status = "✗ 未选择连接".into();
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
        app.confirm = Some(Confirm { sql, reasons });
        return;
    }
    app.push_history(&sql);
    execute_sql(app, tx, sql);
}

fn execute_sql(app: &mut App, tx: &Tx, sql: String) {
    let Some(cfg) = app.selected.clone() else {
        app.status = "✗ 未选择连接".into();
        return;
    };
    app.loading = true;
    app.status = "执行中…".into();
    let db = app.current_db();
    spawn_op(&app.backend, tx, Op::Query(Box::new(cfg), db, sql));
}

fn run_cmd_line(app: &mut App, tx: &Tx) {
    let cmd = app.cmd_input.lines().join(" ").trim().to_string();
    if cmd.is_empty() {
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = "✗ 未选择连接".into();
        return;
    };
    app.cmd_input = TextArea::default();
    app.set_placeholder();
    app.loading = true;
    match app.backend_kind {
        Backend::Redis => {
            app.cmd_output
                .push(format!("redis[{}]> {cmd}", app.redis_db));
            spawn_op(
                &app.backend,
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
                    app.status = "✗ use 需要数据库名".into();
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
                .push(format!("mongo({})> {cmd}", app.current_db()));
            spawn_op(
                &app.backend,
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
        KeyCode::Up => f.field = (f.field + FORM_FIELDS.len() - 1) % FORM_FIELDS.len(),
        KeyCode::Down | KeyCode::Tab => f.field = (f.field + 1) % FORM_FIELDS.len(),
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
            f.field = (f.field + FORM_FIELDS.len() - 1) % FORM_FIELDS.len()
        }
        KeyCode::Right | KeyCode::Char('l') => f.field = (f.field + 1) % FORM_FIELDS.len(),
        _ => {}
    }
}

fn save_form(app: &mut App, tx: &Tx) {
    let f = app.form.clone();
    if f.name.trim().is_empty() || f.host.trim().is_empty() {
        app.form.err = "name / host 必填".into();
        return;
    }
    let Ok(db_type) = parse_database_type(&f.db_type) else {
        app.form.err = format!(
            "未知类型: {} (mysql / postgres / redis / mongodb …)",
            f.db_type
        );
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
    app.status = "保存连接…".into();
    spawn_op(&app.backend, tx, Op::AddConn(Box::new(cfg)));
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
    if let Some(confirm) = app.confirm.clone() {
        render_confirm(f, f.area(), &confirm);
    }
}

fn fit_status(msg: &str, width: usize) -> String {
    let n = disp_width(msg);
    if n <= width {
        return msg.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if msg.starts_with('✗') {
        // 错误：错误码 / 表名等根因在前中部，保留头部，尾部（诊断提示）截断
        truncate_disp(msg, width)
    } else {
        // 普通消息：进度类根因常在尾部，保留尾部
        let skip = n - (width - 1);
        let tail: String = msg.chars().skip(skip).collect();
        format!("…{tail}")
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let conn = app
        .selected
        .as_ref()
        .map(|c| format!("{} ({})", c.name, c.db_type.as_str()))
        .unwrap_or_else(|| "未连接".into());
    let db = if app.selected.is_some() && !app.current_db().is_empty() {
        format!(" · db:{}", app.current_db())
    } else {
        String::new()
    };
    let mode = match app.backend_kind {
        Backend::Sql => "",
        Backend::Redis => " · redis",
        Backend::Mongo => " · mongo",
    };
    let spinner = if app.loading {
        format!(" {}", spinner_frame(app.spinner))
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
    match app.focus {
        Focus::Sidebar => parts.push("焦点 侧栏".into()),
        Focus::Editor => parts.push("焦点 SQL".into()),
        Focus::CmdInput => parts.push("焦点 命令".into()),
        Focus::Preview => parts.push("焦点 结果".into()),
    }
    if let Some(ps) = &app.page_state {
        let pages = ps
            .total
            .map(|t| format!("{}", (t as usize).div_ceil(ps.page_size).max(1)))
            .unwrap_or_else(|| "?".into());
        parts.push(format!("页 {}/{}", ps.page + 1, pages));
    }
    let n = result_row_count(app);
    if n > 0 {
        parts.push(format!("行 {}/{}", (app.sel + 1).min(n), n));
    }
    if let Some(grid) = &app.grid {
        if app.grid_kind != GridKind::Columns && !grid.columns.is_empty() {
            let max_cell = max_cell_width(app.layout_mode);
            let inner_w = app.rects.results.width.saturating_sub(2) as usize;
            let gutter = ((grid.rows.len() + 1).to_string().len()).max(2);
            let avail = inner_w.saturating_sub(gutter + 1);
            let off = app.col_offset.min(grid.columns.len().saturating_sub(1));
            let visible = visible_cols(grid, off, avail, max_cell);
            if off + visible < grid.columns.len() || off > 0 {
                parts.push(format!(
                    "列 {}-{}/{}",
                    off + 1,
                    off + visible,
                    grid.columns.len()
                ));
            }
        }
    }
    parts.join(" · ")
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let right = context_info(app);
    let right_w = (disp_width(&right) as u16 + 2).min(area.width / 2);
    let chunks = Layout::horizontal([Constraint::Min(10), Constraint::Length(right_w)]).split(area);
    let style = if app.status.starts_with('✗') {
        Style::default().fg(Color::Red)
    } else if app.status.starts_with('✓') {
        Style::default().fg(Color::Green)
    } else if app.loading {
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

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let text: String = if app.confirm.is_some() {
        "⚠ 危险操作 · Enter/y 执行 · Esc/n 取消".into()
    } else if app.layout_mode == LayoutMode::Narrow {
        match app.page {
            Page::NewConn => "↑↓ 字段 · Enter 编辑/保存 · Esc 返回".into(),
            _ => match app.focus {
                Focus::Sidebar if app.selected.is_none() => {
                    "↑↓ 连接 · Enter 选 · c 新建 · q 隐藏".into()
                }
                Focus::Sidebar => "↑↓ 表 · ←→ 库 · Enter 数据 · r 结构".into(),
                Focus::Editor => "Ctrl-J 运行 · ↑ 历史 · Tab 下一区".into(),
                Focus::CmdInput => "Enter 执行 · Ctrl-L 换模式".into(),
                Focus::Preview => "↑↓ 滚 · n/p 翻页 · h/l 列 · Esc 返回".into(),
            },
        }
    } else {
        match app.page {
            Page::NewConn => "↑↓/h,l 字段 · Enter 编辑/保存 · Esc 返回 · ssl 回车切换".into(),
            _ => match app.focus {
                Focus::Sidebar if app.selected.is_none() => {
                    "↑↓ 选择连接 · Enter 连接 · c 新建连接 · q 显隐列表 · Tab 直接写SQL".into()
                }
                Focus::Sidebar => {
                    "↑↓ 表 · ←→ 切库 · Enter 浏览数据 · r 表结构 · Tab SQL · o 换连接".into()
                }
                Focus::Editor => "Ctrl-J/F5 运行 · Enter 换行 · ↑/↓ 历史 · Tab 下一区 · Esc 侧栏".into(),
                Focus::CmdInput => {
                    "Enter 执行 · [ ] 切 redis db · Ctrl-L 切 sql/redis/mongo · Esc 编辑器".into()
                }
                Focus::Preview => {
                    "↑↓/jk 滚动 · n/p 翻页 · h/l 列 · t 字段/DDL · e 编辑器 · Esc 收起 · NULL 斜体 · '' 空串".into()
                }
            },
        }
    };
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_browse(f: &mut Frame, area: Rect, app: &mut App) {
    let mode = app.layout_mode;
    let has_cmd = app.backend_kind != Backend::Sql;

    // sidebar placement: narrow → top strip; else left column
    let (sidebar, main) = if mode == LayoutMode::Narrow {
        let v = Layout::vertical([Constraint::Length(7), Constraint::Min(4)]).split(area);
        (v[0], v[1])
    } else {
        let sidebar_w = if mode == LayoutMode::Wide { 28 } else { 22 };
        let hz =
            Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(20)]).split(area);
        (hz[0], hz[1])
    };

    app.rects.sidebar = sidebar;

    render_sidebar(f, sidebar, app);

    // main: editor / cmd input / results
    let cmd_h = if has_cmd { 3 } else { 0 };
    let base_editor_h = if mode == LayoutMode::Narrow { 3 } else { 5 };
    // Narrow + Editor 焦点：同帧把编辑器 3→6 行（小屏 height<14 或放不下时不扩）
    let expand_editor = mode == LayoutMode::Narrow
        && app.focus == Focus::Editor
        && app.term_h >= 14
        && main.height as usize >= 6 + cmd_h as usize + 5;
    let editor_h = if expand_editor { 6 } else { base_editor_h };
    let main_chunks = Layout::vertical([
        Constraint::Length(editor_h),
        Constraint::Length(cmd_h),
        Constraint::Min(5),
    ])
    .split(main);
    app.rects.editor = main_chunks[0];
    app.rects.cmd = if has_cmd {
        main_chunks[1]
    } else {
        Rect::default()
    };

    let focused = app.focus == Focus::Editor;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SQL ")
        .border_set(border::ROUNDED)
        .border_style(border_style(focused));
    app.editor.set_block(block);
    f.render_widget(&app.editor, main_chunks[0]);

    if has_cmd {
        let title = match app.backend_kind {
            Backend::Redis => format!(" redis[{}] ", app.redis_db),
            Backend::Mongo => format!(" mongo({}) ", app.current_db()),
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
    render_results_pane(f, res_area, app);
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
            let title = format!(
                " 语句 {} 结果 · {} · Esc 返回脚本 ",
                i + 1,
                if o.grid.note.is_empty() {
                    "".to_string()
                } else {
                    o.grid.note.clone()
                }
            );
            let grid = o.grid.clone();
            render_grid(f, area, app, &grid, GridKind::Query, &title);
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
            "q 显示连接列表"
        }
    } else if app.tables.is_empty() {
        "无表 · Tab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行"
    } else {
        "↑↓ 选表 · Enter 浏览数据 · r 表结构\nTab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行"
    };
    f.render_widget(
        Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 结果 ")
                    .border_set(border::ROUNDED)
                    .border_style(border_style(app.focus == Focus::Preview)),
            ),
        area,
    );
}

fn grid_title(app: &App) -> String {
    match app.grid_kind {
        GridKind::TableData => {
            let Some(ps) = &app.page_state else {
                return " 结果 ".into();
            };
            let rows = app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0);
            let offset = ps.page * ps.page_size;
            let total = ps
                .total
                .map(|t| format!("共 {t} 行"))
                .unwrap_or_else(|| "总数未知".into());
            let more = if ps.has_next { " · n 下一页" } else { "" };
            format!(
                " {}.{} · 第 {} 页 · {}–{} / {} · {}{} ",
                app.current_db(),
                ps.table,
                ps.page + 1,
                if rows == 0 { 0 } else { offset + 1 },
                offset + rows,
                total,
                app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default(),
                more
            )
        }
        GridKind::Columns => {
            let table = app
                .selected_table()
                .map(|t| t.name.clone())
                .unwrap_or_default();
            format!(
                " 表结构 · {table} · {} · t 查看 DDL ",
                app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default()
            )
        }
        GridKind::Query => format!(
            " 结果 · {} ",
            app.grid
                .as_ref()
                .map(|g| g.note.clone())
                .unwrap_or_default()
        ),
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

    let inner_w = area.width.saturating_sub(2) as usize;
    let total_rows = grid.rows.len();

    // row-number gutter
    let gutter = ((total_rows + 1).to_string().len()).max(2) as u16;

    // Fixed layout for the structure field list; windowed layout for data grids.
    if kind == GridKind::Columns {
        let widths = [
            Constraint::Length(gutter),
            Constraint::Percentage(22),
            Constraint::Percentage(20),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Percentage(22),
            Constraint::Percentage(26),
        ];
        let mut header = vec![Cell::from(Span::styled(
            "#",
            Style::default().fg(Color::DarkGray),
        ))];
        header.extend(grid.columns.iter().map(|c| {
            Cell::from(Span::styled(
                c.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        }));
        let rows: Vec<Row> = grid
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut cells = vec![Cell::from(Span::styled(
                    format!("{}", i + 1),
                    Style::default().fg(Color::DarkGray),
                ))];
                cells.extend(row.iter().map(|v| match v {
                    Val::Text(s) if s.is_empty() => Cell::from(""),
                    _ => cell_widget(v, 40),
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
        return;
    }

    let max_cell = max_cell_width(app.layout_mode);
    let avail = inner_w.saturating_sub(gutter as usize + 1);
    let ncols = grid.columns.len();
    let off = app.col_offset.min(ncols.saturating_sub(1));
    let visible = visible_cols(grid, off, avail, max_cell);
    app.col_offset = off;

    let widths: Vec<usize> = (off..off + visible)
        .map(|ci| natural_width(grid, ci, max_cell))
        .collect();

    let mut header = vec![Cell::from(Span::styled(
        "#",
        Style::default().fg(Color::DarkGray),
    ))];
    for (i, ci) in (off..off + visible).enumerate() {
        let name = grid.columns.get(ci).cloned().unwrap_or_default();
        let w = widths.get(i).copied().unwrap_or(MIN_CELL_WIDTH);
        header.push(Cell::from(Span::styled(
            truncate_disp(&name, w),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    }

    let h = (area.height as usize).saturating_sub(3).max(1);
    let nrows = grid.rows.len();
    let start = app
        .sel
        .saturating_sub(h / 2)
        .min(nrows.saturating_sub(h.min(nrows)));
    let rows: Vec<Row> = grid
        .rows
        .iter()
        .enumerate()
        .skip(start)
        .take(h)
        .map(|(i, row)| {
            let mut cells = vec![Cell::from(Span::styled(
                format!("{}", i + 1),
                Style::default().fg(Color::DarkGray),
            ))];
            for (ci, v) in row.iter().enumerate().skip(off).take(visible) {
                let w = widths.get(ci - off).copied().unwrap_or(MIN_CELL_WIDTH);
                cells.push(cell_widget(v, w));
            }
            let mut r = Row::new(cells);
            if i == app.sel {
                r = r.style(highlight_style());
            }
            r
        })
        .collect();

    let mut constraints = vec![Constraint::Length(gutter)];
    constraints.extend(widths.iter().map(|w| Constraint::Length(*w as u16)));
    let table = Table::new(rows, constraints)
        .header(Row::new(header))
        .column_spacing(1)
        .block(block);
    f.render_widget(table, area);
}

fn highlight_style() -> Style {
    Style::default()
        .bg(Color::Rgb(38, 48, 38))
        .add_modifier(Modifier::BOLD)
}

/// Render one cell: NULL and the empty string get distinct visual treatments.
fn cell_widget(v: &Val, w: usize) -> Cell<'static> {
    match v {
        Val::Null => Cell::from(Span::styled(
            "NULL",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        Val::Text(s) if s.is_empty() => Cell::from(Span::styled("''", Style::default().fg(Color::DarkGray))),
        Val::Text(s) => Cell::from(Span::raw(truncate_disp(s, w))),
    }
}

fn render_script_list(f: &mut Frame, area: Rect, app: &mut App, script: &ScriptView) {
    let focused = app.focus == Focus::Preview;
    let errors = script.outcomes.iter().filter(|o| o.error.is_some()).count();
    let affected: u64 = script.outcomes.iter().map(|o| o.affected).sum();
    let title = format!(
        " 脚本 · {} 条语句 · 影响 {} 行 · {} 错误 · Enter 查看结果 ",
        script.outcomes.len(),
        affected,
        errors
    );
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
                Some(e) => format!("✗ {}", truncate_disp(&one_line(e), 16)),
                None if !o.grid.columns.is_empty() => format!("{} 行", o.grid.rows.len()),
                None => format!("影响 {} 行", o.affected),
            };
            let style = if o.error.is_some() {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let mut r = Row::new(vec![
                Cell::from(Span::styled(
                    format!("{}", i + 1),
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::raw(truncate_disp(&one_line(&o.sql), 80))),
                Cell::from(Span::styled(status, style)),
                Cell::from(Span::styled(
                    format!("{}ms", o.ms),
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
            Row::new(vec!["#", "语句", "结果", "耗时"]).style(
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
    let title = format!(
        " 表结构 (DDL) · {table} · {}/{} 行 · t 返回字段 ",
        (app.ddl_scroll as usize + inner_h).min(total),
        total
    );
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
                    .title(" 输出 ")
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
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Green)),
            Span::styled(
                c.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        // database selector row (only when >1)
        if app.databases.len() > 1 {
            let w = (area.width as usize).saturating_sub(4).max(8);
            let dbs: Vec<String> = app
                .databases
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    if i == app.db_index {
                        format!("[{d}]")
                    } else {
                        format!(" {d} ")
                    }
                })
                .collect();
            let mut joined = dbs.join("");
            if disp_width(&joined) > w {
                joined = dbs[app.db_index].clone();
            }
            lines.push(Line::from(Span::styled(
                joined,
                Style::default().fg(Color::Cyan),
            )));
        }

        let cap = (area.height as usize)
            .saturating_sub(2 + if app.databases.len() > 1 { 1 } else { 0 })
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
                format!("{marker}{}{view}", t.name),
                style,
            )));
        }

        let title = format!(" {} ({}) ", c.name, app.tables.len());
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
            "q 显示\n连接列表"
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" 连接 ")
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
    let box_h = (FORM_FIELDS.len() as u16 + 6).min(area.height.saturating_sub(2));
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
    for (i, label) in FORM_FIELDS.iter().enumerate() {
        let active = i == form.field;
        let value = if i == 8 {
            "↵ 保存连接".to_string()
        } else if form.editing && active {
            format!("{}▏", vals[i])
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
            format!("✗ {}", form.err),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(Span::styled(
        "types: mysql postgres sqlite redis mongodb clickhouse sqlserver …",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 新建连接 ")
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
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:11}", truncate_disp(c.db_type.as_str(), 11)),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(
                    truncate_disp(&c.name, w),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 连接 · ↑↓ Enter · c 新建 · q 隐藏 ")
                .border_set(border::ROUNDED),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.conn_list);
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
    let body: Vec<String> = wrap_text(&one_line(&confirm.sql), w.saturating_sub(4) as usize);
    let lines_n = body.len().min(6);
    let content_h = confirm.reasons.len() + lines_n + 3;
    let h = (content_h as u16 + 2).min(area.height.saturating_sub(2));
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
    for l in body.iter().take(6) {
        lines.push(Line::from(Span::styled(
            l.clone(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter/y 执行   Esc/n 取消",
        Style::default().fg(Color::Yellow),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " ⚠ 危险操作确认 ",
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

    #[test]
    fn values_keep_null_and_empty_distinct() {
        assert!(value_to_val(&serde_json::Value::Null).is_null());
        assert_eq!(value_to_val(&serde_json::json!("")).text(), "");
        assert_eq!(value_to_val(&serde_json::json!(42)).text(), "42");
    }
}
