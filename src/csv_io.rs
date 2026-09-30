use crate::prelude::*;

// ─── CSV import ──────────────────────────────────────────────────────────────

/// A CSV column's inferred SQL type. Only used to pick the right literal shape
/// (bare number, TRUE/FALSE, quoted string); the target column's own type still
/// has the final say when the value is written.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ColType {
    Int,
    Float,
    Bool,
    Date,
    DateTime,
    Text,
}

impl ColType {
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) struct ImportCol {
    /// Target column name.
    pub(crate) name: String,
    /// Index into the CSV row when the header matched a table column; `None`
    /// means the column is absent from the CSV and is written as NULL/default.
    pub(crate) src: Option<usize>,
    /// Type inferred from the CSV values (Text when the column is absent).
    pub(crate) ty: ColType,
    /// The table column's declared type, used to keep genuinely numeric values
    /// bare even when the CSV sample looked like text.
    pub(crate) data_type: String,
}

/// How an import treats rows already in the table.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ImportMode {
    /// Append to the existing rows.
    Append,
    /// `DELETE FROM` the table first, then insert (red-confirmed).
    Overwrite,
}

/// What to do when a row fails to insert.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ImportOnError {
    /// Stop at the first failure and report the row number (default).
    Stop,
    /// Skip the bad row and keep going, reporting every skipped row.
    Skip,
}

/// The parsed, decoded and header-aligned CSV, ready to preview and import.
#[derive(Clone)]
pub(crate) struct ImportPlan {
    pub(crate) path: PathBuf,
    pub(crate) file_size: u64,
    pub(crate) encoding: String,
    pub(crate) delimiter: char,
    /// CSV header names in file order.
    pub(crate) headers: Vec<String>,
    /// Every data row (raw field strings, empty = NULL).
    pub(crate) rows: Vec<Vec<String>>,
    /// Target table, database and schema.
    pub(crate) table: String,
    pub(crate) schema: String,
    pub(crate) db: String,
    /// Target columns with their CSV source index and inferred type.
    pub(crate) columns: Vec<ImportCol>,
    /// CSV headers that match no table column (blocks the import when non-empty).
    pub(crate) extra: Vec<String>,
    /// Table columns absent from the CSV (imported as NULL/default).
    pub(crate) missing: Vec<String>,
    pub(crate) mode: ImportMode,
    pub(crate) on_error: ImportOnError,
    /// A blocking problem (unreadable file, no data, header mismatch).
    pub(crate) error: Option<String>,
}

impl ImportPlan {
    /// Columns actually written by the INSERT.
    pub(crate) fn present(&self) -> Vec<&ImportCol> {
        self.columns.iter().filter(|c| c.src.is_some()).collect()
    }
}

/// The file-path step of the import flow (before the CSV is read).
pub(crate) struct ImportPrompt {
    pub(crate) input: TextArea<'static>,
    pub(crate) table: String,
    pub(crate) schema: String,
    pub(crate) db: String,
    /// Inline error from the previous attempt (file missing, bad header …).
    pub(crate) error: Option<String>,
}

/// The outcome of one import run, shown in the completion overlay.
#[derive(Clone)]
pub(crate) struct ImportReport {
    pub(crate) table: String,
    pub(crate) schema: String,
    pub(crate) mode: ImportMode,
    pub(crate) total: usize,
    pub(crate) inserted: usize,
    /// `(1-based data row, error)` for rows skipped in skip mode.
    pub(crate) skipped: Vec<(usize, String)>,
    /// Set when stop mode aborted: the failing row and its error.
    pub(crate) aborted: Option<(usize, String)>,
    pub(crate) elapsed_ms: u128,
}

impl ImportReport {
    pub(crate) fn ok(&self) -> bool {
        self.aborted.is_none()
    }
}

/// The import job handed to the backend task.
pub(crate) struct ImportJob {
    pub(crate) cfg: Box<ConnectionConfig>,
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    /// Present columns only (src is Some), in target order.
    pub(crate) columns: Vec<ImportCol>,
    pub(crate) rows: Vec<Vec<String>>,
    pub(crate) mode: ImportMode,
    pub(crate) on_error: ImportOnError,
}

// ─── export ──────────────────────────────────────────────────────────────────

/// A result-set export format offered by `Ctrl-Y`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ExportFormat {
    Csv,
    JsonArray,
    JsonNdjson,
    Markdown,
    Insert,
    InsertBatch,
}

