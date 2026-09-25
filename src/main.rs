// dbxt — Terminal UI client built on DBX kernel (dbx-core + dbx-mcp LocalBackend)
// Apache-2.0. Reuses DBX connection storage (dbx.db), native drivers, SQL safety.
#![recursion_limit = "512"]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use dbx_core::models::connection::ConnectionConfig;
use dbx_core::types::{ColumnInfo, TableInfo};
use dbx_mcp::backend::{new_connection_config, parse_database_type, DbxBackend, LocalBackend};
use dbx_mcp::paths::storage_db_path;
use futures::StreamExt;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;
use tui_textarea::TextArea;
use uuid::Uuid;

type Tx = tokio::sync::mpsc::UnboundedSender<OpResult>;

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

// ─── value rendering ─────────────────────────────────────────────────────────

fn value_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn truncate_cell(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn note_of(r: &dbx_core::db::QueryResult) -> String {
    if r.rows.is_empty() && r.columns.is_empty() {
        if r.affected_rows > 0 {
            format!("OK · affected {} rows", r.affected_rows)
        } else {
            "OK · 0 rows".into()
        }
    } else {
        format!("{}ms", r.execution_time_ms)
    }
}

// ─── async ops ───────────────────────────────────────────────────────────────

enum Op {
    ListConnections,
    Databases(Box<ConnectionConfig>),
    ListTables(Box<ConnectionConfig>, String),
    Columns(Box<ConnectionConfig>, String, String),
    Query(Box<ConnectionConfig>, String, String),
    Redis(Box<ConnectionConfig>, u32, String),
    Mongo(Box<ConnectionConfig>, String, String),
    AddConn(Box<ConnectionConfig>),
}

enum OpResult {
    Connections(Vec<ConnectionConfig>),
    Databases(Vec<String>),
    Tables(Vec<TableInfo>),
    Columns(Vec<ColumnInfo>),
    Query(Box<dbx_core::db::QueryResult>),
    Redis(String),
    Mongo(String),
    Added(String),
    Error(String),
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
            Ok(c) => OpResult::Columns(c),
            Err(e) => OpResult::Error(format!("columns: {e}")),
        },
        Op::Query(cfg, db, sql) => match backend
            .execute_query(&cfg, &db, &sql, Some(500), Some(60))
            .await
        {
            Ok(r) => OpResult::Query(Box::new(r)),
            Err(e) => OpResult::Error(format!("query: {e}")),
        },
        Op::Redis(cfg, db, cmd) => match backend.execute_redis_command(&cfg, db, &cmd, false).await
        {
            Ok(r) => OpResult::Redis(match serde_json::to_string_pretty(&r.value) {
                Ok(s) => s,
                Err(_) => format!("{:?}", r.value),
            }),
            Err(e) => OpResult::Error(format!("redis: {e}")),
        },
        Op::Mongo(cfg, db, source) => match dbx_core::mongo_shell::parse(&source) {
            Ok(cmd) => match backend.execute_mongo_command(&cfg, &db, &cmd).await {
                Ok(r) => {
                    let mut rows = String::new();
                    for row in r.rows.iter().take(50) {
                        let line: Vec<String> = row.iter().map(value_to_str).collect();
                        rows.push_str(&line.join("  "));
                        rows.push('\n');
                    }
                    OpResult::Mongo(if rows.is_empty() { note_of(&r) } else { rows })
                }
                Err(e) => OpResult::Error(format!("mongo: {e}")),
            },
            Err(e) => OpResult::Error(format!("mongo parse: {e} (例: db.col.find({{}}))")),
        },
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
struct QueryView {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    note: String,
}

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

    columns: Vec<ColumnInfo>,
    show_columns: bool,

    editor: TextArea<'static>,
    results: Option<QueryView>,
    result_state: TableState,
    col_offset: usize, // horizontal window into the result columns (Preview focus)
    loading: bool,
    status: String,

    backend_kind: Backend,
    cmd_input: TextArea<'static>,
    cmd_output: Vec<String>,
    redis_db: u32,
    mongo_db: String,

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
            Backend::Mongo => format!("mongo shell… (db={}) · Ctrl-L 切换", self.mongo_db),
            Backend::Sql => String::new(),
        };
        self.cmd_input.set_placeholder_text(t);
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
        show_columns: false,
        editor: TextArea::default(),
        results: None,
        result_state: TableState::default(),
        col_offset: 0,
        loading: false,
        status: "加载连接…".into(),
        backend_kind: Backend::Sql,
        cmd_input: TextArea::default(),
        cmd_output: Vec::new(),
        redis_db: 0,
        mongo_db: String::new(),
        form: ConnForm::default(),
        layout_mode: LayoutMode::Mid,
        term_h: 0,
        rects: Rects::default(),
    };
    app.editor.set_placeholder_text("SQL … (Ctrl-J / F5 执行)");
    app.set_placeholder();

    spawn_op(&backend, &tx, Op::ListConnections);

    while !app.quit {
        terminal.draw(|f| ui(f, &mut app))?;

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
                        handle_event(&mut app, &tx, ev);
                        // drain any completed ops too
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
            if !app.connections.is_empty() {
                app.conn_list.select(Some(0));
            }
            app.picker_open = app.selected.is_none();
            app.status = format!("{n} 个连接 · ↑↓+Enter 选择 · c 新建");
        }
        OpResult::Databases(dbs) => {
            app.databases = dbs;
            app.db_index = 0;
            // auto-load tables for the first db
            if let Some(cfg) = app.selected.clone() {
                let db = app.current_db();
                app.loading = true;
                app.status = if db.is_empty() {
                    format!("加载 {} 表…", cfg.name)
                } else {
                    format!("加载 {db} 表…")
                };
                spawn_op(&app.backend, tx, Op::ListTables(Box::new(cfg), db));
            }
        }
        OpResult::Tables(ts) => {
            let n = ts.len();
            app.tables = ts;
            app.table_list.select(if n == 0 { None } else { Some(0) });
            app.show_columns = false;
            app.status = format!("{n} 个表/视图 · Enter 看结构 · Tab 编辑SQL");
        }
        OpResult::Columns(cols) => {
            let n = cols.len();
            app.columns = cols;
            app.show_columns = true;
            app.result_state.select(Some(0));
            app.status = format!("{n} 列 · Esc 收起");
        }
        OpResult::Query(r) => {
            let view = QueryView {
                columns: r.columns.clone(),
                rows: r
                    .rows
                    .iter()
                    .map(|row| row.iter().map(value_to_str).collect())
                    .collect(),
                note: note_of(&r),
            };
            app.status = format!(
                "{} · {} 行 · {}",
                app.selected_name(),
                r.rows.len(),
                view.note
            );
            app.results = Some(view);
            app.show_columns = false;
            app.result_state.select(Some(0));
            app.col_offset = 0; // new query → reset the column window
            app.focus = Focus::Preview;
        }
        OpResult::Redis(s) => {
            app.cmd_output.push(s);
            trim_output(&mut app.cmd_output);
        }
        OpResult::Mongo(s) => {
            app.cmd_output.push(s);
            trim_output(&mut app.cmd_output);
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
    // global: cycle backend line sql → redis → mongo
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('l') {
        app.backend_kind = match app.backend_kind {
            Backend::Sql => Backend::Redis,
            Backend::Redis => Backend::Mongo,
            Backend::Mongo => Backend::Sql,
        };
        app.cmd_input = TextArea::default();
        app.set_placeholder();
        return;
    }

    match app.page {
        Page::NewConn => form_key(app, tx, k),
        Page::Browse => browse_key(app, tx, k),
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
        Focus::Preview => preview_key(app, k),
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
                return;
            }
            KeyCode::Char('q') => {
                app.picker_open = !app.picker_open;
                return;
            }
            KeyCode::Tab => {
                app.focus = Focus::Editor;
                return;
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
                return;
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
                return;
            }
            KeyCode::Enter => {
                connect_selected(app, tx);
                return;
            }
            _ => return,
        }
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
            app.results = None;
            app.col_offset = 0;
            app.picker_open = true;
        }
        KeyCode::Char('r') => {
            app.show_columns = !app.show_columns;
            if app.show_columns {
                if let Some(name) = selected_table_name(app) {
                    if let Some(cfg) = app.selected.clone() {
                        app.loading = true;
                        app.status = format!("加载 {name} 结构…");
                        spawn_op(
                            &app.backend,
                            tx,
                            Op::Columns(Box::new(cfg), app.current_db(), name),
                        );
                    }
                } else {
                    app.show_columns = false;
                }
            }
        }
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
        KeyCode::Enter => {
            if let Some(name) = selected_table_name(app) {
                if let Some(cfg) = app.selected.clone() {
                    app.loading = true;
                    app.status = format!("加载 {name} 结构…");
                    spawn_op(
                        &app.backend,
                        tx,
                        Op::Columns(Box::new(cfg), app.current_db(), name),
                    );
                    app.show_columns = true;
                }
            }
        }
        KeyCode::Left | KeyCode::Char('h') => cycle_db(app, tx, false),
        KeyCode::Right | KeyCode::Char('l') => cycle_db(app, tx, true),
        _ => {}
    }
}

