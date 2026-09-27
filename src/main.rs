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
use dbx_core::db::redis_driver::{
    RedisBlob, RedisBlobEncoding, RedisCollectionPage, RedisKeyInfo, RedisValue, RedisValueData,
};
use dbx_core::models::connection::{ConnectionConfig, DatabaseType};
use dbx_core::query::QueryExecutionOptions;
use dbx_core::sql_dialect::{
    build_count_table_sql, build_table_data_select_sql_with_database, is_schema_aware,
    normalize_where_input, qualified_table_name, quote_table_identifier, TableDataSelectSqlOptions,
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
/// Rows per transactional `INSERT` batch during a CSV import. Each chunk is one
/// transaction, so a mid-chunk failure rolls back that chunk only.
const IMPORT_CHUNK: usize = 500;
/// Data rows shown in the import preview.
const IMPORT_SAMPLE_ROWS: usize = 5;
/// Rows sampled when inferring a CSV column's type.
const IMPORT_INFER_SAMPLE: usize = 200;
/// Result rows above which the export overlay warns that generation may take a
/// moment (the work runs on the UI thread).
const EXPORT_SLOW_ROWS: usize = 10_000;
/// Rows per multi-row `INSERT` group for the batch INSERT export.
const EXPORT_INSERT_BATCH: usize = 100;
/// Import watchdog: a large CSV is many sequential statements, so the last-resort
/// ceiling is far wider than a single query's (each statement still has its own
/// 60 s driver timeout).
const OP_WATCHDOG_IMPORT: Duration = Duration::from_secs(1800);
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

#[derive(Clone, Copy, PartialEq, Debug)]
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
    Query,      // arbitrary SQL result
    TableData,  // paginated SELECT * of a table
    Columns,    // table structure (field list)
    RedisValue, // a Redis key's value rendered per type
    MongoDocs,  // paginated MongoDB documents
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

// ─── Redis / Mongo value rendering ───────────────────────────────────────────

/// Decode a standard base64 string without pulling in a dependency. Returns
/// `None` on any malformed input, so a corrupt blob degrades to a placeholder
/// instead of panicking.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in &bytes {
        if b == b'=' {
            break;
        }
        let v = val(b)?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Human-readable form of a Redis blob. UTF-8 values are decoded; binary values
/// are shown as hex so a terminal never receives raw control bytes.
fn redis_blob_text(b: &RedisBlob) -> String {
    let bytes = b64_decode(&b.raw_base64).unwrap_or_default();
    match b.encoding {
        RedisBlobEncoding::Utf8 => String::from_utf8(bytes)
            .map(|s| sanitize_cell(&s))
            .unwrap_or_else(|_| format!("<binary {} bytes>", b.raw_base64.len())),
        RedisBlobEncoding::Binary => {
            if bytes.is_empty() {
                String::new()
            } else {
                let hex: String = bytes.iter().take(64).map(|b| format!("{b:02x}")).collect();
                if bytes.len() > 64 {
                    format!("0x{hex}… ({} bytes)", bytes.len())
                } else {
                    format!("0x{hex}")
                }
            }
        }
    }
}

/// Raw (unsanitized) text of a Redis blob, used to prefill an edit dialog. Only
/// UTF-8 blobs are editable; binary ones return `None`.
fn redis_blob_editable_text(b: &RedisBlob) -> Option<String> {
    if b.encoding != RedisBlobEncoding::Utf8 {
        return None;
    }
    b64_decode(&b.raw_base64).and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Turn a fetched [`RedisValue`] into a grid the existing results pane can draw.
fn redis_value_view(v: RedisValue) -> RedisValueView {
    let mut row_keys: Vec<String> = Vec::new();
    let (columns, rows, note): (Vec<String>, Vec<Vec<Val>>, String) = match &v.data {
        RedisValueData::String {
            content,
            total_bytes,
            truncated,
        } => {
            row_keys.push(String::new());
            let size = total_bytes.unwrap_or(content.raw_base64.len() as u64);
            let mut note = tf("{} 字节", &[&(size)]);
            if *truncated {
                note.push_str(t(" · 已截断"));
            }
            (vec![t("value").into()], vec![vec![Val::Text(redis_blob_text(content))]], note)
        }
        RedisValueData::Json { value } => {
            row_keys.push(String::new());
            (vec![t("value").into()], vec![vec![Val::Text(sanitize_cell(value))]], String::new())
        }
        RedisValueData::List { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    row_keys.push(it.index.to_string());
                    vec![Val::Text(it.index.to_string()), Val::Text(redis_blob_text(&it.value))]
                })
                .collect();
            (
                vec![t("index").into(), t("value").into()],
                rows,
                tf("{} 个元素", &[&(total)]),
            )
        }
        RedisValueData::Set { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(m)]
                })
                .collect();
            (vec![t("member").into()], rows, tf("{} 个成员", &[&(total)]))
        }
        RedisValueData::Hash { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let f = redis_blob_text(&it.field);
                    row_keys.push(f.clone());
                    let ttl = match it.field_ttl {
                        Some(-1) | None => Val::Null,
                        Some(t) => Val::Text(format!("{t}s")),
                    };
                    vec![
                        Val::Text(f),
                        Val::Text(redis_blob_text(&it.value)),
                        ttl,
                    ]
                })
                .collect();
            (
                vec![t("field").into(), t("value").into(), "TTL".into()],
                rows,
                tf("{} 个字段", &[&(total)]),
            )
        }
        RedisValueData::Zset { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(it.score.clone()), Val::Text(m)]
                })
                .collect();
            (
                vec![t("score").into(), t("member").into()],
                rows,
                tf("{} 个成员", &[&(total)]),
            )
        }
        RedisValueData::Stream {
            entries,
            total,
            next_cursor,
        } => {
            let rows = entries
                .iter()
                .map(|e| {
                    row_keys.push(e.id.clone());
                    let fields = e
                        .fields
                        .iter()
                        .map(|f| format!("{}={}", f.field, f.value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    vec![Val::Text(e.id.clone()), Val::Text(fields)]
                })
                .collect();
            let mut note = total
                .map(|t| tf("{} 条", &[&(t)]))
                .unwrap_or_else(|| tf("{} 条", &[&(entries.len())]));
            if next_cursor.is_some() {
                note.push_str(t(" · 更多"));
            }
            (vec![t("id").into(), t("fields").into()], rows, note)
        }
        RedisValueData::Unknown => (
            vec![t("value").into()],
            vec![vec![Val::Text(t("（暂不支持的类型）").to_string())]],
            String::new(),
        ),
    };
    let scan_cursor = redis_value_cursor(&v.data);
    let mut note = note;
    if scan_cursor.is_some() && !note.contains("更多") {
        note.push_str(t(" · 更多（n 加载）"));
    }
    RedisValueView {
        key_display: v.key_display.clone(),
        key_raw: v.key_raw.clone(),
        redis_type: v.redis_type.clone(),
        ttl: v.ttl,
        grid: Grid {
            columns,
            rows,
            note,
        },
        row_keys,
        scan_cursor,
        raw: v,
    }
}

/// The continuation cursor of a collection value (None = complete).
fn redis_value_cursor(data: &RedisValueData) -> Option<u64> {
    match data {
        RedisValueData::List { scan_cursor, .. }
        | RedisValueData::Set { scan_cursor, .. }
        | RedisValueData::Hash { scan_cursor, .. }
        | RedisValueData::Zset { scan_cursor, .. } => *scan_cursor,
        _ => None,
    }
}

/// Turn one `LOAD MORE` collection page into grid rows + row keys + next cursor.
fn redis_collection_page_rows(
    page: &RedisCollectionPage,
) -> (Vec<Vec<Val>>, Vec<String>, Option<u64>) {
    let mut row_keys: Vec<String> = Vec::new();
    match page {
        RedisCollectionPage::List { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    row_keys.push(it.index.to_string());
                    vec![Val::Text(it.index.to_string()), Val::Text(redis_blob_text(&it.value))]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Set { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(m)]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Hash { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let f = redis_blob_text(&it.field);
                    row_keys.push(f.clone());
                    let ttl = match it.field_ttl {
                        Some(-1) | None => Val::Null,
                        Some(t) => Val::Text(format!("{t}s")),
                    };
                    vec![Val::Text(f), Val::Text(redis_blob_text(&it.value)), ttl]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Zset { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(it.score.clone()), Val::Text(m)]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
    }
}

/// Human TTL label for a key: `-1` never expires, `-2` key missing.
fn redis_ttl_label(ttl: i64) -> String {
    match ttl {
        -1 => t("永不过期").to_string(),
        -2 => t("不存在").to_string(),
        n if n >= 0 => format!("{n}s"),
        n => n.to_string(),
    }
}

/// How many keys one batch command may carry. A multi-key `DEL` with thousands
/// of arguments risks a huge line and a slow single round trip, so the batch is
/// split into chunks of this size (also the per-batch safety ceiling).
pub const REDIS_BATCH_LIMIT: usize = 1000;
/// Keys per generated `DEL` command.
pub const REDIS_BATCH_CHUNK: usize = 100;

/// Quote one key / value for the redis-cli tokenizer the backend uses.
fn redis_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Build the `DEL` commands for a batch of display key names, chunked so one
/// command never grows unbounded.
fn redis_batch_del_commands(displays: &[String]) -> Vec<String> {
    if displays.is_empty() {
        return Vec::new();
    }
    displays
        .chunks(REDIS_BATCH_CHUNK)
        .map(|chunk| {
            let keys: Vec<String> = chunk.iter().map(|k| redis_quote(k)).collect();
            format!("DEL {}", keys.join(" "))
        })
        .collect()
}

/// Build one `EXPIRE key seconds` command per key. `ttl` is validated by the
/// caller; a non-numeric value yields an empty plan.
fn redis_batch_ttl_commands(displays: &[String], ttl: &str) -> Vec<String> {
    let ttl = ttl.trim();
    if ttl.parse::<i64>().is_err() {
        return Vec::new();
    }
    displays
        .iter()
        .map(|k| format!("EXPIRE {} {}", redis_quote(k), ttl))
        .collect()
}

/// Plan a prefix replacement: every key starting with `old_prefix` maps to
/// `new_prefix` + the remainder. Keys that do not match are left untouched.
fn redis_prefix_rename_plan(
    displays: &[String],
    old_prefix: &str,
    new_prefix: &str,
) -> Vec<(String, String)> {
    displays
        .iter()
        .filter_map(|k| {
            let rest = k.strip_prefix(old_prefix)?;
            let new_name = format!("{new_prefix}{rest}");
            (new_name != *k).then(|| (k.clone(), new_name))
        })
        .collect()
}

/// Build the `RENAME old new` commands for a prefix replacement.
fn redis_batch_rename_commands(
    displays: &[String],
    old_prefix: &str,
    new_prefix: &str,
) -> Vec<String> {
    redis_prefix_rename_plan(displays, old_prefix, new_prefix)
        .into_iter()
        .map(|(old, new)| format!("RENAME {} {}", redis_quote(&old), redis_quote(&new)))
        .collect()
}

/// Which batch write a key-browser gesture generates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RedisBatchKind {
    Delete,
    Ttl,
    RenamePrefix,
}

/// A generated batch, ready to be shown in the red confirmation layer.
#[derive(Debug)]
pub struct RedisBatchPlan {
    pub commands: Vec<String>,
    /// When set, the red layer must ask for a typed count / `YES` first.
    pub typed_confirm: Option<usize>,
    pub summary: String,
}

/// Pure batch planner: turn a gesture + targets into commands, a typed-confirm
/// requirement and a human summary. Errors are translated status strings.
fn redis_plan_batch(
    kind: RedisBatchKind,
    targets: &[(String, String)],
    all_loaded: bool,
    arg: &str,
) -> Result<RedisBatchPlan, String> {
    let displays: Vec<String> = targets.iter().map(|(_, d)| d.clone()).collect();
    match kind {
        RedisBatchKind::Delete => Ok(RedisBatchPlan {
            commands: redis_batch_del_commands(&displays),
            typed_confirm: all_loaded.then_some(displays.len()),
            summary: tf("批量删除 {} 个 key", &[&displays.len()]),
        }),
        RedisBatchKind::Ttl => {
            let ttl = arg.trim();
            if ttl.parse::<i64>().is_err() {
                return Err(t("TTL 需为整数秒（-1 持久化，0 立即删除）").to_string());
            }
            let commands = redis_batch_ttl_commands(&displays, ttl);
            if commands.is_empty() {
                return Err(t("没有可操作的 key").to_string());
            }
            Ok(RedisBatchPlan {
                commands,
                typed_confirm: None,
                summary: tf("批量设置 TTL={}s · {} 个 key", &[&ttl, &displays.len()]),
            })
        }
        RedisBatchKind::RenamePrefix => {
            let Some((old, new)) = arg.split_once('=') else {
                return Err(t("格式：旧前缀=新前缀，例 app: = new:").to_string());
            };
            let plan = redis_prefix_rename_plan(&displays, old, new);
            if plan.is_empty() {
                return Err(t("没有 key 匹配该前缀（未改名）").to_string());
            }
            Ok(RedisBatchPlan {
                commands: redis_batch_rename_commands(&displays, old, new),
                typed_confirm: None,
                summary: tf("批量前缀重命名 {} → {} · {} 个 key", &[&old, &new, &plan.len()]),
            })
        }
    }
}

/// Flatten a page of MongoDB documents into a grid: the union of top-level keys
/// (with `_id` first) becomes the columns, and each document is one row.
fn mongo_docs_grid(docs: &[serde_json::Value]) -> Grid {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for doc in docs {
        if let serde_json::Value::Object(map) = doc {
            for k in map.keys() {
                if seen.insert(k.clone()) {
                    keys.push(k.clone());
                }
            }
        }
    }
    if let Some(i) = keys.iter().position(|k| k == "_id") {
        let id = keys.remove(i);
        keys.insert(0, id);
    }
    let rows: Vec<Vec<Val>> = docs
        .iter()
        .map(|doc| {
            keys.iter()
                .map(|k| match doc.get(k) {
                    Some(serde_json::Value::Null) | None => Val::Null,
                    Some(serde_json::Value::String(s)) => Val::Text(sanitize_cell(s)),
                    Some(other) => Val::Text(sanitize_cell(&other.to_string())),
                })
                .collect()
        })
        .collect();
    Grid {
        columns: keys,
        rows,
        note: tf("{} 个文档", &[&(docs.len())]),
    }
}

/// True when `s` looks like a 24-char hex ObjectId. A genuine string `_id` with
/// that shape must be marked so the driver does not reinterpret it.
fn is_object_id_hex(s: &str) -> bool {
    s.len() == 24 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Convert a document's `_id` value (in the driver's `bson_to_json` shape) into
/// the `id` argument the MongoDB document APIs expect.
fn mongo_id_arg(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(s) => {
            if is_object_id_hex(s) {
                // `__dbx_mongo_string_id__` + a JSON string tells the driver this
                // is an explicitly typed BSON string, not an ObjectId.
                format!(
                    "__dbx_mongo_string_id__{}",
                    serde_json::Value::String(s.clone())
                )
            } else {
                s.clone()
            }
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Object(map) => {
            if let Some(oid) = map.get("$oid").and_then(|v| v.as_str()) {
                oid.to_string()
            } else {
                serde_json::to_string(id).unwrap_or_default()
            }
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A short, human-readable rendering of an `_id` for a confirmation prompt.
fn mongo_id_label(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::Object(map) => {
            if let Some(oid) = map.get("$oid").and_then(|v| v.as_str()) {
                oid.to_string()
            } else if let Some(n) = map.get("$numberLong").and_then(|v| v.as_str()) {
                n.to_string()
            } else {
                serde_json::to_string(id).unwrap_or_default()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Top-level field diff between two documents, capped for the confirmation
/// layer. Each line is `field: old -> new` (or `+` / `-` for added / removed).
fn mongo_doc_diff(old: &serde_json::Value, new: &serde_json::Value, cap: usize) -> Vec<String> {
    let empty = serde_json::Map::new();
    let old_map = old.as_object().unwrap_or(&empty);
    let new_map = new.as_object().unwrap_or(&empty);
    let mut keys: Vec<&String> = Vec::new();
    for k in old_map.keys().chain(new_map.keys()) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        if k == "_id" {
            continue;
        }
        let before = old_map.get(k);
        let after = new_map.get(k);
        if before == after {
            continue;
        }
        let show = |v: Option<&serde_json::Value>| match v {
            None => "—".to_string(),
            Some(v) => truncate_disp(&one_line(&v.to_string()), 60),
        };
        let mark = if before.is_none() {
            "+ "
        } else if after.is_none() {
            "- "
        } else {
            "~ "
        };
        out.push(format!(
            "{mark}{k}: {} → {}",
            show(before),
            show(after)
        ));
        if out.len() >= cap {
            out.push(t("…（更多字段已省略）").to_string());
            break;
        }
    }
    out
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
    /// Schema the table lives in (empty for engines without one, e.g. MySQL).
    schema: String,
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
    /// Schema the metadata was read from; matched alongside the table name so
    /// `public.orders` and `inv.orders` never swap column metadata.
    schema: String,
    columns: Vec<ColumnInfo>,
}

/// One paginated table-data request (first load, page turn, filter or sort).
struct TableDataReq {
    cfg: Box<ConnectionConfig>,
    db: String,
    schema: String,
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

// ─── CSV import ──────────────────────────────────────────────────────────────

/// A CSV column's inferred SQL type. Only used to pick the right literal shape
/// (bare number, TRUE/FALSE, quoted string); the target column's own type still
/// has the final say when the value is written.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ColType {
    Int,
    Float,
    Bool,
    Date,
    DateTime,
    Text,
}

impl ColType {
    fn label(self) -> &'static str {
        match self {
            ColType::Int => "int",
            ColType::Float => "float",
            ColType::Bool => "bool",
            ColType::Date => "date",
            ColType::DateTime => "datetime",
            ColType::Text => "text",
        }
    }
}

/// One target table column resolved against the CSV header.
#[derive(Clone, Debug)]
struct ImportCol {
    /// Target column name.
    name: String,
    /// Index into the CSV row when the header matched a table column; `None`
    /// means the column is absent from the CSV and is written as NULL/default.
    src: Option<usize>,
    /// Type inferred from the CSV values (Text when the column is absent).
    ty: ColType,
    /// The table column's declared type, used to keep genuinely numeric values
    /// bare even when the CSV sample looked like text.
    data_type: String,
}

/// How an import treats rows already in the table.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ImportMode {
    /// Append to the existing rows.
    Append,
    /// `DELETE FROM` the table first, then insert (red-confirmed).
    Overwrite,
}

/// What to do when a row fails to insert.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ImportOnError {
    /// Stop at the first failure and report the row number (default).
    Stop,
    /// Skip the bad row and keep going, reporting every skipped row.
    Skip,
}

/// The parsed, decoded and header-aligned CSV, ready to preview and import.
#[derive(Clone)]
struct ImportPlan {
    path: PathBuf,
    file_size: u64,
    encoding: String,
    delimiter: char,
    /// CSV header names in file order.
    headers: Vec<String>,
    /// Every data row (raw field strings, empty = NULL).
    rows: Vec<Vec<String>>,
    /// Target table, database and schema.
    table: String,
    schema: String,
    db: String,
    /// Target columns with their CSV source index and inferred type.
    columns: Vec<ImportCol>,
    /// CSV headers that match no table column (blocks the import when non-empty).
    extra: Vec<String>,
    /// Table columns absent from the CSV (imported as NULL/default).
    missing: Vec<String>,
    mode: ImportMode,
    on_error: ImportOnError,
    /// A blocking problem (unreadable file, no data, header mismatch).
    error: Option<String>,
}

impl ImportPlan {
    /// Columns actually written by the INSERT.
    fn present(&self) -> Vec<&ImportCol> {
        self.columns.iter().filter(|c| c.src.is_some()).collect()
    }
}

/// The file-path step of the import flow (before the CSV is read).
struct ImportPrompt {
    input: TextArea<'static>,
    table: String,
    schema: String,
    db: String,
    /// Inline error from the previous attempt (file missing, bad header …).
    error: Option<String>,
}

/// The outcome of one import run, shown in the completion overlay.
#[derive(Clone)]
struct ImportReport {
    table: String,
    schema: String,
    mode: ImportMode,
    total: usize,
    inserted: usize,
    /// `(1-based data row, error)` for rows skipped in skip mode.
    skipped: Vec<(usize, String)>,
    /// Set when stop mode aborted: the failing row and its error.
    aborted: Option<(usize, String)>,
    elapsed_ms: u128,
}

impl ImportReport {
    fn ok(&self) -> bool {
        self.aborted.is_none()
    }
}

/// The import job handed to the backend task.
struct ImportJob {
    cfg: Box<ConnectionConfig>,
    db: String,
    schema: String,
    table: String,
    /// Present columns only (src is Some), in target order.
    columns: Vec<ImportCol>,
    rows: Vec<Vec<String>>,
    mode: ImportMode,
    on_error: ImportOnError,
}

// ─── export ──────────────────────────────────────────────────────────────────

/// A result-set export format offered by `Ctrl-Y`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ExportFormat {
    Csv,
    JsonArray,
    JsonNdjson,
    Markdown,
    Insert,
    InsertBatch,
}

impl ExportFormat {
    fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::JsonArray => "JSON",
            ExportFormat::JsonNdjson => "NDJSON",
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Insert => "INSERT",
            ExportFormat::InsertBatch => t("INSERT (批量)"),
        }
    }
    fn description(self) -> &'static str {
        match self {
            ExportFormat::Csv => t("逗号分隔，NULL 为空字段"),
            ExportFormat::JsonArray => t("JSON 数组，每个对象一行记录"),
            ExportFormat::JsonNdjson => t("每行一个 JSON 对象（NDJSON）"),
            ExportFormat::Markdown => t("Markdown 表格（| 转义）"),
            ExportFormat::Insert => t("每行一条 INSERT INTO 语句"),
            ExportFormat::InsertBatch => t("多行 VALUES 合并为一条 INSERT"),
        }
    }
}

/// All formats in overlay order.
const EXPORT_FORMATS: &[ExportFormat] = &[
    ExportFormat::Csv,
    ExportFormat::JsonArray,
    ExportFormat::JsonNdjson,
    ExportFormat::Markdown,
    ExportFormat::Insert,
    ExportFormat::InsertBatch,
];

/// A generated export waiting for the destination (clipboard or file) step.
struct ExportPending {
    format: ExportFormat,
    /// `(schema, table)` guessed for INSERT exports (`None` for the other
    /// formats).
    table: Option<(String, String)>,
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
    /// Set for Redis writes: Enter runs `cmd` via the Redis console instead of
    /// SQL, then reloads the key list / the named key's value.
    redis: Option<RedisConfirm>,
    /// Set for MongoDB document writes: Enter runs the matching insert / update /
    /// delete through the document driver, then reloads the current page.
    mongo: Option<MongoConfirm>,
}

/// A pending Redis write shown in the red confirmation layer.
#[derive(Clone)]
struct RedisConfirm {
    db: u32,
    /// Single command for a one-key write (SET / EXPIRE / RENAME / HSET / DEL).
    cmd: String,
    /// When non-empty, run these commands in order instead of `cmd`.
    batch: Vec<String>,
    /// Batch targets as `(raw, display)`, kept so a typed re-confirmation can
    /// regenerate / describe exactly what is about to change.
    batch_keys: Vec<(String, String)>,
    reload_value: Option<String>,
    reload_list: bool,
    /// When set, Enter on the red layer first opens a typed confirmation that
    /// must repeat this key count (or `YES`) before the batch runs.
    typed_confirm: Option<usize>,
    /// Human summary used by the typed confirmation prompt.
    summary: String,
}

/// A pending MongoDB document write shown in the red confirmation layer.
#[derive(Clone)]
struct MongoConfirm {
    db: String,
    collection: String,
    action: MongoAction,
}

/// The concrete MongoDB write behind a [`MongoConfirm`].
#[derive(Clone)]
enum MongoAction {
    Insert { doc_json: String },
    Update { id: String, doc_json: String },
    Delete { id: String },
}

/// The JSON editor dialog used to edit / insert a MongoDB document.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MongoDocMode {
    Edit,
    Insert,
}

#[derive(Clone)]
struct MongoDocDialog {
    mode: MongoDocMode,
    db: String,
    collection: String,
    /// The document as fetched (Extended-JSON-ish shape produced by the driver),
    /// used to detect `_id` tampering and to compute the save diff.
    original: serde_json::Value,
    /// `_id` argument for an update / delete (empty for insert).
    id: String,
    editor: TextArea<'static>,
    /// Last validation error, shown under the editor until the next save.
    error: Option<String>,
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
    schema: String,
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

// ─── Redis key browser ───────────────────────────────────────────────────────

/// Server-side SCAN state for the Redis key browser. Keys are appended page by
/// page (never a full `KEYS *`), and `cursor == 0` marks the end of the keyspace.
#[derive(Clone)]
struct RedisScanState {
    keys: Vec<RedisKeyInfo>,
    /// Cursor to resume from; 0 means the scan is exhausted.
    cursor: u64,
    exhausted: bool,
    /// `MATCH` pattern applied server-side (default `*`).
    pattern: String,
    /// `total_keys` hint reported by the server (DBSIZE-like).
    total: u64,
    /// Request generation, so a stale page cannot clobber a fresh scan.
    gen: u64,
    /// A page request is in flight. `n` (load more) is ignored while set, so two
    /// concurrent requests cannot both start from the same cursor and append the
    /// same page twice (a fresh scan resets `cursor` to 0, so pressing `n` before
    /// its reply used to duplicate the first page).
    pending: bool,
}

impl Default for RedisScanState {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            cursor: 0,
            exhausted: false,
            pattern: "*".to_string(),
            total: 0,
            gen: 0,
            pending: false,
        }
    }
}

/// A Redis key's value prepared for the results grid. The raw value is kept so a
/// grid row can be mapped back to its field / member for an edit or delete.
#[derive(Clone)]
struct RedisValueView {
    key_display: String,
    key_raw: String,
    redis_type: String,
    ttl: i64,
    grid: Grid,
    /// Grid row index → field / member / element id the row represents.
    row_keys: Vec<String>,
    /// Cursor for the next collection page, when the value is truncated.
    scan_cursor: Option<u64>,
    /// Raw value, used to prefill a string edit and to know the concrete type.
    raw: RedisValue,
}

/// Which Redis input dialog is open. Every write goes through the shared red
/// confirmation layer afterwards, so nothing mutates without a second Enter.
#[derive(Clone, Copy, PartialEq, Debug)]
enum RedisPromptKind {
    /// `MATCH` pattern for the key browser.
    Pattern,
    /// New TTL in seconds for the focused key.
    Ttl,
    /// New key name for a RENAME.
    Rename,
    /// New string body for the focused string key.
    StringValue,
    /// New value for a hash field.
    HashField,
    /// New TTL applied to a whole batch of selected keys.
    BatchTtl,
    /// `old=new` prefix replacement applied to a whole batch of selected keys.
    BatchRenamePrefix,
    /// Typed re-confirmation of a dangerous batch (repeat the key count / YES).
    BatchConfirm,
}

#[derive(Clone)]
struct RedisPrompt {
    kind: RedisPromptKind,
    title: String,
    /// Key in the form a redis-cli command needs (the decoded display name).
    key_display: String,
    /// Base64 raw key, used to reload the value after the write.
    key_raw: String,
    /// Hash field name, when the prompt edits a hash member.
    field: String,
    input: TextArea<'static>,
    /// Batch targets as `(raw, display)`, empty for single-key prompts.
    batch: Vec<(String, String)>,
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

    fn table(&self, db: &str, schema: &str, table: &str) -> Option<&TablePrefs> {
        self.tables
            .get(&(db.to_string(), table_pref_key(schema, table)))
    }