impl ExportFormat {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::JsonArray => "JSON",
            ExportFormat::JsonNdjson => "NDJSON",
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Insert => "INSERT",
            ExportFormat::InsertBatch => t("INSERT (批量)"),
        }
    }
    pub(crate) fn description(self) -> &'static str {
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
pub(crate) const EXPORT_FORMATS: &[ExportFormat] = &[
    ExportFormat::Csv,
    ExportFormat::JsonArray,
    ExportFormat::JsonNdjson,
    ExportFormat::Markdown,
    ExportFormat::Insert,
    ExportFormat::InsertBatch,
];

/// A generated export waiting for the destination (clipboard or file) step.
pub(crate) struct ExportPending {
    pub(crate) format: ExportFormat,
    /// `(schema, table)` guessed for INSERT exports (`None` for the other
    /// formats).
    pub(crate) table: Option<(String, String)>,
}

/// A file export handed to the background worker: everything the streaming
/// writer needs, captured up front so the render/UI thread never touches the
/// grid again (and never builds the whole document as one String).
pub(crate) struct ExportJob {
    pub(crate) format: ExportFormat,
    pub(crate) path: PathBuf,
    pub(crate) grid: Grid,
    /// Connection used to quote identifiers / literals (INSERT formats).
    pub(crate) cfg: Option<ConnectionConfig>,
    pub(crate) schema: String,
    pub(crate) table: String,
    /// Declared type per column, for the INSERT formats (empty when unknown).
    pub(crate) types: Vec<Option<String>>,
}

#[derive(Clone)]
pub(crate) struct Confirm {
    pub(crate) sql: String,
    pub(crate) reasons: Vec<String>,
    /// True when a successful run should refresh the current table page in place
    /// (row edits, deletes, queued batches) instead of replacing the grid.
    pub(crate) refresh: bool,
    /// True when accepting the confirmation should also drain the queued batch.
    pub(crate) clear_batch: bool,
    /// Set for Redis writes: Enter runs `cmd` via the Redis console instead of
    /// SQL, then reloads the key list / the named key's value.
    pub(crate) redis: Option<RedisConfirm>,
    /// Set for MongoDB document writes: Enter runs the matching insert / update /
    /// delete through the document driver, then reloads the current page.
    pub(crate) mongo: Option<MongoConfirm>,
    /// Set for connection deletion: Enter removes the saved connection (never any
    /// database data).
    pub(crate) conn: Option<ConnConfirm>,
}

/// A pending connection action shown in the red confirmation layer. `disconnect`
/// picks the semantics: `false` deletes the saved config, `true` is a manual
/// disconnect (R47b) that drains the connection's pools.
#[derive(Clone)]
pub(crate) struct ConnConfirm {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) db_type: String,
    /// R47b: true = manual disconnect; false = delete the saved connection.
    pub(crate) disconnect: bool,
    /// R68: when set, the red layer flips this connection's read-only policy to
    /// the given value (a connection-level config field) instead of deleting or
    /// disconnecting it.
    pub(crate) readonly: Option<bool>,
}

/// Liveness of one connection root as the sidebar draws it (R47b).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnStatus {
    /// The kernel holds a pool: the connection can be queried now.
    Active,
    /// No pool (never connected, or explicitly disconnected): expand to connect.
    Idle,
    /// A switch / lazy expand is opening the pool right now.
    Connecting,
}

impl ConnStatus {
    /// The status glyph. Shape carries the state so the connection colour stays
    /// free to carry identity (colour-blind friendly).
    pub(crate) fn shape(self) -> &'static str {
        match self {
            ConnStatus::Active => "●",
            ConnStatus::Idle => "○",
            ConnStatus::Connecting => "◐",
        }
    }
}

/// A pending Redis write shown in the red confirmation layer.
#[derive(Clone)]
pub(crate) struct RedisConfirm {
    pub(crate) db: u32,
    /// Single command for a one-key write (SET / EXPIRE / RENAME / HSET / DEL).
    pub(crate) cmd: String,
    /// When non-empty, run these commands in order instead of `cmd`.
    pub(crate) batch: Vec<String>,
    /// Batch targets as `(raw, display)`, kept so a typed re-confirmation can
    /// regenerate / describe exactly what is about to change.
    pub(crate) batch_keys: Vec<(String, String)>,
    pub(crate) reload_value: Option<String>,
    pub(crate) reload_list: bool,
    /// When set, Enter on the red layer first opens a typed confirmation that
    /// must repeat this key count (or `YES`) before the batch runs.
    pub(crate) typed_confirm: Option<usize>,
    /// Human summary used by the typed confirmation prompt.
    pub(crate) summary: String,
    /// R57: raw keys to drop from the loaded key browser in place after a
    /// successful single-key delete, so the SCAN cursor and loaded pages stay
    /// put (a full rescan would jump the list back to the first page).
    pub(crate) remove_in_place: Vec<String>,
    /// R81: `(raw key, new TTL seconds)` applied to the loaded key browser in
    /// place after a confirmed single-key `T` TTL write, so the list keeps its
    /// SCAN cursor instead of rescanning from page one.
    pub(crate) set_ttl_in_place: Vec<(String, i64)>,
}