fn selected_table_name(app: &App) -> Option<String> {
    let idx = app.table_list.selected()?;
    app.tables.get(idx).map(|t| t.name.clone())
}

fn reload_tables(app: &mut App, tx: &Tx) {
    if let Some(cfg) = app.selected.clone() {
        app.tables.clear();
        app.columns.clear();
        app.show_columns = false;
        app.col_offset = 0;
        app.loading = true;
        let db = app.current_db();
        app.status = format!("切换到 {db} …");
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

fn result_visible_cols(app: &App, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let w = app.rects.results.width.saturating_sub(2) as usize;
    let max_cell = match app.layout_mode {
        LayoutMode::Narrow => 12,
        LayoutMode::Mid => 24,
        LayoutMode::Wide => 42,
    };
    (w / (max_cell + 3)).clamp(1, n)
}

fn move_col(app: &mut App, delta: i32) {
    // 表结构视图固定 4 列，不参与横向滚动
    if app.show_columns {
        return;
    }
    let Some(view) = app.results.as_ref() else {
        return;
    };
    let n = view.columns.len();
    if n == 0 {
        return;
    }
    let visible = result_visible_cols(app, n);
    let max = n.saturating_sub(visible);
    let next = (app.col_offset.min(max) as i32 + delta).clamp(0, max as i32);
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
            app.mongo_db = cfg.database.clone().unwrap_or_default();
            app.results = None;
            app.col_offset = 0;
            app.cmd_output.clear();
            app.loading = true;
            app.status = format!("连接 {} ({})…", cfg.name, cfg.db_type.as_str());
            spawn_op(&app.backend, tx, Op::Databases(Box::new(cfg)));
        }
    }
}