    /// Mutable access that records the entry as changed by this session.
    fn entry(&mut self, db: &str, schema: &str, table: &str) -> &mut TablePrefs {
        let key = (db.to_string(), table_pref_key(schema, table));
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
///
/// Real data can carry more than one such layer (a name written through a latin1
/// connection twice, or a latin1 dump imported into a latin1 connection). One
/// pass is not enough there: the intermediate string already contains non-Latin-1
/// characters (`•`, `™`) and is therefore mistaken for a successful decode. So
/// repeat the reversal until it stops changing (bounded, so a pathological input
/// can never loop) and peel every layer off.
fn fix_double_encoding(s: &str) -> String {
    let mut current = s.to_string();
    // Two layers is the realistic worst case; 4 leaves headroom and still
    // terminates immediately for clean names (first pass is a no-op).
    for _ in 0..4 {
        let next = reverse_double_encoding_once(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// One CP1252→UTF-8 reversal pass. Returns the input unchanged when the bytes
/// are not valid UTF-8 or the result carries no char above U+00FF (i.e. the
/// reversal did not reveal CJK, so it is assumed to have been a false positive).
fn reverse_double_encoding_once(s: &str) -> String {
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
    /// Enumerate the schemas of one database (PostgreSQL and other
    /// schema-aware engines).
    ListSchemas(Box<ConnectionConfig>, String),
    ListTables(Box<ConnectionConfig>, String, String, u64),
    Columns(Box<ConnectionConfig>, String, String, String),
    Ddl(Box<ConnectionConfig>, String, String, String),
    TableData(Box<TableDataReq>),
    TableColumns(Box<ConnectionConfig>, String, String, String),
    Query(Box<ConnectionConfig>, String, String, usize),
    Redis(Box<ConnectionConfig>, u32, String),
    /// Paginated `SCAN` of the key browser.
    RedisScan {
        cfg: Box<ConnectionConfig>,
        db: u32,
        cursor: u64,
        pattern: String,
        count: usize,
        gen: u64,
        /// true → replace the list (fresh scan); false → append the next page.
        append: bool,
    },
    /// Fetch one key's typed value for the detail pane.
    RedisValue {
        cfg: Box<ConnectionConfig>,
        db: u32,
        key_raw: String,
    },
    /// Execute a generated write command, then refresh the key list / value.
    RedisWrite {
        cfg: Box<ConnectionConfig>,
        db: u32,
        cmd: String,
        reload_value: Option<String>,
        reload_list: bool,
    },
    /// Execute a batch of generated Redis commands in order (multi-key DEL /
    /// EXPIRE / RENAME), then refresh the key list.
    RedisBatchWrite {
        cfg: Box<ConnectionConfig>,
        db: u32,
        cmds: Vec<String>,
        reload_list: bool,
    },
    /// Append the next page of a large Redis collection value.
    RedisMore {
        cfg: Box<ConnectionConfig>,
        db: u32,
        key_raw: String,
        key_type: String,
        cursor: u64,
        count: usize,
    },
    /// Paginated document browse for a MongoDB collection.
    MongoDocs {
        cfg: Box<ConnectionConfig>,
        db: String,
        collection: String,
        page: usize,
        page_size: usize,
        filter: String,
        gen: u64,
    },
    /// Collection indexes, shown as the Mongo analogue of a table structure.
    MongoIndexes {
        cfg: Box<ConnectionConfig>,
        db: String,
        collection: String,
    },
    /// Insert a new MongoDB document from the JSON editor.
    MongoInsert {
        cfg: Box<ConnectionConfig>,
        db: String,
        collection: String,
        doc_json: String,
    },
    /// Replace a MongoDB document (`_id` unchanged) from the JSON editor.
    MongoUpdate {
        cfg: Box<ConnectionConfig>,
        db: String,
        collection: String,
        id: String,
        doc_json: String,
    },
    /// Delete one MongoDB document by `_id`.
    MongoDelete {
        cfg: Box<ConnectionConfig>,
        db: String,
        collection: String,
        id: String,
    },
    Mongo(Box<ConnectionConfig>, String, String),
    History(Box<ConnectionConfig>),
    Snippets(Box<ConnectionConfig>),
    /// Save the editor's SQL into DBX's `saved_sql_files` (query favourites).
    SaveSnippet(Box<ConnectionConfig>, String, String),
    DatabasesRefresh(Box<ConnectionConfig>),
    AddConn(Box<ConnectionConfig>),
    /// Read, decode and header-align a CSV against a table's columns, producing
    /// the preview plan.
    ImportPlan {
        cfg: Box<ConnectionConfig>,
        db: String,
        schema: String,
        table: String,
        path: PathBuf,
        /// Request id; a stale plan (cancelled or superseded) is discarded.
        gen: u64,
    },
    /// Execute a prepared CSV import, reporting progress per chunk.
    Import(Box<ImportJob>),
}

impl Op {
    /// Watchdog for this call. SQL statements already carry a 60 s driver
    /// statement timeout; the ceiling here is only the last resort against a
    /// server that never answers, so it is generous for a (possibly
    /// multi-statement) script and tighter for calls that should be instant.
    fn watchdog(&self) -> Duration {
        match self {
            Op::Query(..) => OP_WATCHDOG_SQL,
            Op::Import(_) => OP_WATCHDOG_IMPORT,
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
    /// A table list plus the request id it answers, so a slow reply for a
    /// database / schema the user already left cannot overwrite the current one.
    TablesFor {
        tables: Vec<TableInfo>,
        gen: u64,
    },
    /// Schemas of `db`, in server order. A failed enumeration degrades to an
    /// empty list (plus `warning`) so the flat table list still loads.
    Schemas {
        db: String,
        schemas: Vec<String>,
        warning: Option<String>,
    },
    Columns {
        table: String,
        schema: String,
        columns: Vec<ColumnInfo>,
    },
    Ddl {
        table: String,
        schema: String,
        text: String,
    },
    TableData {
        grid: Box<Grid>,
        total: Option<u64>,
        has_next: bool,
        page: usize,
        table: String,
        schema: String,
        table_type: Option<String>,
        filter: String,
        order_by: Option<String>,
        gen: u64,
    },
    TableColumns {
        table: String,
        schema: String,
        columns: Vec<ColumnInfo>,
    },
    Query(Box<dbx_core::db::QueryResult>, String, usize),
    Script(Vec<StmtOutcome>),
    Redis(String),
    Mongo(String),
    RedisKeys {
        keys: Vec<RedisKeyInfo>,
        cursor: u64,
        total: u64,
        gen: u64,
        append: bool,
    },
    RedisValue(Box<RedisValueView>),
    RedisWritten {
        cmd: String,
        summary: String,
        reload_value: Option<String>,
        reload_list: bool,
    },
    RedisBatchWritten {
        executed: usize,
        total: usize,
        first_error: Option<String>,
        reload_list: bool,
    },
    RedisMore {
        rows: Vec<Vec<Val>>,
        row_keys: Vec<String>,
        cursor: Option<u64>,
    },
    MongoDocs {
        grid: Box<Grid>,
        total: u64,
        has_next: bool,
        page: usize,
        collection: String,
        filter: String,
        gen: u64,
        /// The page's raw documents, retained for edit / delete mapping.
        docs: Vec<serde_json::Value>,
    },
    MongoIndexes {
        collection: String,
        grid: Box<Grid>,
    },
    /// A MongoDB document write finished; the summary is shown and the current
    /// page is reloaded.
    MongoWritten {
        summary: String,
    },
    History(Vec<String>),
    Snippets(Vec<(String, String)>),
    SnippetSaved(String),
    DatabasesRefresh(Vec<String>),
    Added(String),
    /// A CSV preview plan (may carry a content error the preview displays).
    ImportPlan { gen: u64, plan: Box<ImportPlan> },
    /// The plan could not be built (unreadable file, no columns): routed back to
    /// the path prompt.
    ImportFailed { gen: u64, msg: String },
    /// Chunk progress; does not count as the op finishing.
    ImportProgress { done: usize, total: usize },
    ImportDone(Box<ImportReport>),
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

/// The schema to hand the DDL generator. PostgreSQL's renderer always writes a
/// `schema.table` name, so an empty schema yields the unusable `""."table"`;
/// resolve the relation's visible schema (the one the sidebar browsed) first.
/// Other engines treat the database as the schema and stay empty.
async fn resolve_ddl_schema(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    table: &str,
) -> String {
    if !is_postgres_family(cfg.db_type.as_str()) {
        return String::new();
    }
    let sql = format!(
        "SELECT n.nspname FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relname = '{}' AND pg_catalog.pg_table_is_visible(c.oid) LIMIT 1",
        table.replace('\'', "''")
    );
    match backend.execute_query(cfg, db, &sql, Some(1), Some(10)).await {
        Ok(r) => r
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

async fn run_op(backend: &LocalBackend, op: Op, tx: &Tx) -> OpResult {
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
                        tf("无法列举数据库（{}），仅使用配置库 {}", &[&(e), &(fix_double_encoding(db))])
                    }
                    _ => tf("无法列举数据库（{}），将使用连接默认库", &[&(e)]),
                }),
            },
        },
        Op::ListSchemas(cfg, db) => {
            // `list_schemas_core` is the kernel's schema enumerator; it hides
            // system schemas unless the connection opts in (`show_system_schemas`).
            match dbx_core::schema::list_schemas_core(backend.state().as_ref(), &cfg.id, &db).await {
                Ok(schemas) => OpResult::Schemas {
                    db,
                    schemas,
                    warning: None,
                },
                // Not fatal: an engine whose schema list is unavailable (or
                // denied) still browses its default namespace.
                Err(e) => OpResult::Schemas {
                    db,
                    schemas: Vec::new(),
                    warning: Some(tf("无法列举 schema（{}），按默认命名空间浏览", &[&(e)])),
                },
            }
        }
        Op::ListTables(cfg, db, schema, gen) => match backend.list_tables(&cfg, &db, &schema).await {
            Ok(t) => OpResult::TablesFor { tables: t, gen },
            Err(e) => OpResult::Error(format!("list tables: {e}")),
        },
        Op::Columns(cfg, db, schema, table) => match backend.get_columns(&cfg, &db, &schema, &table).await {
            Ok(c) => OpResult::Columns {
                table,
                schema,
                columns: c,
            },
            Err(e) => OpResult::Error(format!("columns: {e}")),
        },
        Op::Ddl(cfg, db, schema, table) => {
            // PostgreSQL renders `"schema"."table"`; when the schema layer did
            // not produce one (a failed `list_schemas`, an engine we do not
            // browse schemas for) resolve the relation's visible schema first,
            // because an empty schema yields the unusable `""."table"`. The
            // *requested* schema is what the reply carries back, so the result
            // guard keeps working even when the fallback resolved a different
            // name.
            let effective = if schema.trim().is_empty() && is_postgres_family(cfg.db_type.as_str()) {
                resolve_ddl_schema(backend, &cfg, &db, &table).await
            } else {
                schema.clone()
            };
            match dbx_core::schema::get_table_ddl_core(
                backend.state().as_ref(),
                &cfg.id,
                &db,
                &effective,
                &table,
                None,
            )
            .await
            {
                Ok(ddl) => OpResult::Ddl {
                    table,
                    schema,
                    text: ddl,
                },
                Err(e) => OpResult::Ddl {
                    table,
                    schema,
                    text: tf("-- 无法获取 DDL: {}", &[&(e)]),
                },
            }
        }
        Op::TableData(req) => {
            let TableDataReq {
                cfg,
                db,
                schema,
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
                schema: (!schema.trim().is_empty()).then(|| schema.clone()),
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
                            let base = build_count_table_sql(
                                Some(cfg.db_type),
                                (!schema.trim().is_empty()).then_some(schema.as_str()),
                                &table,
                            );
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
                        schema,
                        table_type,
                        filter,
                        order_by,
                        gen,
                    }
                }
                Err(e) => OpResult::Error(format!("table data: {e}")),
            }
        }
        Op::TableColumns(cfg, db, schema, table) => {
            match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(columns) => OpResult::TableColumns {
                    table,
                    schema,
                    columns,
                },
                Err(e) => OpResult::Error(format!("table columns: {e}")),
            }
        }
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
        Op::RedisScan {
            cfg,
            db,
            cursor,
            pattern,
            count,
            gen,
            append,
        } => match dbx_core::redis_ops::redis_scan_keys_batch_core(
            backend.state().as_ref(),
            &cfg.id,
            db,
            cursor,
            &pattern,
            count,
            REDIS_SCAN_ITERATIONS,
            true,
        )
        .await
        {
            Ok(r) => OpResult::RedisKeys {
                keys: r.keys,
                cursor: r.cursor,
                total: r.total_keys,
                gen,
                append,
            },
            Err(e) => OpResult::Error(format!("redis scan: {e}")),
        },
        Op::RedisValue { cfg, db, key_raw } => {
            match dbx_core::redis_ops::redis_get_value_in_db_core(
                backend.state().as_ref(),
                &cfg.id,
                db,
                &key_raw,
            )
            .await
            {
                Ok(v) => OpResult::RedisValue(Box::new(redis_value_view(v))),
                Err(e) => OpResult::Error(format!("redis value: {e}")),
            }
        }
        Op::RedisWrite {
            cfg,
            db,
            cmd,
            reload_value,
            reload_list,
        } => match backend.execute_redis_command(&cfg, db, &cmd, true).await {
            Ok(r) => {
                let summary = serde_json::to_string(&r.value).unwrap_or_else(|_| format!("{:?}", r.value));
                OpResult::RedisWritten {
                    cmd,
                    summary: truncate_disp(&one_line(&summary), 160),
                    reload_value,
                    reload_list,
                }
            }
            Err(e) => OpResult::Error(format!("redis: {e}")),
        },
        Op::RedisBatchWrite {
            cfg,
            db,
            cmds,
            reload_list,
        } => {
            let total = cmds.len();
            let mut executed = 0usize;
            let mut first_error: Option<String> = None;
            for cmd in &cmds {
                match backend.execute_redis_command(&cfg, db, cmd, true).await {
                    Ok(_) => executed += 1,
                    Err(e) => {
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                }
            }
            OpResult::RedisBatchWritten {
                executed,
                total,
                first_error,
                reload_list,
            }
        },
        Op::RedisMore {
            cfg,
            db,
            key_raw,
            key_type,
            cursor,
            count,
        } => {
            match dbx_core::redis_ops::redis_load_more_in_db_core(
                backend.state().as_ref(),
                &cfg.id,
                db,
                &key_raw,
                &key_type,
                cursor,
                count,
                None,
                None,
            )
            .await
            {
                Ok(page) => {
                    let (rows, row_keys, cursor) = redis_collection_page_rows(&page);
                    OpResult::RedisMore {
                        rows,
                        row_keys,
                        cursor,
                    }
                }
                Err(e) => OpResult::Error(format!("redis load more: {e}")),
            }
        }
        Op::MongoDocs {
            cfg,
            db,
            collection,
            page,
            page_size,
            filter,
            gen,
        } => {
            let skip = (page * page_size) as u64;
            // Fetch one extra document to detect whether a next page exists.
            let limit = page_size as i64 + 1;
            let filter = if filter.trim().is_empty() {
                None
            } else {
                Some(filter.trim())
            };
            match dbx_core::mongo_ops::mongo_find_documents_core(
                backend.state().as_ref(),
                &cfg.id,
                &db,
                &collection,
                skip,
                limit,
                filter,
                None,
                None,
                None,
            )
            .await
            {
                Ok(r) => {
                    let total = r.total;
                    let mut docs = r.documents;
                    let has_next = docs.len() > page_size;
                    docs.truncate(page_size);
                    let grid = mongo_docs_grid(&docs);
                    OpResult::MongoDocs {
                        grid: Box::new(grid),
                        total,
                        has_next,
                        page,
                        collection,
                        filter: filter.unwrap_or("").to_string(),
                        gen,
                        docs,
                    }
                }
                Err(e) => OpResult::Error(format!("mongo docs: {e}")),
            }
        }
        Op::MongoIndexes { cfg, db, collection } => {
            match dbx_core::mongo_ops::mongo_list_index_specs_core(
                backend.state().as_ref(),
                &cfg.id,
                &db,
                &collection,
            )
            .await
            {
                Ok(specs) => {
                    let qr = dbx_core::mongo_ops::mongo_indexes_query_result(specs, 500);
                    let grid = Grid::from_query(qr.columns, &qr.rows, tf("{} 个索引", &[&(qr.rows.len())]));
                    OpResult::MongoIndexes {
                        collection,
                        grid: Box::new(grid),
                    }
                }
                Err(e) => OpResult::Error(format!("mongo indexes: {e}")),
            }
        }
        Op::MongoInsert {
            cfg,
            db,
            collection,
            doc_json,
        } => match dbx_core::mongo_ops::mongo_insert_document_core(
            backend.state().as_ref(),
            &cfg.id,
            &db,
            &collection,
            &doc_json,
        )
        .await
        {
            Ok(id) => OpResult::MongoWritten {
                summary: tf("已插入文档 _id={}", &[&id]),
            },
            Err(e) => OpResult::Error(format!("mongo insert: {e}")),
        },
        Op::MongoUpdate {
            cfg,
            db,
            collection,
            id,
            doc_json,
        } => match dbx_core::mongo_ops::mongo_update_document_core(
            backend.state().as_ref(),
            &cfg.id,
            &db,
            &collection,
            &id,
            &doc_json,
            None,
        )
        .await
        {
            Ok(n) => OpResult::MongoWritten {
                summary: tf("已更新文档 _id={}（{} 处修改）", &[&id, &n]),
            },
            Err(e) => OpResult::Error(format!("mongo update: {e}")),
        },
        Op::MongoDelete {
            cfg,
            db,
            collection,
            id,
        } => match dbx_core::mongo_ops::mongo_delete_document_core(
            backend.state().as_ref(),
            &cfg.id,
            &db,
            &collection,
            &id,
            None,
        )
        .await
        {
            Ok(n) => OpResult::MongoWritten {
                summary: tf("已删除文档 _id={}（{} 行）", &[&id, &n]),
            },
            Err(e) => OpResult::Error(format!("mongo delete: {e}")),
        },
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
        Op::ImportPlan { cfg, db, schema, table, path, gen } => {
            let expanded = expand_home(&path.to_string_lossy());
            let bytes = match std::fs::read(&expanded) {
                Ok(b) => b,
                Err(e) => return OpResult::ImportFailed { gen, msg: tf("读取文件失败: {}", &[&(e)]) },
            };
            let (text, encoding) = decode_csv_bytes(&bytes);
            let delimiter = detect_delimiter(&text);
            let mut rows = parse_csv(&text, delimiter);
            if rows.is_empty() {
                return OpResult::ImportFailed { gen, msg: t("CSV 为空或无法解析").into() };
            }
            let headers = rows.remove(0);
            if rows.is_empty() {
                return OpResult::ImportFailed { gen, msg: t("CSV 没有数据行").into() };
            }
            let table_columns = match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(c) => c,
                Err(e) => return OpResult::ImportFailed { gen, msg: tf("读取表结构失败: {}", &[&(e)]) },
            };
            let infer_rows: Vec<Vec<String>> = rows.iter().take(IMPORT_INFER_SAMPLE).cloned().collect();
            let (columns, extra, missing) = align_import_columns(&headers, &infer_rows, &table_columns);
            let error = if table_columns.is_empty() {
                Some(t("目标表没有可对齐的列").to_string())
            } else if extra.is_empty() && columns.iter().all(|c| c.src.is_none()) {
                Some(t("CSV 表头与表列不匹配（无任何列名对应）").to_string())
            } else if !extra.is_empty() {
                Some(tf("CSV 有 {} 个多余列无法对齐（{}）", &[&extra.len(), &extra.join(", ")]))
            } else {
                None
            };
            OpResult::ImportPlan {
                gen,
                plan: Box::new(ImportPlan {
                    path: expanded,
                    file_size: bytes.len() as u64,
                    encoding,
                    delimiter,
                    headers,
                    rows,
                    table,
                    schema,
                    db,
                    columns,
                    extra,
                    missing,
                    mode: ImportMode::Append,
                    on_error: ImportOnError::Stop,
                    error,
                }),
            }
        }
        Op::Import(job) => {
            let ImportJob {
                cfg,
                db,
                schema,
                table,
                columns,
                rows,
                mode,
                on_error,
            } = *job;
            let total = rows.len();
            let start = Instant::now();
            // Overwrite clears the table first. The DELETE is not part of the
            // insert chunks, so a later failure leaves an empty (or partially
            // filled) table — that is what an overwrite means.
            if mode == ImportMode::Overwrite {
                let del = format!("DELETE FROM {};", table_ref(cfg.db_type, &schema, &table));
                if let Err(e) = backend.execute_query(&cfg, &db, &del, Some(1), Some(60)).await {
                    return OpResult::ImportDone(Box::new(ImportReport {
                        table,
                        schema,
                        mode,
                        total,
                        inserted: 0,
                        skipped: Vec::new(),
                        aborted: Some((0, tf("清空表失败: {}", &[&(e)]))),
                        elapsed_ms: start.elapsed().as_millis(),
                    }));
                }
            }
            let mut inserted = 0usize;
            let mut skipped: Vec<(usize, String)> = Vec::new();
            let mut aborted: Option<(usize, String)> = None;
            for (ci, chunk) in import_chunks(&rows).into_iter().enumerate() {
                let base = ci * IMPORT_CHUNK;
                let script = chunk
                    .iter()
                    .map(|row| import_insert_sql(&cfg, &schema, &table, &columns, row))
                    .collect::<Vec<_>>()
                    .join("\n");
                match on_error {
                    // Stop mode: one transaction per chunk. A failure rolls the
                    // chunk back and the backend names the failing statement.
                    ImportOnError::Stop => {
                        let options = QueryExecutionOptions {
                            max_rows: Some(1),
                            timeout_secs: Some(60),
                            use_transaction: Some(true),
                            ..Default::default()
                        };
                        match backend.execute_batch(&cfg, &db, None, &script, options).await {
                            Ok(_) => inserted += chunk.len(),
                            Err(e) => {
                                aborted = Some((import_row_of_error(base, &e, chunk.len()), e));
                                break;
                            }
                        }
                    }
                    // Skip mode: auto-commit with per-statement results, so the
                    // exact failing rows can be reported and skipped.
                    ImportOnError::Skip => {
                        let options = QueryExecutionOptions {
                            max_rows: Some(1),
                            timeout_secs: Some(60),
                            continue_on_error: true,
                            ..Default::default()
                        };
                        match backend.execute_batch(&cfg, &db, None, &script, options).await {
                            Ok(results) => {
                                let mut bad = 0usize;
                                for r in &results {
                                    if r.execution_error {
                                        bad += 1;
                                        if let Some(i) = r.statement_index {
                                            skipped.push((
                                                base + i + 1,
                                                r.error_message
                                                    .clone()
                                                    .unwrap_or_else(|| "unknown error".to_string()),
                                            ));
                                        }
                                    }
                                }
                                inserted += results.len().saturating_sub(bad);
                                if results.len() < chunk.len() {
                                    aborted = Some((
                                        base + results.len() + 1,
                                        t("批量在中途停止（连接或会话错误）").to_string(),
                                    ));
                                    break;
                                }
                            }
                            Err(e) => {
                                aborted = Some((base + 1, tf("批量执行失败: {}", &[&(e)])));
                                break;
                            }
                        }
                    }
                }
                let _ = tx.send(OpResult::ImportProgress {
                    done: (base + chunk.len()).min(total),
                    total,
                });
            }
            OpResult::ImportDone(Box::new(ImportReport {
                table,
                schema,
                mode,
                total,
                inserted,
                skipped,
                aborted,
                elapsed_ms: start.elapsed().as_millis(),
            }))
        }
    }
}

fn spawn_op(backend: &Arc<LocalBackend>, tx: &Tx, op: Op) {
    let backend = backend.clone();
    let tx = tx.clone();
    let limit = op.watchdog();
    tokio::spawn(async move {
        // The watchdog is the last resort: a server that accepts the socket but
        // never answers must surface an error, not a spinner that never stops.
        let res = match tokio::time::timeout(limit, run_op(&backend, op, &tx)).await {
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
    /// Schemas of the current database (empty for engines without a schema
    /// layer, e.g. MySQL). Fetched once per database.
    schemas: Vec<String>,
    /// Currently browsed schema; empty when the engine has no schema layer.
    schema: String,
    /// Which database `schemas` belongs to, so a database switch refetches.
    schemas_db: String,
    /// Monotonic id of the latest table-list request; a stale reply is discarded.
    tables_gen: u64,

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
    /// The five most recently browsed `(database, schema, table)` triples.
    recent_tables: Vec<(String, String, String)>,
    recent_open: bool,
    recent_list: ListState,
    /// A `(schema, table)` to open as soon as the (new) table list arrives.
    pending_open_table: Option<(String, String)>,
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
    // ── Redis key browser ──
    redis_scan: RedisScanState,
    redis_list: ListState,
    redis_value: Option<RedisValueView>,
    redis_prompt: Option<RedisPrompt>,
    /// Raw keys multi-selected in the browser (space / shift-range / a).
    redis_selected: HashSet<String>,
    /// Anchor index for a shift range selection.
    redis_anchor: Option<usize>,
    /// A batch confirm awaiting its typed re-confirmation.
    redis_pending_batch: Option<RedisConfirm>,
    // ── MongoDB document browser ──
    mongo_page: usize,
    mongo_filter: String,
    mongo_gen: u64,
    /// Documents of the current page, so `e` / `Del` can map a grid row back to
    /// its source document.
    mongo_docs: Vec<serde_json::Value>,
    /// The JSON editor dialog for a MongoDB document edit / insert.
    mongo_dialog: Option<MongoDocDialog>,

    form: ConnForm,

    // ── CSV import ──
    /// The file-path step, before the CSV is read.
    import_prompt: Option<ImportPrompt>,
    /// The parsed preview / confirmation layer.
    import_plan: Option<Box<ImportPlan>>,
    /// Monotonic id of the latest plan request; a stale reply is discarded.
    import_gen: u64,
    /// Preview scroll offset.
    import_scroll: u16,
    /// `(done, total)` while an import runs.
    import_progress: Option<(usize, usize)>,
    /// The completion overlay.
    import_report: Option<Box<ImportReport>>,

    // ── result export (Ctrl-Y) ──
    /// The format picker.
    export_open: bool,
    export_list: ListState,
    /// The chosen format, waiting for a destination.
    export_pending: Option<ExportPending>,
    /// The destination prompt (blank = clipboard, else a file path).
    export_path: Option<TextArea<'static>>,

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

impl App {
    /// Build the initial application state. Split out of `run_app` so the
    /// render/key layers can be exercised headlessly in tests.
    fn new(
        backend: Arc<LocalBackend>,
        config: TuiConfig,
        config_path: Option<PathBuf>,
        mouse_debug: bool,
        trace_path: Option<PathBuf>,
        drag_pan: DragPan,
    ) -> Self {
        let config_compact = config.compact;
        let mut app = Self {
            backend,
            page: Page::Browse,
            focus: Focus::Sidebar,
            quit: false,
            connections: Vec::new(),
            conn_list: ListState::default(),
            picker_open: true,
            selected: None,
            databases: Vec::new(),
            db_index: 0,
            schemas: Vec::new(),
            schema: String::new(),
            schemas_db: String::new(),
            tables_gen: 0,
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
            drag_pan,
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
            redis_scan: RedisScanState::default(),
            redis_list: ListState::default(),
            redis_value: None,
            redis_prompt: None,
            redis_selected: HashSet::new(),
            redis_anchor: None,
            redis_pending_batch: None,
            mongo_page: 0,
            mongo_filter: String::new(),
            mongo_gen: 0,
            mongo_docs: Vec::new(),
            mongo_dialog: None,
            form: ConnForm::default(),
            import_prompt: None,
            import_plan: None,
            import_gen: 0,
            import_scroll: 0,
            import_progress: None,
            import_report: None,
            export_open: false,
            export_list: ListState::default(),
            export_pending: None,
            export_path: None,
            layout_mode: LayoutMode::Mid,
            term_h: 0,
            rects: Rects::default(),
        };
        app.editor.set_placeholder_text(t("SQL … (Ctrl-J / F5 执行 · ↑ 历史)"));
        app.set_placeholder();
        app
    }
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

    let mut app = App::new(
        backend.clone(),
        config,
        config_path,
        mouse_debug,
        trace_path,
        DragPan::from_env(),
    );

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
    // Chunk progress is an intermediate message: update the readout and leave
    // the op (and the spinner) in flight.
    if let OpResult::ImportProgress { done, total } = res {
        app.import_progress = Some((done, total));
        app.status = tf("导入 {} / {} 行…", &[&done, &total]);
        return;
    }
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
            // A fresh database list invalidates the cached schema list.
            app.schemas.clear();
            app.schemas_db.clear();
            app.schema.clear();
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
                if app.backend_kind == Backend::Redis {
                    app.status = tf("加载 {} keys…", &[&(cfg.name)]);
                    start_redis_scan(app, tx, true);
                    app.spawn(tx, Op::History(Box::new(cfg)));
                } else {
                    app.status = if db.is_empty() {
                        tf("加载 {} 表…", &[&(cfg.name)])
                    } else {
                        tf("加载 {} 表…", &[&(fix_double_encoding(&db))])
                    };
                    spawn_table_list(app, tx);
                    app.spawn(tx, Op::History(Box::new(cfg)));
                }
            }
            // A failed `list_databases` is not fatal (the configured database is
            // still used) but it must not be swallowed either.
            if let Some(w) = warning {
                app.status = format!("⚠ {w}");
            }
        }
        OpResult::Schemas { db, schemas, warning } => {
            // A reply for a database the user already left must not resurrect a
            // stale schema list.
            if db != app.current_db() {
                return;
            }
            app.schemas = schemas;
            app.schemas_db = db.clone();
            // Keep the current schema across a refresh when it still exists;
            // otherwise fall back to `public` (PostgreSQL's default) or the
            // first schema the server listed.
            if !app.schemas.contains(&app.schema) {
                app.schema = default_schema(&app.schemas);
            }
            if let Some(cfg) = app.selected.clone() {
                let schema = app.schema.clone();
                spawn_list_tables(app, tx, Box::new(cfg), db, schema);
            }
            if let Some(w) = warning {
                app.status = format!("⚠ {w}");
            }
        }
        OpResult::TablesFor { tables: ts, gen } => {
            // A slow list for a database / schema the user already left must not
            // clobber the current one (`public.orders` ≠ `inv.orders`).
            if gen != app.tables_gen {
                return;
            }
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
            // A recent-table jump that had to switch database / schema first:
            // open the requested table now that the list has arrived.
            if let Some((schema, name)) = app.pending_open_table.take() {
                if let Some(pos) = app.tables.iter().position(|t| t.name == name) {
                    app.table_list.select(Some(pos));
                    if app.schema != schema {
                        app.schema = schema;
                    }
                    open_table_data(app, tx);
                    return;
                }
                app.status = tf("✗ 未找到表 {}", &[&(fix_double_encoding(&name))]);
                return;
            }
            app.status = if app.table_filter.is_empty() {
                tf("{} 个表/视图 · Enter 数据 · r 结构 · / 过滤 · Tab 编辑SQL", &[&(n)])
            } else {
                tf("过滤「{}」· {}/{} 个表 · Esc 清除", &[&(app.table_filter), &(app.tables.len()), &(n)])
            };
        }
        OpResult::Columns { table, schema, columns: cols } => {
            // Ignore a late result for a table the user has already navigated away from.
            if app.selected_table().map(|t| t.name.clone()).as_deref() != Some(table.as_str())
                || app.schema != schema
            {
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
            app.status = tf("{} 结构 · {} 字段 · t 切换 DDL · Esc 返回", &[&(fix_double_encoding(&table)), &(n)]);
        }
        OpResult::Ddl { table, schema, text } => {
            if app.selected_table().map(|t| t.name.clone()).as_deref() == Some(table.as_str())
                && app.schema == schema
            {
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
            schema,
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
                    .insert(count_cache_key(&app.current_db(), &schema, &table, &filter), t);
            }
            app.grid_kind = GridKind::TableData;
            app.set_grid(*grid);
            // The status line below needs the qualified name after `schema` has
            // moved into `PageState`.
            let schema_label = qualified_display(&schema, &table);
            app.page_state = Some(PageState {
                table: table.clone(),
                schema,
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
            app.status = tf("{}.{} · 第 {} 页 · {} 行 · {}{}", &[&(fix_double_encoding(&app.current_db())), &(fix_double_encoding(&schema_label)), &(page + 1), &(rows), &(total_txt), &(extra)]);
            if let Some(msg) = app.pending_write_msg.take() {
                app.status = tf("{} · 已刷新（第 {} 页）", &[&(msg), &(page + 1)]);
            }
        }
        OpResult::TableColumns { table, schema, columns } => {
            // Only keep metadata that belongs to the table on screen (same
            // database *and* schema: `public.orders` ≠ `inv.orders`).
            let active = app
                .page_state
                .as_ref()
                .is_some_and(|p| p.table == table && p.schema == schema)
                || (app.selected_table().map(|t| t.name.as_str()) == Some(table.as_str())
                    && app.schema == schema);
            if active {
                app.table_meta = Some(TableMeta {
                    table,
                    schema,
                    columns,
                });
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
        OpResult::RedisKeys {
            keys,
            cursor,
            total,
            gen,
            append,
        } => {
            // Drop a reply that belongs to an older scan (pattern / db changed).
            if gen != app.redis_scan.gen {
                return;
            }
            app.redis_scan.pending = false;
            if append {
                app.redis_scan.keys.extend(keys);
            } else {
                app.redis_scan.keys = keys;
            }
            app.redis_scan.cursor = cursor;
            app.redis_scan.total = total;
            app.redis_scan.exhausted = cursor == 0;
            // Drop selections whose key is no longer in the loaded window (a
            // rescan can remove keys); keep them across a load-more append.
            if !app.redis_selected.is_empty() {
                let present: HashSet<String> =
                    app.redis_scan.keys.iter().map(|k| k.key_raw.clone()).collect();
                app.redis_selected.retain(|k| present.contains(k));
                if app.redis_selected.is_empty() {
                    app.redis_anchor = None;
                }
            }
            let n = app.redis_scan.keys.len();
            let sel = app.redis_list.selected().unwrap_or(0).min(n.saturating_sub(1));
            app.redis_list.select((n > 0).then_some(sel));
            app.status = if app.redis_scan.exhausted {
                tf("{} 个 key · 已全部加载", &[&(n)])
            } else {
                tf("{} 个 key · 已加载 {} · n 加载更多", &[&(total), &(n)])
            };
        }
        OpResult::RedisValue(view) => {
            let view = *view;
            app.col_hidden.clear();
            app.result_needle.clear();
            app.result_filter = None;
            app.result_tabs.clear();
            app.result_tab = 0;
            app.grid_kind = GridKind::RedisValue;
            app.set_grid(view.grid.clone());
            app.sel = 0;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.page_state = None;
            app.script = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.redis_value = Some(view.clone());
            app.focus = Focus::Preview;
            let ttl = redis_ttl_label(view.ttl);
            app.status = tf(
                "{} · {} · TTL {}",
                &[&(fix_double_encoding(&view.key_display)), &(view.redis_type), &(ttl)],
            );
        }
        OpResult::RedisWritten {
            cmd,
            summary,
            reload_value,
            reload_list,
        } => {
            app.cmd_output.push(format!("redis[{}]> {cmd}",  app.redis_db));
            app.cmd_output.push(summary.clone());
            trim_output(&mut app.cmd_output);
            app.status = tf("✓ {}", &[&(truncate_disp(&one_line(&cmd), 60))]);
            if reload_list {
                // The open value may no longer exist (DEL / RENAME), so drop it
                // and show the refreshed key list instead of a stale value.
                if reload_value.is_none() {
                    app.redis_value = None;
                    app.clear_grid();
                    // The value pane is gone; hand focus back to the key list.
                    app.focus = Focus::Sidebar;
                }
                start_redis_scan(app, tx, true);
            }
            if let Some(key) = reload_value {
                if let Some(cfg) = app.selected.clone() {
                    app.spawn(
                        tx,
                        Op::RedisValue {
                            cfg: Box::new(cfg),
                            db: app.redis_db,
                            key_raw: key,
                        },
                    );
                }
            }
        }
        OpResult::RedisBatchWritten {
            executed,
            total,
            first_error,
            reload_list,
        } => {
            // A batch invalidates the selection: keys may be gone or renamed.
            app.redis_selected.clear();
            app.redis_anchor = None;
            app.redis_pending_batch = None;
            app.status = match first_error {
                Some(e) => tf("⚠ 批量完成 {}/{}：{}", &[&executed, &total, &truncate_disp(&one_line(&e), 60)]),
                None => tf("✓ 批量完成 {}/{} 条命令", &[&executed, &total]),
            };
            if reload_list {
                app.redis_value = None;
                app.clear_grid();
                app.focus = Focus::Sidebar;
                start_redis_scan(app, tx, true);
            }
        }
        OpResult::RedisMore {
            rows,
            row_keys,
            cursor,
        } => {
            let mut n = 0;
            if let Some(view) = app.redis_value.as_mut() {
                view.grid.rows.extend(rows);
                view.row_keys.extend(row_keys);
                view.scan_cursor = cursor;
                n = view.grid.rows.len();
                view.grid.note = if cursor.is_some() {
                    tf("已加载 {} 项", &[&(n)]) + t(" · 更多（n 加载）")
                } else {
                    tf("已加载 {} 项 · 全部", &[&(n)])
                };
            }
            if let Some(grid) = app.redis_value.as_ref().map(|v| v.grid.clone()) {
                app.set_grid(grid);
            }
            app.status = if cursor.is_some() {
                tf("已加载 {} 项 · n 继续", &[&(n)])
            } else {
                tf("已加载 {} 项 · 全部", &[&(n)])
            };
        }
        OpResult::MongoDocs {
            grid,
            total,
            has_next,
            page,
            collection,
            filter,
            gen,
            docs,
        } => {
            if gen != app.mongo_gen {
                return;
            }
            app.col_hidden.clear();
            app.result_needle.clear();
            app.result_filter = None;
            app.result_tabs.clear();
            app.result_tab = 0;
            app.grid_kind = GridKind::MongoDocs;
            let rows = grid.rows.len();
            app.set_grid(*grid);
            app.mongo_page = page;
            app.mongo_filter = filter.clone();
            app.mongo_docs = docs;
            app.page_state = Some(PageState {
                table: collection.clone(),
                schema: String::new(),
                table_type: None,
                page,
                page_size: MONGO_PAGE,
                total: Some(total),
                has_next,
                filter,
                order_by: None,
            });
            app.sel = app
                .pending_sel
                .take()
                .map(|s| s.min(rows.saturating_sub(1)))
                .unwrap_or(0);
            app.script = None;
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.focus = Focus::Preview;
            app.status = tf(
                "{}.{} · 第 {} 页 · {} 个文档 · 共 {} · n/p 翻页 · f 过滤",
                &[&(fix_double_encoding(&app.current_db())), &(fix_double_encoding(&collection)), &(page + 1), &(rows), &(total)],
            );
        }
        OpResult::MongoIndexes { collection, grid } => {
            let n = grid.rows.len();
            app.grid_kind = GridKind::Columns;
            app.set_grid(*grid);
            app.struct_view = StructView::Fields;
            app.page_state = None;
            app.script = None;
            app.sel = 0;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.focus = Focus::Preview;
            app.status = tf("{} 索引 · {} · Esc 返回", &[&(fix_double_encoding(&collection)), &(n)]);
        }
        OpResult::MongoWritten { summary } => {
            app.status = format!("✓ {summary}");
            app.mongo_dialog = None;
            let page = app.mongo_page;
            reload_mongo_docs(app, tx, page);
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
        OpResult::ImportPlan { gen, plan } => {
            if gen != app.import_gen {
                return;
            }
            app.import_prompt = None;
            app.import_scroll = 0;
            app.status = if plan.error.is_some() {
                t("CSV 预览：存在错误，无法导入").into()
            } else {
                tf(
                    "CSV 预览：{} 行 → {}.{} · Enter 导入",
                    &[
                        &plan.rows.len(),
                        &plan.db,
                        &qualified_display(&plan.schema, &plan.table),
                    ],
                )
            };
            app.import_plan = Some(plan);
        }
        OpResult::ImportFailed { gen, msg } => {
            if gen != app.import_gen {
                return;
            }
            app.status = format!("✗ {msg}");
            if let Some(p) = app.import_prompt.as_mut() {
                p.error = Some(msg);
            }
        }
        OpResult::ImportDone(rep) => {
            let table = rep.table.clone();
            let schema = rep.schema.clone();
            app.import_progress = None;
            // An import writes rows, so every cached COUNT(*) is now suspect —
            // including the target table's own entry when the import came from
            // the sidebar while a different table was open. Reloading the
            // browsed table refills the entry it needs; the rest are dropped so
            // a later open re-runs the COUNT instead of trusting a stale total.
            app.count_cache.clear();
            app.status = if rep.ok() {
                tf(
                    "✓ 导入完成 · 成功 {} 行 · 跳过 {} 行 · {}ms",
                    &[&rep.inserted, &rep.skipped.len(), &rep.elapsed_ms],
                )
            } else {
                let (row, err) = rep
                    .aborted
                    .as_ref()
                    .map(|(r, e)| (*r, e.clone()))
                    .unwrap_or((0, String::new()));
                tf("✗ 导入中止于第 {} 行: {}", &[&row, &err])
            };
            // Refresh the browsed table when it is the import target (same
            // database schema too, so an import into `inv.items` does not
            // reload `public.items`).
            let refresh = rep.ok()
                && app
                    .page_state
                    .as_ref()
                    .is_some_and(|p| p.table == table && p.schema == schema);
            if refresh {
                let (filter, order_by, page) = app
                    .page_state
                    .as_ref()
                    .map(|p| (p.filter.clone(), p.order_by.clone(), p.page))
                    .unwrap_or_default();
                reload_table_view(app, tx, filter, order_by, page);
            }
            app.import_report = Some(rep);
        }
        OpResult::ImportProgress { .. } => {}
        OpResult::Error(e) => {
            app.import_progress = None;
            app.page_pending = false;
            app.pending_sel = None;
            app.pending_focus = None;
            app.pending_write = false;
            app.pending_write_msg = None;
            // A failed scan must not leave the key list permanently unable to
            // load another page.
            app.redis_scan.pending = false;
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
/// Cache key for a table's `COUNT(*)`. The schema is part of the key so
/// `public.orders` and `inv.orders` never share a total.
fn count_cache_key(db: &str, schema: &str, table: &str, filter: &str) -> String {
    format!("{db}\u{1}{schema}\u{1}{table}\u{1}{filter}")
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
                if let Some(mc) = c.mongo {
                    run_mongo_action(app, tx, mc);
                    return;
                }
                if let Some(rc) = c.redis {
                    // A select-all delete demands a typed re-confirmation first.
                    if rc.typed_confirm.is_some() {
                        open_redis_typed_confirm(app, rc);
                        return;
                    }
                    if !rc.batch.is_empty() {
                        run_redis_batch(app, tx, rc.db, rc.batch, rc.reload_list);
                        return;
                    }
                    run_redis_write(app, tx, rc.db, &rc.cmd, rc.reload_value, rc.reload_list);
                    return;
                }
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

/// Open the typed re-confirmation prompt for a dangerous batch (repeat the key
/// count or `YES`). The pending batch is stashed so the prompt can run it.
fn open_redis_typed_confirm(app: &mut App, rc: RedisConfirm) {
    let need = rc.typed_confirm.unwrap_or(0);
    let title = tf("二次确认 · {}", &[&rc.summary]);
    let batch = rc.batch_keys.clone();
    app.redis_pending_batch = Some(rc);
    let mut ta = TextArea::default();
    ta.set_placeholder_text(tf("输入 {} 或 YES", &[&need]));
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchConfirm,
        title,
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch,
        input: ta,
    });
    app.status = tf("二次确认：输入 {} 或 YES", &[&need]);
}

/// Run a confirmed MongoDB document write, then reload the current page.
fn run_mongo_action(app: &mut App, tx: &Tx, mc: MongoConfirm) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.loading = true;
    let op = match mc.action {
        MongoAction::Insert { doc_json } => {
            app.status = tf("插入文档到 {}…", &[&fix_double_encoding(&mc.collection)]);
            Op::MongoInsert {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                doc_json,
            }
        }
        MongoAction::Update { id, doc_json } => {
            app.status = tf("更新文档 {}…", &[&truncate_disp(&id, 40)]);
            Op::MongoUpdate {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                id,
                doc_json,
            }
        }
        MongoAction::Delete { id } => {
            app.status = tf("删除文档 {}…", &[&truncate_disp(&id, 40)]);
            Op::MongoDelete {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                id,
            }
        }
    };
    app.spawn(tx, op);
}

fn browse_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // Overlays are modal, most-specific first. Esc always closes the current one.
    if app.help_open {
        help_key(app, k);
        return;
    }
    // CSV import and result export overlays (newest, so checked before the rest).
    if app.import_report.is_some() {
        import_report_key(app, k);
        return;
    }
    if app.import_plan.is_some() {
        import_plan_key(app, tx, k);
        return;
    }
    if app.import_prompt.is_some() {
        import_prompt_key(app, tx, k);
        return;
    }
    if app.export_path.is_some() {
        export_path_key(app, k);
        return;
    }
    if app.export_open {
        export_key(app, k);
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

    // Redis input dialogs (pattern / TTL / rename / value / batch) are modal.
    if app.redis_prompt.is_some() {
        redis_prompt_key(app, tx, k);
        return;
    }

    // MongoDB document JSON editor is modal.
    if app.mongo_dialog.is_some() {
        mongo_dialog_key(app, k);
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

    // `I` imports a CSV into the focused table (sidebar) or the table open in
    // the data browser. Uppercase on purpose: lowercase `i` is quick-insert in
    // the results pane, and an import is a rare, deliberate action.
    if k.code == KeyCode::Char('I')
        && !k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && app.selected.is_some()
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        open_import_prompt(app);
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

// ── database / schema switcher overlay ──

/// What a `d`-overlay row selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickerKind {
    Schema,
    Database,
}

/// Entries shown by the `d` overlay. Redis exposes its 16 logical databases;
/// schema-aware SQL engines list the current database's schemas first and then
/// the server's databases; everything else lists databases only.
fn picker_entries(app: &App) -> Vec<(PickerKind, String)> {
    if app.backend_kind == Backend::Redis {
        return (0..16)
            .map(|i| (PickerKind::Database, format!("db{i}")))
            .collect();
    }
    let mut out = Vec::with_capacity(app.schemas.len() + app.databases.len());
    for s in &app.schemas {
        out.push((PickerKind::Schema, s.clone()));
    }
    for d in &app.databases {
        out.push((PickerKind::Database, d.clone()));
    }
    out
}

/// Row label. When the schema layer is active the two kinds are prefixed so the
/// single list still reads as "schemas first, then databases".
fn picker_label(app: &App, kind: PickerKind, name: &str) -> String {
    if app.schemas.is_empty() {
        return name.to_string();
    }
    match kind {
        PickerKind::Schema => format!("{} · {}", t("模式"), name),
        PickerKind::Database => format!("{} · {}", t("数据库"), name),
    }
}

fn db_entries(app: &App) -> Vec<String> {
    picker_entries(app)
        .iter()
        .map(|(kind, name)| picker_label(app, *kind, name))
        .collect()
}

fn db_current_index(app: &App) -> usize {
    if app.backend_kind == Backend::Redis {
        return app.redis_db as usize;
    }
    picker_entries(app)
        .iter()
        .position(|(kind, name)| match kind {
            PickerKind::Schema => *name == app.schema,
            PickerKind::Database => *name == app.current_db(),
        })
        .unwrap_or(0)
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
                app.spawn(tx, Op::DatabasesRefresh(Box::new(cfg.clone())));
                // Schemas are cached per database; a refresh is the moment to
                // pick up a schema created outside dbxt.
                if schema_picker_engine(cfg.db_type) {
                    app.schemas.clear();
                    app.schemas_db.clear();
                    app.spawn(tx, Op::ListSchemas(Box::new(cfg), app.current_db()));
                }
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
    if app.backend_kind == Backend::Redis {
        app.redis_db = idx as u32;
        app.redis_value = None;
        app.clear_grid();
        app.set_placeholder();
        app.status = format!("redis db → {idx}");
        start_redis_scan(app, tx, true);
        return;
    }
    let Some((kind, name)) = picker_entries(app).into_iter().nth(idx) else {
        return;
    };
    match kind {
        PickerKind::Schema => {
            app.schema = name.clone();
            app.status = tf("切换 schema → {}", &[&(fix_double_encoding(&name))]);
            reload_tables(app, tx);
        }
        PickerKind::Database => {
            let Some(pos) = app.databases.iter().position(|d| *d == name) else {
                return;
            };
            app.db_index = pos;
            // The cached schema list belongs to the old database.
            app.schemas.clear();
            app.schemas_db.clear();
            app.status = tf("切换数据库 → {}", &[&(fix_double_encoding(&name))]);
            reload_tables(app, tx);
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

    // Redis connection → key browser (SCAN pagination, pattern filter,
    // multi-select batch operations).
    if app.backend_kind == Backend::Redis {
        // Shift + ↑/↓/Home/End extends a multi-select range from the anchor.
        if k.modifiers.contains(KeyModifiers::SHIFT) {
            match k.code {
                KeyCode::Up => {
                    let i = app.redis_list.selected().unwrap_or(0).saturating_sub(1);
                    redis_select_range(app, i);
                    return;
                }
                KeyCode::Down => {
                    let n = app.redis_scan.keys.len();
                    let i = (app.redis_list.selected().unwrap_or(0) + 1).min(n.saturating_sub(1));
                    redis_select_range(app, i);
                    return;
                }
                KeyCode::Home => {
                    redis_select_range(app, 0);
                    return;
                }
                KeyCode::End => {
                    let n = app.redis_scan.keys.len();
                    if n > 0 {
                        redis_select_range(app, n - 1);
                    }
                    return;
                }
                _ => {}
            }
        }
        // Ctrl-D is the delete shortcut the results pane also uses.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('d') {
            redis_batch_delete(app);
            return;
        }
        match k.code {
            KeyCode::Tab => app.focus = Focus::Editor,
            KeyCode::Char('c') => {
                app.page = Page::NewConn;
                app.form = ConnForm::default();
            }
            KeyCode::Char('p') => duplicate_connection(app),
            KeyCode::Char('o') => back_to_picker(app),
            // `r` rescans from cursor 0 with the current pattern.
            KeyCode::Char('r') => {
                app.status = t("重新扫描 keys…").into();
                start_redis_scan(app, tx, true);
            }
            // `/` edits the server-side MATCH pattern.
            KeyCode::Char('/') => open_redis_pattern_prompt(app),
            // `n` fetches the next SCAN page.
            KeyCode::Char('n') => start_redis_scan(app, tx, false),
            // Space toggles one key; `a` selects every loaded key.
            KeyCode::Char(' ') => {
                redis_toggle_select(app);
                let n = app.redis_selected.len();
                app.status = if n > 0 {
                    tf("已选 {} 个 key", &[&n])
                } else {
                    t("已清除选择").into()
                };
            }
            KeyCode::Char('a') => redis_select_all(app),
            // `y` copies the selected key names (or the focused one).
            KeyCode::Char('y') => redis_copy_selection(app),
            // Batch operations act on the selection (focused key when empty).
            KeyCode::Delete => redis_batch_delete(app),
            KeyCode::Char('x') => open_redis_batch_ttl_prompt(app),
            KeyCode::Char('m') => open_redis_batch_rename_prompt(app),
            // Esc clears the selection when there is one.
            KeyCode::Esc => {
                if !app.redis_selected.is_empty() {
                    app.redis_selected.clear();
                    app.redis_anchor = None;
                    app.status = t("已清除选择").into();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = app.redis_scan.keys.len();
                if n > 0 {
                    let i = app.redis_list.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
                    app.redis_list.select(Some(i));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = app.redis_scan.keys.len();
                if n > 0 {
                    let cur = app.redis_list.selected().unwrap_or(0);
                    if cur + 1 >= n && !app.redis_scan.exhausted {
                        // At the last loaded key: pull the next page, keeping the cursor.
                        start_redis_scan(app, tx, false);
                    } else {
                        app.redis_list.select(Some((cur + 1).min(n - 1)));
                    }
                }
            }
            KeyCode::Home => {
                if !app.redis_scan.keys.is_empty() {
                    app.redis_list.select(Some(0));
                }
            }
            KeyCode::End => {
                let n = app.redis_scan.keys.len();
                if n > 0 {
                    app.redis_list.select(Some(n - 1));
                }
                if !app.redis_scan.exhausted {
                    start_redis_scan(app, tx, false);
                }
            }
            KeyCode::Enter => open_redis_value(app, tx),
            KeyCode::Left | KeyCode::Char('h') => cycle_redis_db(app, tx, false),
            KeyCode::Right | KeyCode::Char('l') => cycle_redis_db(app, tx, true),
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
        KeyCode::Char('o') => back_to_picker(app),
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
    if app.backend_kind == Backend::Mongo {
        app.loading = true;
        app.status = tf("加载 {} 索引…", &[&(fix_double_encoding(&table))]);
        let db = app.current_db();
        app.spawn(
            tx,
            Op::MongoIndexes {
                cfg: Box::new(cfg),
                db,
                collection: table,
            },
        );
        return;
    }
    app.loading = true;
    app.status = tf("加载 {} 结构…", &[&(fix_double_encoding(&table))]);
    let db = app.current_db();
    let schema = app.schema.clone();
    app.spawn(
        tx,
        Op::Columns(Box::new(cfg.clone()), db.clone(), schema.clone(), table.clone()),
    );
    app.spawn(tx, Op::Ddl(Box::new(cfg), db, schema, table));
}

fn open_table_data(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| (t.name.clone(), t.table_type.clone())) else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    if app.backend_kind == Backend::Mongo {
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
        app.result_needle.clear();
        app.result_filter = None;
        remember_recent_table(app, &app.current_db(), "", &table.0);
        open_mongo_collection(app, tx);
        return;
    }
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
    let cur_db = app.current_db();
    let cur_schema = app.schema.clone();
    remember_recent_table(app, &cur_db, &cur_schema, &table.0);
    // Restore this table's persisted preferences (db.schema.table granularity).
    let db = app.current_db();
    let schema = app.schema.clone();
    let prefs = app
        .config
        .table(&db, &schema, &table.0)
        .cloned()
        .unwrap_or_default();
    app.col_hidden = prefs.hidden;
    // Fall back to the *global* default (not whatever the previously opened table
    // happened to use), so a table with no stored choice is not contaminated.
    app.compact = prefs.compact.or(app.config.compact);
    let order_by = prefs.order_by;
    app.page_state = Some(PageState {
        table: table.0.clone(),
        schema: schema.clone(),
        table_type: Some(table.1.clone()),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        has_next: false,
        filter: String::new(),
        order_by: order_by.clone(),
    });
    app.loading = true;
    app.status = tf(
        "加载 {} 数据…",
        &[&(qualified_display(&fix_double_encoding(&schema), &fix_double_encoding(&table.0)))],
    );
    // Column metadata powers the `e`/`i` templates (primary-key detection).
    app.spawn(
        tx,
        Op::TableColumns(
            Box::new(cfg.clone()),
            app.current_db(),
            schema.clone(),
            table.0.clone(),
        ),
    );
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &schema, &table.0, ""))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            schema,
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
    // MongoDB documents paginate with skip/limit rather than SQL OFFSET.
    if app.grid_kind == GridKind::MongoDocs {
        app.page_pending = true;
        app.pending_sel = pending_sel;
        reload_mongo_docs(app, tx, page);
        return true;
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
    app.status = tf("加载 {} 第 {} 页…", &[&(fix_double_encoding(&ps.table)), &(page + 1)]);
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &ps.schema, &ps.table, &ps.filter))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            schema: ps.schema.clone(),
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
    app.status = tf("加载 {} 第 {} 页…", &[&(fix_double_encoding(&ps.table)), &(page + 1)]);
    let known = app
        .count_cache
        .get(&count_cache_key(&app.current_db(), &ps.schema, &ps.table, &filter))
        .copied();
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            schema: ps.schema,
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
    if app.selected.is_some() {
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
        app.status = tf("切换到 {} …", &[&(fix_double_encoding(&db))]);
        app.set_placeholder();
        spawn_table_list(app, tx);
    }
}

/// Queue the table list for the current database. Schema-aware engines fetch the
/// schema list first (once per database), then reload the tables for the current
/// schema; everything else goes straight to the flat table list.
fn spawn_table_list(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let db = app.current_db();
    if schema_picker_engine(cfg.db_type) && app.schemas_db != db {
        app.spawn(tx, Op::ListSchemas(Box::new(cfg), db));
    } else {
        let schema = app.schema.clone();
        spawn_list_tables(app, tx, Box::new(cfg), db, schema);
    }
}

/// Queue one table-list request, bumping the generation so an earlier reply can
/// be recognised and dropped.
fn spawn_list_tables(
    app: &mut App,
    tx: &Tx,
    cfg: Box<ConnectionConfig>,
    db: String,
    schema: String,
) {
    app.tables_gen = app.tables_gen.wrapping_add(1);
    let gen = app.tables_gen;
    app.spawn(tx, Op::ListTables(cfg, db, schema, gen));
}

/// Default schema to browse when nothing is remembered: PostgreSQL's `public`
/// when present, else the first schema the server listed.
fn default_schema(schemas: &[String]) -> String {
    schemas
        .iter()
        .find(|s| s.eq_ignore_ascii_case("public"))
        .or_else(|| schemas.first())
        .cloned()
        .unwrap_or_default()
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

/// The focused row read from the unfiltered grid. An active result search keeps
/// a display→source row map, so the row must come from `full_grid` — reading it
/// from the on-screen (filtered) grid with the full-grid index would fail or
/// copy the wrong row.
fn focused_full_row(app: &App) -> Option<Vec<Val>> {
    let grid = full_grid(app)?;
    let idx = app.full_row_index()?;
    grid.rows.get(idx).cloned()
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
            app.backend_kind = backend_for_connection(&cfg);
            app.schemas.clear();
            app.schema.clear();
            app.schemas_db.clear();
            app.clear_grid();
            app.script = None;
            app.ddl = None;
            app.page_state = None;
            app.col_offset = 0;
            app.col_cursor = 0;
            app.cell_popup = None;
            app.cmd_output.clear();
            app.redis_value = None;
            app.redis_prompt = None;
            app.redis_scan = RedisScanState::default();
            app.redis_list = ListState::default();
            app.mongo_filter.clear();
            app.mongo_page = 0;
            app.set_placeholder();
            app.loading = true;
            app.status = tf("连接 {} ({})…", &[&(cfg.name), &(cfg.db_type.as_str())]);
            if app.backend_kind == Backend::Redis {
                // Redis exposes 16 fixed logical databases; there is nothing to
                // enumerate, so go straight to the first SCAN page.
                app.databases = (0..16).map(|i| i.to_string()).collect();
                app.db_index = 0;
                app.redis_db = 0;
                start_redis_scan(app, tx, true);
                app.spawn(tx, Op::History(Box::new(cfg)));
            } else {
                app.spawn(tx, Op::Databases(Box::new(cfg)));
            }
        }
    }
}

/// Pick the interaction mode for a connection: a Redis connection opens the key
/// browser, a MongoDB connection the document browser, everything else SQL.
fn backend_for_connection(cfg: &ConnectionConfig) -> Backend {
    match cfg.db_type.as_str() {
        "redis" | "keydb" | "valkey" => Backend::Redis,
        "mongodb" | "mongo" => Backend::Mongo,
        _ => Backend::Sql,
    }
}

/// How many keys one `SCAN` page asks for.
const REDIS_SCAN_PAGE: usize = 100;
/// How many server-side SCAN cycles a single page performs. `SCAN` is a hint, so
/// a selective `MATCH` can return zero keys for several cycles; iterating a few
/// times server-side keeps an empty first page rare without loading the whole
/// keyspace.
const REDIS_SCAN_ITERATIONS: usize = 5;
/// How many documents one MongoDB page shows.
const MONGO_PAGE: usize = 50;

/// Request the next page of the Redis key browser. `reset` starts a fresh scan
/// (used after a pattern change / logical-db switch / write) instead of
/// appending; a reset also bumps the generation so a late reply is dropped.
fn start_redis_scan(app: &mut App, tx: &Tx, reset: bool) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    if reset {
        app.redis_scan.keys.clear();
        app.redis_scan.cursor = 0;
        app.redis_scan.exhausted = false;
        app.redis_scan.gen = app.redis_scan.gen.wrapping_add(1);
    } else if app.redis_scan.exhausted || app.redis_scan.pending {
        // A page is already in flight: a second request would start from the
        // same cursor and append a duplicate page (the fresh-scan race).
        return;
    }
    let gen = app.redis_scan.gen;
    let cursor = app.redis_scan.cursor;
    let pattern = app.redis_scan.pattern.clone();
    app.redis_scan.pending = true;
    app.loading = true;
    app.spawn(
        tx,
        Op::RedisScan {
            cfg: Box::new(cfg),
            db: app.redis_db,
            cursor,
            pattern,
            count: REDIS_SCAN_PAGE,
            gen,
            append: !reset,
        },
    );
}

/// Load the selected key's typed value into the results pane.
fn open_redis_value(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(key) = app
        .redis_list
        .selected()
        .and_then(|i| app.redis_scan.keys.get(i))
        .map(|k| (k.key_raw.clone(), k.key_display.clone()))
    else {
        app.status = t("先选中一个 key").into();
        return;
    };
    app.loading = true;
    app.status = tf("加载 key {}…", &[&(fix_double_encoding(&key.1))]);
    app.spawn(
        tx,
        Op::RedisValue {
            cfg: Box::new(cfg),
            db: app.redis_db,
            key_raw: key.0,
        },
    );
}

/// Append the next page of a large Redis collection value.
fn redis_load_more(app: &mut App, tx: &Tx) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let Some(cursor) = view.scan_cursor else {
        app.status = t("已全部加载").into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.loading = true;
    app.status = t("加载更多…").into();
    app.spawn(
        tx,
        Op::RedisMore {
            cfg: Box::new(cfg),
            db: app.redis_db,
            key_raw: view.key_raw,
            key_type: view.redis_type,
            cursor,
            count: 200,
        },
    );
}

/// Reload one MongoDB collection page.
fn reload_mongo_docs(app: &mut App, tx: &Tx, page: usize) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    app.mongo_gen = app.mongo_gen.wrapping_add(1);
    let gen = app.mongo_gen;
    app.loading = true;
    app.status = tf("加载 {} 文档…", &[&(fix_double_encoding(&coll))]);
    app.spawn(
        tx,
        Op::MongoDocs {
            cfg: Box::new(cfg),
            db: app.current_db(),
            collection: coll,
            page,
            page_size: MONGO_PAGE,
            filter: app.mongo_filter.clone(),
            gen,
        },
    );
}

/// Open the selected collection in the document browser (Mongo mode).
fn open_mongo_collection(app: &mut App, tx: &Tx) {
    app.mongo_page = 0;
    app.mongo_filter.clear();
    reload_mongo_docs(app, tx, 0);
}

/// Drop the current connection and show the connection picker again.
fn back_to_picker(app: &mut App) {
    app.selected = None;
    app.tables.clear();
    app.tables_all.clear();
    app.table_filter.clear();
    app.columns.clear();
    app.databases.clear();
    app.schemas.clear();
    app.schema.clear();
    app.schemas_db.clear();
    app.clear_grid();
    app.script = None;
    app.ddl = None;
    app.page_state = None;
    app.col_offset = 0;
    app.col_cursor = 0;
    app.cell_popup = None;
    app.redis_value = None;
    app.redis_scan = RedisScanState::default();
    app.redis_list = ListState::default();
    app.redis_selected.clear();
    app.redis_anchor = None;
    app.redis_pending_batch = None;
    app.picker_open = true;
}

/// Switch the Redis logical database and rescan it.
fn cycle_redis_db(app: &mut App, tx: &Tx, forward: bool) {
    app.redis_db = if forward {
        (app.redis_db + 1) % 16
    } else {
        (app.redis_db + 15) % 16
    };
    app.redis_value = None;
    app.clear_grid();
    app.set_placeholder();
    app.redis_selected.clear();
    app.redis_anchor = None;
    app.status = tf("redis db → {}", &[&(app.redis_db)]);
    start_redis_scan(app, tx, true);
}

/// Open the `/` pattern prompt (server-side SCAN MATCH).
fn open_redis_pattern_prompt(app: &mut App) {
    let mut ta = TextArea::from(vec![app.redis_scan.pattern.clone()]);
    ta.set_placeholder_text(t("例: app:*（留空回车 = 全部 *）"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Pattern,
        title: t("key 匹配模式（SCAN MATCH）").to_string(),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open a TTL edit prompt for the focused key.
fn open_redis_ttl_prompt(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let initial = if view.ttl >= 0 { view.ttl.to_string() } else { String::new() };
    let mut ta = TextArea::from(vec![initial]);
    ta.set_placeholder_text(t("秒数（-1 = 持久化，0 = 立即删除）"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Ttl,
        title: tf("设置 TTL · {}", &[&(view.key_display)]),
        key_display: view.key_display.clone(),
        key_raw: view.key_raw.clone(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open a rename prompt for the focused key.
fn open_redis_rename_prompt(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let mut ta = TextArea::from(vec![view.key_display.clone()]);
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Rename,
        title: tf("重命名 key · {}", &[&(view.key_display)]),
        key_display: view.key_display.clone(),
        key_raw: view.key_raw.clone(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open an edit prompt for the focused grid cell: a string key's whole body, or
/// a hash field's value. Other Redis types are read-only for now.
fn open_redis_edit(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    match &view.raw.data {
        RedisValueData::String { content, .. } => {
            let initial = redis_blob_editable_text(content).unwrap_or_default();
            let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
            ta.move_cursor(CursorMove::End);
            app.redis_prompt = Some(RedisPrompt {
                kind: RedisPromptKind::StringValue,
                title: tf("编辑 string · {}", &[&(view.key_display)]),
                key_display: view.key_display.clone(),
                key_raw: view.key_raw.clone(),
                field: String::new(),
                batch: Vec::new(),
                input: ta,
            });
        }
        RedisValueData::Hash { .. } => {
            let Some(field) = view.row_keys.get(app.sel).cloned() else {
                return;
            };
            if field.is_empty() {
                return;
            }
            let initial = app
                .grid
                .as_ref()
                .and_then(|g| g.rows.get(app.sel))
                .and_then(|r| r.get(1))
                .map(|v| v.text().to_string())
                .unwrap_or_default();
            let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
            ta.move_cursor(CursorMove::End);
            app.redis_prompt = Some(RedisPrompt {
                kind: RedisPromptKind::HashField,
                title: tf("编辑 hash 字段 · {} · {}", &[&(view.key_display), &(field)]),
                key_display: view.key_display.clone(),
                key_raw: view.key_raw.clone(),
                field,
                batch: Vec::new(),
                input: ta,
            });
        }
        _ => {
            app.status = t("该类型暂不支持直接编辑，可用命令行修改").into();
        }
    }
}

/// Which write a Redis prompt will generate. Kept as data so the confirmation
/// layer shows the exact command before anything runs.
fn redis_prompt_command(kind: RedisPromptKind, key: &str, field: &str, input: &str) -> String {
    let q = |s: &str| {
        // Quote with double quotes and escape so spaces / quotes survive the
        // redis-cli tokenizer the backend uses.
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    };
    match kind {
        RedisPromptKind::Ttl => format!("EXPIRE {} {}", q(key), input.trim()),
        RedisPromptKind::Rename => format!("RENAME {} {}", q(key), q(input.trim())),
        RedisPromptKind::StringValue => format!("SET {} {}", q(key), q(input)),
        RedisPromptKind::HashField => format!("HSET {} {} {}", q(key), q(field), q(input)),
        RedisPromptKind::Pattern
        | RedisPromptKind::BatchTtl
        | RedisPromptKind::BatchRenamePrefix
        | RedisPromptKind::BatchConfirm => String::new(),
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
    // Redis: row 0 connection, row 1 logical DB, row 2 pattern, then keys.
    if app.backend_kind == Backend::Redis && app.selected.is_some() {
        if rel == 1 {
            open_db_picker(app);
            return;
        }
        let key_row = rel - 3;
        if key_row < 0 {
            return;
        }
        let _ = x;
        let n = app.redis_scan.keys.len();
        let cap = (area.height as usize).saturating_sub(5).max(1);
        let sel = app.redis_list.selected();
        let start = sel
            .unwrap_or(0)
            .saturating_sub(cap / 2)
            .min(n.saturating_sub(cap.min(n)));
        let idx = start + key_row as usize;
        if idx >= n {
            return;
        }
        if app.redis_list.selected() == Some(idx) {
            open_redis_value(app, tx);
        } else {
            app.redis_list.select(Some(idx));
        }
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
        tf("redis db {} · {} keys · d 切换", &[&(app.redis_db), &(app.redis_scan.keys.len())])
    } else if !app.schemas.is_empty() {
        tf(
            "{} · schema {} · d 切换",
            &[
                &(fix_double_encoding(&app.current_db())),
                &(fix_double_encoding(&app.schema)),
            ],
        )
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

/// Execute a confirmed Redis write through the console, then refresh.
fn run_redis_write(
    app: &mut App,
    tx: &Tx,
    db: u32,
    cmd: &str,
    reload_value: Option<String>,
    reload_list: bool,
) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.loading = true;
    app.status = tf("执行 {}…", &[&(truncate_disp(&one_line(cmd), 50))]);
    app.spawn(
        tx,
        Op::RedisWrite {
            cfg: Box::new(cfg),
            db,
            cmd: cmd.to_string(),
            reload_value,
            reload_list,
        },
    );
}

/// `y` in a Redis value / Mongo document grid: copy the focused row as tab-
/// separated text (OSC52 clipboard, with the file fallback).
fn copy_redis_row(app: &mut App) {
    let Some(row) = focused_full_row(app) else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let text = row.iter().map(|v| v.text()).collect::<Vec<_>>().join("\t");
    let n = text.chars().count();
    match clipboard_copy(&text) {
        Some(p) => app.status = tf("✓ 已复制（{} 字符）· 兜底 {}", &[&(n), &(p.display())]),
        None => app.status = tf("✓ 已复制（{} 字符）· OSC52 剪贴板", &[&(n)]),
    }
}

/// Confirm deleting the focused key (DEL). Data-destructive writes always go
/// through the red layer, like every SQL row delete.
fn redis_confirm_delete(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        app.status = t("先选中一个 key").into();
        return;
    };
    let cmd = format!("DEL \"{}\"",  view.key_display.replace('\\', "\\\\").replace('"', "\\\""));
    app.confirm = Some(Confirm {
        sql: cmd.clone(),
        reasons: vec![
            tf("将删除 key {}（不可撤销）", &[&(view.key_display)]),
            t("DEL 不可撤销，Enter 后立即执行").into(),
        ],
        refresh: false,
        clear_batch: false,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd,
            batch: Vec::new(),
            batch_keys: Vec::new(),
            reload_value: None,
            reload_list: true,
            typed_confirm: None,
            summary: String::new(),
        }),
        mongo: None,
    });
    app.status = t("删除确认 · Enter 执行 · Esc 取消").into();
}

/// Run a generated batch of Redis commands in order.
fn run_redis_batch(app: &mut App, tx: &Tx, db: u32, cmds: Vec<String>, reload_list: bool) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let n = cmds.len();
    app.loading = true;
    app.status = tf("批量执行 {} 条命令…", &[&n]);
    app.spawn(
        tx,
        Op::RedisBatchWrite {
            cfg: Box::new(cfg),
            db,
            cmds,
            reload_list,
        },
    );
}

/// The batch targets in list order as `(raw, display)`. When nothing is
/// explicitly selected, the focused key is the single target.
fn redis_selection_targets(
    selected: &HashSet<String>,
    keys: &[RedisKeyInfo],
    focused: Option<usize>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = keys
        .iter()
        .filter(|k| selected.contains(&k.key_raw))
        .map(|k| (k.key_raw.clone(), k.key_display.clone()))
        .collect();
    if out.is_empty() {
        if let Some(i) = focused {
            if let Some(k) = keys.get(i) {
                out.push((k.key_raw.clone(), k.key_display.clone()));
            }
        }
    }
    out
}

fn redis_batch_targets(app: &App) -> Vec<(String, String)> {
    redis_selection_targets(&app.redis_selected, &app.redis_scan.keys, app.redis_list.selected())
}

/// True when the selection covers every loaded key (used to force the extra
/// typed confirmation before a destructive select-all delete).
fn redis_selection_is_all(selected: &HashSet<String>, keys: &[RedisKeyInfo]) -> bool {
    !selected.is_empty() && selected.len() == keys.len()
}

fn redis_all_selected(app: &App) -> bool {
    redis_selection_is_all(&app.redis_selected, &app.redis_scan.keys)
}

/// Toggle the key at `idx` in / out of the selection.
fn redis_selection_toggle(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
    idx: usize,
) {
    let Some(k) = keys.get(idx) else {
        return;
    };
    let raw = k.key_raw.clone();
    if !selected.remove(&raw) {
        selected.insert(raw);
    }
    *anchor = Some(idx);
}

/// Extend the selection from the anchor to `to` (additive).
fn redis_selection_range(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
    to: usize,
) {
    let n = keys.len();
    if n == 0 {
        return;
    }
    let to = to.min(n - 1);
    let a = anchor.unwrap_or(to).min(n - 1);
    let (lo, hi) = if a <= to { (a, to) } else { (to, a) };
    for k in keys.iter().take(hi + 1).skip(lo) {
        selected.insert(k.key_raw.clone());
    }
    *anchor = Some(a);
}

/// Select every loaded key (the `a` gesture).
fn redis_selection_all(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
) {
    for k in keys {
        selected.insert(k.key_raw.clone());
    }
    if !keys.is_empty() {
        *anchor = Some(0);
    }
}

/// Toggle the focused key's selection.
fn redis_toggle_select(app: &mut App) {
    let Some(i) = app.redis_list.selected() else {
        return;
    };
    redis_selection_toggle(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
        i,
    );
}

/// Extend the selection from the anchor to `to` (additive).
fn redis_select_range(app: &mut App, to: usize) {
    redis_selection_range(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
        to,
    );
    let n = app.redis_scan.keys.len();
    if n > 0 {
        app.redis_list.select(Some(to.min(n - 1)));
    }
}

/// Select every loaded key (the `a` gesture).
fn redis_select_all(app: &mut App) {
    if app.redis_scan.keys.is_empty() {
        return;
    }
    redis_selection_all(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
    );
    app.status = tf("已全选 {} 个 key", &[&app.redis_selected.len()]);
}

/// `y` in the key browser: copy the selected key names (or the focused one).
fn redis_copy_selection(app: &mut App) {
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    let text = targets
        .iter()
        .map(|(_, d)| d.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let n = targets.len();
    match clipboard_copy(&text) {
        Some(p) => app.status = tf("✓ 已复制 {} 个 key 名 · 兜底 {}", &[&n, &(p.display())]),
        None => app.status = tf("✓ 已复制 {} 个 key 名 · OSC52 剪贴板", &[&n]),
    }
}

/// A concise confirmation preview: the first commands plus a count summary.
fn redis_batch_preview(cmds: &[String], keys: usize) -> String {
    let mut s = String::new();
    for c in cmds.iter().take(4) {
        s.push_str(c);
        s.push('\n');
    }
    if cmds.len() > 4 {
        s.push_str(&tf("… 其余 {} 条命令", &[&(cmds.len() - 4)]));
        s.push('\n');
    }
    s.push_str(&tf("共 {} 个 key · {} 条命令", &[&keys, &cmds.len()]));
    s
}

/// Open the red confirmation layer for a generated Redis batch.
fn redis_open_batch_confirm(
    app: &mut App,
    commands: Vec<String>,
    targets: Vec<(String, String)>,
    typed_confirm: Option<usize>,
    summary: String,
) {
    let n = targets.len();
    let pattern = app.redis_scan.pattern.clone();
    app.confirm = Some(Confirm {
        sql: redis_batch_preview(&commands, n),
        reasons: vec![
            tf("将影响 {} 个 key（模式 {}）", &[&n, &pattern]),
            t("Enter 后按顺序执行，不可撤销").into(),
        ],
        refresh: false,
        clear_batch: false,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd: String::new(),
            batch: commands,
            batch_keys: targets,
            reload_value: None,
            reload_list: true,
            typed_confirm,
            summary,
        }),
        mongo: None,
    });
    app.status = t("批量确认 · Enter 执行 · Esc 取消").into();
}

/// `Del` in the key browser: batch delete the selected keys.
fn redis_batch_delete(app: &mut App) {
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    if targets.len() > REDIS_BATCH_LIMIT {
        app.status = tf(
            "选中 {} 个 key 超过单页上限 {}，请分批操作（space 取消部分选择）",
            &[&(targets.len()), &(REDIS_BATCH_LIMIT)],
        );
        return;
    }
    let all_loaded = redis_all_selected(app);
    let Ok(plan) = redis_plan_batch(RedisBatchKind::Delete, &targets, all_loaded, "") else {
        return;
    };
    let n = targets.len();
    let pattern = app.redis_scan.pattern.clone();
    redis_open_batch_confirm(app, plan.commands, targets, plan.typed_confirm, plan.summary);
    if let Some(c) = app.confirm.as_mut() {
        c.reasons = vec![
            tf("将批量删除 {} 个 key（模式 {}）", &[&n, &pattern]),
            t("DEL 不可撤销，Enter 后立即执行").into(),
        ];
    }
}

/// `x` in the key browser: open the batch TTL prompt.
fn open_redis_batch_ttl_prompt(app: &mut App) {
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t("秒数（-1 = 持久化，0 = 立即删除）"));
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchTtl,
        title: tf("批量设置 TTL · {} 个 key", &[&targets.len()]),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: targets,
        input: ta,
    });
}

/// `m` in the key browser: open the batch prefix-rename prompt.
fn open_redis_batch_rename_prompt(app: &mut App) {
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    // Prefill the old prefix from the SCAN pattern when it ends with `*`.
    let old = app
        .redis_scan
        .pattern
        .strip_suffix('*')
        .filter(|p| !p.is_empty() && *p != "*")
        .unwrap_or("");
    let mut ta = TextArea::from(vec![format!("{old}=")]);
    ta.set_placeholder_text(t("旧前缀=新前缀，例: app: = new:"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchRenamePrefix,
        title: tf("批量前缀重命名 · {} 个 key", &[&targets.len()]),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: targets,
        input: ta,
    });
}

/// Keys for a Redis value grid: edit the string / hash field, expire, rename,
/// delete, plus the shared search / copy / popup infrastructure.
fn redis_value_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('e') => app.focus = Focus::Editor,
            KeyCode::Char('d') => redis_confirm_delete(app),
            _ => {}
        }
        return;
    }
    if k.code == KeyCode::Esc && !app.result_needle.is_empty() {
        app.result_needle.clear();
        app.rebuild_view();
        app.sel = 0;
        app.status = t("已清除结果搜索").into();
        return;
    }
    match k.code {
        KeyCode::Esc => app.focus = Focus::Sidebar,
        KeyCode::Char('e') => open_redis_edit(app),
        // `n` loads the next page of a large hash / list / set / zset value.
        KeyCode::Char('n') => redis_load_more(app, tx),
        // `x` = expire (TTL), `m` = move / rename.
        KeyCode::Char('x') => open_redis_ttl_prompt(app),
        KeyCode::Char('m') => open_redis_rename_prompt(app),
        KeyCode::Delete => redis_confirm_delete(app),
        KeyCode::Char('o') => open_row_popup(app),
        KeyCode::Char('v') => open_cell_popup(app),
        KeyCode::Char('/') => open_result_filter(app),
        KeyCode::Char('y') => copy_redis_row(app),
        KeyCode::Char('z') => {
            app.freeze_first = !app.freeze_first;
        }
        KeyCode::Up | KeyCode::Char('k') => move_cursor(app, tx, -1),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(app, tx, 1),
        KeyCode::Left | KeyCode::Char('h') => move_col_cursor(app, -1),
        KeyCode::Right | KeyCode::Char('l') => move_col_cursor(app, 1),
        KeyCode::PageUp => screen_move(app, tx, -1),
        KeyCode::PageDown => screen_move(app, tx, 1),
        KeyCode::Home => app.sel = 0,
        KeyCode::End => {
            let n = result_row_count(app);
            if n > 0 {
                app.sel = n - 1;
            }
        }
        KeyCode::Enter => {
            if compact_active(app.compact, app.layout_mode) {
                open_row_popup(app);
            } else {
                open_cell_popup(app);
            }
        }
        _ => {}
    }
}

/// Keys for a MongoDB document grid: JSON filter, pagination and the shared
/// search / copy / popup infrastructure.
fn mongo_docs_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('e') => app.focus = Focus::Editor,
            KeyCode::Char('f') => page_turn(app, tx, true),
            KeyCode::Char('b') => page_turn(app, tx, false),
            KeyCode::Char('d') => mongo_confirm_delete(app),
            _ => {}
        }
        return;
    }
    if k.code == KeyCode::Esc && !app.result_needle.is_empty() {
        app.result_needle.clear();
        app.rebuild_view();
        app.sel = 0;
        app.status = t("已清除结果搜索").into();
        return;
    }
    match k.code {
        KeyCode::Esc => app.focus = Focus::Sidebar,
        KeyCode::Char('f') => open_mongo_filter_prompt(app),
        // Document CRUD: edit / insert / delete (all confirmed).
        KeyCode::Char('e') => open_mongo_edit(app),
        KeyCode::Char('i') => open_mongo_insert(app),
        KeyCode::Delete => mongo_confirm_delete(app),
        KeyCode::Char('y') => copy_redis_row(app),
        KeyCode::Char('o') => open_row_popup(app),
        KeyCode::Char('v') => open_cell_popup(app),
        KeyCode::Char('/') => open_result_filter(app),
        KeyCode::Up | KeyCode::Char('k') => move_cursor(app, tx, -1),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(app, tx, 1),
        KeyCode::Left | KeyCode::Char('h') => move_col_cursor(app, -1),
        KeyCode::Right | KeyCode::Char('l') => move_col_cursor(app, 1),
        KeyCode::PageUp => screen_move(app, tx, -1),
        KeyCode::PageDown => screen_move(app, tx, 1),
        KeyCode::Home => app.sel = 0,
        KeyCode::End => {
            let n = result_row_count(app);
            if n > 0 {
                app.sel = n - 1;
            }
        }
        KeyCode::Char('n') => page_turn(app, tx, true),
        KeyCode::Char('p') => page_turn(app, tx, false),
        KeyCode::Enter => {
            if compact_active(app.compact, app.layout_mode) {
                open_row_popup(app);
            } else {
                open_cell_popup(app);
            }
        }
        _ => {}
    }
}

/// Open the MongoDB JSON filter prompt (prefilled with the active filter).
fn open_mongo_filter_prompt(app: &mut App) {
    let mut ta = TextArea::from(vec![app.mongo_filter.clone()]);
    ta.set_placeholder_text(t("JSON 过滤，例: {\"age\": {\"$gt\": 30}}（留空 = 全部）"));
    ta.move_cursor(CursorMove::End);
    app.filter_prompt = Some(ta);
}

/// The focused document from the retained page, plus its index in that page.
fn mongo_focused_doc(app: &App) -> Option<(usize, serde_json::Value)> {
    let idx = app.full_row_index()?;
    let doc = app.mongo_docs.get(idx)?.clone();
    Some((idx, doc))
}

/// `e` — open the JSON editor for the focused document.
fn open_mongo_edit(app: &mut App) {
    let Some((_idx, doc)) = mongo_focused_doc(app) else {
        app.status = t("没有可编辑的文档").into();
        return;
    };
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let id_value = doc.get("_id").cloned().unwrap_or(serde_json::Value::Null);
    let id = mongo_id_arg(&id_value);
    let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string());
    let mut editor = TextArea::from(text.split('\n').collect::<Vec<_>>());
    editor.move_cursor(CursorMove::Top);
    editor.set_placeholder_text(t("JSON 文档（_id 不可修改）"));
    app.mongo_dialog = Some(MongoDocDialog {
        mode: MongoDocMode::Edit,
        db: app.current_db(),
        collection: coll,
        original: doc,
        id,
        editor,
        error: None,
    });
    app.status = t("编辑文档 · Ctrl-S 校验并保存 · Esc 取消").into();
}

/// `i` — open the JSON editor with an empty document template.
fn open_mongo_insert(app: &mut App) {
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let mut editor = TextArea::from(vec!["{", "  ", "}"]);
    editor.move_cursor(CursorMove::Top);
    editor.move_cursor(CursorMove::Down);
    editor.move_cursor(CursorMove::End);
    editor.set_placeholder_text(t("新文档 JSON（省略 _id 则由 MongoDB 生成）"));
    app.mongo_dialog = Some(MongoDocDialog {
        mode: MongoDocMode::Insert,
        db: app.current_db(),
        collection: coll,
        original: serde_json::Value::Object(serde_json::Map::new()),
        id: String::new(),
        editor,
        error: None,
    });
    app.status = t("插入文档 · Ctrl-S 校验并保存 · Esc 取消").into();
}

/// `Del` — confirm deleting the focused document by `_id`.
fn mongo_confirm_delete(app: &mut App) {
    let Some((_idx, doc)) = mongo_focused_doc(app) else {
        app.status = t("没有可删除的文档").into();
        return;
    };
    let id_value = doc.get("_id").cloned().unwrap_or(serde_json::Value::Null);
    let label = mongo_id_label(&id_value);
    let id = mongo_id_arg(&id_value);
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let db = app.current_db();
    app.confirm = Some(Confirm {
        sql: format!(
            "db.{}.deleteOne({{_id: {}}})",
            fix_double_encoding(&coll),
            serde_json::to_string(&id_value).unwrap_or_default()
        ),
        reasons: vec![
            tf("将删除文档 _id={}（不可撤销）", &[&label]),
            t("Enter 后立即执行").into(),
        ],
        refresh: false,
        clear_batch: false,
        redis: None,
        mongo: Some(MongoConfirm {
            db,
            collection: coll,
            action: MongoAction::Delete { id },
        }),
    });
    app.status = t("删除文档确认 · Enter 执行 · Esc 取消").into();
}

/// Handle keys in the MongoDB JSON editor. Ctrl-S validates and opens the
/// confirmation layer; everything else is text editing.
fn mongo_dialog_key(app: &mut App, k: KeyEvent) {
    let Some(mut d) = app.mongo_dialog.take() else {
        return;
    };
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('s') {
        mongo_dialog_submit(app, d);
        return;
    }
    if k.code == KeyCode::Esc {
        app.status = t("已取消").into();
        return;
    }
    d.editor.input(k);
    d.error = None;
    app.mongo_dialog = Some(d);
}

/// Validate the editor's JSON and route a valid edit / insert through the red
/// confirmation layer (an edit previews its top-level diff first).
fn mongo_dialog_submit(app: &mut App, d: MongoDocDialog) {
    let text = d.editor.lines().join("\n");
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let mut d = d;
            d.error = Some(tf("JSON 非法：{}", &[&e]));
            app.mongo_dialog = Some(d);
            app.status = tf("✗ JSON 非法：{}", &[&e]);
            return;
        }
    };
    if !parsed.is_object() {
        let mut d = d;
        d.error = Some(t("文档必须是 JSON 对象 { … }").to_string());
        app.mongo_dialog = Some(d);
        app.status = t("✗ 文档必须是 JSON 对象").into();
        return;
    }
    let doc_json = serde_json::to_string(&parsed).unwrap_or(text);
    match d.mode {
        MongoDocMode::Insert => {
            let preview = serde_json::to_string_pretty(&parsed).unwrap_or_default();
            app.confirm = Some(Confirm {
                sql: preview,
                reasons: vec![
                    tf(
                        "将向 {}.{} 插入 1 个文档",
                        &[&fix_double_encoding(&d.db), &fix_double_encoding(&d.collection)],
                    ),
                    t("Enter 执行 · Esc 取消").into(),
                ],
                refresh: false,
                clear_batch: false,
                redis: None,
                mongo: Some(MongoConfirm {
                    db: d.db.clone(),
                    collection: d.collection.clone(),
                    action: MongoAction::Insert { doc_json },
                }),
            });
            app.status = t("插入确认 · Enter 执行 · Esc 取消").into();
        }
        MongoDocMode::Edit => {
            let old_id = d.original.get("_id");
            let new_id = parsed.get("_id");
            if old_id != new_id {
                let mut d = d;
                d.error = Some(t("_id 不可修改（请恢复原值）").to_string());
                app.mongo_dialog = Some(d);
                app.status = t("✗ _id 不可修改").into();
                return;
            }
            let diff = mongo_doc_diff(&d.original, &parsed, 12);
            let mut body = String::new();
            if diff.is_empty() {
                body.push_str(t("（没有字段变化）"));
            } else {
                for line in &diff {
                    body.push_str(line);
                    body.push('\n');
                }
            }
            body.push('\n');
            body.push_str(&serde_json::to_string_pretty(&parsed).unwrap_or_default());
            app.confirm = Some(Confirm {
                sql: body,
                reasons: vec![
                    tf(
                        "将替换文档 _id={}",
                        &[&mongo_id_label(old_id.unwrap_or(&serde_json::Value::Null))],
                    ),
                    t("Enter 执行 · Esc 取消").into(),
                ],
                refresh: false,
                clear_batch: false,
                redis: None,
                mongo: Some(MongoConfirm {
                    db: d.db.clone(),
                    collection: d.collection.clone(),
                    action: MongoAction::Update {
                        id: d.id.clone(),
                        doc_json,
                    },
                }),
            });
            app.status = t("更新确认（含 diff）· Enter 执行 · Esc 取消").into();
        }
    }
}

/// Handle a Redis input dialog. Enter turns the input into a command and routes
/// it through the confirmation layer; the pattern dialog is read-only-safe and
/// applies immediately.
fn redis_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut p) = app.redis_prompt.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.redis_pending_batch = None;
        app.status = t("已取消").into();
        return;
    }
    if k.code != KeyCode::Enter {
        p.input.input(k);
        app.redis_prompt = Some(p);
        return;
    }
    let input = p.input.lines().join("\n");
    match p.kind {
        RedisPromptKind::Pattern => {
            let pat = input.trim();
            app.redis_scan.pattern = if pat.is_empty() { "*".to_string() } else { pat.to_string() };
            app.redis_value = None;
            app.clear_grid();
            app.redis_selected.clear();
            app.redis_anchor = None;
            app.status = tf("匹配模式 → {}", &[&(app.redis_scan.pattern)]);
            start_redis_scan(app, tx, true);
            return;
        }
        RedisPromptKind::BatchConfirm => {
            let need = app
                .redis_pending_batch
                .as_ref()
                .and_then(|r| r.typed_confirm)
                .unwrap_or(0);
            let typed = input.trim();
            if typed != need.to_string() && !typed.eq_ignore_ascii_case("YES") {
                app.status = tf("输入不匹配（需 {} 或 YES）", &[&need]);
                app.redis_prompt = Some(p);
                return;
            }
            if let Some(rc) = app.redis_pending_batch.take() {
                if !rc.batch.is_empty() {
                    run_redis_batch(app, tx, rc.db, rc.batch, rc.reload_list);
                }
            }
            return;
        }
        RedisPromptKind::BatchTtl => {
            let ttl = input.trim().to_string();
            let plan = match redis_plan_batch(RedisBatchKind::Ttl, &p.batch, false, &ttl) {
                Ok(plan) => plan,
                Err(e) => {
                    app.status = format!("✗ {e}");
                    app.redis_prompt = Some(p);
                    return;
                }
            };
            let n = p.batch.len();
            let pattern = app.redis_scan.pattern.clone();
            redis_open_batch_confirm(app, plan.commands, p.batch.clone(), plan.typed_confirm, plan.summary);
            if let Some(c) = app.confirm.as_mut() {
                c.reasons = vec![
                    tf(
                        "将对 {} 个 key 设置 TTL={}s（模式 {}）",
                        &[&n, &ttl, &pattern],
                    ),
                    t("Enter 后立即执行").into(),
                ];
            }
            return;
        }
        RedisPromptKind::BatchRenamePrefix => {
            let plan = match redis_plan_batch(
                RedisBatchKind::RenamePrefix,
                &p.batch,
                false,
                &input,
            ) {
                Ok(plan) => plan,
                Err(e) => {
                    app.status = format!("✗ {e}");
                    app.redis_prompt = Some(p);
                    return;
                }
            };
            let Some((old, new)) = input.split_once('=') else {
                return;
            };
            let renamed = redis_prefix_rename_plan(
                &p.batch.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
                old,
                new,
            )
            .len();
            let pattern = app.redis_scan.pattern.clone();
            redis_open_batch_confirm(app, plan.commands, p.batch.clone(), plan.typed_confirm, plan.summary);
            if let Some(c) = app.confirm.as_mut() {
                c.reasons = vec![
                    tf(
                        "将重命名 {} 个 key：{} → {}（模式 {}）",
                        &[&renamed, &old, &new, &pattern],
                    ),
                    t("Enter 后立即执行").into(),
                ];
            }
            return;
        }
        _ => {}
    }
    let cmd = redis_prompt_command(p.kind, &p.key_display, &p.field, &input);
    if cmd.is_empty() {
        return;
    }
    let (reload_value, reload_list) = match p.kind {
        // A renamed key has a new name, so just refresh the list.
        RedisPromptKind::Rename => (None, true),
        RedisPromptKind::StringValue | RedisPromptKind::HashField => (Some(p.key_raw.clone()), false),
        RedisPromptKind::Ttl => (Some(p.key_raw.clone()), true),
        RedisPromptKind::Pattern
        | RedisPromptKind::BatchTtl
        | RedisPromptKind::BatchRenamePrefix
        | RedisPromptKind::BatchConfirm => (None, false),
    };
    app.confirm = Some(Confirm {
        sql: cmd.clone(),
        reasons: vec![
            tf("将执行 {}", &[&(truncate_disp(&one_line(&cmd), 60))]),
            t("Enter 执行 · Esc 取消").into(),
        ],
        refresh: false,
        clear_batch: false,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd,
            batch: Vec::new(),
            batch_keys: Vec::new(),
            reload_value,
            reload_list,
            typed_confirm: None,
            summary: String::new(),
        }),
        mongo: None,
    });
    app.status = t("确认写入 · Enter 执行 · Esc 取消").into();
}