/// A pending MongoDB document write shown in the red confirmation layer.
#[derive(Clone)]
pub(crate) struct MongoConfirm {
    pub(crate) db: String,
    pub(crate) collection: String,
    pub(crate) action: MongoAction,
}

/// The concrete MongoDB write behind a [`MongoConfirm`].
#[derive(Clone)]
pub(crate) enum MongoAction {
    Insert { doc_json: String },
    Update { id: String, doc_json: String },
    Delete { id: String },
}

/// The JSON editor dialog used to edit / insert a MongoDB document.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MongoDocMode {
    Edit,
    Insert,
}

#[derive(Clone)]
pub(crate) struct MongoDocDialog {
    pub(crate) mode: MongoDocMode,
    pub(crate) db: String,
    pub(crate) collection: String,
    /// The document as fetched (Extended-JSON-ish shape produced by the driver),
    /// used to detect `_id` tampering and to compute the save diff.
    pub(crate) original: serde_json::Value,
    /// `_id` argument for an update / delete (empty for insert).
    pub(crate) id: String,
    pub(crate) editor: TextArea<'static>,
    /// Last validation error, shown under the editor until the next save.
    pub(crate) error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum EditKind {
    Update,
    Insert,
}

/// A diff-style confirmation layer for a generated write. UPDATE edits let the
/// new value be typed inline; INSERT shows the row that is about to be added.
#[derive(Clone)]
pub(crate) struct EditDialog {
    pub(crate) kind: EditKind,
    pub(crate) cfg: Box<ConnectionConfig>,
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    // UPDATE fields
    pub(crate) column: String,
    pub(crate) data_type: Option<String>,
    pub(crate) old: Val,
    pub(crate) new_input: TextArea<'static>,
    pub(crate) where_clause: String,
    pub(crate) keys: Vec<String>,
    pub(crate) no_pk: bool,
    // INSERT fields
    pub(crate) insert_sql: String,
    pub(crate) insert_preview: Vec<(String, String)>,
}

/// One logical line of a modal text popup together with the style its value
/// deserves (NULL → grey italic, empty string → grey, ordinary text → plain).
#[derive(Clone)]
pub(crate) struct PopupLine {
    pub(crate) text: String,
    pub(crate) style: Style,
}

/// One styled token run inside a rich popup line (the pretty-JSON cell view).
#[derive(Clone)]
pub(crate) struct PopupSpan {
    pub(crate) text: String,
    pub(crate) style: Style,
}

/// A modal showing one cell's full, untruncated value.
#[derive(Clone)]
pub(crate) struct CellPopup {
    pub(crate) title: String,
    pub(crate) lines: Vec<PopupLine>,
    pub(crate) scroll: u16,
    /// Column name, named in the `y`/`Y` copy status.
    pub(crate) col: String,
    /// The original cell text — what `y`/`Y` copies, never the pretty form.
    pub(crate) raw: String,
    /// Pretty JSON body (styled token runs per line) when [`raw`] is a JSON
    /// object/array; `None` for every other value.
    pub(crate) pretty: Option<Vec<Vec<PopupSpan>>>,
    /// True while the pretty body is shown (JSON cells only).
    pub(crate) show_pretty: bool,
    /// R89: raw text decoded with JSON escape semantics (`Some` even when the
    /// value carries no escapes, so `U` always has a decoded view); `None` when
    /// a `\u` escape is malformed.
    pub(crate) decoded: Option<String>,
    /// R89: the decoded text re-escaped with `\uXXXX` for every non-ASCII
    /// character; `None` for a pure-ASCII value (no `U` third state).
    pub(crate) escaped: Option<String>,
    /// R89: the grey bottom preview line shown when the raw text contains a
    /// `\uXXXX` escape; read-only, never changes the value.
    pub(crate) preview: Option<PopupLine>,
    /// R89: which of the three `U` views the body currently shows.
    pub(crate) u_mode: UMode,
    /// R89: true for a single-value popup (a grid cell / drilled field), false
    /// for the multi-line data-diff summary (whose `lines` are not one value).
    pub(crate) single: bool,
}

/// Build a cell popup, computing the R89 Unicode views for a single-value
/// popup. The multi-line data-diff summary passes `single = false`, so `U`
/// stays inert there and its `lines` render verbatim.
pub(crate) fn make_cell_popup(
    title: String,
    lines: Vec<PopupLine>,
    col: String,
    raw: String,
    pretty: Option<Vec<Vec<PopupSpan>>>,
    show_pretty: bool,
    single: bool,
) -> CellPopup {
    let (decoded, escaped, preview) = if single {
        unicode_views(&raw)
    } else {
        (None, None, None)
    };
    CellPopup {
        title,
        lines,
        scroll: 0,
        col,
        raw,
        pretty,
        show_pretty,
        decoded,
        escaped,
        preview,
        u_mode: UMode::Raw,
        single,
    }
}

/// A modal showing every column of the focused row, one per line. Beyond
/// scrolling it carries a cursor (so `y` / `Enter` act on one column), a
/// column-name filter (`/`, for 40+ column tables) and a vim count prefix
/// (`5j`). `Enter` / `v` drill into the full cell popup, which stays on top so
/// `Esc` returns here before closing the row.
#[derive(Clone)]
pub(crate) struct RowPopup {
    pub(crate) title: String,
    pub(crate) lines: Vec<PopupLine>,
    /// Column name per line, parallel to `lines` (drives the `/` filter).
    pub(crate) cols: Vec<String>,
    /// Display value per line, parallel to `lines` (the text after `name = `,
    /// used by the narrow stacked layout).
    pub(crate) shown: Vec<String>,
    /// Clipboard value per line, parallel to `lines` (drives `y`/`Y` and the
    /// drilled cell popup).
    pub(crate) values: Vec<String>,
    /// R86: pretty-printed JSON token runs per field, parallel to `lines`;
    /// `None` when the field is not a JSON object/array. Drives the in-place
    /// `J` expansion.
    pub(crate) pretty: Vec<Option<Vec<Vec<PopupSpan>>>>,
    /// R86: whether the field is currently expanded to its pretty JSON block.
    pub(crate) expanded: Vec<bool>,
    /// Absolute row number (1-based, across pages) for the drilled cell title.
    pub(crate) row_abs: usize,
    pub(crate) scroll: u16,
    /// Selected entry, an index into the *visible* (filtered) line list.
    pub(crate) cursor: usize,
    /// Column-name substring filter (case-insensitive).
    pub(crate) filter: String,
    /// True while the `/` filter is being typed, so `j`/`k` are text.
    pub(crate) filtering: bool,
    /// R94: active locate needle (`\`), matched against a field's name *or*
    /// displayed value; empty when no locate is active. Unlike `filter` it never
    /// hides a field — it only marks and jumps.
    pub(crate) search: String,
    /// R94: true while the `\` locate input is being typed, so every printable
    /// key extends the needle.
    pub(crate) searching: bool,
    /// R94: entry indices matched by `search`, in field order.
    pub(crate) hits: Vec<usize>,
    /// R94: index into [`RowPopup::hits`] the cursor last landed on.
    pub(crate) hit_idx: usize,
    /// Pending vim count prefix for `j`/`k` / `PageDown`.
    pub(crate) count: String,
}

/// Build a row popup from just its display lines (tests / simple callers); the
/// column and raw-value side tables stay empty.
#[cfg(test)]
pub(crate) fn row_popup_from_lines(title: String, lines: Vec<PopupLine>) -> RowPopup {
    let n = lines.len();
    RowPopup {
        title,
        lines,
        cols: vec![String::new(); n],
        shown: vec![String::new(); n],
        values: vec![String::new(); n],
        pretty: vec![None; n],
        expanded: vec![false; n],
        row_abs: 0,
        scroll: 0,
        cursor: 0,
        filter: String::new(),
        filtering: false,
        search: String::new(),
        searching: false,
        hits: Vec::new(),
        hit_idx: 0,
        count: String::new(),
    }
}

/// Build a plain (non-JSON) cell popup for tests.
#[cfg(test)]
pub(crate) fn cell_popup_from_lines(title: String, lines: Vec<PopupLine>) -> CellPopup {
    let raw = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    make_cell_popup(title.clone(), lines, title, raw, None, false, false)
}

/// Memoised wrap of the modal text popups (R42). A 100 KB cell would otherwise
/// be re-wrapped on every frame while scrolling; the cache is invalidated when a
/// popup opens and recomputed only when the inner width changes.
pub(crate) struct PopupCache {
    pub(crate) width: usize,
    pub(crate) lines: Vec<Line<'static>>,
}