fn result_scroll(app: &mut App, delta: i32) {
    let n = if app.show_columns {
        app.columns.len()
    } else {
        app.results.as_ref().map(|r| r.rows.len()).unwrap_or(0)
    };
    if n == 0 {
        return;
    }
    let cur = app.result_state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, n as i32 - 1);
    app.result_state.select(Some(next as usize));
}

fn mouse(app: &mut App, tx: &Tx, m: MouseEvent) {
    let r = app.rects;
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // hit-test order: picker > cmd > editor > results > sidebar
            if r.picker_visible && rect_contains(r.picker, m.column, m.row) {
                // first row is the top border
                let row_index = m.row as i32 - r.picker.y as i32 - 1;
                if row_index >= 0 {
                    let idx = row_index as usize;
                    if idx < app.connections.len() {
                        if app.conn_list.selected() == Some(idx) {
                            // second tap on the same row confirms the connection
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
                return;
            }
            if rect_contains(r.sidebar, m.column, m.row) {
                app.focus = Focus::Sidebar;
                if app.selected.is_some() {
                    sidebar_click(app, tx, m.row);
                }
                return;
            }
        }
        MouseEventKind::ScrollUp => scroll(app, -1),
        MouseEventKind::ScrollDown => scroll(app, 1),
        MouseEventKind::ScrollLeft => {
            // Shift+水平滚轮：结果列窗口左移
            if app.focus == Focus::Preview {
                move_col(app, -1);
            }
        }
        MouseEventKind::ScrollRight => {
            if app.focus == Focus::Preview {
                move_col(app, 1);
            }
        }
        _ => {}
    }
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
    // mirror the viewport window used by render_sidebar
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
        // second tap on the already-selected table loads its structure (like Enter)
        if let Some(name) = selected_table_name(app) {
            if let Some(cfg) = app.selected.clone() {
                app.loading = true;
                app.status = format!("加载 {name} 结构…");
                spawn_op(
                    &app.backend,
                    tx,
                    Op::Columns(Box::new(cfg), app.current_db(), name),
                );
                app.show_columns = true;
            }
        }
    } else {
        app.table_list.select(Some(idx));
    }
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
        Focus::Preview => result_scroll(app, delta),
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

fn preview_key(app: &mut App, k: KeyEvent) {
    let n = if app.show_columns {
        app.columns.len()
    } else {
        app.results.as_ref().map(|r| r.rows.len()).unwrap_or(0)
    };
    match k.code {
        KeyCode::Esc => {
            app.show_columns = false;
            app.focus = Focus::Sidebar;
        }
        KeyCode::Char('e') | KeyCode::Char('E') => app.focus = Focus::Editor,
        KeyCode::Up | KeyCode::Char('k') => result_scroll(app, -1),
        KeyCode::Down | KeyCode::Char('j') => result_scroll(app, 1),
        KeyCode::PageUp => {
            let i = app.result_state.selected().unwrap_or(0).saturating_sub(20);
            app.result_state.select(Some(i));
        }
        KeyCode::PageDown => {
            if n > 0 {
                let i = (app.result_state.selected().unwrap_or(0) + 20).min(n - 1);
                app.result_state.select(Some(i));
            }
        }
        _ => {}
    }
}

fn run_current(app: &mut App, tx: &Tx) {
    match app.backend_kind {
        Backend::Sql => run_sql(app, tx),
        Backend::Redis | Backend::Mongo => run_cmd_line(app, tx),
    }
}

fn run_sql(app: &mut App, tx: &Tx) {
    let sql = app.editor.lines().join("\n").trim().to_string();
    if sql.is_empty() {
        return;
    }
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
            // `use dbname` switches the mongo database locally
            if let Some(db) = cmd.strip_prefix("use ") {
                app.mongo_db = db.trim().trim_end_matches(';').to_string();
                app.cmd_output
                    .push(format!("switched to db {}", app.mongo_db));
                app.set_placeholder();
                app.loading = false;
                return;
            }
            app.cmd_output
                .push(format!("mongo({})> {cmd}", app.mongo_db));
            spawn_op(
                &app.backend,
                tx,
                Op::Mongo(Box::new(cfg), app.mongo_db.clone(), cmd),
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
        let style = if app.status.starts_with('✗') {
            Style::default().fg(Color::Red)
        } else if app.loading {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let msg = if app.loading {
            format!("⏳ {}", app.status)
        } else {
            app.status.clone()
        };
        let msg = fit_status(&msg, chunks[2].width as usize);
        f.render_widget(Paragraph::new(msg).style(style), chunks[2]);
    }
    if footer_h > 0 {
        render_footer(f, chunks[3], app);
    }

    if app.page == Page::Browse && app.picker_open && app.selected.is_none() {
        render_conn_picker(f, f.area(), app);
    }
}

fn fit_status(msg: &str, width: usize) -> String {
    let n = msg.chars().count();
    if n <= width {
        return msg.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if msg.starts_with('✗') {
        // 错误：错误码 / 根因通常在尾部，保留尾部
        let skip = n - (width - 1);
        let tail: String = msg.chars().skip(skip).collect();
        format!("…{tail}")
    } else {
        let head: String = msg.chars().take(width - 1).collect();
        format!("{head}…")
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let conn = app
        .selected
        .as_ref()
        .map(|c| format!("{} ({})", c.name, c.db_type.as_str()))
        .unwrap_or_else(|| "未连接".into());
    let db = if app.selected.is_some() && !app.databases.is_empty() {
        format!(" · db:{}", app.current_db())
    } else {
        String::new()
    };
    let mode = match app.backend_kind {
        Backend::Sql => "",
        Backend::Redis => " · redis",
        Backend::Mongo => " · mongo",
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
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let text: String = if app.layout_mode == LayoutMode::Narrow {
        match app.page {
            Page::NewConn => "↑↓ 字段 · Enter 编辑/保存 · Esc 返回".into(),
            _ => match app.focus {
                Focus::Sidebar if app.selected.is_none() => {
                    "↑↓ 连接 · Enter 选 · c 新建 · q 隐藏".into()
                }
                Focus::Sidebar => "↑↓ 表 · ←→ 库 · r 结构 · Tab SQL · o 换连接".into(),
                Focus::Editor => "Ctrl-J 运行 · Tab 下一区 · Esc 侧栏".into(),
                Focus::CmdInput => "Enter 执行 · Ctrl-L 换模式".into(),
                Focus::Preview => "↑↓ 滚 · ←→ 库 · h/l 列 · e 编辑 · Esc 收起".into(),
            },
        }
    } else {
        match app.page {
            Page::NewConn => "↑↓/h,l 字段 · Enter 编辑/保存 · Esc 返回 · ssl 回车切换".into(),
            _ => match app.focus {
                Focus::Sidebar if app.selected.is_none() => {
                    "↑↓ 选择连接 · Enter 连接 · c 新建连接 · q 显隐列表 · Tab 直接写SQL".into()
                }
                Focus::Sidebar => "↑↓ 表 · ←→ 切库 · Enter/r 表结构 · Tab SQL编辑器 · o 换连接 · Ctrl-L redis/mongo".into(),
                Focus::Editor => "Ctrl-J/F5 运行 · Tab 下一区 · Esc 侧栏 · 支持粘贴".into(),
                Focus::CmdInput => "Enter 执行 · [ ] 切 redis db · Ctrl-L 切 sql/redis/mongo · Esc 编辑器".into(),
                Focus::Preview => "↑↓/jk 滚动 · ←→ 切库 · h/l 列滚动 · PgUp/PgDn 翻页 · e 回编辑器 · Esc 收起".into(),
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
        .style(if focused {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        });
    app.editor.set_block(block);
    f.render_widget(&app.editor, main_chunks[0]);

    if has_cmd {
        let title = match app.backend_kind {
            Backend::Redis => format!(" redis[{}] ", app.redis_db),
            Backend::Mongo => format!(" mongo({}) ", app.mongo_db),
            Backend::Sql => " cmd ".into(),
        };
        let cfocused = app.focus == Focus::CmdInput;
        let b = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Magenta)))
            .border_set(border::ROUNDED)
            .style(if cfocused {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            });
        app.cmd_input.set_block(b);
        f.render_widget(&app.cmd_input, main_chunks[1]);
    }

    let res_area = main_chunks[2];
    app.rects.results = res_area;
    if app.show_columns {
        render_columns(f, res_area, app);
    } else if app.results.is_some() {
        render_results(f, res_area, app);
    } else if has_cmd && !app.cmd_output.is_empty() {
        let text = app.cmd_output.join("\n");
        f.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 输出 ")
                    .border_set(border::ROUNDED),
            ),
            res_area,
        );
    } else {
        let hint = if app.selected.is_none() {
            if app.picker_open {
                ""
            } else {
                "q 显示连接列表"
            }
        } else {
            "Tab 到 SQL 编辑器\nCtrl-L 切 redis/mongo 命令行"
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" 结果 ")
                        .border_set(border::ROUNDED),
                ),
            res_area,
        );
    }
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &mut App) {
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
            if joined.chars().count() > w {
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
                    .border_set(border::ROUNDED),
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
                        .border_set(border::ROUNDED),
                ),
            area,
        );
    }
}