fn preview_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // A Redis value / Mongo document grid has its own keymap (edit, delete, TTL,
    // rename, JSON filter) that must not fall through to the SQL row actions.
    if app.backend_kind == Backend::Redis && app.grid_kind == GridKind::RedisValue {
        redis_value_key(app, tx, k);
        return;
    }
    if app.backend_kind == Backend::Mongo && app.grid_kind == GridKind::MongoDocs {
        mongo_docs_key(app, tx, k);
        return;
    }
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
            KeyCode::Char('y') => open_export(app),
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
            if app.grid_kind == GridKind::MongoDocs {
                app.mongo_filter = filter.clone();
                app.pending_sel = Some(0);
                if filter.is_empty() {
                    app.status = t("过滤已清除").into();
                } else {
                    app.status = tf("过滤: {}", &[&(filter)]);
                }
                reload_mongo_docs(app, tx, 0);
                return;
            }
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
    // that exact `database.schema.table` so reopening it restores the mode.
    app.config.set_compact(app.compact);
    if let Some(ps) = app.page_state.clone() {
        let db = app.current_db();
        app.config.entry(&db, &ps.schema, &ps.table).compact = app.compact;
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
        app.status = tf("显示列 {} · 已记住", &[&(fix_double_encoding(&name))]);
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
        app.status = tf("隐藏列 {} · 已记住", &[&(fix_double_encoding(&name))]);
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
fn remember_recent_table(app: &mut App, db: &str, schema: &str, table: &str) {
    let entry = (db.to_string(), schema.to_string(), table.to_string());
    app.recent_tables.retain(|e| e != &entry);
    app.recent_tables.insert(0, entry);
    app.recent_tables.truncate(5);
}

fn open_recent(app: &mut App, tx: &Tx, idx: usize) {
    let Some((db, schema, table)) = app.recent_tables.get(idx).cloned() else {
        return;
    };
    app.recent_open = false;
    let db_changed = db != app.current_db();
    // Same database and schema: the table is already listed, jump straight to it.
    if !db_changed && schema == app.schema {
        if let Some(pos) = app.tables.iter().position(|t| t.name == table) {
            app.table_list.select(Some(pos));
            open_table_data(app, tx);
            return;
        }
    }
    if db_changed {
        let Some(pos) = app.databases.iter().position(|d| *d == db) else {
            app.status = tf("✗ 数据库 {} 不在当前连接中", &[&(fix_double_encoding(&db))]);
            return;
        };
        app.db_index = pos;
        // The schema list belongs to the old database; refetch it.
        app.schemas.clear();
        app.schemas_db.clear();
    }
    app.schema = schema.clone();
    app.pending_open_table = Some((schema.clone(), table.clone()));
    app.pending_table = None;
    app.status = tf(
        "切换到 {} 并打开 {} …",
        &[&(fix_double_encoding(&db)), &(fix_double_encoding(&qualified_display(&schema, &table)))],
    );
    reload_tables(app, tx);
}

// ── sidebar table filter (`/`, filter-as-you-type) ──

/// Recompute the visible table list from `tables_all` + `table_filter`, keeping
/// the previously selected table selected when it still matches.
fn apply_table_filter(app: &mut App) {
    let prev = app.selected_table().map(|t| t.name.clone());
    let needle = app.table_filter.trim().to_lowercase();
    // Match the qualified `schema.table` the sidebar draws, so `/inv` finds
    // every table in the `inv` schema.
    let schema = app.schema.clone();
    app.tables = if needle.is_empty() {
        app.tables_all.clone()
    } else {
        app.tables_all
            .iter()
            .filter(|t| {
                qualified_display(&schema, &t.name)
                    .to_lowercase()
                    .contains(&needle)
            })
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

/// Column type from the browsed table's metadata, when available. The schema is
/// matched too, so a `public.orders` metadata set is never applied to
/// `inv.orders`.
fn column_type(app: &App, schema: &str, table: &str, col: &str) -> Option<String> {
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

/// The bare base of a declared type: `numeric(12,2)` → `numeric`,
/// `timestamp with time zone` → `timestamp`, `text[]` → `text[]`.
fn base_type(t: &str) -> String {
    t.trim().to_ascii_lowercase().split(['(', ' ']).next().unwrap_or("").to_string()
}

/// Date/time families whose NOT NULL placeholder should be `CURRENT_TIMESTAMP`
/// rather than an empty string.
fn is_temporal_type(base: &str) -> bool {
    matches!(
        base,
        "timestamp" | "timestamptz" | "datetime" | "date" | "time" | "timetz"
    )
}

/// True for a column the server fills itself — auto-increment, PostgreSQL
/// `serial`, an identity column, or a generated expression — so an INSERT
/// template must omit it instead of writing a literal that skips the sequence.
fn is_server_generated_column(c: &ColumnInfo) -> bool {
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
fn insert_placeholder(c: &ColumnInfo) -> String {
    if c.column_default.as_deref().map(|d| !d.trim().is_empty()).unwrap_or(false) {
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
fn is_postgres_family(db_type: &str) -> bool {
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
fn schema_picker_engine(db_type: DatabaseType) -> bool {
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
fn qualified_display(schema: &str, table: &str) -> String {
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
fn table_pref_key(schema: &str, table: &str) -> String {
    qualified_display(schema, table)
}

/// Quoted, schema-qualified relation name for SQL. An empty schema yields the
/// unqualified `"table"` the pre-R26 code produced.
fn table_ref(db_type: DatabaseType, schema: &str, table: &str) -> String {
    qualified_table_name(Some(db_type), Some(schema), table)
}

/// Uppercase hex for the bytes of `s` (the fallback when a binary cell is not
/// already in the kernel's `0x…` form).
fn hex_of_bytes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// The hex digits of a `0x…` / `\x…` binary rendering, when `s` is one. The
/// kernel hands binary columns to the UI as `0x<hex>`, so recovering the bytes
/// here keeps a copied row from being hex-encoded a second time.
fn binary_hex_digits(s: &str) -> Option<&str> {
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
fn binary_literal(hex: &str, db_type: Option<&str>) -> String {
    if db_type.map(is_postgres_family).unwrap_or(false) {
        format!("'\\x{hex}'::bytea")
    } else {
        format!("X'{hex}'")
    }
}

/// One element of a PostgreSQL array literal. Strings are quoted; numbers and
/// booleans stay bare so the resulting `ARRAY[…]` infers the right element type.
fn array_element_literal(v: &serde_json::Value) -> String {
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
fn array_literal(s: &str, data_type: &str) -> Option<String> {
    let items: Vec<serde_json::Value> = serde_json::from_str(s).ok()?;
    let ty = data_type.trim();
    let cast = if ty.ends_with("[]") { format!("::{ty}") } else { String::new() };
    if items.is_empty() {
        return Some(format!("'{{}}'{cast}"));
    }
    let lits: Vec<String> = items.iter().map(array_element_literal).collect();
    Some(format!("ARRAY[{}]{cast}", lits.join(", ")))
}

/// Literal used by the copy-row-as-INSERT action: binary columns become a hex
/// literal, array columns a PostgreSQL `ARRAY[…]`, everything else follows the
/// edit layer's rules.
fn insert_literal(v: &Val, data_type: Option<&str>, db_type: Option<&str>) -> String {
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
fn build_insert_sql(
    cfg: &ConnectionConfig,
    schema: &str,
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
            insert_literal(
                &v,
                column_type(app, schema, table, c).as_deref(),
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
    let target = if let Some(ps) = &app.page_state {
        Some((ps.schema.clone(), ps.table.clone()))
    } else if let Some(s) = &app.script {
        s.drilled
            .and_then(|i| s.outcomes.get(i))
            .and_then(|o| guess_table_from_sql(&o.sql))
            .map(|t| (String::new(), t))
    } else {
        app.last_sql
            .as_deref()
            .and_then(guess_table_from_sql)
            .map(|t| (String::new(), t))
    };
    let Some((schema, table)) = target else {
        app.status = t("无法从当前结果确定表名（仅表格浏览与含 FROM 的查询支持 y）").into();
        return;
    };
    let sql = build_insert_sql(&cfg, &schema, &table, &full, &row, app);
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
    fn update_sql(&self) -> String {
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
    reasons.push(format!("WHERE {where_clause}"));
    app.confirm = Some(Confirm {
        sql,
        reasons,
        refresh: true,
        clear_batch: false,
        redis: None,
        mongo: None,
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
    let col_list = cols.iter().map(|c| q(&c.name)).collect::<Vec<_>>().join(", ");
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
            redis: None,
            mongo: None,
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
        redis: None,
        mongo: None,
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
    app.config.entry(&db, &ps.schema, &ps.table).order_by = next.clone();
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
            redis: None,
            mongo: None,
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
            // Show the console output rather than a stale value grid.
            app.redis_value = None;
            app.clear_grid();
            app.cmd_output
                .push(format!("redis[{}]> {cmd}",  app.redis_db));
            app.spawn(
                tx,
                Op::Redis(Box::new(cfg), app.redis_db, cmd),
            );
        }
        Backend::Mongo => {
            // Show the console output rather than a stale document grid.
            app.clear_grid();
            app.page_state = None;
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

// ── CSV import: parsing, decoding, inference and alignment ───────────────────

/// Parse CSV text into rows of fields (RFC 4180, stdlib only). Handles quoted
/// fields, doubled quotes inside a quoted field, embedded delimiters and
/// newlines, LF / CRLF line endings, a leading UTF-8 BOM and blank lines.
fn parse_csv(text: &str, delim: char) -> Vec<Vec<String>> {
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
fn detect_delimiter(text: &str) -> char {
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
fn decode_csv_bytes(bytes: &[u8]) -> (String, String) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "UTF-8".to_string());
    }
    let (cow, _, had_errors) = encoding_rs::GB18030.decode(bytes);
    if !had_errors {
        return (cow.into_owned(), "GB18030/GBK".to_string());
    }
    (String::from_utf8_lossy(bytes).into_owned(), "UTF-8 (lossy)".to_string())
}

/// `YYYY-MM-DD` with a plausible month/day (a cheap sanity check, not a
/// calendar).
fn looks_like_date(s: &str) -> bool {
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
fn looks_like_datetime(s: &str) -> bool {
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
fn infer_col_type(values: &[&str]) -> ColType {
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
fn align_import_columns(
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
fn import_literal(raw: &str, ty: ColType, data_type: &str) -> String {
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
fn import_insert_sql(
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
fn import_chunks<T>(rows: &[T]) -> Vec<&[T]> {
    rows.chunks(IMPORT_CHUNK).collect()
}

/// Expand a leading `~` to `$HOME`.
fn expand_home(path: &str) -> PathBuf {
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
fn human_size(bytes: u64) -> String {
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
fn delim_label(d: char) -> &'static str {
    match d {
        '\t' => "TAB",
        ';' => ";",
        _ => ",",
    }
}

/// Row number (1-based) a stop-mode batch error refers to, when the backend
/// names the failing statement (`Statement N failed: …`).
fn import_row_of_error(base: usize, err: &str, chunk_len: usize) -> usize {
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
fn is_canonical_int(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    if body.is_empty() || !body.bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    body == "0" || !body.starts_with('0')
}

/// A canonical decimal: an integer part without leading zeros and an optional
/// fraction. `1e5` and `.5` stay strings (too easy to confuse with text).
fn is_canonical_float(s: &str) -> bool {
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
fn json_scalar(s: &str) -> serde_json::Value {
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
fn grid_row_object(grid: &Grid, row: &[Val]) -> serde_json::Map<String, serde_json::Value> {
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
fn grid_to_json_array(grid: &Grid) -> String {
    let arr: Vec<serde_json::Value> = grid
        .rows
        .iter()
        .map(|row| serde_json::Value::Object(grid_row_object(grid, row)))
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".to_string())
}

/// NDJSON: one compact JSON object per line.
fn grid_to_json_ndjson(grid: &Grid) -> String {
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
fn markdown_cell(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// Markdown table. NULL renders as `NULL` (the grid's convention) while an empty
/// string stays empty.
fn grid_to_markdown(grid: &Grid) -> String {
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

/// `(schema, table)` for an INSERT export, or `None` when it cannot be
/// determined.
fn export_insert_table(app: &App) -> Option<(String, String)> {
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
fn grid_to_inserts(
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
fn grid_to_batch_inserts(
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
fn batch_insert_sql(
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
                        insert_literal(&v, types.get(ci).and_then(|t| t.as_deref()), Some(cfg.db_type.as_str()))
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
fn render_export_content(
    app: &App,
    grid: &Grid,
    format: ExportFormat,
    table: Option<&(String, String)>,
) -> String {
    match format {
        ExportFormat::Csv => grid_to_csv(grid),
        ExportFormat::JsonArray => grid_to_json_array(grid),
        ExportFormat::JsonNdjson => grid_to_json_ndjson(grid),
        ExportFormat::Markdown => grid_to_markdown(grid),
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

    /// Save the current table's hidden-column set under its
    /// `(database, schema, table)`.
    fn persist_cols(&mut self) {
        let Some(ps) = self.page_state.clone() else {
            return;
        };
        let db = self.current_db();
        self.config
            .entry(&db, &ps.schema, &ps.table)
            .hidden = self.col_hidden.clone();
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

/// `Ctrl-Y`: open the export overlay for the focused result grid.
fn open_export(app: &mut App) {
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
    app.status = if rows > EXPORT_SLOW_ROWS {
        tf("选择导出格式（{} 行，生成可能耗时）", &[&rows])
    } else {
        tf("选择导出格式（{} 行）", &[&rows])
    };
}

/// Format-picker keys for the export overlay.
fn export_key(app: &mut App, k: KeyEvent) {
    let n = EXPORT_FORMATS.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.export_open = false;
            app.status = t("已取消导出").into();
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
        KeyCode::Char(c @ '1'..='6') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < n {
                choose_export_format(app, EXPORT_FORMATS[idx]);
            }
        }
        _ => {}
    }
}

/// Pick a format and move on to the destination prompt.
fn choose_export_format(app: &mut App, format: ExportFormat) {
    let table = if matches!(format, ExportFormat::Insert | ExportFormat::InsertBatch) {
        match export_insert_table(app) {
            Some(t) => Some(t),
            None => {
                app.status = t("无法确定表名，INSERT 导出不可用（先浏览表或含 FROM 的查询）").into();
                app.export_open = false;
                return;
            }
        }
    } else {
        None
    };
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t("留空 = 复制到剪贴板 · 输入路径 = 写入文件"));
    app.export_pending = Some(ExportPending { format, table });
    app.export_path = Some(ta);
    app.export_open = false;
}

/// Destination prompt: blank copies via OSC 52, otherwise writes a file.
fn export_path_key(app: &mut App, k: KeyEvent) {
    let Some(mut ta) = app.export_path.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.export_pending = None;
        app.status = t("已取消导出").into();
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
    let content = render_export_content(app, &grid, pending.format, pending.table.as_ref());
    let input = ta.lines().join("\n");
    let path = input.trim();
    let label = pending.format.label();
    if path.is_empty() {
        let n = content.chars().count();
        match clipboard_copy(&content) {
            Some(p) => {
                app.status = tf(
                    "✓ 已导出 {} 到剪贴板（{} 字符）· 兜底 {}",
                    &[&label, &n, &(p.display())],
                )
            }
            None => {
                app.status = tf("✓ 已导出 {} 到剪贴板（{} 字符）", &[&label, &n])
            }
        }
    } else {
        let expanded = expand_home(path);
        match std::fs::write(&expanded, content.as_bytes()) {
            Ok(_) => {
                app.status = tf("✓ 已导出 {} → {}", &[&label, &(expanded.display())])
            }
            Err(e) => app.status = tf("✗ 写入失败: {}", &[&(e)]),
        }
    }
}

// ── CSV import: entry points and modal keys ──────────────────────────────────

/// Resolve the table an `I` import targets: the table open in the data browser
/// (only while the results pane is focused), else the sidebar's highlighted
/// table. The focus check matters — after browsing a table and returning to the
/// sidebar, the highlighted table may be a different one.
fn import_target_table(app: &App) -> Option<(String, String)> {
    if app.focus == Focus::Preview && app.grid_kind == GridKind::TableData {
        if let Some(ps) = &app.page_state {
            return Some((ps.schema.clone(), ps.table.clone()));
        }
    }
    app.selected_table()
        .map(|t| (app.schema.clone(), t.name.clone()))
}

/// `I`: open the CSV import flow for the current table.
fn open_import_prompt(app: &mut App) {
    if app.backend_kind != Backend::Sql {
        app.status = t("仅 SQL 连接支持 CSV 导入").into();
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
fn import_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut p) = app.import_prompt.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        // Invalidate any plan request still in flight so a slow read cannot pop
        // the preview open after the user cancelled.
        app.import_gen = app.import_gen.wrapping_add(1);
        app.status = t("已取消导入").into();
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
fn import_plan_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut plan) = app.import_plan.take() else {
        return;
    };
    match k.code {
        KeyCode::Esc => {
            app.import_scroll = 0;
            app.status = t("已取消导入").into();
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
            start_import(app, tx, &plan);
            return;
        }
        _ => {}
    }
    app.import_plan = Some(plan);
}

/// Turn the preview plan into a backend job and start it.
fn start_import(app: &mut App, tx: &Tx, plan: &ImportPlan) {
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
fn import_report_key(app: &mut App, k: KeyEvent) {
    if matches!(
        k.code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')
    ) {
        app.import_report = None;
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
    // Overlays are drawn lowest-precedence first so the topmost one on screen is
    // the one the key router actually owns (see `footer_ctx`, which lists the
    // same order).
    if app.col_picker_open {
        render_col_picker(f, f.area(), app);
    }
    if app.recent_open {
        render_recent_tables(f, f.area(), app);
    }
    if app.table_prompt.is_some() {
        render_table_filter(f, f.area(), app);
    }
    if app.mongo_dialog.is_some() {
        render_mongo_dialog(f, f.area(), app);
    }
    if app.redis_prompt.is_some() {
        render_redis_prompt(f, f.area(), app);
    }
    if app.db_picker_open {
        render_db_picker(f, f.area(), app);
    }
    if app.snippet_open {
        render_snippets(f, f.area(), app);
    }
    if app.snippet_name.is_some() {
        render_snippet_name(f, f.area(), app);
    }
    // The completion popup sits just under the editor, over whatever is below.
    if app.completion.is_some() {
        render_completion(f, app);
    }
    if app.result_filter.is_some() {
        render_result_filter(f, f.area(), app);
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
    if app.export_open {
        render_export(f, f.area(), app);
    }
    if app.export_path.is_some() {
        render_export_path(f, f.area(), app);
    }
    if app.import_prompt.is_some() {
        render_import_prompt(f, f.area(), app);
    }
    if app.import_plan.is_some() {
        render_import_plan(f, f.area(), app);
    }
    if app.import_report.is_some() {
        render_import_report(f, f.area(), app);
    }
    if app.help_open {
        render_help(f, f.area(), app);
    }
    if app.edit_dialog.is_some() {
        render_edit_dialog(f, f.area(), app);
    }
    if let Some(confirm) = app.confirm.clone() {
        render_confirm(f, f.area(), &confirm);
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
    let db = if app.selected.is_some() {
        if app.backend_kind == Backend::Redis {
            format!(" · db:{}",  app.redis_db)
        } else if !app.current_db().is_empty() {
            let schema = if app.schema.is_empty() {
                String::new()
            } else {
                format!(".{}",  fix_double_encoding(&app.schema))
            };
            format!(" · db:{}{schema}",  fix_double_encoding(&app.current_db()))
        } else {
            String::new()
        }
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
    // Redis multi-select count (batch operations target the selection).
    if app.backend_kind == Backend::Redis && !app.redis_selected.is_empty() {
        parts.push(tf("已选 {}", &[&(app.redis_selected.len())]));
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
    Confirm,
    EditDialog,
    Help,
    ImportReport,
    ImportPlan,
    ImportPrompt,
    ExportPath,
    ExportPicker,
    FilterPrompt,
    Popup,
    ResultFilter,
    Completion,
    SnippetName,
    Snippets,
    DbPicker,
    RedisPrompt,
    MongoDoc,
    TablePrompt,
    Recent,
    ColPicker,
    ConnPicker,
    NewConn,
    RedisKeys,
    RedisValue,
    MongoDocs,
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

/// Pick the footer group from the app state. The order mirrors the key router
/// exactly (`key` checks confirm / edit-dialog before `browse_key`, and
/// `browse_key` checks its overlays in the order below), so the footer can never
/// describe a different surface than the one the keyboard is actually on.
fn footer_ctx(app: &App) -> FooterCtx {
    let view = if app.confirm.is_some() {
        FooterView::Confirm
    } else if app.edit_dialog.is_some() {
        FooterView::EditDialog
    } else if app.help_open {
        FooterView::Help
    } else if app.import_report.is_some() {
        FooterView::ImportReport
    } else if app.import_plan.is_some() {
        FooterView::ImportPlan
    } else if app.import_prompt.is_some() {
        FooterView::ImportPrompt
    } else if app.export_path.is_some() {
        FooterView::ExportPath
    } else if app.export_open {
        FooterView::ExportPicker
    } else if app.filter_prompt.is_some() {
        FooterView::FilterPrompt
    } else if app.row_popup.is_some() || app.cell_popup.is_some() {
        FooterView::Popup
    } else if app.result_filter.is_some() {
        FooterView::ResultFilter
    } else if app.completion.is_some() {
        FooterView::Completion
    } else if app.snippet_name.is_some() {
        FooterView::SnippetName
    } else if app.snippet_open {
        FooterView::Snippets
    } else if app.db_picker_open {
        FooterView::DbPicker
    } else if app.redis_prompt.is_some() {
        FooterView::RedisPrompt
    } else if app.mongo_dialog.is_some() {
        FooterView::MongoDoc
    } else if app.table_prompt.is_some() {
        FooterView::TablePrompt
    } else if app.recent_open {
        FooterView::Recent
    } else if app.col_picker_open {
        FooterView::ColPicker
    } else if app.page == Page::NewConn {
        FooterView::NewConn
    } else if app.picker_open && app.selected.is_none() {
        FooterView::ConnPicker
    } else if app.backend_kind == Backend::Redis && app.selected.is_some() && app.focus == Focus::Sidebar {
        FooterView::RedisKeys
    } else if app.backend_kind == Backend::Redis && app.grid_kind == GridKind::RedisValue {
        FooterView::RedisValue
    } else if app.backend_kind == Backend::Mongo && app.grid_kind == GridKind::MongoDocs {
        FooterView::MongoDocs
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
        // R20–R22 overlays: import / export / Redis input dialogs. Without
        // these arms the footer fell through to the page's group while an
        // overlay owned the keyboard.
        FooterView::ImportPrompt => vec![("Enter", t("预览")), ("Esc", t("取消"))],
        FooterView::ImportPlan => vec![
            ("Enter", t("导入")),
            ("m", t("追加/覆盖")),
            ("s", t("出错处理")),
            ("↑↓", t("滚动")),
            ("Esc", t("取消")),
        ],
        FooterView::ImportReport => vec![("Enter/Esc", t("关闭"))],
        FooterView::ExportPicker => vec![
            ("↑↓", t("选择")),
            ("Enter", t("确定")),
            ("1-6", t("快选")),
            ("Esc", t("取消")),
        ],
        FooterView::ExportPath => vec![("Enter", t("导出")), ("Esc", t("取消"))],
        FooterView::RedisPrompt => vec![("Enter", t("确认")), ("Esc", t("取消"))],
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
        FooterView::MongoDoc => vec![
            ("Ctrl-S", t("校验并保存")),
            ("Esc", t("取消")),
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
        FooterView::RedisKeys => vec![
            ("↑↓", t("key")),
            ("Space", t("勾选")),
            ("a", t("全选")),
            ("Enter", t("查看值")),
            ("Del", t("批量删")),
            ("x", t("批量TTL")),
            ("m", t("批量改名")),
            ("/", t("匹配模式")),
            ("n", t("更多")),
            ("r", t("重扫")),
            ("d", t("逻辑库")),
            ("Tab", t("命令台")),
        ],
        FooterView::RedisValue => vec![
            ("↑↓", t("行")),
            ("←→", t("列")),
            ("Enter", t("详情")),
            ("e", t("编辑")),
            ("x", t("TTL")),
            ("m", t("重命名")),
            ("n", t("更多")),
            ("Del", t("删 key")),
            ("/", t("搜索")),
        ],
        FooterView::MongoDocs => vec![
            ("↑↓", t("行")),
            ("←→", t("列")),
            ("Enter", t("详情")),
            ("e", t("编辑")),
            ("i", t("插入")),
            ("Del", t("删文档")),
            ("n/p", t("翻页")),
            ("f", t("JSON 过滤")),
            ("/", t("搜索")),
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
        if app.backend_kind == Backend::Redis {
            text.push_str(&tf("▸ {} · {} keys", &[&(truncate_disp(&c.name, 16)), &(app.redis_scan.keys.len())]));
            text.push_str(&format!(" · db{}",  app.redis_db));
            if let Some(v) = &app.redis_value {
                text.push_str(&format!(" · {}",  fix_double_encoding(&v.key_display)));
            }
        } else {
            text.push_str(&tf("▸ {} · {} 表", &[&(truncate_disp(&c.name, 16)), &(app.tables.len())]));
            let db = app.current_db();
            if !db.is_empty() {
                let schema = if app.schema.is_empty() {
                    String::new()
                } else {
                    format!(".{}",  fix_double_encoding(&app.schema))
                };
                text.push_str(&format!(" · {db}{schema}",  db = fix_double_encoding(&db)));
            }
            if let Some(t) = app.selected_table() {
                text.push_str(&format!(" · {}",  fix_double_encoding(&t.name)));
            }
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
    } else if app.backend_kind == Backend::Redis {
        if app.redis_scan.keys.is_empty() {
            t("无 key · Tab 到命令台 · / 匹配模式 · r 重扫")
        } else {
            t("↑↓ 选 key · Enter 查看值 · n 更多 · / 匹配模式\nTab 到命令台 · Ctrl-L 切换模式")
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
            let table_label = fix_double_encoding(&qualified_display(&ps.schema, &ps.table));
            tf(" {}{}.{} · 第 {} 页 · {}–{} / {} · {}{}{} ", &[&(search_marker(app)), &(fix_double_encoding(&app.current_db())), &(table_label), &(ps.page + 1), &(if rows == 0 { 0 } else { offset + 1 }), &(offset + rows), &(total), &(app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default()), &(more), &(page_state_extra(ps))])
        }
        GridKind::Columns => {
            let table = app
                .selected_table()
                .map(|t| fix_double_encoding(&qualified_display(&app.schema, &t.name)))
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
        GridKind::RedisValue => {
            let note = app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default();
            match &app.redis_value {
                Some(v) => tf(
                    " {}Redis · {} · {} · TTL {} · {} · e 编辑 x TTL m 重命名 Del 删除 ",
                    &[&(search_marker(app)), &(fix_double_encoding(&v.key_display)), &(v.redis_type), &(redis_ttl_label(v.ttl)), &(note)],
                ),
                None => tf(" {}Redis value · {} ", &[&(search_marker(app)), &(note)]),
            }
        }
        GridKind::MongoDocs => {
            let Some(ps) = &app.page_state else {
                return t(" 文档 ").into();
            };
            let rows = app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0);
            let offset = ps.page * ps.page_size;
            let total = ps.total.map(|t| tf("共 {} 个", &[&(t)])).unwrap_or_else(|| t("总数未知").into());
            let more = if ps.has_next { t(" · n 下一页") } else { "" };
            let filt = if ps.filter.trim().is_empty() {
                String::new()
            } else {
                tf(" · 过滤 {}", &[&(truncate_disp(&ps.filter, 24))])
            };
            tf(
                " {}{}.{} · 第 {} 页 · {}–{} / {} · {}{}{} ",
                &[&(search_marker(app)), &(fix_double_encoding(&app.current_db())), &(fix_double_encoding(&qualified_display(&ps.schema, &ps.table))), &(ps.page + 1), &(if rows == 0 { 0 } else { offset + 1 }), &(offset + rows), &(total), &(app.grid.as_ref().map(|g| g.note.clone()).unwrap_or_default()), &(more), &(filt)],
            )
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
    // The bar sits on the bottom border, so a zero-height results pane (a tiny
    // terminal squeezes the pane to nothing) has no row to draw it on and
    // `area.height - 1` would underflow.
    if visible > 0 && scrollable_total > visible && inner_w >= 16 && area.height > 0 {
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
    let title = tf(" 表结构 (DDL) · {} · {}/{} 行 · t 返回字段 ", &[&(fix_double_encoding(&table)), &((app.ddl_scroll as usize + inner_h).min(total)), &(total)]);
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

        // Redis connections browse keys, not tables.
        if app.backend_kind == Backend::Redis {
            render_redis_sidebar(f, area, app, &mut lines);
            return;
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
                format!(
                    "{marker}{}{view}",
                    fix_double_encoding(&qualified_display(&app.schema, &t.name))
                ),
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

/// Single-letter type badge shown next to a key in the sidebar.
fn redis_type_badge(t: &str) -> (&'static str, Color) {
    match t.to_ascii_lowercase().as_str() {
        "string" => ("S", Color::Green),
        "list" => ("L", Color::Yellow),
        "set" => ("E", Color::Cyan),
        "zset" => ("Z", Color::Magenta),
        "hash" => ("H", Color::Blue),
        "stream" => ("X", Color::LightRed),
        "rejson-rl" | "json" => ("J", Color::LightYellow),
        _ => ("?", Color::DarkGray),
    }
}

/// Sidebar body for a Redis connection: a `/` pattern row followed by the SCAN
/// key list with type + TTL badges.
fn render_redis_sidebar(f: &mut Frame, area: Rect, app: &App, lines: &mut Vec<Line>) {
    let focused = app.focus == Focus::Sidebar;
    let w = (area.width as usize).saturating_sub(4).max(6);
    // pattern row
    let (mark, text, style) = if app.redis_scan.pattern == "*" {
        (
            "/ ",
            t("/ 匹配模式（SCAN MATCH）").to_string(),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (
            "▸ ",
            format!("/{} · {} keys",  app.redis_scan.pattern,  app.redis_scan.keys.len()),
            Style::default().fg(Color::Yellow),
        )
    };
    lines.push(Line::from(vec![
        Span::styled(mark, Style::default().fg(Color::Yellow)),
        Span::styled(truncate_disp(&text, w), style),
    ]));

    // Header rows: connection + db row + pattern row.
    let cap = (area.height as usize).saturating_sub(5).max(1);
    let n = app.redis_scan.keys.len();
    let sel = app.redis_list.selected();
    let start = sel
        .unwrap_or(0)
        .saturating_sub(cap / 2)
        .min(n.saturating_sub(cap.min(n)));
    for (i, key) in app.redis_scan.keys.iter().enumerate().skip(start).take(cap) {
        let (badge, color) = redis_type_badge(&key.key_type);
        let ttl = if key.ttl >= 0 {
            format!(" {}s",  key.ttl)
        } else {
            String::new()
        };
        let picked = app.redis_selected.contains(&key.key_raw);
        let marker = if sel == Some(i) { "▸" } else { " " };
        let check = if picked { "[x]" } else { "[ ]" };
        let check_style = if picked {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let name_w = w.saturating_sub(8 + ttl.len());
        let name = truncate_disp(&fix_double_encoding(&key.key_display), name_w.max(4));
        let row_style = if sel == Some(i) {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(marker, row_style),
            Span::styled(check, check_style),
            Span::styled(badge, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {name}"), row_style),
            Span::styled(ttl, Style::default().fg(Color::DarkGray)),
        ]));
    }
    if !app.redis_scan.exhausted {
        lines.push(Line::from(Span::styled(
            t("  n 加载更多…"),
            Style::default().fg(Color::DarkGray),
        )));
    } else if n == 0 {
        lines.push(Line::from(Span::styled(
            t("  （无匹配 key）"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let conn = app.selected.as_ref().map(|c| c.name.clone()).unwrap_or_default();
    let title = if app.redis_selected.is_empty() {
        format!(" {} · {} keys ",  conn,  n)
    } else {
        tf(" {} · {} keys · 已选 {} ", &[&(conn), &(n), &(app.redis_selected.len())])
    };
    f.render_widget(
        Paragraph::new(lines.clone()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        ),
        area,
    );
}

fn render_form(f: &mut Frame, area: Rect, app: &mut App) {
    let form = app.form.clone();    let box_w = if app.layout_mode == LayoutMode::Narrow {
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

/// Height and top offset for a list / picker overlay so it always fits inside
/// `area`. `Clear` (and every ratatui widget) panics when asked to draw outside
/// the buffer, and a tiny terminal makes the naive `area.height - 2` shrink
/// below the three-row border minimum, so the box is clamped to the area here.
/// Center a `w × h` overlay box inside `area`, clamping both dimensions to the
/// area first. ratatui's widgets (and `Clear` in particular) panic when asked to
/// draw outside the buffer, and a tiny terminal can make a fixed overlay taller
/// than the screen, so every centered overlay goes through here.
fn centered_overlay(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn overlay_list_box(rows: usize, area: Rect) -> (u16, u16) {
    if area.height == 0 {
        return (area.y, 0);
    }
    let want = (rows as u16).saturating_add(2);
    let h = want.min(area.height).max(3.min(area.height));
    let y = area.y + area.height.saturating_sub(h) / 2;
    (y, h)
}

fn render_conn_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        56
    });
    let (y, h) = overlay_list_box(app.connections.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
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
    let (y, h) = overlay_list_box(entries.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
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
        _ if !app.schemas.is_empty() => t(" 模式 / 数据库 · ↑↓ Enter · Esc 关 "),
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
    let (y, h) = overlay_list_box(app.snippets.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
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
    let (y, h) = overlay_list_box(grid.columns.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
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
    let (y, h) = overlay_list_box(app.recent_tables.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
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
        .map(|(db, schema, table)| {
            let here = *db == cur_db;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if here { "● " } else { "○ " },
                    Style::default().fg(if here { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(
                    fix_double_encoding(&qualified_display(schema, table)),
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
            // Identifiers are shown decoded (a name stored through a latin1
            // connection is CP1252 mojibake); `item.text` stays raw so the
            // accepted fragment is still the name the server actually knows.
            let shown = fix_double_encoding(&item.text);
            Line::from(vec![
                Span::styled(format!("{:<room$}",  truncate_disp(&shown, room)), style),
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
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
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
                        truncate_disp(
                            &d.keys
                                .iter()
                                .map(|k| fix_double_encoding(k))
                                .collect::<Vec<_>>()
                                .join(", "),
                            inner_w.saturating_sub(6),
                        ),
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
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(" ✎ 编辑 {}.{} ", &[&(fix_double_encoding(&d.db)), &(fix_double_encoding(&qualified_display(&d.schema, &d.table)))]),
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
                tf("新增一行到 {}.{}", &[&(fix_double_encoding(&d.db)), &(fix_double_encoding(&qualified_display(&d.schema, &d.table)))]),
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
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(" ➕ 插入 {}.{} ", &[&(fix_double_encoding(&d.db)), &(fix_double_encoding(&qualified_display(&d.schema, &d.table)))]),
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

fn render_redis_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(74)
        }
    };
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = app
        .redis_prompt
        .as_ref()
        .map(|p| p.title.clone())
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {} · {} ", truncate_disp(&title, 40), t("Enter 确认 · Esc 取消")),
            Style::default().fg(Color::Yellow),
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let ta_h = inner.height.saturating_sub(2).max(1);
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
        height: inner.height.saturating_sub(ta_h),
    };
    if let Some(p) = app.redis_prompt.as_mut() {
        p.input.set_block(Block::default());
        f.render_widget(&p.input, ta_area);
    }
    if hint_area.height > 0 {
        let hint = match app.redis_prompt.as_ref().map(|p| p.kind) {
            Some(RedisPromptKind::Pattern) => t("SCAN MATCH 模式，例 app:* · 支持 * ? []"),
            Some(RedisPromptKind::Ttl) => t("秒数；-1 = 持久化，0 = 立即删除"),
            Some(RedisPromptKind::Rename) => t("新 key 名（已存在的 key 会被覆盖）"),
            Some(RedisPromptKind::StringValue) => t("新的 string 内容（支持多行）"),
            Some(RedisPromptKind::HashField) => t("新的 hash 字段值"),
            Some(RedisPromptKind::BatchTtl) => t("秒数；-1 = 持久化，0 = 立即删除"),
            Some(RedisPromptKind::BatchRenamePrefix) => t("旧前缀=新前缀，例 app: = new:"),
            Some(RedisPromptKind::BatchConfirm) => t("输入 key 数或 YES 以确认删除"),
            None => "",
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            hint_area,
        );
    }
}

/// The MongoDB document JSON editor: a full-height text area plus a validation
/// / hint line at the bottom.
fn render_mongo_dialog(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(d) = app.mongo_dialog.clone() else {
        return;
    };
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 34 {
            area.width
        } else {
            avail.min(86)
        }
    };
    let h = area.height.saturating_sub(2).max(5);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = match d.mode {
        MongoDocMode::Edit => tf(
            " ✎ 编辑文档 {}.{} ",
            &[&fix_double_encoding(&d.db), &fix_double_encoding(&d.collection)],
        ),
        MongoDocMode::Insert => tf(
            " ➕ 插入文档 {}.{} ",
            &[&fix_double_encoding(&d.db), &fix_double_encoding(&d.collection)],
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let bottom_h = 2u16.min(inner.height.saturating_sub(1));
    let ta_h = inner.height.saturating_sub(bottom_h).max(1);
    let ta_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: ta_h,
    };
    if let Some(dd) = app.mongo_dialog.as_mut() {
        dd.editor.set_block(Block::default());
        f.render_widget(&dd.editor, ta_area);
    }
    let mut lines: Vec<Line> = Vec::new();
    if let Some(err) = &d.error {
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(Color::Red),
        )));
    } else if d.mode == MongoDocMode::Edit {
        lines.push(Line::from(Span::styled(
            t("_id 不可修改 · Ctrl-S 预览 diff 后确认"),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            t("输入 JSON 对象，可省略 _id"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(Span::styled(
        t("Ctrl-S 校验并保存 · Esc 取消 · 方向键移动"),
        Style::default().fg(Color::DarkGray),
    )));
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + ta_h,
        width: inner.width,
        height: inner.height.saturating_sub(ta_h),
    };
    f.render_widget(Paragraph::new(lines), hint_area);
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
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = if app.grid_kind == GridKind::MongoDocs {
        t(" MongoDB 过滤 (JSON) · Enter 应用 · Esc 取消 · 留空清除 ")
    } else {
        t(" WHERE 过滤 · Enter 应用 · Esc 取消 · 留空清除 ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
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
        let hints = if app.grid_kind == GridKind::MongoDocs {
            vec![
                Line::from(Span::styled(
                    t("JSON: {\"age\": {\"$gt\": 30}} · {\"name\": \"Ada\"}"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    t("运算符: $eq $gt $lt $in $regex $exists · 留空 = 全部"),
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    t("语法: = != <> > < >= <= LIKE IN BETWEEN IS NULL · AND/OR · 字符串单引号"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    t("MySQL 反引号 `col` · PG 双引号 \"col\"（区分大小写）"),
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        };
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
    ("I", "导入 CSV 到当前表（预览 + 追加/覆盖确认）"),
    ("d", "数据库 / 模式列表（PG 等支持 schema 的连接；浮层内 r 刷新）"),
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
    ("Ctrl-Y", "导出当前结果（CSV / JSON / NDJSON / Markdown / INSERT）"),
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
    ("— Redis key 浏览器 —", ""),
    ("↑ ↓ / Enter", "选择 key / 查看 value"),
    ("Space / Shift+↑↓", "勾选 key / 范围选（a 全选已加载）"),
    ("/", "编辑 SCAN MATCH 模式（留空 = 全部）"),
    ("n / End", "加载下一 SCAN 页"),
    ("r", "以当前模式重扫"),
    ("[ ]", "切换逻辑 db"),
    ("Del / x / m", "批量删除 / 设 TTL / 前缀重命名选中 key（均确认）"),
    ("y", "复制选中的 key 名（每行一个）"),
    ("value: e x m Del", "编辑 string·hash 字段 / TTL / 重命名 / 删除 key（均确认）"),
    ("value: n", "大集合继续加载 200 项"),
    ("— MongoDB 文档浏览器 —", ""),
    ("Enter", "浏览 collection 文档（JSON 网格）"),
    ("n / p", "文档翻页"),
    ("f", "JSON 过滤（如 {\"age\": {\"$gt\": 30}}，留空清除）"),
    ("e / i / Del", "编辑 / 插入 / 删除文档（均确认，_id 不可改）"),
    ("r", "查看 collection 索引"),
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
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
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
                        format!("{:<key_w$}", t(k)),
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
    let box_area = centered_overlay(area, w, h);
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
    let sql_room = (box_area.height as usize).saturating_sub(confirm.reasons.len() + 5);
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

// ── CSV import overlays ──────────────────────────────────────────────────────

fn render_import_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(78)
        }
    };
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" 导入 CSV · 输入文件路径 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
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
    let error = app
        .import_prompt
        .as_ref()
        .and_then(|p| p.error.clone());
    let target = app
        .import_prompt
        .as_ref()
        .map(|p| format!("{}.{}",  p.db,  qualified_display(&p.schema, &p.table)))
        .unwrap_or_default();
    if let Some(p) = app.import_prompt.as_mut() {
        p.input.set_block(Block::default());
        f.render_widget(&p.input, ta_area);
    }
    if hint_h > 0 {
        let first = match error {
            Some(e) => Line::from(Span::styled(
                format!("✗ {e}"),
                Style::default().fg(Color::Red),
            )),
            None => Line::from(Span::styled(
                t("~ 展开为 $HOME · UTF-8/GBK 自动探测 · 首行视为表头"),
                Style::default().fg(Color::DarkGray),
            )),
        };
        let second = Line::from(Span::styled(
            tf("目标表: {}", &[&target]),
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(Paragraph::new(vec![first, second]), hint_area);
    }
}

/// Build the preview lines for an import plan.
fn import_plan_lines(plan: &ImportPlan) -> Vec<PopupLine> {
    let plain = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Yellow);
    let bad = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let mut lines: Vec<PopupLine> = Vec::new();
    lines.push(PopupLine {
        text: tf(
            "目标表: {}.{}",
            &[&plan.db, &qualified_display(&plan.schema, &plan.table)],
        ),
        style: head,
    });
    lines.push(PopupLine {
        text: tf(
            "文件: {} ({})",
            &[&plan.path.display(), &human_size(plan.file_size)],
        ),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "编码 {} · 分隔符 {} · 数据行 {}",
            &[&plan.encoding, &delim_label(plan.delimiter), &plan.rows.len()],
        ),
        style: plain,
    });
    let mode_label = if plan.mode == ImportMode::Overwrite {
        t("覆盖（先清空表）")
    } else {
        t("追加")
    };
    let err_label = if plan.on_error == ImportOnError::Skip {
        t("跳过继续")
    } else {
        t("遇错停止")
    };
    lines.push(PopupLine {
        text: tf(
            "模式 {}（m 切换） · 错误行 {}（s 切换）",
            &[&mode_label, &err_label],
        ),
        style: if plan.mode == ImportMode::Overwrite {
            bad
        } else {
            warn
        },
    });
    if let Some(err) = &plan.error {
        lines.push(PopupLine {
            text: format!("✗ {err}"),
            style: bad,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "列映射（{} 列 · {} 缺失）",
            &[&plan.columns.len(), &plan.missing.len()],
        ),
        style: head,
    });
    for c in &plan.columns {
        let src = match c.src {
            Some(i) => plan.headers.get(i).cloned().unwrap_or_default(),
            None => "—".to_string(),
        };
        let suffix = if c.src.is_none() {
            t("  ⚠ 缺失→默认")
        } else {
            ""
        };
        lines.push(PopupLine {
            text: format!("  {} ← {} · {}{}", c.name, src, c.ty.label(), suffix),
            style: if c.src.is_none() { warn } else { plain },
        });
    }
    if !plan.extra.is_empty() {
        lines.push(PopupLine {
            text: tf("  ⚠ 多余列: {}", &[&plan.extra.join(", ")]),
            style: bad,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "预览（前 {} 行）",
            &[&plan.rows.len().min(IMPORT_SAMPLE_ROWS)],
        ),
        style: head,
    });
    for row in plan.rows.iter().take(IMPORT_SAMPLE_ROWS) {
        let cells: Vec<String> = plan
            .columns
            .iter()
            .map(|c| match c.src {
                Some(i) => row.get(i).cloned().unwrap_or_else(|| "NULL".into()),
                None => "NULL".into(),
            })
            .collect();
        lines.push(PopupLine {
            text: format!("  {}", cells.join(" | ")),
            style: dim,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: if plan.error.is_some() {
            t("Esc 取消（存在错误，无法导入）").to_string()
        } else {
            t("Enter 开始导入 · m 追加/覆盖 · s 遇错停止/跳过 · ↑↓ 滚动 · Esc 取消").to_string()
        },
        style: warn,
    });
    lines
}

fn render_import_plan(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(plan) = app.import_plan.clone() else {
        return;
    };
    let lines = import_plan_lines(&plan);
    let w = {
        let avail = area.width.saturating_sub(2);
        if avail < 24 {
            area.width
        } else {
            avail.min(96)
        }
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;
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
    let max_h = area.height.saturating_sub(2).max(3);
    let h = ((total as u16) + 2).min(max_h);
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
    f.render_widget(Clear, box_area);
    let max_scroll = total.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    let scroll = app.import_scroll.min(max_scroll);
    app.import_scroll = scroll;
    let (style, title) = if plan.error.is_some() {
        (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            t(" ⚠ CSV 导入 · 无法导入 "),
        )
    } else if plan.mode == ImportMode::Overwrite {
        (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            t(" ⚠ CSV 导入 · 覆盖确认 "),
        )
    } else {
        (Style::default().fg(Color::Cyan), t(" CSV 导入预览 "))
    };
    f.render_widget(
        Paragraph::new(body).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(style),
        ),
        box_area,
    );
}

fn render_import_report(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(rep) = app.import_report.clone() else {
        return;
    };
    let plain = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Yellow);
    let bad = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let mut lines: Vec<PopupLine> = Vec::new();
    let mode_label = if rep.mode == ImportMode::Overwrite {
        t("覆盖")
    } else {
        t("追加")
    };
    lines.push(PopupLine {
        text: tf(
            "目标表: {}",
            &[&qualified_display(&rep.schema, &rep.table)],
        ),
        style: head,
    });
    lines.push(PopupLine {
        text: tf("模式: {}", &[&mode_label]),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "总行数 {} · 成功 {} · 跳过 {} · 耗时 {}ms",
            &[&rep.total, &rep.inserted, &rep.skipped.len(), &rep.elapsed_ms],
        ),
        style: if rep.ok() { plain } else { warn },
    });
    if let Some((row, err)) = &rep.aborted {
        lines.push(PopupLine {
            text: tf("✗ 中止于第 {} 行: {}", &[&row, &err]),
            style: bad,
        });
    }
    if !rep.skipped.is_empty() {
        lines.push(PopupLine {
            text: String::new(),
            style: plain,
        });
        lines.push(PopupLine {
            text: tf("跳过的行（{}）", &[&rep.skipped.len()]),
            style: head,
        });
        for (row, err) in rep.skipped.iter().take(50) {
            lines.push(PopupLine {
                text: tf("  第 {} 行: {}", &[&row, &err]),
                style: dim,
            });
        }
        if rep.skipped.len() > 50 {
            lines.push(PopupLine {
                text: tf("  … 其余 {} 行", &[&(rep.skipped.len() - 50)]),
                style: dim,
            });
        }
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: t("Enter/Esc 关闭").to_string(),
        style: warn,
    });

    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(84)
        }
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;
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
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let style = if rep.ok() {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };
    let title = if rep.ok() {
        t(" CSV 导入完成 ")
    } else {
        t(" ⚠ CSV 导入未完成 ")
    };
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(style),
        ),
        box_area,
    );
}

// ── export overlays ──────────────────────────────────────────────────────────

fn render_export(f: &mut Frame, area: Rect, app: &mut App) {
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 30 {
            area.width
        } else {
            avail.min(76)
        }
    };
    let h = (EXPORT_FORMATS.len() as u16 + 3).min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let rows = active_grid(app).map(|g| g.rows.len()).unwrap_or(0);
    let title = if rows > EXPORT_SLOW_ROWS {
        tf(" 导出结果 · {} 行（生成可能耗时）· Esc 取消 ", &[&rows])
    } else {
        tf(" 导出结果 · {} 行 · ↑↓ Enter · Esc ", &[&rows])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let items: Vec<ListItem> = EXPORT_FORMATS
        .iter()
        .enumerate()
        .map(|(i, fmt)| {
            let text = format!("{}. {} — {}", i + 1, fmt.label(), fmt.description());
            ListItem::new(Line::from(Span::styled(text, Style::default())))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut st = app.export_list.clone();
    f.render_stateful_widget(list, inner, &mut st);
    app.export_list = st;
}

fn render_export_path(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(pending) = app.export_pending.as_ref() else {
        return;
    };
    let fmt = pending.format;
    let w = {
        let avail = area.width.saturating_sub(4);
        if avail < 24 {
            area.width
        } else {
            avail.min(78)
        }
    };
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(tf(" 导出 {} · 目标 ", &[&fmt.label()]))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
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
    if let Some(ta) = app.export_path.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, ta_area);
    }
    if hint_h > 0 {
        let hints = vec![
            Line::from(Span::styled(
                t("留空 = 复制到剪贴板（OSC52）· 输入路径 = 写入文件（支持 ~）"),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                t("Enter 确认 · Esc 取消"),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hints), hint_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `LocalBackend` on a throwaway store so the render / key layers can be
    /// exercised headlessly. Opened once per test process and shared; the tests
    /// below never spawn an op, they only render.
    fn test_backend() -> Arc<LocalBackend> {
        use std::sync::OnceLock;
        static B: OnceLock<Arc<LocalBackend>> = OnceLock::new();
        B.get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("dbxt-render-test-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            Arc::new(
                rt.block_on(LocalBackend::open(&dir.join("dbx.db")))
                    .expect("open test backend"),
            )
        })
        .clone()
    }

    fn test_app() -> App {
        App::new(
            test_backend(),
            TuiConfig::default(),
            None,
            false,
            None,
            DragPan::Off,
        )
    }

    /// A connection config for a given driver, used to exercise the Redis / Mongo
    /// view selection without opening a real socket.
    fn test_conn(db_type: &str) -> ConnectionConfig {
        new_connection_config(
            format!("id-{db_type}"),
            format!("test-{db_type}"),
            parse_database_type(db_type).unwrap(),
            "127.0.0.1".into(),
            1,
            "u".into(),
            "p".into(),
            None,
            false,
            None,
        )
        .unwrap()
    }

    /// Draw the whole UI into a headless buffer. Returns the rendered text rows
    /// so a test can assert what actually reached the screen.
    fn draw(app: &mut App, w: u16, h: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
        term.draw(|f| ui(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect()
    }

    fn sample_grid() -> Grid {
        Grid {
            columns: (0..8).map(|i| format!("column_{i}")).collect(),
            rows: (0..4)
                .map(|r| {
                    (0..8)
                        .map(|c| Val::Text(format!("r{r}c{c}")))
                        .collect::<Vec<_>>()
                })
                .collect(),
            note: String::new(),
        }
    }

    #[test]
    fn extreme_sizes_do_not_panic() {
        let mut app = test_app();
        app.grid_kind = GridKind::TableData;
        app.set_grid(sample_grid());
        app.focus = Focus::Preview;
        // 40×12 (phone-ish), 250×70 (huge), and a few degenerate heights where
        // the results pane is squeezed to zero rows.
        for (w, h) in [(40u16, 12u16), (250, 70), (20, 6), (40, 2), (18, 1), (1, 1)] {
            draw(&mut app, w, h);
        }
    }

    fn redis_sample_view() -> RedisValueView {
        let hash = RedisValue {
            key_display: "app:user:42".into(),
            key_raw: base64_encode(b"app:user:42"),
            ttl: 60,
            redis_type: "hash".into(),
            data: RedisValueData::Hash {
                items: (0..30)
                    .map(|i| dbx_core::db::redis_driver::RedisHashItem {
                        field: RedisBlob {
                            raw_base64: base64_encode(format!("field_{i}").as_bytes()),
                            encoding: RedisBlobEncoding::Utf8,
                        },
                        value: RedisBlob {
                            raw_base64: base64_encode(format!("value_{i}").as_bytes()),
                            encoding: RedisBlobEncoding::Utf8,
                        },
                        field_ttl: Some(-1),
                    })
                    .collect(),
                total: 30,
                scan_cursor: Some(30),
            },
        };
        redis_value_view(hash)
    }

    /// The R20–R22 views (Redis key list / value grid, Mongo document grid) at the
    /// two acceptance sizes plus degenerate ones. The Redis sidebar is long enough
    /// to need SCAN paging, the value grid is wide enough to need the horizontal
    /// scrollbar, and the Mongo grid has nested-object cells.
    #[test]
    fn redis_and_mongo_views_render_at_extreme_sizes() {
        let sizes = [(40u16, 12u16), (250, 70), (20, 6), (40, 2), (1, 1)];
        let mut app = test_app();
        app.picker_open = false;
        app.selected = Some(test_conn("redis"));
        app.backend_kind = Backend::Redis;
        app.redis_scan.keys = (0..40)
            .map(|i| RedisKeyInfo {
                key_display: format!("app:key:{i}"),
                key_raw: base64_encode(format!("app:key:{i}").as_bytes()),
                key_type: "hash".into(),
                ttl: -1,
                size: 12,
                value_preview: String::new(),
            })
            .collect();
        app.redis_scan.total = 40;
        app.redis_scan.exhausted = false;
        app.focus = Focus::Sidebar;
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }

        let view = redis_sample_view();
        app.grid_kind = GridKind::RedisValue;
        app.set_grid(view.grid.clone());
        app.redis_value = Some(view);
        app.focus = Focus::Preview;
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }

        let docs: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "_id": i,
                    "name": format!("n{i}"),
                    "nested": {"a": 1, "b": "x"},
                    "tags": [1, 2, 3],
                })
            })
            .collect();
        app.backend_kind = Backend::Mongo;
        app.selected = Some(test_conn("mongodb"));
        app.grid_kind = GridKind::MongoDocs;
        app.set_grid(mongo_docs_grid(&docs));
        app.mongo_docs = docs;
        app.page_state = Some(PageState {
            table: "coll".into(),
            schema: String::new(),
            table_type: None,
            page: 0,
            page_size: MONGO_PAGE,
            total: Some(30),
            has_next: false,
            filter: String::new(),
            order_by: None,
        });
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
    }

    /// One overlay fixture for [`overlays_render_at_extreme_sizes`].
    type OverlayCase = (&'static str, Box<dyn Fn(&mut App)>);

    /// Every modal overlay that can sit over a browse page, rendered at the two
    /// acceptance sizes and at a degenerate one. `Clear` panics on an out-of-
    /// bounds rect, so this is the guard for the whole overlay family.
    #[test]
    fn overlays_render_at_extreme_sizes() {
        let sizes = [(40u16, 12u16), (250, 70), (20, 6), (1, 1)];
        let mut app = test_app();
        app.picker_open = false;
        app.selected = Some(test_conn("mysql"));
        app.backend_kind = Backend::Sql;
        app.grid_kind = GridKind::TableData;
        app.set_grid(sample_grid());
        app.focus = Focus::Preview;

        let reset = |app: &mut App| {
            app.help_open = false;
            app.export_open = false;
            app.export_path = None;
            app.export_pending = None;
            app.import_prompt = None;
            app.import_plan = None;
            app.import_report = None;
            app.redis_prompt = None;
            app.mongo_dialog = None;
            app.confirm = None;
            app.edit_dialog = None;
            app.completion = None;
            app.cell_popup = None;
            app.row_popup = None;
            app.db_picker_open = false;
            app.col_picker_open = false;
            app.recent_open = false;
            app.snippet_open = false;
            app.snippet_name = None;
            app.table_prompt = None;
            app.result_filter = None;
            app.filter_prompt = None;
        };

        let cases: Vec<OverlayCase> = vec![
            ("help", Box::new(|a| a.help_open = true)),
            ("export-picker", Box::new(|a| a.export_open = true)),
            (
                "export-path",
                Box::new(|a| {
                    a.export_pending = Some(ExportPending {
                        format: ExportFormat::Csv,
                        table: None,
                    });
                    a.export_path = Some(TextArea::default());
                }),
            ),
            (
                "import-prompt",
                Box::new(|a| {
                    a.import_prompt = Some(ImportPrompt {
                        input: TextArea::default(),
                        table: "t".into(),
                        schema: String::new(),
                        db: "d".into(),
                        error: None,
                    })
                }),
            ),
            (
                "import-plan",
                Box::new(|a| {
                    a.import_plan = Some(Box::new(ImportPlan {
                        path: PathBuf::from("/tmp/x.csv"),
                        file_size: 123,
                        encoding: "UTF-8".into(),
                        delimiter: ',',
                        headers: vec!["a".into(), "b".into()],
                        rows: vec![vec!["1".into(), "2".into()]],
                        table: "t".into(),
                        schema: String::new(),
                        db: "d".into(),
                        columns: vec![ImportCol {
                            name: "a".into(),
                            src: Some(0),
                            ty: ColType::Int,
                            data_type: "int".into(),
                        }],
                        extra: Vec::new(),
                        missing: vec!["b".into()],
                        mode: ImportMode::Append,
                        on_error: ImportOnError::Stop,
                        error: None,
                    }))
                }),
            ),
            (
                "import-report",
                Box::new(|a| {
                    a.import_report = Some(Box::new(ImportReport {
                        table: "t".into(),
                        schema: String::new(),
                        mode: ImportMode::Append,
                        total: 1,
                        inserted: 1,
                        skipped: vec![(1, "bad".into())],
                        aborted: None,
                        elapsed_ms: 5,
                    }))
                }),
            ),
            (
                "redis-prompt",
                Box::new(|a| {
                    a.redis_prompt = Some(RedisPrompt {
                        kind: RedisPromptKind::Rename,
                        title: "t".into(),
                        key_display: "k".into(),
                        key_raw: "aw==".into(),
                        field: String::new(),
                        batch: Vec::new(),
                        input: TextArea::default(),
                    })
                }),
            ),
            (
                "mongo-dialog",
                Box::new(|a| {
                    a.mongo_dialog = Some(MongoDocDialog {
                        mode: MongoDocMode::Insert,
                        db: "d".into(),
                        collection: "c".into(),
                        original: serde_json::json!({}),
                        id: String::new(),
                        editor: TextArea::from(vec!["{", "}"]),
                        error: None,
                    })
                }),
            ),
            (
                "confirm",
                Box::new(|a| {
                    a.confirm = Some(Confirm {
                        sql: "DELETE FROM t".into(),
                        reasons: vec!["no WHERE".into()],
                        refresh: false,
                        clear_batch: false,
                        redis: None,
                        mongo: None,
                    })
                }),
            ),
            (
                "edit-dialog",
                Box::new(|a| {
                    a.edit_dialog = Some(EditDialog {
                        kind: EditKind::Update,
                        cfg: Box::new(test_conn("mysql")),
                        db: "d".into(),
                        schema: String::new(),
                        table: "t".into(),
                        column: "c".into(),
                        data_type: Some("int".into()),
                        old: Val::Text("1".into()),
                        new_input: TextArea::default(),
                        where_clause: "id = 1".into(),
                        keys: vec!["id".into()],
                        no_pk: false,
                        insert_sql: String::new(),
                        insert_preview: Vec::new(),
                    })
                }),
            ),
            (
                "completion",
                Box::new(|a| {
                    a.completion = Some(Completion {
                        items: vec![CompletionItem {
                            text: "users".into(),
                            kind: 'T',
                        }],
                        sel: 0,
                        replace: 0,
                    })
                }),
            ),
            (
                "cell-popup",
                Box::new(|a| {
                    a.cell_popup = Some(CellPopup {
                        title: "cell".into(),
                        lines: vec![PopupLine {
                            text: "x".into(),
                            style: Style::default(),
                        }],
                        scroll: 0,
                    })
                }),
            ),
            (
                "row-popup",
                Box::new(|a| {
                    a.row_popup = Some(RowPopup {
                        title: "row".into(),
                        lines: vec![PopupLine {
                            text: "x".into(),
                            style: Style::default(),
                        }],
                        scroll: 0,
                    })
                }),
            ),
            ("db-picker", Box::new(|a| a.db_picker_open = true)),
            ("col-picker", Box::new(|a| a.col_picker_open = true)),
            ("recent", Box::new(|a| a.recent_open = true)),
            ("snippets", Box::new(|a| a.snippet_open = true)),
            ("snippet-name", Box::new(|a| a.snippet_name = Some(TextArea::default()))),
            ("table-prompt", Box::new(|a| a.table_prompt = Some(TextArea::default()))),
            ("result-filter", Box::new(|a| a.result_filter = Some(TextArea::default()))),
            ("filter-prompt", Box::new(|a| a.filter_prompt = Some(TextArea::default()))),
        ];

        for (name, set) in &cases {
            reset(&mut app);
            set(&mut app);
            for (w, h) in sizes {
                draw(&mut app, w, h);
            }
            let _ = name;
        }
    }

    /// The new-connection form on its own page, at tiny sizes (it clamps its own
    /// box, so this is the regression guard for that path).
    #[test]
    fn new_connection_form_renders_at_extreme_sizes() {
        let mut app = test_app();
        app.page = Page::NewConn;
        for (w, h) in [(40u16, 12u16), (250, 70), (20, 6), (1, 1)] {
            draw(&mut app, w, h);
        }
    }

    /// `y` in a Redis / Mongo grid must copy the focused row even when a result
    /// search is active: the search keeps a display→source map, and reading the
    /// row from the filtered grid with the full-grid index used to fail.
    #[test]
    fn focused_row_survives_an_active_result_search() {
        let mut app = test_app();
        app.grid_kind = GridKind::Query;
        app.set_grid(Grid {
            columns: vec!["name".into()],
            rows: vec![
                vec![Val::Text("ada".into())],
                vec![Val::Text("admin".into())],
                vec![Val::Text("bob".into())],
            ],
            note: String::new(),
        });
        assert_eq!(focused_full_row(&app).unwrap()[0].text(), "ada");

        app.result_needle = "admin".into();
        app.rebuild_view();
        app.sel = 0;
        assert_eq!(app.grid.as_ref().unwrap().rows.len(), 1);
        assert_eq!(focused_full_row(&app).unwrap()[0].text(), "admin");

        // A broader search (two matches) still resolves the cursor's row.
        app.result_needle = "a".into();
        app.rebuild_view();
        app.sel = 1;
        assert_eq!(focused_full_row(&app).unwrap()[0].text(), "admin");
    }

    /// `n` before the first SCAN page lands must not queue a second request from
    /// the same cursor: that used to append the first page twice (the fresh-scan
    /// race). The `pending` flag makes the second call a no-op until the reply.
    #[test]
    fn redis_load_more_is_ignored_while_a_page_is_in_flight() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
            let mut app = test_app();
            app.selected = Some(test_conn("redis"));
            app.backend_kind = Backend::Redis;
            start_redis_scan(&mut app, &tx, true);
            assert!(app.redis_scan.pending, "a reset scan is in flight");
            let spawned = app.pending_ops;
            start_redis_scan(&mut app, &tx, false);
            assert_eq!(app.pending_ops, spawned, "the second `n` must not spawn");
            // The reply releases the guard...
            app.redis_scan.pending = false;
            // ...and a reset always supersedes (bumps gen) and re-arms it.
            start_redis_scan(&mut app, &tx, true);
            assert!(app.redis_scan.pending);
        });
    }

    /// A >1 MB text cell and a binary column in all six export formats: none may
    /// panic, and the binary column must become a hex literal in the INSERT
    /// formats so the row round-trips instead of being mangled as a string.
    #[test]
    fn export_handles_huge_and_binary_cells_in_every_format() {
        let mut app = test_app();
        app.selected = Some(test_conn("mysql"));
        app.table_meta = Some(TableMeta {
            table: "t".into(),
            schema: String::new(),
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    data_type: "int".into(),
                    ..Default::default()
                },
                ColumnInfo {
                    name: "payload".into(),
                    data_type: "longblob".into(),
                    ..Default::default()
                },
                ColumnInfo {
                    name: "big".into(),
                    data_type: "text".into(),
                    ..Default::default()
                },
            ],
        });
        let huge = "x".repeat(1_100_000);
        let grid = Grid {
            columns: vec!["id".into(), "payload".into(), "big".into()],
            rows: vec![vec![
                Val::Text("1".into()),
                Val::Text("\u{0}\u{1}raw".into()),
                Val::Text(huge.clone()),
            ]],
            note: String::new(),
        };
        for format in EXPORT_FORMATS {
            let out = render_export_content(&app, &grid, *format, Some(&("".to_string(), "t".to_string())));
            assert!(!out.is_empty(), "{format:?} produced nothing");
        }
        // The blob column is copied as `X'…'`, never as a quoted string.
        let insert = render_export_content(
            &app,
            &grid,
            ExportFormat::Insert,
            Some(&("".to_string(), "t".to_string())),
        );
        assert!(insert.contains("X'0001726177'"), "{insert}");
        // The 1 MB text cell survives verbatim in CSV and Markdown.
        let csv = render_export_content(&app, &grid, ExportFormat::Csv, Some(&("".to_string(), "t".to_string())));
        assert!(csv.contains(&huge));
        let md = render_export_content(&app, &grid, ExportFormat::Markdown, Some(&("".to_string(), "t".to_string())));
        assert!(md.contains(&huge));
    }

    /// An import writes rows, so the session COUNT(*) cache must be dropped —
    /// otherwise a table browsed before the import (imported into from the
    /// sidebar) would show a stale total on its next open.
    #[test]
    fn import_done_invalidates_the_count_cache() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.count_cache.insert("d\u{1}\u{1}t\u{1}".into(), 5);
        app.import_progress = Some((1, 1));
        let rep = ImportReport {
            table: "t".into(),
            schema: String::new(),
            mode: ImportMode::Append,
            total: 1,
            inserted: 1,
            skipped: Vec::new(),
            aborted: None,
            elapsed_ms: 1,
        };
        apply_op_result(&mut app, OpResult::ImportDone(Box::new(rep)), &tx);
        assert!(app.count_cache.is_empty(), "stale COUNT(*) survived an import");
        assert!(app.import_progress.is_none());
        assert!(app.import_report.is_some());
    }

    #[test]
    fn grid_at_zero_height_does_not_panic() {
        // The results pane can be squeezed to zero rows on a tiny terminal; the
        // horizontal scrollbar is drawn on the bottom border and must not do
        // `area.height - 1` arithmetic on a zero-height area.
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = test_app();
        let grid = sample_grid();
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 0,
            };
            render_grid(f, area, &mut app, &grid, GridKind::TableData, " t ");
        })
        .unwrap();
    }

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
        assert_eq!(
            count_cache_key("db", "", "t", ""),
            count_cache_key("db", "", "t", "")
        );
        assert_ne!(
            count_cache_key("db", "", "t", "a = 1"),
            count_cache_key("db", "", "t", "a = 2")
        );
        assert_ne!(
            count_cache_key("db1", "", "t", ""),
            count_cache_key("db2", "", "t", "")
        );
        // The schema is part of the key: `public.orders` ≠ `inv.orders`.
        assert_ne!(
            count_cache_key("db", "public", "orders", ""),
            count_cache_key("db", "inv", "orders", "")
        );
    }

    #[test]
    fn page_state_extra_reports_filter_and_sort() {
        let ps = PageState {
            table: "t".into(),
            schema: String::new(),
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
            schema: String::new(),
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
    fn double_encoding_is_reversed_through_multiple_layers() {
        // The same name written through a latin1 connection *twice*: the server
        // stores the CP1252 form of the already-mojibake string. A single pass
        // stops at the single-mojibake form (it still contains `•`/`™`, i.e.
        // chars > U+00FF, so the one-pass heuristic thinks it succeeded) — which
        // is exactly the `ä¿…ç•™è¡¨` that was still reported in the sidebar.
        let single = "\u{e4}\u{bf}\u{9d}\u{e7}\u{2022}\u{2122}\u{e8}\u{a1}\u{a8}";
        let double = "\u{c3}\u{a4}\u{c2}\u{bf}\u{c2}\u{9d}\u{c3}\u{a7}\u{e2}\u{20ac}\u{a2}\u{e2}\u{201e}\u{a2}\u{c3}\u{a8}\u{c2}\u{a1}\u{c2}\u{a8}";
        assert_eq!(reverse_double_encoding_once(double), single);
        assert_eq!(fix_double_encoding(double), "保留表");
        // Peeling a layer off already-decoded text must not change it again.
        assert_eq!(fix_double_encoding(&fix_double_encoding(double)), "保留表");
    }

    #[test]
    fn double_encoding_leaves_clean_names_alone() {
        // Correctly stored CJK contains chars > U+00FF and must pass through.
        assert_eq!(fix_double_encoding("保留表"), "保留表");
        assert_eq!(fix_double_encoding("users"), "users");
        // A Latin-1 name whose bytes are not valid UTF-8 is left untouched.
        assert_eq!(fix_double_encoding("café"), "café");
        // A lone CP1252 punctuation char maps to an invalid UTF-8 byte and is
        // therefore not treated as mojibake.
        assert_eq!(fix_double_encoding("™"), "™");
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
        // CSV import has a documented sidebar binding.
        assert!(keys.contains(&"I"), "help is missing the CSV import binding");
    }

    #[test]
    fn help_has_no_bare_uppercase_shortcuts() {
        // Regression guard for the R8 keymap: every shortcut must be lowercase,
        // a named key, or a Ctrl/Alt/Shift/F-key combination — never a lone
        // uppercase letter the user has to reach with Shift. `I` is the single
        // deliberate exception: CSV import is rare and deliberate, and lowercase
        // `i` is already quick-insert in the results pane.
        for (key, _) in HELP_ROWS {
            if key.starts_with('—') {
                continue;
            }
            for tok in key.split(['/', ' ', '+']).filter(|t| !t.is_empty()) {
                if tok == "I" {
                    continue;
                }
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
        // A write statement still becomes a plain EXPLAIN: PostgreSQL's
        // `EXPLAIN ANALYZE` would actually run the INSERT/UPDATE/DELETE.
        assert_eq!(
            explain_sql_for("postgres", "INSERT INTO t (a) VALUES (1);").as_deref(),
            Some("EXPLAIN INSERT INTO t (a) VALUES (1)")
        );
        assert_eq!(
            explain_sql_for("postgres", "DELETE FROM t WHERE id = 1;").as_deref(),
            Some("EXPLAIN DELETE FROM t WHERE id = 1")
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

    // ── R22: CSV import ──

    fn col_info(name: &str, ty: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            ..Default::default()
        }
    }

    fn mysql_cfg() -> ConnectionConfig {
        new_connection_config(
            "t".into(),
            "t".into(),
            parse_database_type("mysql").unwrap(),
            "127.0.0.1".into(),
            13306,
            "root".into(),
            String::new(),
            Some("shop".into()),
            false,
            None,
        )
        .unwrap()
    }

    #[test]
    fn parse_csv_handles_quotes_commas_and_newlines() {
        let rows = parse_csv("a,b\n\"x,1\",\"he said \"\"hi\"\"\"\n\"multi\nline\",2\n", ',');
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["a", "b"]);
        assert_eq!(rows[1], vec!["x,1", "he said \"hi\""]);
        assert_eq!(rows[2], vec!["multi\nline", "2"]);
    }

    #[test]
    fn parse_csv_strips_bom_blank_lines_and_crlf() {
        let rows = parse_csv("\u{feff}a,b\r\n1,2\r\n\r\n", ',');
        assert_eq!(rows, vec![vec!["a", "b"], vec!["1", "2"]]);
        // A trailing delimiter keeps the empty field.
        assert_eq!(parse_csv("a,\n", ','), vec![vec!["a", ""]]);
    }

    #[test]
    fn detect_delimiter_prefers_the_densest() {
        assert_eq!(detect_delimiter("a,b,c\n1,2,3"), ',');
        assert_eq!(detect_delimiter("a;b;c\n1;2;3"), ';');
        assert_eq!(detect_delimiter("a\tb\tc"), '\t');
        // Quoted separators do not count.
        assert_eq!(detect_delimiter("\"a,b\";\"c,d\";e"), ';');
        assert_eq!(detect_delimiter("single"), ',');
    }

    #[test]
    fn gbk_bytes_decode_to_chinese_text() {
        let (bytes, _, _) = encoding_rs::GB18030.encode("编号,名称\n1,北京\n");
        let (text, enc) = decode_csv_bytes(&bytes);
        assert_eq!(enc, "GB18030/GBK");
        assert!(text.contains("北京"), "{text}");
        let (utf, enc2) = decode_csv_bytes("名称\n中文\n".as_bytes());
        assert_eq!(enc2, "UTF-8");
        assert!(utf.contains("中文"));
    }

    #[test]
    fn type_inference_covers_common_shapes() {
        assert_eq!(infer_col_type(&["1", "2", "-3"]), ColType::Int);
        assert_eq!(infer_col_type(&["1.5", "2", "-0.25"]), ColType::Float);
        assert_eq!(infer_col_type(&["true", "FALSE"]), ColType::Bool);
        assert_eq!(infer_col_type(&["2026-06-27", "1999-01-02"]), ColType::Date);
        assert_eq!(
            infer_col_type(&["2026-06-27 10:00", "1999-01-02 23:59:59"]),
            ColType::DateTime
        );
        assert_eq!(infer_col_type(&["abc", "1"]), ColType::Text);
        // Empty values are ignored; an all-empty column is text.
        assert_eq!(infer_col_type(&["", "42", " "]), ColType::Int);
        assert_eq!(infer_col_type(&["", ""]), ColType::Text);
    }

    #[test]
    fn header_alignment_matches_missing_and_extra() {
        let headers = vec!["ID".to_string(), "name".to_string(), "junk".to_string()];
        let rows = vec![vec!["1".to_string(), "Ada".to_string(), "x".to_string()]];
        let cols = vec![
            col_info("id", "int"),
            col_info("name", "varchar(20)"),
            col_info("age", "int"),
        ];
        let (mapped, extra, missing) = align_import_columns(&headers, &rows, &cols);
        assert_eq!(mapped.len(), 3);
        assert_eq!(mapped[0].src, Some(0));
        assert_eq!(mapped[0].ty, ColType::Int);
        assert_eq!(mapped[1].src, Some(1));
        assert_eq!(mapped[1].ty, ColType::Text);
        assert_eq!(mapped[2].src, None);
        assert_eq!(extra, vec!["junk"]);
        assert_eq!(missing, vec!["age"]);
    }

    #[test]
    fn import_literal_nulls_bools_and_quotes() {
        assert_eq!(import_literal("", ColType::Text, "varchar(10)"), "NULL");
        assert_eq!(import_literal("42", ColType::Int, "int"), "42");
        assert_eq!(import_literal("3.5", ColType::Float, "double"), "3.5");
        assert_eq!(import_literal("true", ColType::Bool, "tinyint"), "TRUE");
        assert_eq!(import_literal("FALSE", ColType::Bool, "tinyint"), "FALSE");
        assert_eq!(import_literal("O'Brien", ColType::Text, "text"), "'O''Brien'");
        // A numeric target keeps a number bare even when inference said text.
        assert_eq!(import_literal("42", ColType::Text, "int"), "42");
        assert_eq!(
            import_literal("2026-06-27", ColType::Date, "date"),
            "'2026-06-27'"
        );
    }

    #[test]
    fn import_insert_sql_uses_only_present_columns() {
        let cfg = mysql_cfg();
        let cols = vec![
            ImportCol {
                name: "id".into(),
                src: Some(0),
                ty: ColType::Int,
                data_type: "int".into(),
            },
            ImportCol {
                name: "name".into(),
                src: Some(1),
                ty: ColType::Text,
                data_type: "varchar(20)".into(),
            },
            ImportCol {
                name: "age".into(),
                src: None,
                ty: ColType::Text,
                data_type: "int".into(),
            },
        ];
        let sql = import_insert_sql(&cfg, "", "users", &cols, &["7".into(), "Ada".into()]);
        assert_eq!(sql, "INSERT INTO `users` (`id`, `name`) VALUES (7, 'Ada');");
    }

    #[test]
    fn import_chunks_split_at_the_chunk_size() {
        let rows: Vec<usize> = (0..(IMPORT_CHUNK * 2 + 3)).collect();
        let chunks = import_chunks(&rows);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), IMPORT_CHUNK);
        assert_eq!(chunks[1].len(), IMPORT_CHUNK);
        assert_eq!(chunks[2].len(), 3);
    }

    #[test]
    fn import_error_row_reads_the_statement_index() {
        assert_eq!(import_row_of_error(1000, "Statement 3 failed: Duplicate entry", 500), 1003);
        // Unparseable / out-of-range errors fall back to the chunk start.
        assert_eq!(import_row_of_error(1000, "boom", 500), 1001);
        assert_eq!(import_row_of_error(1000, "Statement 999 failed", 500), 1001);
    }

    // ── R22: export formats ──

    #[test]
    fn json_export_keeps_leading_zero_strings() {
        let grid = Grid {
            columns: vec!["zip".into(), "n".into(), "ok".into(), "nil".into()],
            rows: vec![vec![
                Val::Text("0123".into()),
                Val::Text("42".into()),
                Val::Text("true".into()),
                Val::Null,
            ]],
            note: String::new(),
        };
        let out = grid_to_json_array(&grid);
        assert!(out.contains("\"zip\": \"0123\""), "{out}");
        assert!(out.contains("\"n\": 42"), "{out}");
        assert!(out.contains("\"ok\": true"), "{out}");
        assert!(out.contains("\"nil\": null"), "{out}");
        let nd = grid_to_json_ndjson(&grid);
        assert_eq!(nd.lines().count(), 1);
        assert!(nd.starts_with('{') && nd.trim_end().ends_with('}'));
    }

    #[test]
    fn markdown_export_escapes_pipes_and_newlines() {
        let grid = Grid {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Val::Text("x|y".into()), Val::Null],
                vec![Val::Text("l1\nl2".into()), Val::Text("ok".into())],
            ],
            note: String::new(),
        };
        let md = grid_to_markdown(&grid);
        assert!(md.starts_with("| a | b |\n| --- | --- |\n"), "{md}");
        assert!(md.contains("| x\\|y | NULL |"), "{md}");
        assert!(md.contains("| l1<br>l2 | ok |"), "{md}");
    }

    #[test]
    fn batch_insert_groups_rows() {
        let cfg = mysql_cfg();
        let columns = vec!["id".to_string(), "name".to_string()];
        let types = vec![Some("int".to_string()), Some("varchar(20)".to_string())];
        let rows = vec![
            vec![Val::Text("1".into()), Val::Text("a".into())],
            vec![Val::Text("2".into()), Val::Null],
            vec![Val::Text("3".into()), Val::Text("c".into())],
        ];
        let out = batch_insert_sql(&cfg, "", "t", &columns, &rows, &types, 2);
        assert_eq!(
            out,
            "INSERT INTO `t` (`id`, `name`) VALUES\n(1, 'a'),\n(2, NULL);\nINSERT INTO `t` (`id`, `name`) VALUES\n(3, 'c');\n"
        );
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
        assert_eq!(insert_literal(&Val::Null, None, None), "NULL");
        assert_eq!(insert_literal(&Val::Text(String::new()), None, None), "''");
        assert_eq!(
            insert_literal(&Val::Text("O'Brien".into()), None, None),
            "'O''Brien'"
        );
        assert_eq!(
            insert_literal(&Val::Text("a\\b".into()), None, None),
            "'a\\\\b'"
        );
        // A numeric column keeps a real number bare.
        assert_eq!(
            insert_literal(&Val::Text("42".into()), Some("int"), None),
            "42"
        );
        // A binary column becomes a portable hex literal.
        assert_eq!(
            insert_literal(&Val::Text("\u{0}\u{1}A".into()), Some("varbinary(8)"), Some("mysql")),
            "X'000141'"
        );
        // PostgreSQL bytea needs its own form — `X'…'` is a bit string there.
        assert_eq!(
            insert_literal(&Val::Text("AB".into()), Some("bytea"), Some("postgres")),
            "'\\x4142'::bytea"
        );
    }

    #[test]
    fn insert_literal_recovers_binary_bytes_from_0x_rendering() {
        // The kernel renders bytea/blob cells as `0x<hex>`; re-hexing that text
        // used to write the ASCII of `0x…` instead of the original bytes.
        assert_eq!(
            insert_literal(&Val::Text("0xdeadbeef".into()), Some("bytea"), Some("postgres")),
            "'\\xdeadbeef'::bytea"
        );
        assert_eq!(
            insert_literal(&Val::Text("0xDEADBEEF".into()), Some("blob"), Some("mysql")),
            "X'DEADBEEF'"
        );
        assert_eq!(
            insert_literal(&Val::Text("\\x0a1b".into()), Some("bytea"), Some("postgres")),
            "'\\x0a1b'::bytea"
        );
        // A non-hex `0x…`-looking string still falls back to raw-byte hex.
        assert_eq!(
            insert_literal(&Val::Text("0xzz".into()), Some("bytea"), Some("postgres")),
            "'\\x30787A7A'::bytea"
        );
        assert_eq!(binary_hex_digits("0xdead"), Some("dead"));
        assert_eq!(binary_hex_digits("0xde"), Some("de"));
        assert_eq!(binary_hex_digits("0xdea"), None);
        assert_eq!(binary_hex_digits("plain"), None);
    }

    #[test]
    fn insert_literal_renders_postgres_arrays() {
        // PostgreSQL arrays arrive as JSON; an INSERT needs `ARRAY[…]`, not the
        // JSON text (which the server rejects as a malformed array literal).
        assert_eq!(
            insert_literal(&Val::Text("[\"admin\",\"beta\"]".into()), Some("text[]"), Some("postgres")),
            "ARRAY['admin', 'beta']::text[]"
        );
        assert_eq!(
            insert_literal(&Val::Text("[1,2,3]".into()), Some("integer[]"), Some("postgres")),
            "ARRAY[1, 2, 3]::integer[]"
        );
        assert_eq!(
            insert_literal(&Val::Text("[]".into()), Some("text[]"), Some("postgres")),
            "'{}'::text[]"
        );
        // An element containing a quote or comma is quoted safely.
        assert_eq!(
            insert_literal(&Val::Text("[\"a,b\",\"O'Brien\"]".into()), Some("text[]"), Some("postgres")),
            "ARRAY['a,b', 'O''Brien']::text[]"
        );
        // A non-array column is untouched.
        assert_eq!(
            insert_literal(&Val::Text("[1,2]".into()), Some("jsonb"), Some("postgres")),
            "'[1,2]'"
        );
    }

    #[test]
    fn postgres_family_detection() {
        assert!(is_postgres_family("postgres"));
        assert!(is_postgres_family("PostgreSQL"));
        assert!(is_postgres_family("opengauss"));
        assert!(is_postgres_family("kingbase"));
        assert!(!is_postgres_family("mysql"));
        assert!(!is_postgres_family("sqlite"));
        assert!(!is_postgres_family("redis"));
    }

    #[test]
    fn identifier_quoting_follows_the_dialect() {
        let pg = parse_database_type("postgres").unwrap();
        let my = parse_database_type("mysql").unwrap();
        // PostgreSQL double-quotes; MySQL backticks. Reserved words included.
        assert_eq!(quote_table_identifier(Some(pg), "select"), "\"select\"");
        assert_eq!(quote_table_identifier(Some(pg), "accounts"), "\"accounts\"");
        assert_eq!(quote_table_identifier(Some(my), "select"), "`select`");
    }

    #[test]
    fn binary_type_detection_ignores_length_params() {
        assert!(is_binary_type("BLOB"));
        assert!(is_binary_type("varbinary(255)"));
        assert!(is_binary_type("bytea"));
        assert!(!is_binary_type("varchar(255)"));
        assert!(!is_binary_type("text"));
    }

    fn column(name: &str, ty: &str, nullable: bool, default: Option<&str>, extra: Option<&str>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            is_nullable: nullable,
            column_default: default.map(str::to_string),
            extra: extra.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn insert_template_skips_server_generated_columns() {
        // MySQL auto-increment, PostgreSQL serial, identity and generated.
        assert!(is_server_generated_column(&column("id", "int", false, None, Some("auto_increment"))));
        assert!(is_server_generated_column(&column("id", "bigint", false, None, Some("bigserial"))));
        assert!(is_server_generated_column(&column(
            "id",
            "bigint",
            false,
            None,
            Some("generated by default as identity")
        )));
        assert!(is_server_generated_column(&column(
            "total",
            "numeric",
            false,
            None,
            Some("generated always as (a + b) stored")
        )));
        // A plain nextval default also means the sequence owns the value.
        assert!(is_server_generated_column(&column(
            "id",
            "bigint",
            false,
            Some("nextval('t_id_seq'::regclass)"),
            None
        )));
        // Ordinary columns are not skipped.
        assert!(!is_server_generated_column(&column("email", "text", false, None, None)));
        assert!(!is_server_generated_column(&column("balance", "numeric(12,2)", false, Some("0.00"), None)));
    }

    #[test]
    fn insert_placeholder_is_valid_for_the_column_type() {
        // PostgreSQL NOT NULL columns: the old rule emitted `''`, which the
        // server rejects for boolean / timestamp / numeric.
        assert_eq!(insert_placeholder(&column("active", "boolean", false, None, None)), "FALSE");
        assert_eq!(
            insert_placeholder(&column("created_at", "timestamp with time zone", false, None, None)),
            "CURRENT_TIMESTAMP"
        );
        assert_eq!(insert_placeholder(&column("qty", "integer", false, None, None)), "0");
        assert_eq!(insert_placeholder(&column("meta", "jsonb", false, None, None)), "'{}'");
        assert_eq!(insert_placeholder(&column("tags", "text[]", false, None, None)), "'{}'");
        assert_eq!(insert_placeholder(&column("email", "text", false, None, None)), "''");
        // A declared default is delegated to the server.
        assert_eq!(
            insert_placeholder(&column("balance", "numeric(12,2)", false, Some("0.00"), None)),
            "DEFAULT"
        );
        // Nullable columns stay NULL.
        assert_eq!(insert_placeholder(&column("note", "text", true, None, None)), "NULL");
        // An enum NOT NULL picks its first label.
        let mut mood = column("feeling", "mood", false, None, None);
        mood.enum_values = Some(vec!["happy".into(), "sad".into()]);
        assert_eq!(insert_placeholder(&mood), "'happy'");
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
        let e = cfg.entry("shop", "", "orders");
        e.hidden = ["secret".to_string()].into_iter().collect();
        e.compact = Some(false);
        e.order_by = Some("\"id\" DESC".into());
        cfg.save(&path);
        let back = TuiConfig::load(&path);
        assert_eq!(back.compact, Some(true));
        let p = back.table("shop", "", "orders").unwrap();
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
        assert!(cfg.table("db", "", "t").is_none());
        // A nested entry with every field the wrong type degrades to defaults.
        let p = cfg.table("db3", "", "t").expect("entry kept as defaults");
        assert!(p.hidden.is_empty() && p.compact.is_none() && p.order_by.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_save_merges_concurrent_sessions_and_clears_entries() {
        let path = std::env::temp_dir().join(format!("dbxt-merge-{}.json", Uuid::new_v4()));
        // Session A stores a sort for one table.
        let mut a = TuiConfig::default();
        a.entry("db", "", "a").order_by = Some("\"id\" ASC".into());
        a.save(&path);
        // Session B knows nothing about table `a` (older snapshot) and writes `b`.
        // Its save must not wipe A's entry.
        let mut b = TuiConfig::default();
        b.entry("db", "", "b").hidden = ["x".to_string()].into_iter().collect();
        b.save(&path);
        let after = TuiConfig::load(&path);
        assert!(after.table("db", "", "a").is_some(), "B must not clobber A");
        assert!(after.table("db", "", "b").is_some());
        // Resetting a table to defaults removes its stored entry instead of
        // silently keeping the stale one.
        let mut c = TuiConfig::default();
        c.entry("db", "", "a");
        c.save(&path);
        let cleared = TuiConfig::load(&path);
        assert!(cleared.table("db", "", "a").is_none());
        assert!(cleared.table("db", "", "b").is_some(), "unrelated entry survives");
        let _ = std::fs::remove_file(&path);
    }

    // ── R26: schema-aware browsing ──

    #[test]
    fn schema_picker_engine_is_opt_in() {
        assert!(schema_picker_engine(parse_database_type("postgres").unwrap()));
        assert!(schema_picker_engine(parse_database_type("sqlserver").unwrap()));
        assert!(schema_picker_engine(parse_database_type("oracle").unwrap()));
        // Embedded / single-namespace engines keep the flat list.
        assert!(!schema_picker_engine(parse_database_type("sqlite").unwrap()));
        assert!(!schema_picker_engine(parse_database_type("duckdb").unwrap()));
        assert!(!schema_picker_engine(parse_database_type("mysql").unwrap()));
    }

    #[test]
    fn qualified_table_reference_quotes_the_schema() {
        let pg = parse_database_type("postgres").unwrap();
        let mysql = parse_database_type("mysql").unwrap();
        assert_eq!(table_ref(pg, "inv", "items"), "\"inv\".\"items\"");
        // No schema → the pre-R26 unqualified form, so MySQL / SQLite are unchanged.
        assert_eq!(table_ref(pg, "", "items"), "\"items\"");
        assert_eq!(table_ref(mysql, "", "items"), "`items`");
        // A MySQL database qualifier is valid too.
        assert_eq!(table_ref(mysql, "shop", "items"), "`shop`.`items`");
    }

    #[test]
    fn qualified_display_and_pref_key_fold_in_the_schema() {
        assert_eq!(qualified_display("inv", "items"), "inv.items");
        assert_eq!(qualified_display("", "items"), "items");
        assert_eq!(table_pref_key("public", "orders"), "public.orders");
        assert_eq!(table_pref_key("", "orders"), "orders");
    }

    #[test]
    fn import_insert_sql_qualifies_the_schema() {
        let cfg = test_conn("postgres");
        let cols = vec![
            ImportCol {
                name: "item_id".into(),
                src: Some(0),
                ty: ColType::Int,
                data_type: "integer".into(),
            },
            ImportCol {
                name: "sku".into(),
                src: Some(1),
                ty: ColType::Text,
                data_type: "text".into(),
            },
        ];
        let sql = import_insert_sql(&cfg, "inv", "items", &cols, &["1".into(), "SKU-1".into()]);
        assert_eq!(
            sql,
            "INSERT INTO \"inv\".\"items\" (\"item_id\", \"sku\") VALUES (1, 'SKU-1');"
        );
    }

    #[test]
    fn update_sql_qualifies_the_schema() {
        let cfg = test_conn("postgres");
        let sql = build_update_sql(
            &cfg,
            "inv",
            "items",
            "qty",
            Some("integer"),
            "5",
            "\"item_id\" = 1",
        );
        assert_eq!(
            sql,
            "UPDATE \"inv\".\"items\"\nSET \"qty\" = 5\nWHERE \"item_id\" = 1;"
        );
    }

    #[test]
    fn batch_insert_sql_qualifies_the_schema() {
        let cfg = test_conn("postgres");
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![vec![Val::Text("1".into()), Val::Text("a".into())]];
        let types = vec![Some("integer".to_string()), Some("text".to_string())];
        let out = batch_insert_sql(&cfg, "inv", "items", &columns, &rows, &types, 10);
        assert!(
            out.starts_with("INSERT INTO \"inv\".\"items\" (\"id\", \"name\") VALUES"),
            "{out}"
        );
    }

    #[test]
    fn column_type_is_matched_per_schema() {
        let mut app = test_app();
        app.table_meta = Some(TableMeta {
            table: "orders".into(),
            schema: "public".into(),
            columns: vec![ColumnInfo {
                name: "amount".into(),
                data_type: "numeric(10,2)".into(),
                ..Default::default()
            }],
        });
        assert_eq!(
            column_type(&app, "public", "orders", "amount").as_deref(),
            Some("numeric(10,2)")
        );
        // Same table name in another schema must not inherit the metadata.
        assert!(column_type(&app, "inv", "orders", "amount").is_none());
    }

    #[test]
    fn picker_lists_schemas_before_databases() {
        let mut app = test_app();
        app.selected = Some(test_conn("postgres"));
        app.databases = vec!["shop".into(), "postgres".into()];
        app.schemas = vec!["public".into(), "inv".into()];
        app.schema = "inv".into();
        let entries = picker_entries(&app);
        assert_eq!(
            entries,
            vec![
                (PickerKind::Schema, "public".into()),
                (PickerKind::Schema, "inv".into()),
                (PickerKind::Database, "shop".into()),
                (PickerKind::Database, "postgres".into()),
            ]
        );
        // The highlight starts on the current schema, not the current database.
        assert_eq!(db_current_index(&app), 1);
        // Labels separate the two kinds once the schema layer is on.
        let labels = db_entries(&app);
        assert!(labels[0].starts_with("模式"), "{labels:?}");
        assert!(labels[2].starts_with("数据库"), "{labels:?}");
    }

    #[test]
    fn picker_is_flat_without_schemas() {
        let mut app = test_app();
        app.selected = Some(test_conn("mysql"));
        app.databases = vec!["shop".into()];
        app.schemas.clear();
        assert_eq!(
            picker_entries(&app),
            vec![(PickerKind::Database, "shop".into())]
        );
        // MySQL labels keep their pre-R26 plain form.
        assert_eq!(db_entries(&app), vec!["shop".to_string()]);
    }

    #[test]
    fn stale_table_list_is_discarded() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        let table = |name: &str| TableInfo {
            name: name.into(),
            table_type: "TABLE".into(),
            comment: None,
            parent_schema: None,
            parent_name: None,
        };
        app.tables_gen = 2;
        app.tables_all = vec![table("inv_items")];
        // A reply for an older generation (a slow `public` list the user already
        // left) must not replace the current one.
        apply_op_result(
            &mut app,
            OpResult::TablesFor {
                tables: vec![table("public_orders")],
                gen: 1,
            },
            &tx,
        );
        assert_eq!(app.tables_all.len(), 1);
        assert_eq!(app.tables_all[0].name, "inv_items");
        // The current generation is applied.
        apply_op_result(
            &mut app,
            OpResult::TablesFor {
                tables: vec![table("inv_items"), table("inv_orders")],
                gen: 2,
            },
            &tx,
        );
        assert_eq!(app.tables_all.len(), 2);
    }

    #[test]
    fn default_schema_prefers_public() {
        assert_eq!(default_schema(&["inv".into(), "public".into()]), "public");
        assert_eq!(default_schema(&["inv".into(), "other".into()]), "inv");
        assert_eq!(default_schema(&[]), "");
    }

    #[test]
    fn config_persists_per_schema_table_keys() {
        let path = std::env::temp_dir().join(format!("dbxt-schema-{}.json", Uuid::new_v4()));
        let mut cfg = TuiConfig::default();
        cfg.entry("shop", "public", "orders").order_by = Some("\"id\" DESC".into());
        cfg.entry("shop", "inv", "orders").hidden = ["secret".to_string()].into_iter().collect();
        cfg.save(&path);
        let back = TuiConfig::load(&path);
        // Same table name, two schemas → two independent entries.
        assert_eq!(
            back.table("shop", "public", "orders")
                .unwrap()
                .order_by
                .as_deref(),
            Some("\"id\" DESC")
        );
        assert!(back
            .table("shop", "inv", "orders")
            .unwrap()
            .hidden
            .contains("secret"));
        // The public entry kept no hidden set, and vice versa.
        assert!(back.table("shop", "public", "orders").unwrap().hidden.is_empty());
        assert!(back.table("shop", "inv", "orders").unwrap().order_by.is_none());
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
        // R20–R22 overlays have their own groups (the footer used to leak the
        // page group while these owned the keyboard).
        assert_eq!(
            keys(FooterView::ImportPrompt, Focus::Preview, true),
            vec!["Enter", "Esc", "?"]
        );
        assert_eq!(
            keys(FooterView::ImportPlan, Focus::Preview, true),
            vec!["Enter", "m", "s", "↑↓", "Esc", "?"]
        );
        assert_eq!(
            keys(FooterView::ImportReport, Focus::Preview, true),
            vec!["Enter/Esc", "?"]
        );
        assert_eq!(
            keys(FooterView::ExportPicker, Focus::Preview, true),
            vec!["↑↓", "Enter", "1-6", "Esc", "?"]
        );
        assert_eq!(
            keys(FooterView::ExportPath, Focus::Preview, true),
            vec!["Enter", "Esc", "?"]
        );
        assert_eq!(
            keys(FooterView::RedisPrompt, Focus::Preview, true),
            vec!["Enter", "Esc", "?"]
        );
        // Every group ends with the pinned help key.
        for view in [
            FooterView::Help,
            FooterView::Confirm,
            FooterView::EditDialog,
            FooterView::Browse,
            FooterView::NewConn,
            FooterView::ColPicker,
            FooterView::Snippets,
            FooterView::ImportPrompt,
            FooterView::ImportPlan,
            FooterView::ImportReport,
            FooterView::ExportPicker,
            FooterView::ExportPath,
            FooterView::RedisPrompt,
            FooterView::RedisKeys,
            FooterView::RedisValue,
            FooterView::MongoDocs,
            FooterView::MongoDoc,
            FooterView::Popup,
            FooterView::FilterPrompt,
            FooterView::DbPicker,
            FooterView::Recent,
            FooterView::Completion,
            FooterView::SnippetName,
            FooterView::ConnPicker,
            FooterView::TablePrompt,
            FooterView::ResultFilter,
        ] {
            let h = footer_hints_ctx(FooterCtx {
                view,
                focus: Focus::Preview,
                has_connection: true,
            });
            assert_eq!(h.last().unwrap().0, "?", "{view:?}");
        }
    }

    /// The footer group must follow the *keyboard* owner. Each overlay is set on
    /// a real `App` and `footer_ctx` must name it, mirroring the key router.
    #[test]
    fn footer_view_tracks_every_overlay() {
        let mut app = test_app();
        // Fresh app: no connection, picker open.
        assert_eq!(footer_ctx(&app).view, FooterView::ConnPicker);

        app.help_open = true;
        assert_eq!(footer_ctx(&app).view, FooterView::Help);
        app.help_open = false;

        app.export_open = true;
        assert_eq!(footer_ctx(&app).view, FooterView::ExportPicker);
        app.export_open = false;

        app.export_path = Some(TextArea::default());
        assert_eq!(footer_ctx(&app).view, FooterView::ExportPath);
        app.export_path = None;

        app.import_prompt = Some(ImportPrompt {
            input: TextArea::default(),
            table: "t".into(),
            schema: String::new(),
            db: "d".into(),
            error: None,
        });
        assert_eq!(footer_ctx(&app).view, FooterView::ImportPrompt);
        app.import_prompt = None;

        app.import_report = Some(Box::new(ImportReport {
            table: "t".into(),
            schema: String::new(),
            mode: ImportMode::Append,
            total: 1,
            inserted: 1,
            skipped: Vec::new(),
            aborted: None,
            elapsed_ms: 1,
        }));
        assert_eq!(footer_ctx(&app).view, FooterView::ImportReport);
        app.import_report = None;

        app.redis_prompt = Some(RedisPrompt {
            kind: RedisPromptKind::Pattern,
            title: "t".into(),
            key_display: String::new(),
            key_raw: String::new(),
            field: String::new(),
            batch: Vec::new(),
            input: TextArea::default(),
        });
        assert_eq!(footer_ctx(&app).view, FooterView::RedisPrompt);
        app.redis_prompt = None;

        // `confirm` is checked first in `key`, so it must win over a browse
        // overlay that happens to be open underneath it.
        app.export_open = true;
        app.confirm = Some(Confirm {
            sql: "DELETE FROM t".into(),
            reasons: Vec::new(),
            refresh: false,
            clear_batch: false,
            redis: None,
            mongo: None,
        });
        assert_eq!(footer_ctx(&app).view, FooterView::Confirm);
        app.confirm = None;
        app.export_open = false;
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

    /// Scan the real source for `t("…")` / `tf("…")` call sites and require an
    /// English translation for each Chinese literal. The `ALL_KEYS` list is only
    /// as good as its manual upkeep; this reads the call sites themselves, so a
    /// new overlay added without a table entry fails the build instead of
    /// silently falling back to Chinese under `DBXT_LANG=en`.
    #[test]
    fn every_call_site_has_english() {
        let src = include_str!("main.rs");
        let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        let chars: Vec<char> = src.chars().collect();
        let mut missing: Vec<String> = Vec::new();
        let mut checked = 0usize;
        let mut i = 0usize;
        while i < chars.len() {
            // A `t(` or `tf(` call: `t` not part of a longer identifier.
            let prev_ok = i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
            let after = if chars[i] == 't' && chars.get(i + 1) == Some(&'(') {
                Some(i + 2)
            } else if chars[i] == 't' && chars.get(i + 1) == Some(&'f') && chars.get(i + 2) == Some(&'(') {
                Some(i + 3)
            } else {
                None
            };
            if let Some(mut j) = after {
                if prev_ok {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'"') {
                        let mut lit = String::new();
                        let mut k = j + 1;
                        while k < chars.len() {
                            match chars[k] {
                                '\\' if k + 1 < chars.len() => {
                                    let e = chars[k + 1];
                                    lit.push(match e {
                                        'n' => '\n',
                                        't' => '\t',
                                        'r' => '\r',
                                        other => other,
                                    });
                                    k += 2;
                                }
                                '"' => break,
                                c => {
                                    lit.push(c);
                                    k += 1;
                                }
                            }
                        }
                        checked += 1;
                        let leaked: &'static str = Box::leak(lit.clone().into_boxed_str());
                        if cjk(&lit) && ui_text::t_lang(leaked, ui_text::Lang::En) == lit.as_str() {
                            missing.push(lit);
                        }
                        i = k;
                    }
                }
            }
            i += 1;
        }
        // Guard against a broken scanner silently checking nothing.
        assert!(checked > 400, "scanner only saw {checked} call sites");
        assert!(
            missing.is_empty(),
            "{} t()/tf() literals have no English translation: {missing:#?}",
            missing.len()
        );
    }

    /// The `?` help cheat-sheet is data, not literal `t()` calls, so it needs its
    /// own guard: every description must translate, and every keycap must stay
    /// language-neutral (the key column is printed verbatim in both languages).
    #[test]
    fn help_rows_are_translated_and_keycaps_are_neutral() {
        let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        for (key, desc) in HELP_ROWS {
            // The key column is rendered through `t` as well, so a CJK label
            // (a section header or a descriptive row) must have an English form.
            if cjk(key) {
                assert_ne!(
                    ui_text::t_lang(key, ui_text::Lang::En),
                    *key,
                    "help key {key:?} has no English translation"
                );
            }
            if !desc.is_empty() {
                assert_ne!(
                    ui_text::t_lang(desc, ui_text::Lang::En),
                    *desc,
                    "help description {desc:?} has no English translation"
                );
            }
        }
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

    // ─── Redis / Mongo helpers ───────────────────────────────────────────────

    fn blob(s: &str) -> RedisBlob {
        RedisBlob {
            raw_base64: base64_encode(s.as_bytes()),
            encoding: RedisBlobEncoding::Utf8,
        }
    }

    #[test]
    fn b64_decode_roundtrips_and_rejects_garbage() {
        assert_eq!(b64_decode("YXBwOnVzZXI=").unwrap(), b"app:user");
        assert_eq!(b64_decode("QWRh").unwrap(), b"Ada");
        // Whitespace is tolerated, padding may be omitted.
        assert_eq!(b64_decode(" QW Rh ").unwrap(), b"Ada");
        // A stray character is not silently dropped.
        assert!(b64_decode("!!!!").is_none());
        assert!(b64_decode("").unwrap().is_empty());
    }

    #[test]
    fn redis_blob_text_decodes_utf8_and_hexes_binary() {
        assert_eq!(redis_blob_text(&blob("hello world")), "hello world");
        let binary = RedisBlob {
            raw_base64: base64_encode(&[0x00, 0xff, 0x10]),
            encoding: RedisBlobEncoding::Binary,
        };
        assert_eq!(redis_blob_text(&binary), "0x00ff10");
        // A UTF-8 blob that is not valid UTF-8 must not panic.
        let broken = RedisBlob {
            raw_base64: base64_encode(&[0xff, 0xfe]),
            encoding: RedisBlobEncoding::Utf8,
        };
        assert!(redis_blob_text(&broken).starts_with("<binary"));
    }

    #[test]
    fn redis_value_view_renders_each_type() {
        let hash = RedisValue {
            key_display: "app:user".into(),
            key_raw: "YXBwOnVzZXI=".into(),
            ttl: -1,
            redis_type: "hash".into(),
            data: RedisValueData::Hash {
                items: vec![
                    dbx_core::db::redis_driver::RedisHashItem {
                        field: blob("name"),
                        value: blob("Ada"),
                        field_ttl: Some(-1),
                    },
                    dbx_core::db::redis_driver::RedisHashItem {
                        field: blob("role"),
                        value: blob("admin"),
                        field_ttl: Some(60),
                    },
                ],
                total: 2,
                scan_cursor: None,
            },
        };
        let view = redis_value_view(hash);
        assert_eq!(view.grid.columns, vec!["field", "value", "TTL"]);
        assert_eq!(view.grid.rows.len(), 2);
        assert_eq!(view.grid.rows[0][0].text(), "name");
        assert_eq!(view.grid.rows[0][1].text(), "Ada");
        assert!(view.grid.rows[0][2].is_null());
        assert_eq!(view.grid.rows[1][2].text(), "60s");
        assert_eq!(view.row_keys, vec!["name", "role"]);

        let string = RedisValue {
            key_display: "k".into(),
            key_raw: "aw==".into(),
            ttl: 120,
            redis_type: "string".into(),
            data: RedisValueData::String {
                content: blob("hello world"),
                total_bytes: Some(11),
                truncated: false,
            },
        };
        let view = redis_value_view(string);
        assert_eq!(view.grid.columns, vec!["value"]);
        assert_eq!(view.grid.rows[0][0].text(), "hello world");
        assert!(view.grid.note.contains("11"));

        let zset = RedisValue {
            key_display: "z".into(),
            key_raw: "eg==".into(),
            ttl: -1,
            redis_type: "zset".into(),
            data: RedisValueData::Zset {
                items: vec![dbx_core::db::redis_driver::RedisZsetItem {
                    score: "1.5".into(),
                    member: blob("alice"),
                }],
                total: 1,
                scan_cursor: None,
            },
        };
        let view = redis_value_view(zset);
        assert_eq!(view.grid.columns, vec!["score", "member"]);
        assert_eq!(view.grid.rows[0][0].text(), "1.5");
        assert_eq!(view.grid.rows[0][1].text(), "alice");
    }

    #[test]
    fn redis_prompt_commands_quote_and_shape() {
        assert_eq!(
            redis_prompt_command(RedisPromptKind::Ttl, "app:user", "", "120"),
            "EXPIRE \"app:user\" 120"
        );
        assert_eq!(
            redis_prompt_command(RedisPromptKind::Rename, "a", "", "b"),
            "RENAME \"a\" \"b\""
        );
        assert_eq!(
            redis_prompt_command(RedisPromptKind::StringValue, "k", "", "hi there"),
            "SET \"k\" \"hi there\""
        );
        assert_eq!(
            redis_prompt_command(RedisPromptKind::HashField, "k", "name", "Ada"),
            "HSET \"k\" \"name\" \"Ada\""
        );
        // Quotes and backslashes are escaped so the tokenizer sees one argument.
        assert_eq!(
            redis_prompt_command(RedisPromptKind::StringValue, "k", "", "a\"b\\c"),
            "SET \"k\" \"a\\\"b\\\\c\""
        );
        assert!(redis_prompt_command(RedisPromptKind::Pattern, "", "", "*").is_empty());
    }

    #[test]
    fn mongo_docs_grid_unions_keys_with_id_first() {
        let docs = vec![
            serde_json::json!({"name": "Ada", "age": 36}),
            serde_json::json!({"_id": "x", "name": "Bob", "city": "Paris"}),
        ];
        let grid = mongo_docs_grid(&docs);
        assert_eq!(grid.columns[0], "_id");
        assert!(grid.columns.contains(&"name".to_string()));
        assert!(grid.columns.contains(&"city".to_string()));
        // A missing field is NULL, not the empty string.
        let row0 = &grid.rows[0];
        let id_idx = grid.columns.iter().position(|c| c == "_id").unwrap();
        assert!(row0[id_idx].is_null());
        assert_eq!(grid.rows.len(), 2);
    }

    #[test]
    fn backend_kind_is_inferred_from_the_connection_type() {
        let mk = |t: &str| {
            new_connection_config(
                "id".to_string(),
                t.to_string(),
                parse_database_type(t).unwrap(),
                "h".to_string(),
                1,
                "u".to_string(),
                String::new(),
                None,
                false,
                None,
            )
            .unwrap()
        };
        assert_eq!(backend_for_connection(&mk("redis")), Backend::Redis);
        assert_eq!(backend_for_connection(&mk("mongodb")), Backend::Mongo);
        assert_eq!(backend_for_connection(&mk("mysql")), Backend::Sql);
        assert_eq!(backend_for_connection(&mk("postgres")), Backend::Sql);
    }

    #[test]
    fn redis_ttl_and_type_badges() {
        assert_eq!(redis_ttl_label(-1), "永不过期");
        assert_eq!(redis_ttl_label(-2), "不存在");
        assert_eq!(redis_ttl_label(90), "90s");
        assert_eq!(redis_type_badge("string").0, "S");
        assert_eq!(redis_type_badge("hash").0, "H");
        assert_eq!(redis_type_badge("list").0, "L");
        assert_eq!(redis_type_badge("zset").0, "Z");
        assert_eq!(redis_type_badge("stream").0, "X");
        assert_eq!(redis_type_badge("weird").0, "?");
    }

    // ── R21: Redis batch key operations ──

    fn rk(raw: &str, display: &str) -> RedisKeyInfo {
        RedisKeyInfo {
            key_display: display.to_string(),
            key_raw: raw.to_string(),
            key_type: "string".to_string(),
            ttl: -1,
            size: 0,
            value_preview: String::new(),
        }
    }

    #[test]
    fn redis_multi_select_toggles_and_ranges() {
        let keys = vec![rk("a", "app:1"), rk("b", "app:2"), rk("c", "app:3"), rk("d", "app:4")];
        let mut sel: HashSet<String> = HashSet::new();
        let mut anchor: Option<usize> = None;
        redis_selection_toggle(&mut sel, &mut anchor, &keys, 0);
        assert!(sel.contains("a"));
        assert_eq!(anchor, Some(0));
        redis_selection_toggle(&mut sel, &mut anchor, &keys, 0);
        assert!(sel.is_empty(), "space toggles off");
        // A range extends from the anchor, additively.
        redis_selection_toggle(&mut sel, &mut anchor, &keys, 1);
        redis_selection_range(&mut sel, &mut anchor, &keys, 3);
        assert_eq!(sel.len(), 3);
        assert!(sel.contains("b") && sel.contains("c") && sel.contains("d"));
        assert_eq!(anchor, Some(1));
        // A reverse range keeps everything and adds the earlier keys.
        redis_selection_range(&mut sel, &mut anchor, &keys, 0);
        assert!(sel.contains("a"));
        // Targets preserve list order, not set order.
        let targets = redis_selection_targets(&sel, &keys, None);
        assert_eq!(
            targets.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn redis_select_all_and_target_fallback() {
        let keys = vec![rk("a", "app:1"), rk("b", "app:2")];
        let mut sel: HashSet<String> = HashSet::new();
        let mut anchor: Option<usize> = None;
        assert!(!redis_selection_is_all(&sel, &keys));
        redis_selection_all(&mut sel, &mut anchor, &keys);
        assert!(redis_selection_is_all(&sel, &keys));
        assert_eq!(anchor, Some(0));
        // An empty selection falls back to the focused key.
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(
            redis_selection_targets(&empty, &keys, Some(1)),
            vec![("b".to_string(), "app:2".to_string())]
        );
        assert!(redis_selection_targets(&empty, &keys, None).is_empty());
    }

    #[test]
    fn redis_batch_del_chunks_and_quotes() {
        let displays: Vec<String> = (0..250).map(|i| format!("app:{i}")).collect();
        let cmds = redis_batch_del_commands(&displays);
        assert_eq!(cmds.len(), 3, "100 + 100 + 50");
        assert!(cmds[0].starts_with("DEL \"app:0\" \"app:1\""));
        assert!(cmds[2].ends_with("\"app:249\""));
        // Quotes / backslashes survive the redis-cli tokenizer.
        assert_eq!(
            redis_batch_del_commands(&["a\"b\\c".to_string()]),
            vec!["DEL \"a\\\"b\\\\c\"".to_string()]
        );
        assert!(redis_batch_del_commands(&[]).is_empty());
    }

    #[test]
    fn redis_prefix_rename_plan_filters_and_rewrites() {
        let displays = vec!["app:1".to_string(), "other:2".to_string(), "app:3".to_string()];
        assert_eq!(
            redis_prefix_rename_plan(&displays, "app:", "new:"),
            vec![
                ("app:1".to_string(), "new:1".to_string()),
                ("app:3".to_string(), "new:3".to_string()),
            ]
        );
        // A same-prefix replacement is a no-op and is dropped.
        assert!(redis_prefix_rename_plan(&displays, "app:", "app:").is_empty());
        // An empty old prefix prepends to every key.
        assert_eq!(redis_prefix_rename_plan(&displays, "", "x").len(), 3);
        let cmds = redis_batch_rename_commands(&displays, "app:", "new:");
        assert_eq!(cmds, vec!["RENAME \"app:1\" \"new:1\"", "RENAME \"app:3\" \"new:3\""]);
    }

    #[test]
    fn redis_batch_ttl_validates_and_generates() {
        let displays = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            redis_batch_ttl_commands(&displays, "60"),
            vec!["EXPIRE \"a\" 60", "EXPIRE \"b\" 60"]
        );
        assert!(redis_batch_ttl_commands(&displays, "nope").is_empty());
        assert!(redis_batch_ttl_commands(&displays, "1.5").is_empty());
    }

    #[test]
    fn redis_batch_confirm_flow_requires_typed_count_only_for_select_all_delete() {
        let targets = vec![("a".into(), "app:1".into()), ("b".into(), "app:2".into())];
        let plan = redis_plan_batch(RedisBatchKind::Delete, &targets, false, "").unwrap();
        assert_eq!(plan.typed_confirm, None);
        assert_eq!(plan.commands.len(), 1);
        // Selecting every loaded key escalates to a typed re-confirmation.
        let plan = redis_plan_batch(RedisBatchKind::Delete, &targets, true, "").unwrap();
        assert_eq!(plan.typed_confirm, Some(2));
        // TTL / rename never demand the typed confirm, even on a select-all.
        assert_eq!(
            redis_plan_batch(RedisBatchKind::Ttl, &targets, true, "30")
                .unwrap()
                .typed_confirm,
            None
        );
        assert_eq!(
            redis_plan_batch(RedisBatchKind::RenamePrefix, &targets, true, "app:=new:")
                .unwrap()
                .typed_confirm,
            None
        );
        // Invalid arguments surface an error instead of a broken command.
        assert!(redis_plan_batch(RedisBatchKind::Ttl, &targets, false, "abc").is_err());
        assert!(redis_plan_batch(RedisBatchKind::RenamePrefix, &targets, false, "no-equals").is_err());
    }

    // ── R21: MongoDB document CRUD ──

    #[test]
    fn mongo_id_arg_preserves_string_object_id_shape() {
        // A real ObjectId arrives as {"$oid": ...} and is passed as its hex form.
        assert_eq!(
            mongo_id_arg(&serde_json::json!({"$oid": "507f1f77bcf86cd799439011"})),
            "507f1f77bcf86cd799439011"
        );
        // A genuine 24-hex string _id must be marked so it is not reinterpreted.
        assert_eq!(
            mongo_id_arg(&serde_json::json!("507f1f77bcf86cd799439011")),
            "__dbx_mongo_string_id__\"507f1f77bcf86cd799439011\""
        );
        assert_eq!(mongo_id_arg(&serde_json::json!("customer-42")), "customer-42");
        assert_eq!(mongo_id_arg(&serde_json::json!(42)), "42");
        assert_eq!(
            mongo_id_arg(&serde_json::json!({"$numberLong": "2048938405781032962"})),
            "{\"$numberLong\":\"2048938405781032962\"}"
        );
        assert_eq!(mongo_id_label(&serde_json::json!({"$oid": "abc"})), "abc");
        assert_eq!(mongo_id_label(&serde_json::json!("plain")), "plain");
    }

    #[test]
    fn mongo_doc_diff_reports_top_level_changes() {
        let old = serde_json::json!({"_id": {"$oid": "x"}, "name": "Ada", "age": 30, "gone": true});
        let new = serde_json::json!({"_id": {"$oid": "x"}, "name": "Grace", "age": 30, "added": 1});
        let diff = mongo_doc_diff(&old, &new, 10);
        assert!(diff.iter().any(|l| l.contains("name") && l.contains("Ada") && l.contains("Grace")));
        assert!(diff.iter().any(|l| l.starts_with("+ added")));
        assert!(diff.iter().any(|l| l.starts_with("- gone")));
        assert!(!diff.iter().any(|l| l.contains("age")), "unchanged fields are omitted");
        assert!(!diff.iter().any(|l| l.contains("_id")), "_id is never part of the diff");
        assert!(mongo_doc_diff(&old, &old, 10).is_empty());
    }
}