fn render_columns(f: &mut Frame, area: Rect, app: &mut App) {
    // own the data first to avoid borrows across render_stateful_widget
    let cols: Vec<(String, String, bool, String)> = app
        .columns
        .iter()
        .map(|c| {
            (
                if c.is_primary_key {
                    format!("🔑{}", c.name)
                } else {
                    c.name.clone()
                },
                c.data_type.clone(),
                c.is_nullable,
                c.column_default.clone().unwrap_or_default(),
            )
        })
        .collect();

    let widths = [
        Constraint::Percentage(32),
        Constraint::Percentage(22),
        Constraint::Percentage(12),
        Constraint::Percentage(34),
    ];
    let rows = cols.iter().map(|(n, t, nullable, d)| {
        Row::new(vec![
            Cell::from(n.clone()),
            Cell::from(t.clone()),
            Cell::from(if *nullable { "Y" } else { "N" }),
            Cell::from(d.clone()),
        ])
    });
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["column", "type", "null", "default"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 表结构 · Esc 收起 ")
                .border_set(border::ROUNDED),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(table, area, &mut app.result_state);
}

fn render_results(f: &mut Frame, area: Rect, app: &mut App) {
    // own the view to avoid borrow conflicts
    let Some(view) = app.results.clone() else {
        return;
    };
    if view.columns.is_empty() {
        f.render_widget(
            Paragraph::new(view.note).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 结果 ")
                    .border_set(border::ROUNDED),
            ),
            area,
        );
        return;
    }

    let w = area.width.saturating_sub(2) as usize;
    let max_cell = match app.layout_mode {
        LayoutMode::Narrow => 12,
        LayoutMode::Mid => 24,
        LayoutMode::Wide => 42,
    };
    let n = view.columns.len();
    let visible = result_visible_cols(app, n);
    let col_w = (w / visible).saturating_sub(2).clamp(5, max_cell);
    // clamp the horizontal window and slice header + cells by col_offset..+visible
    let max_off = n.saturating_sub(visible);
    if app.col_offset > max_off {
        app.col_offset = max_off;
    }
    let off = app.col_offset;

    let cols: Vec<String> = view
        .columns
        .iter()
        .skip(off)
        .take(visible)
        .map(|c| truncate_cell(c, col_w))
        .collect();
    let widths: Vec<Constraint> = vec![Constraint::Percentage((100 / visible) as u16); visible];

    // viewport rows only (scroll around selection)
    let h = (area.height as usize).saturating_sub(3).max(1);
    let sel = app.result_state.selected().unwrap_or(0);
    let start = sel
        .saturating_sub(h / 2)
        .min(view.rows.len().saturating_sub(h.min(view.rows.len())));
    let rows = view.rows.iter().skip(start).take(h).map(|row| {
        Row::new(
            row.iter()
                .skip(off)
                .take(visible)
                .map(|v| Cell::from(truncate_cell(v, col_w))),
        )
    });

    let col_window = if off == 0 && visible >= n {
        format!("{n}/{n}")
    } else {
        format!("{}-{}/{n}", off + 1, off + visible)
    };
    let title = format!(
        " 结果 · {}/{} 行 · 列 {} · {} ",
        if start > 0 {
            format!("{start}–{}", (start + h).min(view.rows.len()))
        } else {
            format!("{}", h.min(view.rows.len()))
        },
        view.rows.len(),
        col_window,
        view.note
    );
    let table = Table::new(rows, widths)
        .header(
            Row::new(cols).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(table, area, &mut app.result_state);
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
        .style(Style::default().fg(Color::Green));
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
                    format!("{:11}", truncate_cell(c.db_type.as_str(), 11)),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(
                    truncate_cell(&c.name, w),
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
