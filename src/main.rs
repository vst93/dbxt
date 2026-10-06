// dbxt — Terminal UI client built on DBX kernel (dbx-core + dbx-mcp LocalBackend)
// Apache-2.0. Reuses DBX connection storage (dbx.db), native drivers, SQL safety.
#![recursion_limit = "512"]

mod ui_text;

mod batch_export;
mod comments;
mod csv_io;
mod ddl_export;
mod diffui;
mod docgen;
mod editor;
mod filter;
mod input;
mod jsonview;
mod last_session;
mod materialize;
mod mcp;
mod migrate;
mod mongo;
mod nav;
mod numfmt;
mod parity;
mod prelude;
mod redis;
mod render;
mod render_help;
mod render_overlay;
mod results;
mod rowops;
mod runner;
mod search;
mod sidebar;
mod sqlfmt;
mod sqlite_open;
mod state;
#[cfg(test)]
mod tests;
mod textutil;
mod transfer;
mod tui_config;

use crate::prelude::*;

type Tx = tokio::sync::mpsc::UnboundedSender<OpResult>;

/// Rows fetched per table-data page (one extra row is fetched to detect a next page).
const PAGE_SIZE: usize = 50;
/// Above this row count the table browser stops running an exact `COUNT(*)` on
/// the first load and reports a lower bound (`>500000 行`) instead, so a huge
/// InnoDB table no longer pays a multi-second full scan just to show a total.
/// `DBXT_COUNT_SAMPLE_LIMIT=0` disables the cap and always counts exactly.
const COUNT_SAMPLE_LIMIT_DEFAULT: u64 = 500_000;
/// A page this deep on the plain OFFSET path (no primary key to seek by) earns a
/// one-off "deep paging is slow" hint.
const DEEP_PAGE_HINT_AFTER: usize = 1000;
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
/// Streaming export writes a (possibly large) file to local disk. Local IO is
/// fast, but a very wide 20k-row result on a slow filesystem still deserves a
/// generous ceiling rather than the generic 60 s fallback.
const OP_WATCHDOG_EXPORT: Duration = Duration::from_secs(600);
/// The global search walks every table (one bounded query each), so it is the
/// longest-running op; the per-statement driver timeout still caps each query.
const OP_WATCHDOG_SEARCH: Duration = Duration::from_secs(600);
/// Buffered writer size for a streaming export (1 MiB keeps syscalls rare while
/// bounding the memory held above the OS page cache).
const EXPORT_BUF_BYTES: usize = 1 << 20;
/// Default per-table scan ceiling for the global search (`Alt-G`): one
/// `SELECT … LIMIT n` per table, so even a wide table answers quickly.
/// `DBXT_SEARCH_SCAN_LIMIT` overrides it.
const DEFAULT_SEARCH_SCAN_LIMIT: usize = 1000;
/// Default table-size ceiling for the global search. A table estimated above
/// this many rows is skipped (and reported) rather than full-scanned;
/// `DBXT_SEARCH_MAX_ROWS` overrides it.
const DEFAULT_SEARCH_MAX_ROWS: u64 = 1_000_000;
/// Cap on the number of hits the global-search overlay retains.
const SEARCH_MAX_HITS: usize = 500;
/// R64: cap on the matching cells the in-result `\` find keeps. The scan is
/// client-side over the already-loaded page (never a query), so the only cost
/// is the highlight/jump list; beyond this many matches the list stops growing
/// and the status bar says so. `DBXT_CELL_FIND_LIMIT` overrides it.
const DEFAULT_CELL_FIND_LIMIT: usize = 500;
/// R66: rows the `g c` popup's column-value stats scan. The stats are computed
/// in place from the already-loaded page (never a query); a page larger than
/// this is sampled to its first `COL_STATS_SCAN_LIMIT` rows and the popup says
/// so instead of scanning unbounded data every frame.
const COL_STATS_SCAN_LIMIT: usize = 5000;
/// R66: minimum `g c` popup inner width for the stats to sit *beside* the column
/// list; below it they stack under the list, so a phone still shows them.
const COL_STATS_SIDE_MIN: usize = 64;
/// R72: width of the value-distribution sparkline beside the `g c` distinct
/// count, in display cells (one bar per bucket).
const COL_SPARK_W: usize = 12;
/// R72: below this terminal width the sparkline is dropped, so a phone keeps the
/// plain non-null / null / distinct counts readable.
const COL_SPARK_MIN_W: u16 = 56;
/// A `.sql` file larger than this warns before its script is executed.
const FILE_LOAD_WARN_BYTES: u64 = 2 * 1024 * 1024;
/// Rows fetched per chunk from each side of a data compare. Small enough to
/// keep only a few thousand values resident, large enough that a whole-table
/// compare is not dominated by round trips.
const DATA_CHUNK: usize = 1000;
/// Cap on the difference rows a data compare retains. Past it the compare keeps
/// counting (so the summary stays accurate) but stops storing rows; a bigger
/// result should be exported and diffed, not shown in a terminal list.
const DATA_MAX_DIFF_ROWS: usize = 5000;
/// R90: rows read from each side of a row-order (no-primary-key) data compare.
/// Positional align is only meaningful over a bounded read, so both sides are
/// capped at this; the status line reports the compare as `已比 500 行`.
const DATA_ROW_ORDER_LIMIT: usize = 500;
/// A data compare is many sequential bounded queries, so the last-resort
/// watchdog is generous (every statement still has its own driver timeout).
const OP_WATCHDOG_DATA_DIFF: Duration = Duration::from_secs(900);
/// Rows read per source chunk during a data transfer (`Alt-T`). Reuses the
/// keyset paging of R34; a chunk is then written as one or more `INSERT`
/// batches so the target transaction stays small.
const TRANSFER_CHUNK: usize = 1000;
/// Rows per transactional `INSERT` batch on the target. Matches the CSV-import
/// chunk size, so a mid-batch failure rolls back at most this many rows.
const TRANSFER_INSERT_BATCH: usize = 500;
/// A transfer whose source estimate reaches this many rows warns for a second
/// confirmation before it starts (a full-table copy should be deliberate).
const TRANSFER_WARN_ROWS: u64 = 1_000_000;
/// The bounded `COUNT(*)` probes at most this many rows: once it is reached the
/// estimate is reported as "at least this many" rather than scanning a whole
/// huge table just to warn.
const TRANSFER_COUNT_PROBE: u64 = 1_000_001;
/// A data transfer is a long sequence of chunk reads and batched writes; the
/// last-resort watchdog is generous (each statement keeps its 60 s timeout).
const OP_WATCHDOG_TRANSFER: Duration = Duration::from_secs(3600);
/// R96: the explicit `Ctrl-P` full-list probe runs one minimal packet per saved
/// connection. At most this many are in flight at once, so probing a long list
/// stays fast without flooding the server.
const PROBE_CONCURRENCY: usize = 4;
/// R96: per-connection ceiling for the full-list probe. A link slower than this
/// is reported as `超时` / timeout and does not hold up the rest of the batch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// R96: last-resort watchdog for the full-list probe. With
/// [`PROBE_CONCURRENCY`] and [`PROBE_TIMEOUT`] a batch takes at most
/// `ceil(n / 4) * 3 s`; this covers a few hundred connections.
const OP_WATCHDOG_PROBE_ALL: Duration = Duration::from_secs(600);
/// R53: how many editor lines above / below the caret the passive bracket
/// highlight reads. The pair is resolved inside this window only, so the cost of
/// a frame never depends on the size of the buffer — a SQL file of any length
/// costs the same as a one-line query. A pair that reaches further than this is
/// simply left unhighlighted (`%` still jumps there on demand).
const BRACKET_SCAN_LINES: usize = 200;
/// A second, byte-sized safety valve on top of [`BRACKET_SCAN_LINES`]: 401 very
/// long lines could still be megabytes, so a window wider than this is skipped
/// (an editor line that big has no useful bracket pairing to show anyway).
const BRACKET_SCAN_BYTES: usize = 256 * 1024;
/// R56: the editor dims every statement except the one under the caret. Under
/// this many bytes the whole buffer is split at `;` once per frame (cheap enough
/// the spec allows it); above it only ±[`STMT_DIM_SCAN_LINES`] lines around the
/// caret are considered, the same windowing the bracket highlight uses.
const STMT_DIM_MAX_BYTES: usize = 256 * 1024;
/// R56: the ±line window for the active-statement dim on a buffer over
/// [`STMT_DIM_MAX_BYTES`].
const STMT_DIM_SCAN_LINES: usize = 500;

// ─── async ops ───────────────────────────────────────────────────────────────

/// Hard cap on saved SQL favourites (DBX `saved_sql_files`) per connection. The
/// list is a quick-recall surface, not a library: past 100 the `/` filter stops
/// being enough, so a save is refused with a "delete some first" hint.
const SNIPPET_LIMIT: usize = 100;

/// True when `used` favourites already fill the list, so a new save must be
/// refused (the caller shows a "delete some first" hint).
fn snippet_limit_reached(used: usize) -> bool {
    used >= SNIPPET_LIMIT
}

/// One row of the `Ctrl-O` favourites list (DBX's `saved_sql_files`): the store
/// id (needed to delete), the display label (`folder/name`) and the SQL text.
#[derive(Clone)]
struct SnippetRow {
    id: String,
    label: String,
    sql: String,
}

enum Op {
    ListConnections,
    /// R48: DBX Desktop's persisted sidebar groups (`sidebar_layout.layout_json`).
    /// A read-only store query that never touches a database connection; an
    /// unreadable / missing table degrades to the flat list.
    SidebarLayout,
    /// R55: persist the desktop sidebar tree after a group rename / row move.
    /// Written through the kernel store (the single sidebar_layout write point),
    /// never a raw SQL edit of dbx.db.
    SaveSidebarLayout(Box<serde_json::Value>),
    /// Enumerate a connection's databases. The id is [`App::conn_gen`], bumped on
    /// every switch so a slow reply for the connection the user just left is
    /// dropped instead of overwriting the new one's list.
    Databases(Box<ConnectionConfig>, u64),
    /// R83: register a session-only (quick-open SQLite) connection in the
    /// kernel's runtime config cache *without* writing it to the store. Every
    /// store-backed operation resolves a connection by id from that cache, so
    /// the insert must finish before the first query. The bool activates the
    /// connection once registered; a plain cache refresh after a saved-list
    /// reload passes `false`, so re-registering never steals the focus.
    RegisterTempConn(Box<ConnectionConfig>, bool),
    /// R83: drop a temporary connection from the runtime cache and close its
    /// pools (used when the user deletes it). Never touches the store.
    UnregisterTempConn(String),
    /// R52: read the server version once when a connection becomes active — a
    /// single free metadata query (`SELECT version()`, Redis `INFO server`,
    /// Mongo `db.version()`) whose reply is cached for the session. A failure is
    /// not an error the user must see; it just leaves the status bar unnamed.
    ServerVersion(Box<ConnectionConfig>),
    /// R87: an explicit, user-invoked health probe (`P` on the connection tree).
    /// One minimal round trip per backend (`SELECT 1` / `PING` / `db.runCommand({ping:1})`),
    /// whose duration is reported as the connection's latency. Never runs
    /// automatically — this is the one place the browse redline allows a second
    /// query, and only on a deliberate keystroke.
    HealthProbe(Box<ConnectionConfig>),
    /// R96: the explicit full-list probe (`Ctrl-P` on the connection panel) —
    /// one minimal packet per saved connection, at most [`PROBE_CONCURRENCY`] in
    /// flight and [`PROBE_TIMEOUT`] each. Streams a partial result per finished
    /// connection so the tree tail and the progress line update live, then a
    /// final summary. Still a deliberate keystroke — never a background poll.
    ProbeAll(Vec<ConnectionConfig>),
    /// R43: enumerate a non-active connection's databases for the sidebar tree.
    /// `gen` is that connection's own request id, so only the newest reply for
    /// that root is kept.
    TreeDatabases(Box<ConnectionConfig>, u64),
    /// R47b: refresh the cached per-connection liveness from the kernel. This is
    /// a pure registry read ([`AppState::is_connection_open`]), so checking the
    /// state can never itself open a connection.
    ConnStatus {
        ids: Vec<String>,
    },
    /// R47b: manual disconnect — drain the connection's pools through the
    /// kernel's user-disconnect path, which rolls back manual-transaction
    /// sessions before closing (never a bare drain).
    Disconnect {
        id: String,
        name: String,
    },
    /// Enumerate the schemas of one database (PostgreSQL and other
    /// schema-aware engines).
    ListSchemas(Box<ConnectionConfig>, String),
    ListTables(Box<ConnectionConfig>, String, String, u64),
    Columns(Box<ConnectionConfig>, String, String, String),
    /// R109: one explicit column fetch for the sidebar table outline (`>`).
    /// Deliberate keystroke only — never prefetched; the reply lands in the
    /// session outline cache instead of the structure view.
    OutlineColumns(Box<ConnectionConfig>, String, String, String),
    Ddl(Box<ConnectionConfig>, String, String, String),
    /// R107: the explicit complete-DDL fetch behind `D` (dialect source
    /// statement, or the kernel's single-table DDL path).
    TableDdl(Box<ConnectionConfig>, String, String, String),
    /// R102: best-effort read of one table's comment for the structure view.
    TableComment(Box<ConnectionConfig>, String, String, String),
    TableData(Box<TableDataReq>),
    TableColumns(Box<ConnectionConfig>, String, String, String),
    Query(
        Box<ConnectionConfig>,
        String,
        String,
        usize,
        &'static str,
        u64,
    ),
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
    /// R104: the explicit `M` memory sample — one `MEMORY USAGE <key> SAMPLES 0`
    /// per loaded key, at most [`REDIS_MEM_SAMPLE_LIMIT`] keys and
    /// [`REDIS_MEM_CONCURRENCY`] in flight. Streams a partial result per key so
    /// the tails fill in live, then a final summary. Never automatic.
    RedisMemProbe {
        cfg: Box<ConnectionConfig>,
        db: u32,
        /// `(key_raw, key_display)` pairs in current list order.
        keys: Vec<(String, String)>,
        gen: u64,
        truncated: bool,
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
    /// Lazily fetch one database's aggregate size (and, for the current
    /// database, its per-table row estimates) on an explicit `s` (R45). Never
    /// runs on startup or automatically.
    DbSize {
        cfg: Box<ConnectionConfig>,
        db: String,
        schema: String,
        gen: u64,
    },
    History(Box<ConnectionConfig>),
    /// Load the query-history panel: recent entries (newest first, unique SQL)
    /// plus the SQL texts already saved as favourites, in one round trip.
    HistoryPanel(Box<ConnectionConfig>),
    /// Delete one history entry by id (config-store only; never the database).
    HistoryDelete {
        id: String,
    },
    /// Toggle a statement in / out of DBX's `saved_sql_files` favourites.
    HistoryFavorite {
        cfg: Box<ConnectionConfig>,
        sql: String,
        name: String,
    },
    Snippets(Box<ConnectionConfig>),
    /// Save the editor's SQL into DBX's `saved_sql_files` (query favourites).
    SaveSnippet(Box<ConnectionConfig>, String, String),
    /// Delete one saved SQL favourite by id (config-store only; never the DB).
    SnippetDelete {
        id: String,
    },
    DatabasesRefresh(Box<ConnectionConfig>),
    AddConn(Box<ConnectionConfig>),
    /// Copy a saved connection under a fresh id / `-copy` name (tree `Y`), so a
    /// test / prod twin does not have to be re-entered by hand (R45).
    CopyConn(Box<ConnectionConfig>),
    /// Replace an existing saved connection (id is preserved). The kernel has no
    /// UPDATE, so the op removes then re-adds the same id.
    UpdateConn(Box<ConnectionConfig>),
    /// R55: rename a saved connection in place. A *single-connection* store
    /// upsert (the app passes the already-loaded config), so it rewrites only
    /// this one row and can never drop the connection's secrets or its live
    /// pools the way a remove-then-add would.
    RenameConn(Box<ConnectionConfig>),
    /// R68: persist a connection's read-only policy (a connection-level config
    /// field). Same single-row store upsert as [`Op::RenameConn`], so a toggle
    /// rewrites only this row and cannot drop secrets or live pools.
    SetConnReadOnly(Box<ConnectionConfig>),
    /// Remove a saved connection from DBX's store (config only; never touches
    /// the database's data).
    DeleteConn {
        id: String,
        name: String,
    },
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
    /// Stream a result set to a file on a background thread, so a large export
    /// never freezes the UI and never holds the whole document in memory.
    Export(Box<ExportJob>),
    /// R108: package every result tab in one file (multi-sheet XLSX or a ZIP of
    /// per-tab `.sql` files) on a background thread.
    BatchExport(Box<BatchExportJob>),
    /// Scan every text column of every table on one database / schema for a
    /// term, reporting progress between tables. `cancel` lets the UI abort
    /// between tables.
    GlobalSearch {
        cfg: Box<ConnectionConfig>,
        db: String,
        schema: String,
        needle: String,
        scan_limit: usize,
        max_rows: u64,
        gen: u64,
        cancel: Arc<AtomicBool>,
    },
    /// R100: walk every table of one database / schema and build its data
    /// dictionary. Reports progress between tables; `cancel` lets the UI abort
    /// (the partial document is discarded).
    DataDictionary {
        cfg: Box<ConnectionConfig>,
        db: String,
        schema: String,
        gen: u64,
        cancel: Arc<AtomicBool>,
    },
    /// Fetch both tables' columns and indexes and diff them (source is the
    /// desired structure, the generated ALTER rewrites the target).
    DiffTable {
        src_cfg: Box<ConnectionConfig>,
        src_db: String,
        src_schema: String,
        src_table: String,
        tgt_cfg: Box<ConnectionConfig>,
        tgt_db: String,
        tgt_schema: String,
        tgt_table: String,
        gen: u64,
    },
    /// Compare two databases' table lists.
    DiffDatabase {
        src_cfg: Box<ConnectionConfig>,
        src_db: String,
        src_schema: String,
        tgt_cfg: Box<ConnectionConfig>,
        tgt_db: String,
        tgt_schema: String,
        gen: u64,
    },
    /// List a (possibly other) connection's tables to populate the diff target
    /// picker when comparing across connections.
    DiffTablesFor {
        cfg: Box<ConnectionConfig>,
        db: String,
        schema: String,
        gen: u64,
    },
    /// Compare the rows of two tables by primary key, in chunks. Streams
    /// [`OpResult::DataDiffProgress`] between chunks and aborts when `cancel`
    /// is set (keeping the rows found so far).
    DataDiff {
        src_cfg: Box<ConnectionConfig>,
        src_db: String,
        src_schema: String,
        src_table: String,
        tgt_cfg: Box<ConnectionConfig>,
        tgt_db: String,
        tgt_schema: String,
        tgt_table: String,
        where_input: String,
        gen: u64,
        cancel: Arc<AtomicBool>,
    },
    /// Copy one table's structure and/or rows between two SQL connections
    /// (possibly cross-dialect). Streams [`OpResult::TransferProgress`] between
    /// batches and aborts when `cancel` is set (keeping the committed batches).
    DataTransfer(Box<TransferJob>),
    /// Add (and optionally replace-first) a batch of imported connections.
    /// `skipped` / `needs_password` are carried through so the completion status
    /// can report the duplicate-skip count and the “password missing” count.
    ImportConns {
        items: Vec<(Option<String>, Box<ConnectionConfig>)>,
        skipped: usize,
        needs_password: usize,
    },
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
            Op::Export(_) => OP_WATCHDOG_EXPORT,
            Op::BatchExport(_) => OP_WATCHDOG_EXPORT,
            Op::GlobalSearch { .. } => OP_WATCHDOG_SEARCH,
            Op::DataDictionary { .. } => OP_WATCHDOG_SEARCH,
            Op::DataDiff { .. } => OP_WATCHDOG_DATA_DIFF,
            Op::DataTransfer(_) => OP_WATCHDOG_TRANSFER,
            Op::ProbeAll(_) => OP_WATCHDOG_PROBE_ALL,
            _ => OP_WATCHDOG_FALLBACK,
        }
    }
}

enum OpResult {
    Connections(Vec<ConnectionConfig>),
    /// R83: a temporary connection is now in the kernel's runtime cache; the
    /// UI activates it next when `activate` is true.
    TempConnRegistered {
        cfg: Box<ConnectionConfig>,
        activate: bool,
    },
    /// R83: a temporary connection was removed from the runtime cache.
    TempConnUnregistered,
    /// R52: a one-shot server-version read for the connection `id`; `version` is
    /// `None` when the backend could not answer (the read is best-effort).
    /// R63: `rtt` is how long that same read took — a free connect-time latency
    /// probe (no extra query), `None` whenever the version read failed.
    ServerVersion {
        id: String,
        version: Option<String>,
        rtt: Option<Duration>,
    },
    /// R87: the reply to an explicit `P` health probe. `rtt` is `Some` on a
    /// successful round trip, `error` is `Some` with the (localized) failure
    /// reason otherwise. Either way the status bar — never a popup — reports it.
    HealthProbe {
        id: String,
        name: String,
        rtt: Option<Duration>,
        error: Option<String>,
    },
    /// R96: one connection finished during the `Ctrl-P` full-list probe. Updates
    /// the RTT cache / failure set and advances the progress line; it is a
    /// side-channel message, so it never stops the spinner.
    ProbeAllPartial {
        id: String,
        rtt: Option<Duration>,
    },
    /// R96: the full-list probe finished — the status bar summarises
    /// `8 条 · 7 通 · 1 超时` (or the timeout count when some failed).
    ProbeAllDone {
        total: usize,
        ok: usize,
        failed: usize,
    },
    /// R48/R55: the parsed desktop sidebar groups plus the raw store value.
    /// The raw JSON is kept so a group rename / row move can patch just the
    /// affected entry and persist the whole tree back, preserving every
    /// desktop-only field the parser ignores. Empty = flat list.
    SidebarLayout {
        layout: Box<SidebarLayout>,
        raw: Option<Box<serde_json::Value>>,
    },
    /// R55: the desktop sidebar tree was persisted (or the error that stopped
    /// it, so a failed write is never silent).
    SidebarLayoutSaved(Option<String>),
    /// R55: a connection rename landed; carries the updated config so the
    /// picker and tree can merge it in place.
    ConnRenamed(Box<ConnectionConfig>),
    /// R68: a connection's read-only policy was persisted; carries the updated
    /// config so the tree / editor / guards merge the new write policy in place.
    ConnReadOnlySet(Box<ConnectionConfig>),
    /// A saved connection was removed (id + name for the status line).
    ConnDeleted {
        id: String,
        name: String,
    },
    Databases {
        databases: Vec<String>,
        /// Set when the driver could not enumerate databases but the
        /// connection's configured database is still usable — surfaced so the
        /// failure is never silent.
        warning: Option<String>,
        /// [`App::conn_gen`] at request time; a stale reply is dropped.
        gen: u64,
    },
    /// R43: a non-active connection's database list for the sidebar tree, or the
    /// error that prevented it (shown as an error row, never a broken tree).
    TreeDatabases {
        conn_id: String,
        databases: Vec<String>,
        error: Option<String>,
        gen: u64,
    },
    /// R47b: per-connection liveness `(connection id, is open)`, from the
    /// kernel's pool registry.
    ConnStatus(Vec<(String, bool)>),
    /// R47b: a manual disconnect finished. `error` keeps the tree usable when
    /// the kernel could not drain the pools.
    ConnDisconnected {
        id: String,
        name: String,
        error: Option<String>,
    },
    /// One database's lazily fetched size info (R45). `error` keeps the tree
    /// usable when the engine denies the metadata query.
    DbSize {
        db: String,
        info: Box<DbSizeInfo>,
        error: Option<String>,
        gen: u64,
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
    /// R107: the complete-DDL reply behind `D`. `text` is `Some` on success;
    /// `error` is `Some` (and `text` `None`) when the fetch failed, so a
    /// permission / unsupported-object failure is never rendered as a half
    /// statement.
    TableDdl {
        table: String,
        schema: String,
        text: Option<String>,
        error: Option<String>,
    },
    /// R102: a table's comment (best-effort; `None` = none / read failed).
    TableComment {
        table: String,
        schema: String,
        comment: Option<String>,
    },
    TableData {
        grid: Box<Grid>,
        total: Option<u64>,
        /// True when `total` is only a lower bound (the sample cap was hit).
        total_lower_bound: bool,
        has_next: bool,
        page: usize,
        table: String,
        schema: String,
        table_type: Option<String>,
        filter: String,
        order_by: Option<String>,
        /// Primary-key cursor carried on to the next/previous page.
        keyset: Option<KeysetCursor>,
        gen: u64,
    },
    TableColumns {
        table: String,
        schema: String,
        columns: Vec<ColumnInfo>,
        /// R56: cached index metadata so `g c` can mark `MUL` columns without a
        /// query of its own (best effort; empty when the backend cannot list).
        indexes: Vec<IndexInfo>,
        /// R97: cached foreign keys so the cell popup can offer a jump to the
        /// referenced row (best effort; empty when the backend cannot list).
        foreign_keys: Vec<ForeignKeyInfo>,
    },
    /// R109: the reply to one sidebar outline fetch. `key` is the outline cache
    /// key; a failure is carried in-band so the outline can close with a red
    /// status instead of tearing down unrelated state through the generic
    /// error path.
    OutlineColumns {
        key: String,
        table: String,
        result: Result<Vec<ColumnInfo>, String>,
    },
    Query(Box<dbx_core::db::QueryResult>, String, usize, QueryTag),
    Script(Vec<StmtOutcome>, QueryTag),
    /// R99: a query (or script) failed. Carries the [`QueryTag`] so a failure
    /// belonging to a soft-cancelled run is dropped instead of surfacing a
    /// stale error. A live failure flows through the generic error path.
    QueryFailed {
        tag: QueryTag,
        msg: String,
    },
    /// R84: one statement dbxt itself just sent to the server (success or
    /// failure), for the in-memory session run log the Alt-H panel shows at the
    /// top. Client-side bookkeeping only; never persisted and triggers no query.
    SessionRun(Box<SessionRun>),
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
    /// R104: one key finished during an `M` memory sample. Updates the cache and
    /// advances the live `采样中 i/n` line; a side channel that never stops the
    /// spinner.
    RedisMemPartial {
        gen: u64,
        key_raw: String,
        mem: Option<u64>,
    },
    /// R104: the `M` sample finished — the status bar summarises the top key and
    /// the total (and notes a truncated run).
    RedisMemDone {
        gen: u64,
        truncated: bool,
    },
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
    /// The history panel's data: rows newest-first plus favourited SQL texts.
    HistoryPanel {
        rows: Vec<HistoryRow>,
        favorites: Vec<String>,
    },
    HistoryDeleted {
        id: String,
        error: Option<String>,
    },
    HistoryFavorite {
        sql: String,
        favorited: bool,
        error: Option<String>,
    },
    Snippets(Vec<SnippetRow>),
    SnippetSaved(String),
    SnippetDeleted {
        id: String,
        error: Option<String>,
    },
    /// A save was refused before it reached the store (e.g. the 100-item cap).
    SnippetRejected(String),
    DatabasesRefresh(Vec<String>),
    Added(String),
    /// A connection copy (`Y` in the tree) finished; the saved twin is merged
    /// into the picker and the tree without leaving the browse view (R45).
    ConnCopied(Box<ConnectionConfig>),
    /// A batch connection import finished: the saved configs (so the picker can
    /// be refreshed without a round trip) plus the duplicate-skip count, the
    /// “password missing” count and any per-connection errors.
    ConnsImported {
        saved: Vec<ConnectionConfig>,
        skipped: usize,
        needs_password: usize,
        failed: Vec<String>,
    },
    /// A CSV preview plan (may carry a content error the preview displays).
    ImportPlan {
        gen: u64,
        plan: Box<ImportPlan>,
    },
    /// The plan could not be built (unreadable file, no columns): routed back to
    /// the path prompt.
    ImportFailed {
        gen: u64,
        msg: String,
    },
    /// Chunk progress; does not count as the op finishing.
    ImportProgress {
        done: usize,
        total: usize,
    },
    /// R74: per-statement progress for a multi-statement SQL batch; like
    /// [`OpResult::ImportProgress`] it does not count as the op finishing.
    QueryProgress {
        done: usize,
        total: usize,
    },
    ImportDone(Box<ImportReport>),
    /// A background file export finished (or failed). Carries the timing and
    /// byte count for the status line.
    ExportDone {
        format: ExportFormat,
        path: PathBuf,
        rows: usize,
        bytes: u64,
        elapsed_ms: u128,
        error: Option<String>,
    },
    /// R108: a batch (all-tabs) export finished. `tabs`/`rows` describe the
    /// payload, `truncated` names any tab whose sheet hit the R105 row cap.
    BatchExportDone {
        kind: BatchExportKind,
        path: PathBuf,
        tabs: usize,
        rows: usize,
        bytes: u64,
        elapsed_ms: u128,
        truncated: Vec<String>,
        error: Option<String>,
    },
    /// Intermediate global-search progress; like [`OpResult::ImportProgress`] it
    /// does not count as the op finishing.
    SearchProgress {
        gen: u64,
        done: usize,
        total: usize,
    },
    /// A global search finished. `skipped` lists tables too large to scan.
    SearchDone {
        gen: u64,
        hits: Vec<SearchHit>,
        skipped: Vec<(String, u64)>,
        tables: usize,
        truncated: bool,
    },
    /// A global search was aborted between tables (Esc).
    SearchCancelled {
        gen: u64,
    },
    /// R100: intermediate data-dictionary progress (`done` tables of `total`).
    /// A side-channel message, so it does not count as the op finishing.
    DictProgress {
        gen: u64,
        done: usize,
        total: usize,
    },
    /// R100: a data dictionary finished; `content` is the whole Markdown doc.
    DictReady {
        gen: u64,
        db: String,
        tables: usize,
        content: String,
    },
    /// R100: a dictionary walk was aborted between tables (Esc).
    DictCancelled {
        gen: u64,
    },
    /// A blocking SSH prompt (host-key TOFU / keyboard-interactive) the kernel
    /// handshake is waiting on. Not an op completion — handled before the
    /// spinner accounting, like [`OpResult::ImportProgress`].
    SshPrompt(Box<SshPromptEnvelope>),
    /// A best-effort host-key notice (changed / rejected / learn failed).
    SshNotice(Box<SshHostKeyNotice>),
    /// A two-table structure diff finished (source → target).
    DiffReady {
        gen: u64,
        diff: Box<TableDiff>,
    },
    /// A two-database table-list diff finished.
    DbDiffReady {
        gen: u64,
        diff: Box<DbDiff>,
    },
    /// A cross-connection target's table list, for the diff picker.
    DiffTablesFor {
        gen: u64,
        db: String,
        schema: String,
        tables: Vec<String>,
    },
    /// Intermediate data-compare progress; like [`OpResult::ImportProgress`] it
    /// does not count as the op finishing.
    DataDiffProgress {
        gen: u64,
        done: usize,
        total: usize,
    },
    /// A two-table data compare finished (or was aborted; see `result.cancelled`).
    DataDiffDone {
        gen: u64,
        result: Box<DataCompare>,
    },
    /// Intermediate transfer progress: rows written and chunks done so far.
    TransferProgress {
        gen: u64,
        rows: u64,
        chunks: usize,
        elapsed_ms: u128,
        total: Option<u64>,
    },
    /// A transfer's source is large; it waits for a second confirmation before
    /// copying. The wizard sets its warning and re-dispatches with
    /// `allow_large = true` on the next Enter.
    TransferNeedsConfirm {
        gen: u64,
        estimated: u64,
    },
    /// A transfer finished (or was aborted / cancelled; see the report).
    TransferDone {
        gen: u64,
        report: Box<TransferReport>,
    },
    Error(String),
}

/// Persist one executed statement into DBX's shared query history so the
/// editor's `↑`/`↓` recall and the `Alt-H` panel see dbxt's own runs, not just
/// the ones the desktop app recorded. Best-effort: a storage hiccup must never
/// turn a successful query into a failure.
async fn record_history(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    sql: &str,
    error: Option<String>,
    elapsed_ms: u64,
    origin: &str,
) {
    let success = error.is_none();
    let entry = dbx_core::history::HistoryEntry {
        id: Uuid::new_v4().to_string(),
        connection_id: cfg.id.clone(),
        connection_name: cfg.name.clone(),
        database: db.to_string(),
        sql: sql.to_string(),
        executed_at: now_iso8601(),
        execution_time_ms: elapsed_ms as u128,
        success,
        error,
        activity_kind: "query".to_string(),
        operation: String::new(),
        target: String::new(),
        affected_rows: None,
        rollback_sql: None,
        // dbxt stamps its own run origin here (`editor` / `script` / `direct`)
        // so the Alt-H panel can show where a statement came from (R45).
        details_json: Some(serde_json::json!({ "dbxt_origin": origin }).to_string()),
        // dbxt is a local TUI client, not the MCP server: mark the entry as a
        // plain SQL run so it shares the desktop/CLI retention bucket.
        source: "sql".to_string(),
        mcp_tool_name: None,
        mcp_request_json: None,
        mcp_response_json: None,
        mcp_session_id: None,
    };
    let _ = backend.state().storage.save_history_entry(&entry).await;
}

/// R84: wrap a just-finished server run as a [`OpResult::SessionRun`] message so
/// the UI thread can push it onto the in-memory session log. Kept separate from
/// [`record_history`] (which persists to DBX's store) so a storage hiccup can
/// never cost the session log, and vice versa.
fn session_run_msg(
    sql: &str,
    duration_ms: u64,
    success: bool,
    origin: &'static str,
    connection_name: &str,
) -> OpResult {
    OpResult::SessionRun(Box::new(SessionRun {
        sql: sql.to_string(),
        executed_at: now_iso8601(),
        duration_ms,
        success,
        origin,
        connection_name: connection_name.to_string(),
    }))
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
    parts.push(format!("{}ms", r.execution_time_ms));
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
    let grid = Grid::from_query(result.columns, result.column_types, &result.rows, note);
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
    match backend
        .execute_query(cfg, db, &sql, Some(1), Some(10))
        .await
    {
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

/// Build the SQL for one table-browser page, plus whether the fetched rows must
/// be reversed before display (a backwards keyset read fetches nearest-first).
///
/// With a keyset plan the page is always fetched in primary-key order, which is
/// what makes the seek predicate sound; a `Before` read walks the key backwards
/// and reverses afterwards so the page is still shown in display order.
#[allow(clippy::too_many_arguments)]
fn build_table_page_query(
    cfg: &ConnectionConfig,
    schema: Option<&str>,
    table: &str,
    table_type: Option<&str>,
    page: usize,
    page_size: usize,
    filter: &str,
    order_by: Option<&str>,
    keyset_pk: &[String],
    keyset_asc: bool,
    seek: &PageSeek,
) -> (String, bool) {
    let (seek_op, fetch_asc, reverse) = match seek {
        PageSeek::Before(_) => (if keyset_asc { "<" } else { ">" }, !keyset_asc, true),
        _ => (if keyset_asc { ">" } else { "<" }, keyset_asc, false),
    };
    let keyset_where = match seek {
        PageSeek::After(v) | PageSeek::Before(v) if !keyset_pk.is_empty() => {
            table_data_keyset_predicate(Some(cfg.db_type), keyset_pk, v, seek_op)
        }
        _ => None,
    };
    let where_input = match (&keyset_where, filter.is_empty()) {
        (Some(k), true) => Some(k.clone()),
        (Some(k), false) => Some(format!("({filter}) AND ({k})")),
        (None, true) => None,
        (None, false) => Some(filter.to_string()),
    };
    let order_by_effective = if keyset_pk.is_empty() {
        order_by.map(str::to_string)
    } else {
        Some(
            keyset_pk
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        quote_table_identifier(Some(cfg.db_type), c),
                        if fetch_asc { "ASC" } else { "DESC" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    let keyset_active = !keyset_pk.is_empty() && keyset_where.is_some();
    let offset = if keyset_active { 0 } else { page * page_size };
    let options = TableDataSelectSqlOptions {
        database_type: Some(cfg.db_type),
        schema: schema.map(str::to_string),
        table_name: table.to_string(),
        table_type: table_type.map(str::to_string),
        limit: Some(page_size + 1),
        offset: Some(offset),
        where_input,
        order_by: order_by_effective,
        ..Default::default()
    };
    (
        build_table_data_select_sql_with_database(options, false),
        reverse,
    )
}

/// The derived-table `COUNT(*)` that stops once the sample cap is reached:
/// `SELECT COUNT(*) FROM (SELECT 1 FROM t [WHERE …] LIMIT cap+1) dbxt_count`.
///
/// `SELECT 1` keeps the scan index-only, so a huge InnoDB table is sampled in
/// milliseconds instead of materialising 500k full rows. Only dialects with the
/// plain `LIMIT` pager are handled here; anything else returns `None` and the
/// caller runs the exact `COUNT(*)` as it always did.
fn bounded_count_sql(
    cfg: &ConnectionConfig,
    schema: Option<&str>,
    table: &str,
    filter: &str,
    limit: u64,
) -> Option<String> {
    if table_pagination_strategy(Some(cfg.db_type)) != TablePaginationStrategy::LimitOffset {
        return None;
    }
    let qualified = qualified_table_name(Some(cfg.db_type), schema, table);
    let where_clause = if filter.is_empty() {
        String::new()
    } else {
        format!(" WHERE ({filter})")
    };
    Some(format!(
        "SELECT COUNT(*) AS row_count FROM (SELECT 1 FROM {qualified}{where_clause} LIMIT {}) dbxt_count",
        limit.saturating_add(1)
    ))
}

/// Row count for a table page whose total is not cached yet: a bounded sample
/// when the dialect allows one, else the exact `COUNT(*)`. Returns
/// `(value, is_lower_bound)`.
async fn sample_row_count(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: Option<&str>,
    table: &str,
    filter: &str,
) -> Option<(u64, bool)> {
    match count_sample_limit() {
        // No cap: always run the exact count.
        None => exact_row_count(backend, cfg, db, schema, table, filter)
            .await
            .map(|n| (n, false)),
        Some(limit) => match bounded_count_sql(cfg, schema, table, filter, limit) {
            None => exact_row_count(backend, cfg, db, schema, table, filter)
                .await
                .map(|n| (n, false)),
            Some(sql) => match backend
                .execute_query(cfg, db, &sql, Some(1), Some(15))
                .await
            {
                Ok(c) => match c
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(count_value)
                {
                    Some(n) => Some(classify_sample(n, limit)),
                    // Should not happen, but never lose the total entirely.
                    None => exact_row_count(backend, cfg, db, schema, table, filter)
                        .await
                        .map(|n| (n, false)),
                },
                // A dialect quirk in the sample query still gets its exact count.
                Err(_) => exact_row_count(backend, cfg, db, schema, table, filter)
                    .await
                    .map(|n| (n, false)),
            },
        },
    }
}

/// Exact `COUNT(*)` for a table, honouring the active `WHERE` (best effort).
async fn exact_row_count(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: Option<&str>,
    table: &str,
    filter: &str,
) -> Option<u64> {
    let base = build_count_table_sql(Some(cfg.db_type), schema, table);
    let sql = if filter.is_empty() {
        base
    } else {
        format!("{base} WHERE ({filter})")
    };
    let c = backend
        .execute_query(cfg, db, &sql, Some(1), Some(15))
        .await
        .ok()?;
    c.rows
        .first()
        .and_then(|row| row.first())
        .and_then(count_value)
}

/// One `COUNT(*)` cell, as a number.
fn count_value(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// The primary-key tuples of a page's first and last rows, or `None` when the
/// result does not carry every key column (or a key came back NULL).
fn keyset_cursor(
    pk: &[String],
    ascending: bool,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
) -> Option<KeysetCursor> {
    if pk.is_empty() || rows.is_empty() {
        return None;
    }
    let idx: Option<Vec<usize>> = pk
        .iter()
        .map(|c| columns.iter().position(|col| col.eq_ignore_ascii_case(c)))
        .collect();
    let idx = idx?;
    let tuple = |row: &Vec<serde_json::Value>| {
        idx.iter()
            .map(|&i| row.get(i).cloned().unwrap_or(serde_json::Value::Null))
            .collect::<Vec<_>>()
    };
    let first = tuple(rows.first()?);
    let last = tuple(rows.last()?);
    if first
        .iter()
        .chain(last.iter())
        .any(serde_json::Value::is_null)
    {
        return None;
    }
    Some(KeysetCursor {
        pk: pk.to_vec(),
        ascending,
        first,
        last,
    })
}

/// The free one-liner that names the server for a SQL dialect. SQLite has no
/// `version()`; every other supported engine spells it `SELECT version()`.
fn server_version_query(db_type: &str) -> &'static str {
    match db_type.to_ascii_lowercase().as_str() {
        // SQLite-family engines expose the version as `sqlite_version()`.
        "sqlite" | "sqlite3" | "libsql" | "turso" | "rqlite" | "cloudflare-d1" => {
            "SELECT sqlite_version()"
        }
        // SQL Server spells it `@@VERSION` rather than `version()`.
        "sqlserver" | "mssql" => "SELECT @@VERSION",
        _ => "SELECT version()",
    }
}

/// `redis_version:` out of an `INFO server` reply. The reply arrives as a bulk
/// string (or, under some drivers, an array of lines), so both shapes are tried.
fn parse_redis_version(v: &serde_json::Value) -> Option<String> {
    let text = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|i| match i {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    text.lines()
        .find_map(|l| l.trim().strip_prefix("redis_version:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the active backend's server version once. Best-effort: any error just
/// yields `None`, so the status bar simply omits the version.
///
/// Only the first row of the first column is read, with a short timeout; this is
/// free metadata, not a scan.
fn scalar_version(r: dbx_core::db::QueryResult) -> Option<String> {
    first_cell_text(&r.rows)
}

/// The trimmed text of the grid's top-left cell, or `None` when the grid is
/// empty or the cell text is blank. Kept separate from `scalar_version` so the
/// parsing is unit-testable without building a full `QueryResult`.
fn first_cell_text(rows: &[Vec<serde_json::Value>]) -> Option<String> {
    rows.first()
        .and_then(|row| row.first())
        .map(value_to_val)
        .map(|v| v.text().trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_server_version(backend: &LocalBackend, cfg: &ConnectionConfig) -> Option<String> {
    match cfg.db_type.as_str() {
        "redis" | "keydb" | "valkey" => backend
            .execute_redis_command(cfg, 0, "INFO server", true)
            .await
            .ok()
            .and_then(|r| parse_redis_version(&r.value)),
        "mongodb" | "mongo" => {
            let db = cfg
                .database
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| "admin".to_string());
            match dbx_core::mongo_shell::parse("db.version()") {
                Ok(cmd) => backend
                    .execute_mongo_command(cfg, &db, &cmd)
                    .await
                    .ok()
                    .and_then(scalar_version),
                Err(_) => None,
            }
        }
        other => {
            let db = cfg.database.clone().unwrap_or_default();
            backend
                .execute_query(cfg, &db, server_version_query(other), Some(1), Some(5))
                .await
                .ok()
                .and_then(scalar_version)
        }
    }
}

/// R87: one minimal round trip for the explicit `P` health probe, timed. The
/// driver-specific probe is `SELECT 1` (SQL), `PING` (Redis) and
/// `db.runCommand({ping:1})` (Mongo). Returns the round-trip duration, or the
/// driver's error text so the status bar can name the failure.
async fn probe_connection(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
) -> Result<Duration, String> {
    let started = Instant::now();
    match cfg.db_type.as_str() {
        "redis" | "keydb" | "valkey" => backend
            .execute_redis_command(cfg, 0, "PING", true)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        "mongodb" | "mongo" => {
            let db = cfg
                .database
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| "admin".to_string());
            match dbx_core::mongo_shell::parse("db.runCommand({ ping: 1 })") {
                Ok(cmd) => backend
                    .execute_mongo_command(cfg, &db, &cmd)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            }
        }
        _ => {
            let db = cfg.database.clone().unwrap_or_default();
            backend
                .execute_query(cfg, &db, "SELECT 1", Some(1), Some(5))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
    .map(|_| started.elapsed())
}

/// R96: run one probe per item with a bounded fan-out and a per-probe
/// ceiling. At most `limit` probes are in flight; each is raced against
/// `per_timeout`, and a timeout becomes an `Err` just like a driver error, so
/// the caller can count it. `on_done` fires as each probe finishes (whatever
/// order), which is what drives the live progress line. Returns
/// `(ok, failed)`. Generic over the item and the probe so it can be unit-tested
/// without a socket.
pub(crate) async fn probe_batch<T, P, Fut>(
    items: Vec<T>,
    limit: usize,
    per_timeout: Duration,
    probe: P,
    mut on_done: impl FnMut(&T, Result<Duration, String>),
) -> (usize, usize)
where
    T: Clone,
    P: Fn(T) -> Fut,
    Fut: std::future::Future<Output = Result<Duration, String>>,
{
    let mut stream = futures::stream::iter(items)
        .map(|item| {
            let fut = probe(item.clone());
            async move {
                let res = match tokio::time::timeout(per_timeout, fut).await {
                    Ok(r) => r,
                    Err(_) => Err(PROBE_TIMEOUT_SENTINEL.to_string()),
                };
                (item, res)
            }
        })
        .buffer_unordered(limit.max(1));
    let (mut ok, mut failed) = (0usize, 0usize);
    while let Some((item, res)) = stream.next().await {
        if res.is_ok() {
            ok += 1;
        } else {
            failed += 1;
        }
        on_done(&item, res);
    }
    (ok, failed)
}

/// R96: the marker a per-probe timeout carries back from [`probe_connections`].
/// Only used for unit tests / diagnostics — the UI turns any `Err` into the
/// `· 超时` tail, so the exact text is never shown.
pub(crate) const PROBE_TIMEOUT_SENTINEL: &str = "__probe_timeout__";

/// R104: run one `MEMORY USAGE` probe per item with a bounded fan-out (the R96
/// `Ctrl-P` pattern, but the payload is `Option<u64>` — `None` for a nil /
/// failed / timed-out key). At most `limit` probes are in flight; `on_done`
/// fires as each finishes, which drives the live progress line. Generic over the
/// item and the probe so it can be unit-tested without a socket.
pub(crate) async fn redis_mem_probe_batch<T, P, Fut>(
    items: Vec<T>,
    limit: usize,
    probe: P,
    mut on_done: impl FnMut(&T, Option<u64>),
) where
    T: Clone,
    P: Fn(T) -> Fut,
    Fut: std::future::Future<Output = Option<u64>>,
{
    let mut stream = futures::stream::iter(items)
        .map(|item| {
            let fut = probe(item.clone());
            async move { (item, fut.await) }
        })
        .buffer_unordered(limit.max(1));
    while let Some((item, mem)) = stream.next().await {
        on_done(&item, mem);
    }
}

async fn run_op(backend: &LocalBackend, op: Op, tx: &Tx) -> OpResult {
    match op {
        Op::ListConnections => match backend.load_connections().await {
            Ok(cs) => OpResult::Connections(cs),
            Err(e) => OpResult::Error(format!("load connections: {e}")),
        },
        // R83: inject a quick-open connection into the runtime cache so the
        // store-backed operations can resolve it by id. Nothing is persisted.
        Op::RegisterTempConn(cfg, activate) => {
            backend
                .state()
                .configs
                .write()
                .await
                .insert(cfg.id.clone(), (*cfg).clone());
            OpResult::TempConnRegistered { cfg, activate }
        }
        Op::UnregisterTempConn(id) => {
            backend.state().configs.write().await.remove(&id);
            backend.state().remove_connection_pools_detached(&id).await;
            OpResult::TempConnUnregistered
        }
        // R48: read the desktop sidebar tree. A missing table (a store the
        // desktop never wrote) or an unreadable value is not an error — it just
        // means “no groups”, so the tree stays flat.
        Op::SidebarLayout => {
            let raw = backend
                .state()
                .storage
                .load_sidebar_layout()
                .await
                .ok()
                .flatten();
            let layout = raw.as_ref().map(parse_sidebar_layout).unwrap_or_default();
            OpResult::SidebarLayout {
                layout: Box::new(layout),
                raw: raw.map(Box::new),
            }
        }
        // R55: the single sidebar-layout write point. Group rename / row move
        // patch the raw JSON the tree was parsed from and save it here, so the
        // desktop's own fields survive untouched.
        Op::SaveSidebarLayout(raw) => match backend.state().storage.save_sidebar_layout(&raw).await
        {
            Ok(()) => OpResult::SidebarLayoutSaved(None),
            Err(e) => OpResult::SidebarLayoutSaved(Some(e)),
        },
        Op::Databases(cfg, gen) => match backend.list_databases(&cfg).await {
            Ok(dbs) if !dbs.is_empty() => OpResult::Databases {
                databases: dbs,
                warning: None,
                gen,
            },
            // A backend that legitimately exposes no database list (e.g. SQLite)
            // still connects using the configured database.
            Ok(_) => OpResult::Databases {
                databases: vec![cfg.database.clone().unwrap_or_default()],
                warning: None,
                gen,
            },
            Err(e) => {
                let warning = if cfg.has_effective_ssh_tunnels() {
                    ssh_connect_error_message(&cfg, &e)
                } else {
                    match cfg.database.as_deref() {
                        Some(db) if !db.is_empty() => tf(
                            "无法列举数据库（{}），仅使用配置库 {}",
                            &[&(e), &(fix_double_encoding(db))],
                        ),
                        _ => tf("无法列举数据库（{}），将使用连接默认库", &[&(e)]),
                    }
                };
                OpResult::Databases {
                    databases: vec![cfg.database.clone().unwrap_or_default()],
                    warning: Some(warning),
                    gen,
                }
            }
        },
        Op::TreeDatabases(cfg, gen) => {
            let conn_id = cfg.id.clone();
            match backend.list_databases(&cfg).await {
                Ok(dbs) => OpResult::TreeDatabases {
                    conn_id,
                    databases: if dbs.is_empty() {
                        vec![cfg.database.clone().unwrap_or_default()]
                    } else {
                        dbs
                    },
                    error: None,
                    gen,
                },
                Err(e) => {
                    let msg = if cfg.has_effective_ssh_tunnels() {
                        ssh_connect_error_message(&cfg, &e)
                    } else {
                        tf("无法列举数据库（{}）", &[&(e)])
                    };
                    OpResult::TreeDatabases {
                        conn_id,
                        databases: Vec::new(),
                        error: Some(msg),
                        gen,
                    }
                }
            }
        }
        // R52: read the server version once, on connect, and cache it. This is
        // the only version query dbxt ever issues; it never runs on the browse
        // hot path.
        Op::ServerVersion(cfg) => {
            // R63: time the version read so its duration doubles as the
            // connection's round-trip latency. No second probe is issued — the
            // redline stays "one free query at connect".
            let started = Instant::now();
            let version = fetch_server_version(backend, &cfg).await;
            let rtt = version.is_some().then(|| started.elapsed());
            OpResult::ServerVersion {
                id: cfg.id.clone(),
                version,
                rtt,
            }
        }
        // R87: an explicit `P` probe — one minimal round trip, timed. This is
        // the single user-invoked exception to the "no query on the browse hot
        // path" redline.
        Op::HealthProbe(cfg) => {
            let (id, name) = (cfg.id.clone(), cfg.name.clone());
            match probe_connection(backend, &cfg).await {
                Ok(rtt) => OpResult::HealthProbe {
                    id,
                    name,
                    rtt: Some(rtt),
                    error: None,
                },
                Err(e) => OpResult::HealthProbe {
                    id,
                    name,
                    rtt: None,
                    error: Some(if cfg.has_effective_ssh_tunnels() {
                        ssh_connect_error_message(&cfg, &e)
                    } else {
                        e
                    }),
                },
            }
        }
        // R96: the explicit full-list probe. Streams a partial result per
        // finished connection (so the tree tail and progress line update live),
        // then returns the summary. A timeout at the per-probe ceiling is
        // reported exactly like a driver error: both mean "this one did not
        // answer in time".
        Op::ProbeAll(cfgs) => {
            let total = cfgs.len();
            let (ok, failed) = probe_batch(
                cfgs,
                PROBE_CONCURRENCY,
                PROBE_TIMEOUT,
                |cfg: ConnectionConfig| async move { probe_connection(backend, &cfg).await },
                |cfg, res| {
                    let _ = tx.send(OpResult::ProbeAllPartial {
                        id: cfg.id.clone(),
                        rtt: res.ok(),
                    });
                },
            )
            .await;
            OpResult::ProbeAllDone { total, ok, failed }
        }
        // R47b: pure registry reads. `is_connection_open` never opens a
        // connection, so the status refresh cannot itself change the state it
        // reports.
        Op::ConnStatus { ids } => {
            let state = backend.state();
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                let open = state.is_connection_open(&id).await;
                out.push((id, open));
            }
            OpResult::ConnStatus(out)
        }
        // R47b: the kernel's user-disconnect path. `remove_connection_pools`
        // invalidates the lifecycle, rolls back every manual-transaction session
        // (so an open BEGIN cannot survive the disconnect), then closes the
        // pools. This is the only disconnect path; dbxt never drains bare.
        Op::Disconnect { id, name } => {
            backend.state().remove_connection_pools(&id).await;
            OpResult::ConnDisconnected {
                id,
                name,
                error: None,
            }
        }
        Op::DbSize {
            cfg,
            db,
            schema,
            gen,
        } => {
            let dt = cfg.db_type.as_str();
            if is_mysql_family(dt) {
                match backend
                    .execute_query(&cfg, &db, &mysql_db_size_sql(&db), None, Some(30))
                    .await
                {
                    Ok(r) => OpResult::DbSize {
                        db,
                        info: Box::new(parse_db_size_info(&r.rows)),
                        error: None,
                        gen,
                    },
                    Err(e) => OpResult::DbSize {
                        db,
                        info: Box::default(),
                        error: Some(e.to_string()),
                        gen,
                    },
                }
            } else if is_postgres_family(dt) {
                let mut info = DbSizeInfo::default();
                let mut error: Option<String> = None;
                match backend
                    .execute_query(&cfg, &db, &pg_db_size_sql(&db), None, Some(30))
                    .await
                {
                    Ok(r) => {
                        info.total_bytes = r
                            .rows
                            .first()
                            .and_then(|row| row.first())
                            .and_then(json_u64);
                    }
                    Err(e) => error = Some(e.to_string()),
                }
                match backend
                    .execute_query(&cfg, &db, &pg_table_size_sql(&schema), None, Some(30))
                    .await
                {
                    Ok(r) => {
                        let t = parse_db_size_info(&r.rows);
                        info.rows = t.rows;
                        info.sizes = t.sizes;
                    }
                    Err(e) => {
                        if error.is_none() {
                            error = Some(e.to_string());
                        }
                    }
                }
                OpResult::DbSize {
                    db,
                    info: Box::new(info),
                    error,
                    gen,
                }
            } else {
                OpResult::DbSize {
                    db,
                    info: Box::default(),
                    error: Some(t("该引擎不支持尺寸查询").into()),
                    gen,
                }
            }
        }
        Op::ListSchemas(cfg, db) => {
            // `list_schemas_core` is the kernel's schema enumerator; it hides
            // system schemas unless the connection opts in (`show_system_schemas`).
            match dbx_core::schema::list_schemas_core(backend.state().as_ref(), &cfg.id, &db).await
            {
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
        Op::ListTables(cfg, db, schema, gen) => match backend.list_tables(&cfg, &db, &schema).await
        {
            Ok(t) => OpResult::TablesFor { tables: t, gen },
            Err(e) => OpResult::Error(format!("list tables: {e}")),
        },
        Op::Columns(cfg, db, schema, table) => {
            match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(c) => OpResult::Columns {
                    table,
                    schema,
                    columns: c,
                },
                Err(e) => OpResult::Error(format!("columns: {e}")),
            }
        }
        Op::OutlineColumns(cfg, db, schema, table) => {
            let key = outline_key(&cfg.id, &db, &schema, &table);
            match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(columns) => OpResult::OutlineColumns {
                    key,
                    table,
                    result: Ok(columns),
                },
                Err(e) => OpResult::OutlineColumns {
                    key,
                    table,
                    result: Err(e.to_string()),
                },
            }
        }
        Op::Ddl(cfg, db, schema, table) => {
            // PostgreSQL renders `"schema"."table"`; when the schema layer did
            // not produce one (a failed `list_schemas`, an engine we do not
            // browse schemas for) resolve the relation's visible schema first,
            // because an empty schema yields the unusable `""."table"`. The
            // *requested* schema is what the reply carries back, so the result
            // guard keeps working even when the fallback resolved a different
            // name.
            let effective = if schema.trim().is_empty() && is_postgres_family(cfg.db_type.as_str())
            {
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
        Op::TableDdl(cfg, db, schema, table) => {
            // R107: the explicit `D` fetch. MySQL / SQLite issue their plain
            // source statement and take the DDL cell; every other dialect goes
            // through the kernel's single-table DDL path. The result is a
            // `Result`, so a failure is reported instead of rendered.
            let effective = if schema.trim().is_empty() && is_postgres_family(cfg.db_type.as_str())
            {
                resolve_ddl_schema(backend, &cfg, &db, &table).await
            } else {
                schema.clone()
            };
            let outcome: std::result::Result<String, String> =
                match table_source_sql(cfg.db_type, &db, &effective, &table) {
                    Some(sql) => match backend
                        .execute_query(&cfg, &db, &sql, Some(1), Some(30))
                        .await
                    {
                        Ok(r) => ddl_from_rows(cfg.db_type, &r.rows),
                        Err(e) => Err(e),
                    },
                    None => {
                        dbx_core::schema::get_table_ddl_core(
                            backend.state().as_ref(),
                            &cfg.id,
                            &db,
                            &effective,
                            &table,
                            None,
                        )
                        .await
                    }
                };
            let (text, error) = match outcome {
                Ok(ddl) if !ddl.trim().is_empty() => (Some(ddl), None),
                Ok(_) => (None, Some(t("DDL 结果为空").to_string())),
                Err(e) => (None, Some(e)),
            };
            OpResult::TableDdl {
                table,
                schema,
                text,
                error,
            }
        }
        Op::TableComment(cfg, db, schema, table) => {
            // Best effort: a failed comment read (an engine without comments, a
            // permission issue) simply reports no comment and never an error.
            let effective = if schema.trim().is_empty() && is_postgres_family(cfg.db_type.as_str())
            {
                resolve_ddl_schema(backend, &cfg, &db, &table).await
            } else {
                schema.clone()
            };
            let comment = dbx_core::schema::get_table_comment_core(
                backend.state().as_ref(),
                &cfg.id,
                &db,
                &effective,
                &table,
            )
            .await
            .ok()
            .flatten()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
            OpResult::TableComment {
                table,
                schema,
                comment,
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
                keyset_pk,
                keyset_asc,
                seek,
                gen,
            } = *req;
            // A user filter may be typed with a leading WHERE; strip it so it can
            // be embedded as a predicate.
            let filter = normalize_where_input(Some(&filter));
            let schema_opt = (!schema.trim().is_empty()).then(|| schema.clone());
            let (sql, reverse) = build_table_page_query(
                &cfg,
                schema_opt.as_deref(),
                &table,
                table_type.as_deref(),
                page,
                page_size,
                &filter,
                order_by.as_deref(),
                &keyset_pk,
                keyset_asc,
                &seek,
            );
            let query_timeout = cfg.effective_query_timeout_secs();
            match backend
                .execute_query(&cfg, &db, &sql, Some(page_size + 1), Some(query_timeout))
                .await
            {
                Ok(r) => {
                    let columns = r.columns;
                    let types = r.column_types;
                    let mut rows = r.rows;
                    let ms = r.execution_time_ms;
                    let has_next = if reverse {
                        // Walking backwards: the row we came from is always
                        // ahead of the page we just fetched.
                        !rows.is_empty()
                    } else {
                        rows.len() > page_size
                    };
                    rows.truncate(page_size);
                    if reverse {
                        rows.reverse();
                    }
                    // Primary-key tuples of this page's edges, carried back so
                    // `n`/`p` can seek from here.
                    let keyset = keyset_cursor(&keyset_pk, keyset_asc, &columns, &rows);
                    let grid = Grid::from_query(columns, types, &rows, format!("{ms}ms"));
                    // Row count: cached if the session already knows it, else a
                    // bounded sample. Counting only up to the cap keeps the first
                    // page fast on a huge table — a full InnoDB `COUNT(*)` is a
                    // multi-second scan — at the cost of a `>N` lower bound.
                    let (total, total_lower_bound) = match known_total {
                        Some((v, lb)) => (Some(v), lb),
                        None => match sample_row_count(
                            backend,
                            &cfg,
                            &db,
                            schema_opt.as_deref(),
                            &table,
                            &filter,
                        )
                        .await
                        {
                            Some((v, lb)) => (Some(v), lb),
                            None => (None, false),
                        },
                    };
                    OpResult::TableData {
                        grid: Box::new(grid),
                        total,
                        total_lower_bound,
                        has_next,
                        page,
                        table,
                        schema,
                        table_type,
                        filter,
                        order_by,
                        keyset,
                        gen,
                    }
                }
                Err(e) => OpResult::Error(format!(
                    "table data: {}",
                    query_error_text(&e, query_timeout)
                )),
            }
        }
        Op::TableColumns(cfg, db, schema, table) => {
            match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(columns) => {
                    // R56: fetch the indexes in the same metadata pass (best
                    // effort) so the `g c` popup can mark a non-unique index
                    // column `MUL` from cache. MUL is a MySQL-family concept, so
                    // the extra read is skipped elsewhere.
                    let indexes = if is_mysql_family(cfg.db_type.as_str()) {
                        list_indexes_best_effort(backend, &cfg, &db, &schema, &table).await
                    } else {
                        Vec::new()
                    };
                    // R97: foreign keys ride the same metadata pass (best
                    // effort) so the cell popup can offer a jump to the
                    // referenced row without a query of its own. Every SQL
                    // backend that reports them is welcome; a driver that
                    // cannot simply yields an empty list.
                    let foreign_keys =
                        list_foreign_keys_best_effort(backend, &cfg, &db, &schema, &table).await;
                    OpResult::TableColumns {
                        table,
                        schema,
                        columns,
                        indexes,
                        foreign_keys,
                    }
                }
                Err(e) => OpResult::Error(format!("table columns: {e}")),
            }
        }
        Op::Query(cfg, db, sql, cap, origin, epoch) => {
            let cap = cap.max(1);
            // R99: the reply carries this tag so a soft-cancelled run's late
            // result can be identified and dropped.
            let tag = QueryTag {
                conn_id: cfg.id.clone(),
                epoch,
            };
            // R58: the connection's own limit drives the driver-side timeout, so
            // a form-set 30 s bounds the query instead of the old fixed 60 s.
            let timeout = cfg.effective_query_timeout_secs();
            // Record only a fresh run, never a `Ctrl-N` load-more (which re-runs
            // the same statement with a higher cap and would duplicate it).
            let record = cap <= QUERY_MAX_ROWS;
            let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
            if statements.len() > 1 {
                let options = QueryExecutionOptions {
                    max_rows: Some(cap),
                    timeout_secs: Some(timeout),
                    ..Default::default()
                };
                // R74: run the batch through the core's progress-aware entry
                // point so the status bar can show `3/7` as statements land.
                // This is the same call `LocalBackend::execute_batch` makes,
                // just with a progress callback attached.
                let progress: Option<dbx_core::query::ExecuteMultiProgressCallback> = {
                    let tx = tx.clone();
                    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
                    Some(Arc::new(move |p: dbx_core::query::ExecuteMultiProgress| {
                        // Coalesce to ~10/s (but always forward the last
                        // statement) so a thousand-statement script cannot
                        // flood the UI channel.
                        let now = std::time::Instant::now();
                        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                        if p.completed < p.total
                            && now.duration_since(*last) < std::time::Duration::from_millis(100)
                        {
                            return;
                        }
                        *last = now;
                        drop(last);
                        let _ = tx.send(OpResult::QueryProgress {
                            done: p.completed,
                            total: p.total,
                        });
                    }))
                };
                let batch =
                    dbx_core::query::execute_multi_core_with_options_for_client_and_progress(
                        backend.state().as_ref(),
                        &cfg.id,
                        &db,
                        &sql,
                        None,
                        None,
                        options,
                        progress,
                    )
                    .await;
                match batch {
                    Ok(results) => {
                        let results: Vec<BatchStatementResult> = results
                            .into_iter()
                            .map(BatchStatementResult::from)
                            .collect();
                        let mut outcomes: Vec<StmtOutcome> = Vec::new();
                        let mut total_ms: u64 = 0;
                        for (idx, r) in results.into_iter().enumerate() {
                            let text = statements
                                .get(idx)
                                .cloned()
                                .unwrap_or_else(|| format!("-- statement {}", idx + 1));
                            total_ms = total_ms.saturating_add(r.result.execution_time_ms as u64);
                            outcomes.push(stmt_outcome(text, r));
                        }
                        if record {
                            record_history(backend, &cfg, &db, &sql, None, total_ms, origin).await;
                            let _ =
                                tx.send(session_run_msg(&sql, total_ms, true, origin, &cfg.name));
                        }
                        OpResult::Script(outcomes, tag.clone())
                    }
                    Err(e) => {
                        if record {
                            record_history(backend, &cfg, &db, &sql, Some(e.clone()), 0, origin)
                                .await;
                            let _ = tx.send(session_run_msg(&sql, 0, false, origin, &cfg.name));
                        }
                        OpResult::QueryFailed {
                            tag: tag.clone(),
                            msg: format!("script: {}", query_error_text(&e, timeout)),
                        }
                    }
                }
            } else {
                match backend
                    .execute_query(&cfg, &db, &sql, Some(cap), Some(timeout))
                    .await
                {
                    Ok(r) => {
                        if record {
                            record_history(
                                backend,
                                &cfg,
                                &db,
                                &sql,
                                None,
                                r.execution_time_ms as u64,
                                origin,
                            )
                            .await;
                            let _ = tx.send(session_run_msg(
                                &sql,
                                r.execution_time_ms as u64,
                                true,
                                origin,
                                &cfg.name,
                            ));
                        }
                        OpResult::Query(Box::new(r), sql, cap, tag)
                    }
                    Err(e) => {
                        if record {
                            record_history(backend, &cfg, &db, &sql, Some(e.clone()), 0, origin)
                                .await;
                            let _ = tx.send(session_run_msg(&sql, 0, false, origin, &cfg.name));
                        }
                        OpResult::QueryFailed {
                            tag,
                            msg: format!("query: {}", query_error_text(&e, timeout)),
                        }
                    }
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
        // R104: the explicit `M` memory sample. One `MEMORY USAGE … SAMPLES 0`
        // per key, 4 in flight, a 10 s ceiling each. A driver / server error or
        // a timeout is `None` (`· ?`), never a hard failure — a Redis < 4.0
        // server simply reports every key as unavailable instead of aborting.
        Op::RedisMemProbe {
            cfg,
            db,
            keys,
            gen,
            truncated,
        } => {
            redis_mem_probe_batch(
                keys,
                REDIS_MEM_CONCURRENCY,
                |(_, display): (String, String)| {
                    let cfg = cfg.clone();
                    async move {
                        let cmd = redis_memory_command(&display);
                        match tokio::time::timeout(
                            REDIS_MEM_TIMEOUT,
                            backend.execute_redis_command(&cfg, db, &cmd, true),
                        )
                        .await
                        {
                            Ok(Ok(r)) => parse_memory_usage(&r.value),
                            _ => None,
                        }
                    }
                },
                |(raw, _), mem| {
                    let _ = tx.send(OpResult::RedisMemPartial {
                        gen,
                        key_raw: raw.clone(),
                        mem,
                    });
                },
            )
            .await;
            OpResult::RedisMemDone { gen, truncated }
        }
        Op::RedisWrite {
            cfg,
            db,
            cmd,
            reload_value,
            reload_list,
        } => match backend.execute_redis_command(&cfg, db, &cmd, true).await {
            Ok(r) => {
                let summary =
                    serde_json::to_string(&r.value).unwrap_or_else(|_| format!("{:?}", r.value));
                OpResult::RedisWritten {
                    cmd,
                    summary: truncate_disp(&one_line(&summary), 160),
                    reload_value,
                    reload_list,
                }
            }
            Err(e) => OpResult::Error(if cfg.has_effective_ssh_tunnels() {
                ssh_connect_error_message(&cfg, &e)
            } else {
                format!("redis: {e}")
            }),
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
        }
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
                    let grid = mongo_docs_grid_with_sizes(&docs);
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
        Op::MongoIndexes {
            cfg,
            db,
            collection,
        } => {
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
                    let grid = Grid::from_query(
                        qr.columns,
                        qr.column_types,
                        &qr.rows,
                        tf("{} 个索引", &[&(qr.rows.len())]),
                    );
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
                    Err(_) => format!("{:?}", r.value),
                }),
                Err(e) => OpResult::Error(if cfg.has_effective_ssh_tunnels() {
                    ssh_connect_error_message(&cfg, &e)
                } else {
                    format!("redis: {e}")
                }),
            }
        }
        Op::Mongo(cfg, db, source) => match dbx_core::mongo_shell::parse(&source) {
            Ok(cmd) => match backend.execute_mongo_command(&cfg, &db, &cmd).await {
                Ok(r) => {
                    let mut rows = String::new();
                    for row in r.rows.iter().take(50) {
                        let line: Vec<String> = row
                            .iter()
                            .map(value_to_val)
                            .map(|v| v.text().to_string())
                            .collect();
                        rows.push_str(&line.join("  "));
                        rows.push('\n');
                    }
                    OpResult::Mongo(if rows.is_empty() { note_of(&r) } else { rows })
                }
                Err(e) => OpResult::Error(if cfg.has_effective_ssh_tunnels() {
                    ssh_connect_error_message(&cfg, &e)
                } else {
                    format!("mongo: {e}")
                }),
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
        Op::HistoryPanel(cfg) => {
            match backend
                .state()
                .storage
                .load_history_entries(300, 0, Some("query".to_string()))
                .await
            {
                Ok(entries) => {
                    // Newest first (DBX returns descending order). The rows stay
                    // *raw* (unmerged) here: `App::rebuild_history_rows` merges
                    // them with the in-memory session log in one pass, so the
                    // `×n` repeat counts survive (R84).
                    let mut raw: Vec<HistoryRow> = Vec::new();
                    for e in entries {
                        let sql = e.sql.trim().to_string();
                        if sql.is_empty() {
                            continue;
                        }
                        raw.push(HistoryRow {
                            id: e.id,
                            sql,
                            executed_at: e.executed_at,
                            connection_name: e.connection_name,
                            success: e.success,
                            duration_ms: if e.execution_time_ms > 0 {
                                e.execution_time_ms as u64
                            } else {
                                0
                            },
                            origin: history_origin_from_details(e.details_json.as_deref()),
                            count: 1,
                            session: false,
                        });
                        if raw.len() >= 300 {
                            break;
                        }
                    }
                    let rows = raw;
                    let favorites = match backend.state().storage.load_saved_sql_library().await {
                        Ok(lib) => lib
                            .files
                            .into_iter()
                            // Only this connection's snippets, plus unscoped ones.
                            .filter(|f| f.connection_id.is_empty() || f.connection_id == cfg.id)
                            .map(|f| f.sql)
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                    OpResult::HistoryPanel { rows, favorites }
                }
                Err(e) => OpResult::Error(format!("history: {e}")),
            }
        }
        Op::HistoryDelete { id } => match backend.state().storage.delete_history_entry(&id).await {
            Ok(()) => OpResult::HistoryDeleted { id, error: None },
            Err(e) => OpResult::HistoryDeleted { id, error: Some(e) },
        },
        Op::HistoryFavorite { cfg, sql, name } => {
            match backend.state().storage.load_saved_sql_library().await {
                Ok(lib) => {
                    let existing: Vec<String> = lib
                        .files
                        .iter()
                        .filter(|f| {
                            f.sql == sql
                                && (f.connection_id.is_empty() || f.connection_id == cfg.id)
                        })
                        .map(|f| f.id.clone())
                        .collect();
                    if !existing.is_empty() {
                        // Already a favourite: remove every matching copy.
                        let mut err = None;
                        for id in &existing {
                            if let Err(e) = backend.state().storage.delete_saved_sql_file(id).await
                            {
                                err = Some(e);
                            }
                        }
                        OpResult::HistoryFavorite {
                            sql,
                            favorited: err.is_some(),
                            error: err,
                        }
                    } else {
                        // Cap check before adding (the toggle-off branch above is
                        // exempt: removing never grows the list).
                        let used = lib
                            .files
                            .iter()
                            .filter(|f| f.connection_id.is_empty() || f.connection_id == cfg.id)
                            .count();
                        if snippet_limit_reached(used) {
                            return OpResult::SnippetRejected(tf(
                                "收藏已达上限 {} 条（当前 {}），请先在列表里按 d 删除",
                                &[&(SNIPPET_LIMIT), &(used)],
                            ));
                        }
                        let now = now_iso8601();
                        let file = dbx_core::saved_sql::SavedSqlFile {
                            id: Uuid::new_v4().to_string(),
                            connection_id: cfg.id.clone(),
                            folder_id: None,
                            name,
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
                            Ok(()) => OpResult::HistoryFavorite {
                                sql: file.sql,
                                favorited: true,
                                error: None,
                            },
                            Err(e) => OpResult::HistoryFavorite {
                                sql: file.sql,
                                favorited: false,
                                error: Some(e),
                            },
                        }
                    }
                }
                Err(e) => OpResult::HistoryFavorite {
                    sql,
                    favorited: false,
                    error: Some(e),
                },
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
                let mut items: Vec<SnippetRow> = lib
                    .files
                    .into_iter()
                    // Only this connection's snippets, plus unscoped ones, so the
                    // overlay never mixes another database's SQL.
                    .filter(|f| f.connection_id.is_empty() || f.connection_id == cfg.id)
                    .map(|f| {
                        let label = match folder_name(&f.folder_id) {
                            Some(folder) if !folder.is_empty() => format!("{folder}/{}", f.name),
                            _ => f.name.clone(),
                        };
                        SnippetRow {
                            id: f.id,
                            label,
                            sql: f.sql,
                        }
                    })
                    .collect();
                items.sort_by_key(|a| a.label.to_lowercase());
                OpResult::Snippets(items)
            }
            Err(e) => OpResult::Error(format!("snippets: {e}")),
        },
        Op::SnippetDelete { id } => {
            match backend.state().storage.delete_saved_sql_file(&id).await {
                Ok(()) => OpResult::SnippetDeleted { id, error: None },
                Err(e) => OpResult::SnippetDeleted { id, error: Some(e) },
            }
        }
        Op::SaveSnippet(cfg, name, sql) => {
            // Cap check up front: the favourite list is a quick-recall surface,
            // so a save past the limit is refused before anything is written.
            let used = match backend.state().storage.load_saved_sql_library().await {
                Ok(lib) => lib
                    .files
                    .iter()
                    .filter(|f| f.connection_id.is_empty() || f.connection_id == cfg.id)
                    .count(),
                Err(_) => 0,
            };
            if snippet_limit_reached(used) {
                return OpResult::SnippetRejected(tf(
                    "收藏已达上限 {} 条（当前 {}），请先在列表里按 d 删除",
                    &[&(SNIPPET_LIMIT), &(used)],
                ));
            }
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
            Ok(saved) => OpResult::Added(tf(
                "已保存: {} ({})",
                &[&(saved.name), &(saved.db_type.as_str())],
            )),
            Err(e) => OpResult::Error(format!("save: {e}")),
        },
        Op::UpdateConn(cfg) => {
            // No kernel UPDATE; drop and re-add the same id. A failure to remove
            // is not fatal on its own — the add below reports the real problem.
            let id = cfg.id.clone();
            let _ = backend.remove_connection_for_mcp(&id).await;
            match backend.add_connection_for_mcp(*cfg).await {
                Ok(saved) => OpResult::Added(tf(
                    "已更新: {} ({})",
                    &[&(saved.name), &(saved.db_type.as_str())],
                )),
                Err(e) => OpResult::Error(format!("update: {e}")),
            }
        }
        // R55: rename in place. `save_connections` is a single-connection upsert
        // (delete + re-insert *this* id inside one transaction) that leaves every
        // other saved connection and every stored secret untouched — the app
        // passes the fully-loaded config, so the secret re-encrypts identically.
        // The kernel's in-memory config map is refreshed to match.
        Op::RenameConn(cfg) => {
            let cfg = *cfg;
            let id = cfg.id.clone();
            match backend
                .state()
                .storage
                .save_connections(std::slice::from_ref(&cfg))
                .await
            {
                Ok(()) => {
                    backend
                        .state()
                        .configs
                        .write()
                        .await
                        .insert(id, cfg.clone());
                    OpResult::ConnRenamed(Box::new(cfg))
                }
                Err(e) => OpResult::Error(format!("rename: {e}")),
            }
        }
        // R68: persist a read-only toggle through the same single-row upsert as
        // a rename, so only this connection's row is rewritten (secrets and live
        // pools untouched) and the kernel's in-memory config map stays in step.
        Op::SetConnReadOnly(cfg) => {
            let cfg = *cfg;
            let id = cfg.id.clone();
            match backend
                .state()
                .storage
                .save_connections(std::slice::from_ref(&cfg))
                .await
            {
                Ok(()) => {
                    backend
                        .state()
                        .configs
                        .write()
                        .await
                        .insert(id, cfg.clone());
                    OpResult::ConnReadOnlySet(Box::new(cfg))
                }
                Err(e) => OpResult::Error(format!("read_only: {e}")),
            }
        }
        Op::DeleteConn { id, name } => match backend.remove_connection_for_mcp(&id).await {
            Ok(true) => OpResult::ConnDeleted { id, name },
            Ok(false) => OpResult::Error(tf("连接不存在: {}", &[&name])),
            Err(e) => OpResult::Error(format!("delete: {e}")),
        },
        Op::CopyConn(cfg) => match backend.add_connection_for_mcp(*cfg).await {
            Ok(saved) => OpResult::ConnCopied(Box::new(saved)),
            Err(e) => OpResult::Error(format!("copy: {e}")),
        },
        Op::ImportConns {
            items,
            skipped,
            needs_password,
        } => {
            let mut saved: Vec<ConnectionConfig> = Vec::new();
            let mut failed: Vec<String> = Vec::new();
            for (replace_id, cfg) in items {
                // An overwrite is remove-then-add with the same id (the kernel
                // has no UPDATE), mirroring [`Op::UpdateConn`].
                if let Some(id) = replace_id {
                    let _ = backend.remove_connection_for_mcp(&id).await;
                }
                match backend.add_connection_for_mcp(*cfg).await {
                    Ok(conn) => saved.push(conn),
                    Err(e) => failed.push(e.to_string()),
                }
            }
            OpResult::ConnsImported {
                saved,
                skipped,
                needs_password,
                failed,
            }
        }
        Op::ImportPlan {
            cfg,
            db,
            schema,
            table,
            path,
            gen,
        } => {
            let expanded = expand_home(&path.to_string_lossy());
            let bytes = match std::fs::read(&expanded) {
                Ok(b) => b,
                Err(e) => {
                    return OpResult::ImportFailed {
                        gen,
                        msg: tf("读取文件失败: {}", &[&(e)]),
                    }
                }
            };
            let (text, encoding) = decode_csv_bytes(&bytes);
            let delimiter = detect_delimiter(&text);
            let mut rows = parse_csv(&text, delimiter);
            if rows.is_empty() {
                return OpResult::ImportFailed {
                    gen,
                    msg: t("CSV 为空或无法解析").into(),
                };
            }
            let headers = rows.remove(0);
            if rows.is_empty() {
                return OpResult::ImportFailed {
                    gen,
                    msg: t("CSV 没有数据行").into(),
                };
            }
            let table_columns = match backend.get_columns(&cfg, &db, &schema, &table).await {
                Ok(c) => c,
                Err(e) => {
                    return OpResult::ImportFailed {
                        gen,
                        msg: tf("读取表结构失败: {}", &[&(e)]),
                    }
                }
            };
            let infer_rows: Vec<Vec<String>> =
                rows.iter().take(IMPORT_INFER_SAMPLE).cloned().collect();
            let (columns, extra, missing) =
                align_import_columns(&headers, &infer_rows, &table_columns);
            let error = if table_columns.is_empty() {
                Some(t("目标表没有可对齐的列").to_string())
            } else if extra.is_empty() && columns.iter().all(|c| c.src.is_none()) {
                Some(t("CSV 表头与表列不匹配（无任何列名对应）").to_string())
            } else if !extra.is_empty() {
                Some(tf(
                    "CSV 有 {} 个多余列无法对齐（{}）",
                    &[&extra.len(), &extra.join(", ")],
                ))
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
                if let Err(e) = backend
                    .execute_query(&cfg, &db, &del, Some(1), Some(60))
                    .await
                {
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
                        match backend
                            .execute_batch(&cfg, &db, None, &script, options)
                            .await
                        {
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
                        match backend
                            .execute_batch(&cfg, &db, None, &script, options)
                            .await
                        {
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
        Op::Export(job) => {
            let ExportJob {
                format,
                path,
                grid,
                cfg,
                schema,
                table,
                types,
            } = *job;
            let rows = grid.rows.len();
            // The writer is blocking (file IO + CPU); keep it off the async
            // worker pool so the render loop keeps its thread.
            let write_path = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let file = std::fs::File::create(&write_path)?;
                let mut w = BufWriter::with_capacity(EXPORT_BUF_BYTES, file);
                write_export(&mut w, cfg.as_ref(), &schema, &table, &types, &grid, format)?;
                w.flush()?;
                let bytes = w
                    .into_inner()
                    .ok()
                    .and_then(|f| f.metadata().ok())
                    .map(|m| m.len())
                    .unwrap_or(0);
                Ok::<(u64, u128), std::io::Error>((bytes, start.elapsed().as_millis()))
            })
            .await;
            match result {
                Ok(Ok((bytes, elapsed_ms))) => OpResult::ExportDone {
                    format,
                    path,
                    rows,
                    bytes,
                    elapsed_ms,
                    error: None,
                },
                Ok(Err(e)) => OpResult::ExportDone {
                    format,
                    path,
                    rows,
                    bytes: 0,
                    elapsed_ms: 0,
                    error: Some(e.to_string()),
                },
                Err(e) => OpResult::Error(format!("export: {e}")),
            }
        }
        Op::BatchExport(job) => {
            let BatchExportJob {
                kind,
                path,
                tabs,
                cfg,
                base,
            } = *job;
            let tab_count = tabs.len();
            let rows = batch_rows(&tabs);
            let write_path = path.clone();
            // File IO + XLSX/ZIP assembly is blocking; keep the render thread
            // free exactly like the single-tab export path.
            let result = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let job = BatchExportJob {
                    kind,
                    path: write_path.clone(),
                    tabs,
                    cfg,
                    base,
                };
                let outcome = run_batch_export(&job)?;
                let bytes = std::fs::metadata(&write_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                Ok::<(BatchExportOutcome, u64, u128), String>((
                    outcome,
                    bytes,
                    start.elapsed().as_millis(),
                ))
            })
            .await;
            match result {
                Ok(Ok((outcome, bytes, elapsed_ms))) => OpResult::BatchExportDone {
                    kind,
                    path,
                    tabs: tab_count,
                    rows,
                    bytes,
                    elapsed_ms,
                    truncated: outcome.truncated,
                    error: None,
                },
                Ok(Err(e)) => OpResult::BatchExportDone {
                    kind,
                    path,
                    tabs: tab_count,
                    rows,
                    bytes: 0,
                    elapsed_ms: 0,
                    truncated: Vec::new(),
                    error: Some(e),
                },
                Err(e) => OpResult::Error(format!("batch export: {e}")),
            }
        }
        Op::GlobalSearch {
            cfg,
            db,
            schema,
            needle,
            scan_limit,
            max_rows,
            gen,
            cancel,
        } => {
            run_global_search(
                backend, &cfg, &db, &schema, &needle, scan_limit, max_rows, gen, &cancel, tx,
            )
            .await
        }
        Op::DataDictionary {
            cfg,
            db,
            schema,
            gen,
            cancel,
        } => run_data_dictionary(backend, &cfg, &db, &schema, gen, &cancel, tx).await,
        Op::DiffTable {
            src_cfg,
            src_db,
            src_schema,
            src_table,
            tgt_cfg,
            tgt_db,
            tgt_schema,
            tgt_table,
            gen,
        } => {
            let src =
                match fetch_diff_side(backend, &src_cfg, &src_db, &src_schema, &src_table).await {
                    Ok(s) => s,
                    Err(e) => return OpResult::Error(format!("diff source: {e}")),
                };
            let tgt =
                match fetch_diff_side(backend, &tgt_cfg, &tgt_db, &tgt_schema, &tgt_table).await {
                    Ok(s) => s,
                    Err(e) => return OpResult::Error(format!("diff target: {e}")),
                };
            OpResult::DiffReady {
                gen,
                diff: Box::new(build_table_diff(src, tgt)),
            }
        }
        Op::DiffDatabase {
            src_cfg,
            src_db,
            src_schema,
            tgt_cfg,
            tgt_db,
            tgt_schema,
            gen,
        } => {
            let src = match backend.list_tables(&src_cfg, &src_db, &src_schema).await {
                Ok(t) => t,
                Err(e) => return OpResult::Error(format!("diff source tables: {e}")),
            };
            let tgt = match backend.list_tables(&tgt_cfg, &tgt_db, &tgt_schema).await {
                Ok(t) => t,
                Err(e) => return OpResult::Error(format!("diff target tables: {e}")),
            };
            let mut src_names: Vec<String> = src.into_iter().map(|t| t.name).collect();
            let mut tgt_names: Vec<String> = tgt.into_iter().map(|t| t.name).collect();
            // Deterministic, human-friendly order.
            src_names.sort_by_key(|a| a.to_ascii_lowercase());
            tgt_names.sort_by_key(|a| a.to_ascii_lowercase());
            let src_set: HashSet<String> =
                src_names.iter().map(|n| n.to_ascii_lowercase()).collect();
            let tgt_set: HashSet<String> =
                tgt_names.iter().map(|n| n.to_ascii_lowercase()).collect();
            let mut entries: Vec<DbDiffEntry> = Vec::new();
            for n in &src_names {
                let mark = if tgt_set.contains(&n.to_ascii_lowercase()) {
                    DbTableMark::Both
                } else {
                    DbTableMark::OnlySrc
                };
                entries.push(DbDiffEntry {
                    table: n.clone(),
                    mark,
                });
            }
            for n in &tgt_names {
                if !src_set.contains(&n.to_ascii_lowercase()) {
                    entries.push(DbDiffEntry {
                        table: n.clone(),
                        mark: DbTableMark::OnlyTgt,
                    });
                }
            }
            let src_label = if src_schema.trim().is_empty() {
                fix_double_encoding(&src_db)
            } else {
                format!(
                    "{}.{}",
                    fix_double_encoding(&src_db),
                    fix_double_encoding(&src_schema)
                )
            };
            let tgt_label = if tgt_schema.trim().is_empty() {
                fix_double_encoding(&tgt_db)
            } else {
                format!(
                    "{}.{}",
                    fix_double_encoding(&tgt_db),
                    fix_double_encoding(&tgt_schema)
                )
            };
            OpResult::DbDiffReady {
                gen,
                diff: Box::new(DbDiff {
                    src_label,
                    tgt_label,
                    entries,
                    src_db,
                    tgt_db,
                    src_schema,
                    tgt_schema,
                }),
            }
        }
        Op::DiffTablesFor {
            cfg,
            db,
            schema,
            gen,
        } => match backend.list_tables(&cfg, &db, &schema).await {
            Ok(tables) => {
                let mut names: Vec<String> = tables.into_iter().map(|t| t.name).collect();
                names.sort_by_key(|a| a.to_ascii_lowercase());
                OpResult::DiffTablesFor {
                    gen,
                    db,
                    schema,
                    tables: names,
                }
            }
            Err(e) => OpResult::Error(format!("diff target tables: {e}")),
        },
        Op::DataDiff {
            src_cfg,
            src_db,
            src_schema,
            src_table,
            tgt_cfg,
            tgt_db,
            tgt_schema,
            tgt_table,
            where_input,
            gen,
            cancel,
        } => {
            run_data_diff(
                backend,
                &src_cfg,
                &src_db,
                &src_schema,
                &src_table,
                &tgt_cfg,
                &tgt_db,
                &tgt_schema,
                &tgt_table,
                &where_input,
                gen,
                &cancel,
                tx,
            )
            .await
        }
        Op::DataTransfer(job) => Box::pin(run_data_transfer(backend, *job, tx)).await,
    }
}

/// R56: list a table's indexes, treating any failure as “none” — used where the
/// indexes only enrich something else (the `g c` popup key mark, a diff, the
/// keyset primary key) and a driver that cannot list them must not fail the call.
async fn list_indexes_best_effort(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
) -> Vec<IndexInfo> {
    dbx_core::schema::list_indexes_core(backend.state().as_ref(), &cfg.id, db, schema, table)
        .await
        .unwrap_or_default()
}

/// R97: list a table's foreign keys, treating any failure as “none” — the keys
/// only enrich the cell popup (a jump to the referenced row), so a driver that
/// cannot list them must never fail the column load.
async fn list_foreign_keys_best_effort(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
) -> Vec<ForeignKeyInfo> {
    dbx_core::schema::list_foreign_keys_core(backend.state().as_ref(), &cfg.id, db, schema, table)
        .await
        .unwrap_or_default()
}

/// Fetch one side of a table diff: columns (required) plus indexes (best
/// effort — a driver that cannot list them still diffs columns).
async fn fetch_diff_side(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
) -> Result<DiffSide, String> {
    let columns = backend.get_columns(cfg, db, schema, table).await?;
    let indexes = list_indexes_best_effort(backend, cfg, db, schema, table).await;
    Ok(DiffSide {
        db: db.to_string(),
        schema: schema.to_string(),
        table: table.to_string(),
        db_type: cfg.db_type,
        columns,
        indexes,
    })
}

/// One side's chunked row stream for the data compare.
#[derive(Default)]
struct DataSideStream {
    buf: VecDeque<Vec<Val>>,
    exhausted: bool,
    /// The last row consumed; its primary key drives the next keyset query.
    last: Option<Vec<Val>>,
}

/// Primary-key columns for one side, from its primary index (best effort).
async fn resolve_data_pk(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
) -> Vec<String> {
    let indexes = list_indexes_best_effort(backend, cfg, db, schema, table).await;
    pk_from_metadata(columns, &indexes)
}

/// `COUNT(*)` for one side, honouring the compare's `WHERE` (best effort — a
/// failure just means “size unknown”).
async fn data_count(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    filter: &str,
) -> Option<u64> {
    let base = build_count_table_sql(
        Some(cfg.db_type),
        (!schema.trim().is_empty()).then_some(schema),
        table,
    );
    let sql = if filter.trim().is_empty() {
        base
    } else {
        format!("{base} WHERE ({})", filter.trim())
    };
    let r = backend
        .execute_query(cfg, db, &sql, Some(1), Some(15))
        .await
        .ok()?;
    r.rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        })
}

/// A keyset key tuple holding a NULL cannot drive a `>` / `<` seek (`k > NULL`
/// is never true), so a caller about to build the next chunk must stop instead
/// of issuing a query that returns nothing and looks like end-of-table.
fn pk_tuple_has_null(tuple: &[Val]) -> bool {
    tuple.iter().any(|v| matches!(v, Val::Null))
}

/// Feed one fetched chunk into a side stream: append the rows, mark the side
/// exhausted on a short/empty chunk, and advance the keyset cursor to the
/// chunk's last primary key (so the next refill reads the following rows).
/// Returns `true` when rows were appended.
fn feed_chunk(stream: &mut DataSideStream, rows: Vec<Vec<Val>>, pk_len: usize) -> bool {
    if rows.is_empty() {
        stream.exhausted = true;
        return false;
    }
    if let Some(last) = rows.last() {
        stream.last = Some(last[..pk_len.min(last.len())].to_vec());
    }
    let n = rows.len();
    for row in rows {
        stream.buf.push_back(row);
    }
    // A short chunk means the table ended.
    if n < DATA_CHUNK {
        stream.exhausted = true;
    }
    true
}

/// Fill one side's buffer with the next `DATA_CHUNK` rows (keyset-paginated).
/// Returns `Ok(true)` when a query ran and returned rows.
#[allow(clippy::too_many_arguments)]
async fn data_refill(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    select: &[String],
    pk_names: &[String],
    pk_types: &[String],
    filter: &str,
    stream: &mut DataSideStream,
) -> Result<bool, String> {
    if stream.exhausted || !stream.buf.is_empty() {
        return Ok(false);
    }
    // A NULL in the last row's key cannot drive a `k > NULL` seek (`k > NULL` is
    // never true), so the next chunk would read empty and the merge would look
    // finished. Stop loudly instead of silently truncating the compare.
    if stream.last.as_deref().is_some_and(pk_tuple_has_null) {
        return Err(t("主键含 NULL，无法按主键继续分块对比").into());
    }
    let sql = build_data_select(
        cfg.db_type,
        schema,
        table,
        select,
        pk_names,
        pk_types,
        filter,
        stream.last.as_deref(),
        DATA_CHUNK,
    );
    let r = backend
        .execute_query(cfg, db, &sql, Some(DATA_CHUNK), Some(60))
        .await?;
    let rows: Vec<Vec<Val>> = r
        .rows
        .iter()
        .map(|row| row.iter().map(value_to_val).collect())
        .collect();
    Ok(feed_chunk(stream, rows, pk_names.len()))
}

/// `db.schema.table` for the overlay title / copied summary (mirrors
/// [`DiffSide::label`]).
fn data_table_label(db: &str, schema: &str, table: &str) -> String {
    let rel = qualified_display(schema, table);
    let prefix = if db.trim().is_empty() {
        String::new()
    } else {
        format!("{}.", fix_double_encoding(db))
    };
    format!("{}{}", prefix, fix_double_encoding(&rel))
}

/// R65: the status bar's session identity — where the open data view actually
/// lives. The connection name already leads the left block, so this adds only
/// the location: `database.table`, with the schema folded in when it differs
/// from the database (PostgreSQL) and skipped when it duplicates it (MySQL,
/// where the schema *is* the database). Below 56 columns (the same gate as the
/// server version) it collapses to the bare table name, so a phone status bar
/// keeps its row / column readout. `None` when no table data view is open.
fn session_label(db: &str, schema: &str, table: &str, term_w: u16) -> Option<String> {
    let table = fix_double_encoding(table);
    if table.trim().is_empty() {
        return None;
    }
    if term_w != 0 && term_w < 56 {
        return Some(table);
    }
    let db = fix_double_encoding(db);
    let schema = fix_double_encoding(schema);
    let mut out = String::new();
    if !db.trim().is_empty() {
        out.push_str(&db);
        out.push('.');
    }
    if !schema.trim().is_empty() && !schema.eq_ignore_ascii_case(&db) {
        out.push_str(&schema);
        out.push('.');
    }
    out.push_str(&table);
    Some(out)
}

/// The data-compare worker: resolve both sides' primary keys, forecast counts,
/// then merge-join chunk by chunk. Progress is streamed between chunks and the
/// shared `cancel` flag aborts, keeping the rows found so far.
#[allow(clippy::too_many_arguments)]
async fn run_data_diff(
    backend: &LocalBackend,
    src_cfg: &ConnectionConfig,
    src_db: &str,
    src_schema: &str,
    src_table: &str,
    tgt_cfg: &ConnectionConfig,
    tgt_db: &str,
    tgt_schema: &str,
    tgt_table: &str,
    where_input: &str,
    gen: u64,
    cancel: &AtomicBool,
    tx: &Tx,
) -> OpResult {
    let src_cols = match backend
        .get_columns(src_cfg, src_db, src_schema, src_table)
        .await
    {
        Ok(c) => c,
        Err(e) => return OpResult::Error(format!("data diff source columns: {e}")),
    };
    let tgt_cols = match backend
        .get_columns(tgt_cfg, tgt_db, tgt_schema, tgt_table)
        .await
    {
        Ok(c) => c,
        Err(e) => return OpResult::Error(format!("data diff target columns: {e}")),
    };
    let src_pk = resolve_data_pk(backend, src_cfg, src_db, src_schema, src_table, &src_cols).await;
    let tgt_pk = resolve_data_pk(backend, tgt_cfg, tgt_db, tgt_schema, tgt_table, &tgt_cols).await;
    let cross = src_cfg.db_type != tgt_cfg.db_type;
    // R90: no primary key on either side → align by row order (positional).
    let positional = src_pk.is_empty() && tgt_pk.is_empty();
    let align = if positional {
        match build_positional_align(&src_cols, &tgt_cols, cross) {
            Ok(a) => a,
            Err(reason) => return OpResult::Error(reason),
        }
    } else {
        match build_data_align(&src_cols, &tgt_cols, &src_pk, &tgt_pk, cross) {
            Ok(a) => a,
            Err(reason) => return OpResult::Error(reason),
        }
    };
    let filter = normalize_where_input(Some(where_input));
    // Forecast both sizes so the user can judge scale before the compare ends.
    let src_count = data_count(backend, src_cfg, src_db, src_schema, src_table, &filter).await;
    let tgt_count = data_count(backend, tgt_cfg, tgt_db, tgt_schema, tgt_table, &filter).await;
    if positional {
        return run_positional_data_diff(
            backend, src_cfg, src_db, src_schema, src_table, tgt_cfg, tgt_db, tgt_schema,
            tgt_table, align, filter, src_count, tgt_count, gen, cancel, tx,
        )
        .await;
    }

    let src_select = align.src_select();
    let tgt_select = align.tgt_select();
    let src_types = align.src_types();
    let tgt_types = align.tgt_types();
    let src_pk_names = align.src_pk_names();
    let tgt_pk_names = align.tgt_pk_names();
    let modes: Vec<PkCmp> = align.pk().iter().map(pk_cmp_mode).collect();
    let total_chunks = chunk_total(src_count, tgt_count);

    let mut src_stream = DataSideStream::default();
    let mut tgt_stream = DataSideStream::default();
    let mut rows: Vec<DataDiffRow> = Vec::new();
    let mut only_src = 0usize;
    let mut only_tgt = 0usize;
    let mut differing = 0usize;
    let mut compared = 0usize;
    let mut truncated = false;
    let mut cancelled = false;
    let mut done = 0usize;

    // Store a row unless the cap is hit; the counters keep running either way.
    macro_rules! keep {
        ($row:expr) => {
            if rows.len() < DATA_MAX_DIFF_ROWS {
                rows.push($row);
            } else {
                truncated = true;
            }
        };
    }

    loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let mut refreshed = false;
        if src_stream.buf.is_empty() {
            match data_refill(
                backend,
                src_cfg,
                src_db,
                src_schema,
                src_table,
                &src_select,
                &src_pk_names,
                &src_types,
                &filter,
                &mut src_stream,
            )
            .await
            {
                Ok(true) => {
                    done += 1;
                    refreshed = true;
                }
                Ok(false) => {}
                Err(e) => return OpResult::Error(format!("data diff source: {e}")),
            }
        }
        if tgt_stream.buf.is_empty() {
            match data_refill(
                backend,
                tgt_cfg,
                tgt_db,
                tgt_schema,
                tgt_table,
                &tgt_select,
                &tgt_pk_names,
                &tgt_types,
                &filter,
                &mut tgt_stream,
            )
            .await
            {
                Ok(true) => {
                    done += 1;
                    refreshed = true;
                }
                Ok(false) => {}
                Err(e) => return OpResult::Error(format!("data diff target: {e}")),
            }
        }
        if refreshed {
            let _ = tx.send(OpResult::DataDiffProgress {
                gen,
                done,
                total: total_chunks,
            });
        }
        match merge_next(
            &align,
            &modes,
            src_stream.buf.front().map(Vec::as_slice),
            tgt_stream.buf.front().map(Vec::as_slice),
        ) {
            MergeStep::Done => break,
            MergeStep::SrcOnly => {
                let row = src_stream.buf.pop_front().unwrap();
                only_src += 1;
                compared += 1;
                keep!(only_data_row(&align, &row, RowMark::OnlySrc));
            }
            MergeStep::TgtOnly => {
                let row = tgt_stream.buf.pop_front().unwrap();
                only_tgt += 1;
                compared += 1;
                keep!(only_data_row(&align, &row, RowMark::OnlyTgt));
            }
            MergeStep::Both(diff) => {
                src_stream.buf.pop_front();
                tgt_stream.buf.pop_front();
                compared += 1;
                if let Some(row) = diff {
                    differing += 1;
                    keep!(row);
                }
            }
        }
    }
    // A cancel that lands while the last chunk is in flight still counts.
    if cancel.load(Ordering::Relaxed) {
        cancelled = true;
    }

    OpResult::DataDiffDone {
        gen,
        result: Box::new(DataCompare {
            src_label: data_table_label(src_db, src_schema, src_table),
            tgt_label: data_table_label(tgt_db, tgt_schema, tgt_table),
            src_db_type: src_cfg.db_type,
            tgt_schema: tgt_schema.to_string(),
            tgt_table: tgt_table.to_string(),
            tgt_db_type: tgt_cfg.db_type,
            src_count,
            tgt_count,
            filter,
            align,
            rows,
            only_src,
            only_tgt,
            differing,
            compared,
            positional: false,
            truncated,
            cancelled,
        }),
    }
}

/// R90: the row-order data compare for two tables that have no primary key.
/// Both sides are read once, capped at [`DATA_ROW_ORDER_LIMIT`] rows, then
/// paired position by position. Pure classification happens in
/// [`compare_positional_row`] / [`positional_only_row`], so this worker only
/// owns the two reads and the row cap.
#[allow(clippy::too_many_arguments)]
async fn run_positional_data_diff(
    backend: &LocalBackend,
    src_cfg: &ConnectionConfig,
    src_db: &str,
    src_schema: &str,
    src_table: &str,
    tgt_cfg: &ConnectionConfig,
    tgt_db: &str,
    tgt_schema: &str,
    tgt_table: &str,
    align: DataAlign,
    filter: String,
    src_count: Option<u64>,
    tgt_count: Option<u64>,
    gen: u64,
    cancel: &AtomicBool,
    _tx: &Tx,
) -> OpResult {
    if cancel.load(Ordering::Relaxed) {
        return OpResult::DataDiffDone {
            gen,
            result: Box::new(DataCompare {
                src_label: data_table_label(src_db, src_schema, src_table),
                tgt_label: data_table_label(tgt_db, tgt_schema, tgt_table),
                src_db_type: src_cfg.db_type,
                tgt_schema: tgt_schema.to_string(),
                tgt_table: tgt_table.to_string(),
                tgt_db_type: tgt_cfg.db_type,
                src_count,
                tgt_count,
                filter,
                align,
                rows: Vec::new(),
                only_src: 0,
                only_tgt: 0,
                differing: 0,
                compared: 0,
                positional: true,
                truncated: false,
                cancelled: true,
            }),
        };
    }
    let src_select = align.src_select();
    let tgt_select = align.tgt_select();
    let src_rows = match positional_fetch(
        backend,
        src_cfg,
        src_db,
        src_schema,
        src_table,
        &src_select,
        &filter,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return OpResult::Error(format!("data diff source: {e}")),
    };
    let tgt_rows = match positional_fetch(
        backend,
        tgt_cfg,
        tgt_db,
        tgt_schema,
        tgt_table,
        &tgt_select,
        &filter,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return OpResult::Error(format!("data diff target: {e}")),
    };
    let n = src_rows.len().max(tgt_rows.len());
    let mut rows: Vec<DataDiffRow> = Vec::new();
    let (mut only_src, mut only_tgt, mut differing) = (0usize, 0usize, 0usize);
    let mut truncated = false;
    for pos in 0..n {
        match (src_rows.get(pos), tgt_rows.get(pos)) {
            (Some(s), Some(t)) => {
                if let Some(row) = compare_positional_row(&align, pos, s, t) {
                    differing += 1;
                    if rows.len() < DATA_MAX_DIFF_ROWS {
                        rows.push(row);
                    } else {
                        truncated = true;
                    }
                }
            }
            (Some(s), None) => {
                only_src += 1;
                let row = positional_only_row(&align, pos, s, RowMark::OnlySrc);
                if rows.len() < DATA_MAX_DIFF_ROWS {
                    rows.push(row);
                } else {
                    truncated = true;
                }
            }
            (None, Some(t)) => {
                only_tgt += 1;
                let row = positional_only_row(&align, pos, t, RowMark::OnlyTgt);
                if rows.len() < DATA_MAX_DIFF_ROWS {
                    rows.push(row);
                } else {
                    truncated = true;
                }
            }
            (None, None) => {}
        }
    }
    OpResult::DataDiffDone {
        gen,
        result: Box::new(DataCompare {
            src_label: data_table_label(src_db, src_schema, src_table),
            tgt_label: data_table_label(tgt_db, tgt_schema, tgt_table),
            src_db_type: src_cfg.db_type,
            tgt_schema: tgt_schema.to_string(),
            tgt_table: tgt_table.to_string(),
            tgt_db_type: tgt_cfg.db_type,
            src_count,
            tgt_count,
            filter,
            align,
            rows,
            only_src,
            only_tgt,
            differing,
            compared: n,
            positional: true,
            truncated,
            cancelled: false,
        }),
    }
}

/// Read up to [`DATA_ROW_ORDER_LIMIT`] rows in the engine's natural order.
async fn positional_fetch(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    select: &[String],
    filter: &str,
) -> Result<Vec<Vec<Val>>, String> {
    let sql = build_positional_select(
        cfg.db_type,
        schema,
        table,
        select,
        filter,
        DATA_ROW_ORDER_LIMIT,
    );
    let r = backend
        .execute_query(cfg, db, &sql, Some(DATA_ROW_ORDER_LIMIT), Some(60))
        .await?;
    Ok(r.rows
        .iter()
        .map(|row| row.iter().map(value_to_val).collect())
        .collect())
}

// ── data transfer worker ──

/// The bounded source `COUNT(*)` used as the transfer's size forecast.
async fn transfer_count(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    filter: &str,
    limit: Option<u64>,
) -> Option<u64> {
    let sql = build_transfer_count_sql(cfg.db_type, schema, table, filter, limit);
    let r = backend
        .execute_query(cfg, db, &sql, Some(1), Some(30))
        .await
        .ok()?;
    r.rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        })
}

/// Read one chunk of source rows for a transfer: keyset-paginated when the
/// primary key is available, otherwise plain `LIMIT … OFFSET` (deterministic
/// only up to the first column, hence the generator's warning).
#[allow(clippy::too_many_arguments)]
async fn transfer_read_chunk(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    table: &str,
    align: &TransferAlign,
    filter: &str,
    last: Option<&[Val]>,
    offset: u64,
    read_limit: usize,
) -> Result<Vec<Vec<Val>>, String> {
    let cols = align.src_select();
    let sql = if align.keyset() {
        // A NULL in the key of the last copied row makes `k > NULL` never true;
        // the next read would come back empty and the copy would stop early,
        // silently dropping the remaining rows. Abort with a clear reason
        // instead (the batches already committed are kept).
        if last.is_some_and(pk_tuple_has_null) {
            return Err(t("源表主键含 NULL，无法按主键继续分块搬运").into());
        }
        build_data_select(
            cfg.db_type,
            schema,
            table,
            &cols,
            &align.pk_names,
            &align.pk_types,
            filter,
            last,
            read_limit,
        )
    } else {
        build_transfer_offset_select(
            cfg.db_type,
            schema,
            table,
            &cols,
            filter,
            read_limit,
            offset,
        )
    };
    let r = backend
        .execute_query(cfg, db, &sql, Some(read_limit), Some(60))
        .await?;
    Ok(r.rows
        .iter()
        .map(|row| row.iter().map(value_to_val).collect())
        .collect())
}

/// Display a primary-key tuple (`1, 42`) for the report's breakpoint line.
fn transfer_pk_display(pk: &[Val]) -> String {
    pk.iter()
        .map(|v| value_display(v).0)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Execute one `INSERT` batch on the target, retrying a transport failure once.
/// Returns the per-statement results (skip mode) or an empty vec (stop mode).
async fn transfer_exec_batch(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    script: &str,
    stop_mode: bool,
) -> Result<Vec<BatchStatementResult>, String> {
    let options = if stop_mode {
        QueryExecutionOptions {
            max_rows: Some(1),
            timeout_secs: Some(60),
            use_transaction: Some(true),
            ..Default::default()
        }
    } else {
        QueryExecutionOptions {
            max_rows: Some(1),
            timeout_secs: Some(60),
            continue_on_error: true,
            ..Default::default()
        }
    };
    match backend
        .execute_batch(cfg, db, None, script, options.clone())
        .await
    {
        Ok(v) => Ok(v),
        // One retry: a dropped connection or a transient deadlock should not
        // abort a long copy on its first hiccup.
        Err(_) => backend.execute_batch(cfg, db, None, script, options).await,
    }
}

/// The data-transfer worker: fetch the source shape, create / reshape the target
/// when asked, then stream rows through keyset paging into transactional
/// batches. Progress is sent between chunks; the shared `cancel` flag aborts
/// between batches, keeping everything already committed.
async fn run_data_transfer(backend: &LocalBackend, job: TransferJob, tx: &Tx) -> OpResult {
    let TransferJob {
        src_cfg,
        src_db,
        src_schema,
        src_table,
        tgt_cfg,
        tgt_db,
        tgt_schema,
        tgt_table,
        mode,
        conflict,
        on_error,
        where_input,
        limit,
        with_indexes,
        with_auto_increment,
        allow_large,
        gen,
        cancel,
    } = job;
    let start = Instant::now();
    let src_label = data_table_label(&src_db, &src_schema, &src_table);
    let tgt_label = data_table_label(&tgt_db, &tgt_schema, &tgt_table);
    let error_report =
        |aborted: (u64, String), created: bool, estimated: Option<u64>| OpResult::TransferDone {
            gen,
            report: Box::new(TransferReport {
                src_label: src_label.clone(),
                tgt_label: tgt_label.clone(),
                src_db_type: src_cfg.db_type,
                tgt_db_type: tgt_cfg.db_type,
                mode,
                conflict,
                on_error,
                tgt_db: tgt_db.clone(),
                tgt_schema: tgt_schema.clone(),
                tgt_table: tgt_table.clone(),
                tgt_conn_id: tgt_cfg.id.clone(),
                src_rows: 0,
                moved: 0,
                skipped: Vec::new(),
                aborted: Some(aborted),
                cancelled: false,
                estimated,
                created,
                breakpoint: None,
                warnings: Vec::new(),
                elapsed_ms: start.elapsed().as_millis(),
                chunks_done: 0,
            }),
        };

    // 1. Source shape.
    let src_cols = match backend
        .get_columns(&src_cfg, &src_db, &src_schema, &src_table)
        .await
    {
        Ok(c) => c,
        Err(e) => return error_report((0, format!("读取源表结构失败: {e}")), false, None),
    };
    let src_indexes = dbx_core::schema::list_indexes_core(
        backend.state().as_ref(),
        &src_cfg.id,
        &src_db,
        &src_schema,
        &src_table,
    )
    .await
    .unwrap_or_default();
    let src_pk = pk_from_metadata(&src_cols, &src_indexes);
    let filter = normalize_where_input(Some(&where_input));

    // 2. Size forecast + the >1M confirmation gate (only for a row copy).
    let estimated = if mode == TransferMode::CreateOnly {
        None
    } else {
        transfer_count(
            backend,
            &src_cfg,
            &src_db,
            &src_schema,
            &src_table,
            &filter,
            limit,
        )
        .await
    };
    if mode != TransferMode::CreateOnly && !allow_large {
        if let Some(n) = estimated {
            if n >= TRANSFER_WARN_ROWS {
                return OpResult::TransferNeedsConfirm { gen, estimated: n };
            }
        }
    }

    // 3. Structure (CREATE + optional DROP when overwriting).
    let create = mode != TransferMode::CreateOnly && mode != TransferMode::Append;
    let tgt_exists = backend
        .list_tables(&tgt_cfg, &tgt_db, &tgt_schema)
        .await
        .map(|ts| ts.iter().any(|t| t.name.eq_ignore_ascii_case(&tgt_table)))
        .unwrap_or(false);
    if mode == TransferMode::Append && !tgt_exists {
        return error_report((0, tf("目标表不存在: {}", &[&tgt_label])), false, estimated);
    }
    let mut created = false;
    let mut warnings: Vec<String> = Vec::new();
    if create {
        if tgt_exists {
            match conflict {
                TransferConflict::Stop => {
                    return error_report(
                        (0, tf("目标表已存在: {}（按 o 选择覆盖）", &[&tgt_label])),
                        false,
                        estimated,
                    );
                }
                TransferConflict::Drop => {
                    let drop = format!(
                        "DROP TABLE {};",
                        table_ref(tgt_cfg.db_type, &tgt_schema, &tgt_table)
                    );
                    if let Err(e) = backend
                        .execute_query(&tgt_cfg, &tgt_db, &drop, Some(1), Some(60))
                        .await
                    {
                        return error_report(
                            (0, format!("DROP TABLE 失败: {e}")),
                            false,
                            estimated,
                        );
                    }
                }
            }
        }
        let generated = generate_transfer_create(
            &src_cols,
            &src_indexes,
            &src_pk,
            src_cfg.db_type,
            tgt_cfg.db_type,
            &tgt_schema,
            &tgt_table,
            with_indexes,
            with_auto_increment,
        );
        let (script, warn) = match generated {
            Ok(v) => v,
            Err(e) => return error_report((0, e), false, estimated),
        };
        warnings = warn;
        let options = QueryExecutionOptions {
            max_rows: Some(1),
            timeout_secs: Some(120),
            ..Default::default()
        };
        if let Err(e) = backend
            .execute_batch(&tgt_cfg, &tgt_db, None, &script, options)
            .await
        {
            return error_report((0, format!("建表失败: {e}")), false, estimated);
        }
        created = true;
    }

    // 4. Append-only finishes here.
    if mode == TransferMode::CreateOnly {
        return OpResult::TransferDone {
            gen,
            report: Box::new(TransferReport {
                src_label,
                tgt_label,
                src_db_type: src_cfg.db_type,
                tgt_db_type: tgt_cfg.db_type,
                mode,
                conflict,
                on_error,
                tgt_db,
                tgt_schema,
                tgt_table,
                tgt_conn_id: tgt_cfg.id.clone(),
                src_rows: 0,
                moved: 0,
                skipped: Vec::new(),
                aborted: None,
                cancelled: false,
                estimated,
                created,
                breakpoint: None,
                warnings,
                elapsed_ms: start.elapsed().as_millis(),
                chunks_done: 0,
            }),
        };
    }

    // 5. Write alignment. For a fresh table the target columns mirror the source
    //    with mapped types; for append only the name-intersection is written.
    let tgt_cols = if create {
        Vec::new()
    } else {
        match backend
            .get_columns(&tgt_cfg, &tgt_db, &tgt_schema, &tgt_table)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                return error_report((0, format!("读取目标表结构失败: {e}")), created, estimated)
            }
        }
    };
    let align = match build_transfer_align(
        &src_cols,
        &tgt_cols,
        &src_pk,
        src_cfg.db_type,
        tgt_cfg.db_type,
        create,
    ) {
        Ok(a) => a,
        Err(e) => return error_report((0, e), created, estimated),
    };
    let tref = table_ref(tgt_cfg.db_type, &tgt_schema, &tgt_table);
    let col_names = align
        .cols
        .iter()
        .map(|c| quote_table_identifier(Some(tgt_cfg.db_type), &c.tgt_name))
        .collect::<Vec<_>>()
        .join(", ");

    // 6. The copy loop.
    let mut src_rows = 0u64;
    let mut moved = 0u64;
    let mut skipped: Vec<(u64, String)> = Vec::new();
    let mut aborted: Option<(u64, String)> = None;
    let mut cancelled = false;
    let mut chunks_done = 0usize;
    let mut breakpoint: Option<String> = None;
    let mut last_pk: Option<Vec<Val>> = None;
    let mut offset = 0u64;
    let read_limit = |src_rows: u64| -> usize {
        match limit {
            Some(l) => TRANSFER_CHUNK.min(l.saturating_sub(src_rows).max(1) as usize),
            None => TRANSFER_CHUNK,
        }
    };
    'outer: loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if limit.is_some_and(|l| src_rows >= l) {
            break;
        }
        let rl = read_limit(src_rows);
        let rows = match transfer_read_chunk(
            backend,
            &src_cfg,
            &src_db,
            &src_schema,
            &src_table,
            &align,
            &filter,
            last_pk.as_deref(),
            offset,
            rl,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                aborted = Some((src_rows + 1, format!("读取源数据失败: {e}")));
                break;
            }
        };
        if rows.is_empty() {
            break;
        }
        let n = rows.len();
        let chunk_base = src_rows;
        src_rows += n as u64;
        // Advance the cursor before writing so an abort keeps the right position.
        if align.keyset() {
            last_pk = rows.last().and_then(|r| align.pk_of(r));
        } else {
            offset += n as u64;
        }
        for (bi, batch) in rows.chunks(TRANSFER_INSERT_BATCH).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'outer;
            }
            let base_row = chunk_base + (bi * TRANSFER_INSERT_BATCH) as u64;
            let script = batch
                .iter()
                .map(|row| {
                    let vals = align
                        .cols
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let v = row.get(i).cloned().unwrap_or(Val::Null);
                            transfer_value_literal(&v, &c.src_type, &c.tgt_type, tgt_cfg.db_type)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("INSERT INTO {tref} ({col_names}) VALUES ({vals});")
                })
                .collect::<Vec<_>>()
                .join("\n");
            match transfer_exec_batch(
                backend,
                &tgt_cfg,
                &tgt_db,
                &script,
                on_error == TransferOnError::Stop,
            )
            .await
            {
                Ok(results) => {
                    if on_error == TransferOnError::Stop {
                        moved += batch.len() as u64;
                    } else {
                        let mut bad = 0usize;
                        for (i, r) in results.iter().enumerate() {
                            if r.execution_error {
                                bad += 1;
                                skipped.push((
                                    base_row + i as u64 + 1,
                                    r.error_message
                                        .clone()
                                        .unwrap_or_else(|| "unknown error".to_string()),
                                ));
                            }
                        }
                        moved += results.len().saturating_sub(bad) as u64;
                        if results.len() < batch.len() {
                            aborted = Some((
                                base_row + results.len() as u64 + 1,
                                t("批量在中途停止（连接或会话错误）").to_string(),
                            ));
                            break 'outer;
                        }
                    }
                    if let Some(last) = batch.last() {
                        if let Some(pk) = align.pk_of(last) {
                            breakpoint = Some(transfer_pk_display(&pk));
                        }
                    }
                }
                Err(e) => {
                    aborted = Some((base_row + 1, format!("写入失败: {e}")));
                    break 'outer;
                }
            }
        }
        chunks_done += 1;
        let _ = tx.send(OpResult::TransferProgress {
            gen,
            rows: moved,
            chunks: chunks_done,
            elapsed_ms: start.elapsed().as_millis(),
            total: estimated,
        });
        if n < rl {
            break;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        cancelled = true;
    }

    OpResult::TransferDone {
        gen,
        report: Box::new(TransferReport {
            src_label,
            tgt_label,
            src_db_type: src_cfg.db_type,
            tgt_db_type: tgt_cfg.db_type,
            mode,
            conflict,
            on_error,
            tgt_db,
            tgt_schema,
            tgt_table,
            tgt_conn_id: tgt_cfg.id.clone(),
            src_rows,
            moved,
            skipped,
            aborted,
            cancelled,
            estimated,
            created,
            breakpoint,
            warnings,
            elapsed_ms: start.elapsed().as_millis(),
            chunks_done,
        }),
    }
}

/// The global-search worker: enumerate tables, skip the big ones, then scan each
/// remaining table's text columns with one bounded query. Progress is streamed
/// back between tables so the overlay can show `done/total`, and the shared
/// `cancel` flag aborts before the next table.
#[allow(clippy::too_many_arguments)]
async fn run_global_search(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    needle: &str,
    scan_limit: usize,
    max_rows: u64,
    gen: u64,
    cancel: &AtomicBool,
    tx: &Tx,
) -> OpResult {
    let tables = match backend.list_tables(cfg, db, schema).await {
        Ok(t) => t,
        Err(e) => return OpResult::Error(format!("search: list tables: {e}")),
    };
    // Base tables only: a view is usually a join, so scanning it duplicates its
    // underlying tables and can be far more expensive.
    let tables: Vec<TableInfo> = tables
        .into_iter()
        .filter(|t| !t.table_type.to_ascii_uppercase().contains("VIEW"))
        .collect();
    let total = tables.len();
    // Approximate counts, best effort: a failure just means "scan anyway".
    let estimates: HashMap<String, u64> = match backend
        .execute_query(
            cfg,
            db,
            &build_search_estimates_sql(cfg.db_type, schema),
            None,
            Some(20),
        )
        .await
    {
        Ok(r) => parse_search_estimates(&r.rows),
        Err(_) => HashMap::new(),
    };
    let needle_lower = needle.to_lowercase();
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut skipped: Vec<(String, u64)> = Vec::new();
    let mut truncated = false;
    for (i, table) in tables.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(OpResult::SearchProgress {
                gen,
                done: i,
                total,
            });
            return OpResult::SearchCancelled { gen };
        }
        let _ = tx.send(OpResult::SearchProgress {
            gen,
            done: i,
            total,
        });
        if let Some(est) = search_skip_reason(estimates.get(&table.name).copied(), max_rows) {
            skipped.push((table.name.clone(), est));
            continue;
        }
        let columns = match backend.get_columns(cfg, db, schema, &table.name).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let text_cols: Vec<String> = columns
            .iter()
            .filter(|c| is_text_search_column(&c.data_type))
            .map(|c| c.name.clone())
            .collect();
        if text_cols.is_empty() {
            continue;
        }
        let pk: Vec<String> = columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect();
        let dtypes: HashMap<String, String> = columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone()))
            .collect();
        let sql = build_search_scan_sql(
            cfg.db_type,
            schema,
            &table.name,
            &text_cols,
            needle,
            scan_limit,
        );
        let Ok(r) = backend
            .execute_query(cfg, db, &sql, Some(scan_limit.max(1)), Some(60))
            .await
        else {
            continue;
        };
        let col_names = r.columns.clone();
        // Only columns that are actually text participate in matching, so a
        // numeric column returned by `SELECT *` is never mis-flagged.
        let text_set: HashSet<&str> = text_cols.iter().map(|s| s.as_str()).collect();
        for row in &r.rows {
            if truncated {
                break;
            }
            let vals: Vec<Val> = row.iter().map(value_to_val).collect();
            for (ci, cname) in col_names.iter().enumerate() {
                if !text_set.contains(cname.as_str()) {
                    continue;
                }
                let Some(v) = vals.get(ci) else {
                    continue;
                };
                let text = v.text();
                // NULL and the empty string are skipped, and the needle can
                // never be a substring of an empty cell anyway.
                if text.is_empty() || !text.to_lowercase().contains(&needle_lower) {
                    continue;
                }
                hits.push(SearchHit {
                    schema: schema.to_string(),
                    table: table.name.clone(),
                    column: cname.clone(),
                    matched: text.to_string(),
                    filter: search_hit_filter(cfg.db_type, &col_names, &vals, &dtypes, &pk, cname),
                });
                if hits.len() >= SEARCH_MAX_HITS {
                    truncated = true;
                    break;
                }
            }
        }
    }
    let _ = tx.send(OpResult::SearchProgress {
        gen,
        done: total,
        total,
    });
    OpResult::SearchDone {
        gen,
        hits,
        skipped,
        tables: total,
        truncated,
    }
}

/// R100: walk one database / schema and build its data dictionary. Views are
/// included — a dictionary of the whole database should describe them too, and
/// their column metadata is as cheap as a table's. Every table contributes its
/// columns (required) plus its indexes and foreign keys (best effort); a table
/// whose columns cannot be read is skipped rather than failing the whole walk.
#[allow(clippy::too_many_arguments)]
async fn run_data_dictionary(
    backend: &LocalBackend,
    cfg: &ConnectionConfig,
    db: &str,
    schema: &str,
    gen: u64,
    cancel: &AtomicBool,
    tx: &Tx,
) -> OpResult {
    let tables = match backend.list_tables(cfg, db, schema).await {
        Ok(t) => t,
        Err(e) => return OpResult::Error(format!("data dictionary: list tables: {e}")),
    };
    let total = tables.len();
    let _ = tx.send(OpResult::DictProgress {
        gen,
        done: 0,
        total,
    });
    let mut metas: Vec<TableMeta> = Vec::with_capacity(total);
    for (i, table) in tables.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(OpResult::DictProgress {
                gen,
                done: i,
                total,
            });
            return OpResult::DictCancelled { gen };
        }
        let _ = tx.send(OpResult::DictProgress {
            gen,
            done: i,
            total,
        });
        let Ok(columns) = backend.get_columns(cfg, db, schema, &table.name).await else {
            continue;
        };
        let indexes = list_indexes_best_effort(backend, cfg, db, schema, &table.name).await;
        let foreign_keys =
            list_foreign_keys_best_effort(backend, cfg, db, schema, &table.name).await;
        metas.push(TableMeta {
            table: table.name.clone(),
            schema: schema.to_string(),
            columns,
            indexes,
            foreign_keys,
        });
    }
    let _ = tx.send(OpResult::DictProgress {
        gen,
        done: total,
        total,
    });
    let dialect = cfg.db_type.as_str().to_string();
    let content = database_dictionary_markdown(db, &dialect, &now_iso8601(), &metas);
    OpResult::DictReady {
        gen,
        db: db.to_string(),
        tables: total,
        content,
    }
}

fn spawn_op(backend: &Arc<LocalBackend>, tx: &Tx, op: Op) {
    let backend = backend.clone();
    let tx = tx.clone();
    let limit = op.watchdog();
    // R99: a query timeout must carry the query's tag so a soft-cancelled run's
    // timeout is dropped like any other late reply instead of surfacing an error.
    let query_tag = match &op {
        Op::Query(cfg, _, _, _, _, epoch) => Some(QueryTag {
            conn_id: cfg.id.clone(),
            epoch: *epoch,
        }),
        _ => None,
    };
    tokio::spawn(async move {
        // The watchdog is the last resort: a server that accepts the socket but
        // never answers must surface an error, not a spinner that never stops.
        let res = match tokio::time::timeout(limit, run_op(&backend, op, &tx)).await {
            Ok(r) => r,
            Err(_) => {
                let msg = tf(
                    "操作超时（{}s）· 服务器无响应或网络中断，请检查连接后用 d 重连",
                    &[&(limit.as_secs())],
                );
                match query_tag {
                    Some(tag) => OpResult::QueryFailed { tag, msg },
                    None => OpResult::Error(msg),
                }
            }
        };
        let _ = tx.send(res);
    });
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

/// R110: the current iteration number, surfaced by the About dialog. Bumped by
/// hand each round; the about dialog's `{n} keybindings · R{m} rounds` line and
/// its test read this one constant.
pub(crate) const DBXT_ROUND: u32 = 110;

/// R110: the git commit this binary was built from, injected by `build.rs`
/// (short SHA) or a release environment. `None` for a tarball / crates.io
/// build with no `.git`, so the field is safely omitted.
pub(crate) fn build_sha() -> Option<&'static str> {
    option_env!("DBXT_GIT_SHA").filter(|s| !s.is_empty())
}

/// R110: the build date (the commit's own date) injected by `build.rs`. Same
/// safe-omission contract as [`build_sha`].
pub(crate) fn build_date() -> Option<&'static str> {
    option_env!("DBXT_BUILD_DATE").filter(|s| !s.is_empty())
}

/// R110: format the `--version` line from its parts, so the enrichment is
/// unit-tested without the compile-time environment. `dbxt 0.0.4` stays the
/// bare line; when a commit and/or date is known it is appended in parentheses.
/// The leading `dbxt x.y.z` is preserved, so `cmd/install.sh`'s semver grep
/// keeps working.
fn version_line_with(version: &str, sha: Option<&str>, date: Option<&str>) -> String {
    let mut extra: Vec<String> = Vec::new();
    if let Some(s) = sha.filter(|s| !s.is_empty()) {
        extra.push(format!("commit {s}"));
    }
    if let Some(d) = date.filter(|d| !d.is_empty()) {
        extra.push(format!("built {d}"));
    }
    if extra.is_empty() {
        format!("dbxt {version}")
    } else {
        format!("dbxt {version} ({})", extra.join(", "))
    }
}

/// The exact `--version` line. Kept in one place so the format is tested once.
fn version_line() -> String {
    version_line_with(dbxt_version(), build_sha(), build_date())
}

/// `dbxt --help`: a short usage summary. The full manual lives in the README.
///
/// Kept as a plain string (rather than a series of `println!`) so the exact
/// text is unit-testable and so the caller can route it through
/// [`write_stdout`], which tolerates a closed pipe.
fn help_text() -> String {
    format!(
        "dbxt {} — {}\n\n{}: dbxt [DBX_STORE] [--last] | dbxt mcp [--http]\n\n{}:\n{}\n\n{}:\n{}\n{}\n{}\n{}\n\n{}: https://github.com/vst93/dbxt\n",
        dbxt_version(),
        t("DBX 的终端界面"),
        t("用法"),
        t("参数"),
        t("  DBX_STORE  dbx.db 文件或其所在目录（默认：DBX_DATA_DIR 或平台默认位置）"),
        t("选项"),
        t("  -h, --help     显示本帮助"),
        t("  -V, --version  显示版本"),
        t("  --last         启动即恢复上次会话的连接与库表（失败逐级降级）"),
        t("  mcp            以 DBX 原生 MCP 服务运行（默认 stdio，--http 为 Streamable HTTP）"),
        t("文档"),
    )
}

/// R98: the parsed command line (after `--help` / `--version` are answered).
#[derive(Default, PartialEq, Debug)]
pub(crate) struct CliArgs {
    pub(crate) store: Option<String>,
    pub(crate) want_last: bool,
}

/// R98: parse the arguments (excluding `argv[0]`). `-h` / `--help` / `-V` /
/// `--version` are recognised but answered by the caller; an unknown option is
/// a usage error (`Err`). `--last` sets the auto-resume flag; any other bare
/// word is the store path (the last one wins).
pub(crate) fn parse_cli_args(args: &[String]) -> std::result::Result<CliArgs, String> {
    let mut out = CliArgs::default();
    for a in args {
        match a.as_str() {
            "-h" | "--help" | "-V" | "--version" => {}
            "--last" => out.want_last = true,
            s if s.len() > 1 && s.starts_with('-') => return Err(s.to_string()),
            s => out.store = Some(s.to_string()),
        }
    }
    Ok(out)
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

fn main() -> Result<()> {
    // A generous worker stack is deliberate: the PostgreSQL / SQL Server drivers
    // parse every statement with `sqlparser`, whose DDL handling is deeply
    // recursive, and the async call chain above it is long. The 2 MiB tokio
    // default can be exhausted by a debug build while running a large
    // `CREATE TABLE` / multi-statement script, aborting the whole process. The
    // stack is reserved lazily, so the larger ceiling costs nothing until used.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<()> {
    // Resolve the UI language once, before any text is drawn.
    ui_text::set_lang(ui_text::detect_lang());
    // `--version` / `--help` answer before the TUI is initialised, so they work
    // over a pipe (the install scripts query `--version`) and without a terminal.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `dbxt mcp` runs DBX's own MCP server. It is answered before the TUI's
    // terminal check because a stdio MCP client talks over stdout, which is
    // not a terminal.
    if args.first().is_some_and(|a| a == "mcp") {
        return run_mcp(&args[1..]).await;
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        write_stdout(&help_text())?;
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        write_stdout(&format!("{}\n", version_line()))?;
        return Ok(());
    }
    let CliArgs { store, want_last } = match parse_cli_args(&args) {
        Ok(c) => c,
        Err(s) => {
            // An unknown option is a usage error (exit 2, like cmd/install.sh):
            // previously `dbxt --foo` was taken as a store path, created a file
            // literally named `--foo`, and then failed inside the TUI.
            write_stderr(&format!(
                "{}\n{}: dbxt [DBX_STORE] [--last]  (-h/--help)\n",
                tf("未知选项: {}", &[&s]),
                t("用法"),
            ));
            std::process::exit(2);
        }
    };
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
    let mut db_path: PathBuf = match store {
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
    // R116: from the encrypted-secret-store kernel onward, dbxt must not open a
    // store that still holds plaintext secrets. Do the desktop-equivalent
    // upgrade here (with its own backup + verification) so a user without the
    // desktop app is not locked out. A fresh or already-encrypted store is a
    // cheap read-only no-op.
    ensure_encrypted_store(&db_path)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let backend = Arc::new(LocalBackend::open(&db_path).await.map_err(|e| {
        let detail = humanize_backend_error(&e);
        anyhow::anyhow!("{}", tf("打开 DBX 存储文件失败 ({}): {}\n(可用 DBX_DATA_DIR 指定目录，或把 dbx.db 文件路径作为第一个位置参数传入)", &[&format!("{:?}", db_path), &(detail)]))
    })?);

    let terminal = ratatui::init();
    // ratatui 0.29's init() does not enable mouse capture; do it explicitly so the
    // touch (Down) / wheel (Scroll) layer receives events. crossterm's command
    // enables normal (1000), button-event (1002) and any-event (1003) tracking plus
    // SGR encoding (1006), so press, release, `Drag` and bare `Moved` all reach us —
    // the drag path a phone's horizontal swipe needs is therefore live.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run_app(terminal, backend, want_last).await;
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
        let config_num_fmt = config.num_fmt.unwrap_or_default();
        let config_stripe = config.stripe.unwrap_or(false);
        // R79: editor input assist ships on. A `tui.json` value still wins.
        let config_editor_indent = config.editor_indent.unwrap_or(true);
        let config_editor_pairs = config.editor_pairs.unwrap_or(true);
        // R88: the statement gutter is a visual aid, so it defaults off.
        let config_stmt_gutter = config.stmt_gutter.unwrap_or(false);
        let mut app = Self {
            backend,
            page: Page::Browse,
            focus: Focus::Sidebar,
            quit: false,
            connections: Vec::new(),
            temp_conns: Vec::new(),
            conn_list: ListState::default(),
            picker_open: true,
            conn_sort: ConnSort::Name,
            tip_idx: ui_text::tip_start_index(),
            conn_gen: 0,
            last_conn_id: None,
            conn_recent: Vec::new(),
            conn_recent_open: false,
            conn_recent_list: ListState::default(),
            conn_pointers: HashMap::new(),
            pending_restore: None,
            switch_notice: None,
            last_session: None,
            want_last: false,
            resume_last: false,
            resume_note: None,
            session_opened: false,
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
            tree_search: String::new(),
            tree_search_prompt: None,
            tree_search_prev: None,
            table_sort: TableSort::Name,
            table_jump_letter: None,
            tree_conn_open: std::collections::HashSet::new(),
            tree_conn_closed: std::collections::HashSet::new(),
            tree_db_closed: std::collections::HashSet::new(),
            tree_dbs: std::collections::HashMap::new(),
            tree_db_state: std::collections::HashMap::new(),
            tree_gen: std::collections::HashMap::new(),
            sidebar_layout: SidebarLayout::default(),
            sidebar_layout_raw: None,
            rename_edit: None,
            group_closed: config.group_closed.clone(),
            db_sizes: std::collections::HashMap::new(),
            db_size_state: std::collections::HashMap::new(),
            db_size_gen: std::collections::HashMap::new(),
            conn_live: HashMap::new(),
            conn_connecting: HashSet::new(),
            server_versions: HashMap::new(),
            server_rtts: HashMap::new(),
            conn_probe_failed: HashSet::new(),
            probe_all: None,
            latency_sort: false,
            recent_sort: RecentSort::Recent,
            side_rows: Vec::new(),
            side_sel: 0,
            side_table_seen: None,
            redis_filter: String::new(),
            redis_filter_prompt: None,
            redis_type_filter: None,
            redis_sort: RedisSort::Scan,
            redis_jump_letter: None,
            redis_mem: HashMap::new(),
            redis_mem_sort: false,
            redis_mem_probe: None,
            redis_mem_gen: 0,
            show_stmt_timing: false,
            columns: Vec::new(),
            ddl: None,
            struct_view: StructView::Fields,
            ddl_scroll: 0,
            ddl_popup: None,
            ddl_popup_pending: None,
            table_comment: None,
            table_comment_loaded: false,
            comment_edit: None,
            comment_refresh: false,
            materialize_prompt: None,
            materialize_write: None,
            pending_materialize_msg: None,
            editor: TextArea::default(),
            history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
            editor_undo: None,
            editor_clip_ring: Vec::new(),
            editor_clip_idx: None,
            editor_clip_last: String::new(),
            editor_find: None,
            editor_find_needle: String::new(),
            editor_find_idx: None,
            editor_find_snapshot: Vec::new(),
            editor_error_spans: Vec::new(),
            editor_error_idx: 0,
            editor_error_snapshot: Vec::new(),
            editor_error_base: String::new(),
            history_open: false,
            history_list: ListState::default(),
            history_rows: Vec::new(),
            history_persisted: Vec::new(),
            session_runs: Vec::new(),
            history_favorites: HashSet::new(),
            history_needle: String::new(),
            history_filter: None,
            history_view: Vec::new(),
            history_confirm: None,
            pending_run_origin: "editor",
            pending_scope: None,
            direct_run: false,
            grid: None,
            grid_kind: GridKind::Query,
            page_state: None,
            script: None,
            sel: 0,
            col_offset: 0,
            col_cursor: 0,
            ref_row: None,
            hbar_until: None,
            vis_cols: 0,
            grid_max_cell: 44,
            // R112: the first-column freeze is off by default (visual default
            // red line); `g F` turns it on for the active result tab only.
            freeze_first: false,
            // R113: the absolute row-number column is off by default (visual
            // default red line); `g N` turns it on for the active result tab.
            show_row_numbers: false,
            frozen_cols: Vec::new(),
            cell_popup: None,
            error_popup: None,
            row_popup: None,
            compact: config_compact,
            num_fmt: config_num_fmt,
            stripe: config_stripe,
            num_summary: false,
            editor_indent: config_editor_indent,
            editor_pairs: config_editor_pairs,
            stmt_gutter: config_stmt_gutter,
            editor_gutter: 0,
            col_hidden: HashSet::new(),
            col_picker_open: false,
            col_picker_list: ListState::default(),
            cols_popup_open: false,
            cols_popup_scroll: 0,
            table_info_open: false,
            table_info_scroll: 0,
            cols_popup_needle: String::new(),
            cols_popup_filter: None,
            cols_popup_sel: 0,
            grid_full: None,
            recent_tables: Vec::new(),
            recent_open: false,
            recent_list: ListState::default(),
            table_jump_open: false,
            table_jump_needle: String::new(),
            table_jump_list: ListState::default(),
            nav_history: Vec::new(),
            nav_pos: 0,
            pending_open_redis_key: None,
            nav_landing: None,
            row_hint_shown: false,
            row_hint_until: None,
            popup_cache: None,
            pending_open_table: None,
            pending_table_filter: None,
            completion: None,
            config,
            config_path,
            result_filter: None,
            result_needle: String::new(),
            result_rows: Vec::new(),
            col_filter_name: None,
            col_filter_needle: String::new(),
            col_filter_prompt: None,
            locate_prompt: None,
            locate_needle: String::new(),
            locate_col: None,
            cell_find_prompt: None,
            cell_find_needle: String::new(),
            cell_find_hits: Vec::new(),
            cell_find_idx: 0,
            cell_find_capped: false,
            col_jump: None,
            goto_prompt: None,
            last_sql: None,
            last_executed: None,
            quit_armed: false,
            search_input: None,
            search_query: String::new(),
            search_open: false,
            search_list: ListState::default(),
            search_hits: Vec::new(),
            search_progress: None,
            search_running: false,
            search_skipped: Vec::new(),
            search_gen: 0,
            search_cancel: Arc::new(AtomicBool::new(false)),
            search_truncated: false,
            dict_prompt: None,
            dict_content: None,
            dict_db: String::new(),
            dict_confirm: None,
            dict_running: false,
            dict_progress: None,
            dict_gen: 0,
            dict_cancel: Arc::new(AtomicBool::new(false)),
            diff_picker: None,
            diff: None,
            db_diff: None,
            diff_gen: 0,
            data_diff: None,
            data_where: None,
            data_diff_gen: 0,
            data_progress: None,
            data_cancel: Arc::new(AtomicBool::new(false)),
            result_snapshot: HashMap::new(),
            result_diff: None,
            file_load_prompt: None,
            file_load_plan: None,
            sqlite_open: None,
            filter_prompt: None,
            edit_dialog: None,
            pending_write: false,
            pending_write_msg: None,
            pending_fk_msg: None,
            batch: Vec::new(),
            pane_override: [None; 3],
            auto_collapse: false,
            help_open: false,
            help_scroll: 0,
            help_needle: String::new(),
            help_filter: None,
            about: None,
            session_start: Instant::now(),
            session_started_wall: chrono::Local::now().format("%H:%M:%S").to_string(),
            pan_mode: false,
            drag_pan,
            gesture: PanGesture::default(),
            pending_tap: None,
            tap: DoubleTap::default(),
            popup_tap: DoubleTap::default(),
            mouse_epoch: Instant::now(),
            editor_vp: EditorViewport::default(),
            row_popup_hit: Vec::new(),
            mouse_debug,
            mouse_log: VecDeque::new(),
            trace_path,
            last_event: None,
            result_tabs: Vec::new(),
            pinned_result: None,
            result_tab: 0,
            query_more: None,
            snippet_open: false,
            snippet_list: ListState::default(),
            snippets: Vec::new(),
            snippet_needle: String::new(),
            snippet_filter: None,
            snippet_view: Vec::new(),
            snippet_confirm: None,
            snippet_insert: false,
            snippet_name: None,
            template_open: false,
            template_list: ListState::default(),
            template_needle: String::new(),
            template_filter: None,
            template_view: Vec::new(),
            template_active: false,
            template_ph_start: None,
            table_meta: None,
            outline_open: None,
            outline_cache: HashMap::new(),
            outline_pending: None,
            count_cache: HashMap::new(),
            pending_sel: None,
            pending_focus: None,
            page_pending: false,
            page_gen: 0,
            pending_open_page: false,
            deep_page_hint_shown: false,
            pending_deep_hint: false,
            db_picker_open: false,
            db_list: ListState::default(),
            pending_table: None,
            grid_gutter: 0,
            grid_frozen: 0,
            grid_frozen_cols: Vec::new(),
            grid_widths: Vec::new(),
            grid_avail: 0,
            grid_epoch: 0,
            width_cache: None,
            col_width_mem: ColWidthMemory::default(),
            row_sel_anchor: None,
            confirm: None,
            loading: true,
            // The initial `ListConnections` below is the one call not spawned through
            // `App::spawn`, so it is pre-counted here.
            pending_ops: 1,
            loading_since: Some(Instant::now()),
            spinner: 0,
            cancel_epoch: HashMap::new(),
            queries_running: HashMap::new(),
            query_slots_released: HashMap::new(),
            cancel_announced: HashMap::new(),
            status: t("加载连接…").into(),
            flash_until: None,
            flash_text: String::new(),
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
            redis_ttl_clock: Instant::now(),
            mongo_page: 0,
            mongo_filter: String::new(),
            mongo_gen: 0,
            mongo_docs: Vec::new(),
            mongo_docs_base: Vec::new(),
            mongo_size_sort: MongoSizeSort::Natural,
            mongo_field_prompt: None,
            mongo_path_prompt: None,
            mongo_dialog: None,
            form: ConnForm::default(),
            ssh_prompt: None,
            ssh_notice: None,
            import_prompt: None,
            import_plan: None,
            import_gen: 0,
            import_scroll: 0,
            import_progress: None,
            import_report: None,
            transfer: None,
            transfer_report: None,
            transfer_gen: 0,
            transfer_progress: None,
            transfer_cancel: Arc::new(AtomicBool::new(false)),
            export_open: false,
            export_list: ListState::default(),
            export_pending: None,
            export_path: None,
            last_export_path: None,
            export_memory_dir: None,
            batch_export_pending: None,
            batch_export_confirm: None,
            conn_export: None,
            conn_import_path: None,
            conn_import_plan: None,
            layout_mode: LayoutMode::Mid,
            term_h: 0,
            term_w: 0,
            help_mini: false,
            pending_g: false,
            count_buf: String::new(),
            count_deadline: None,
            rects: Rects::default(),
        };
        app.editor
            .set_placeholder_text(t("SQL … (Ctrl-J 当前句/选区 · F5 全部 · ↑ 历史)"));
        app.set_placeholder();
        app
    }
}

async fn run_app(
    mut terminal: ratatui::DefaultTerminal,
    backend: Arc<LocalBackend>,
    want_last: bool,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();

    // Bridge the kernel's process-global SSH prompt / notice gateways into the
    // app's result channel so a host-key TOFU or keyboard-interactive challenge
    // can be answered in the TUI. Without a gateway the kernel fails closed
    // (an unknown host key is never trusted), which is the right default but
    // makes new bastions unusable.
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<SshPromptEnvelope>(8);
    ssh_prompt::install_ssh_prompt_gateway(prompt_tx);
    let (notice_tx, mut notice_rx) = tokio::sync::mpsc::channel::<SshHostKeyNotice>(8);
    ssh_prompt::install_ssh_notice_gateway(notice_tx);
    {
        let op_tx = tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(env) = prompt_rx.recv() => {
                        let _ = op_tx.send(OpResult::SshPrompt(Box::new(env)));
                    }
                    Some(notice) = notice_rx.recv() => {
                        let _ = op_tx.send(OpResult::SshNotice(Box::new(notice)));
                    }
                    else => break,
                }
            }
        });
    }

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(180));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // `DBXT_EVENT_TRACE=<path>` (or `=1` for the default path) records mouse and
    // resize events so a user can tell us exactly what their phone terminal sends.
    // `DBXT_MOUSE_DEBUG=1` does the same, defaults the log to the temp dir, and adds
    // a live on-screen readout.
    let mouse_debug = std::env::var_os("DBXT_MOUSE_DEBUG").is_some_and(|v| !v.is_empty());
    let trace_path = env_log_path("DBXT_EVENT_TRACE", "dbxt-events.log").or_else(|| {
        mouse_debug
            .then(|| env_log_path("DBXT_MOUSE_DEBUG", "dbxt-mouse.log"))
            .flatten()
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
    // R98: load the last session (best-effort) and seed the startup highlight /
    // `--last` auto-resume request.
    if let Some(path) = last_session_path() {
        app.last_session = LastSession::load(&path);
    }
    app.want_last = want_last;

    // Pre-counted by `pending_ops: 1` in the initializer above.
    spawn_op(&backend, &tx, Op::ListConnections);
    // R48: the desktop sidebar groups load in parallel with the connection list.
    spawn_op(&backend, &tx, Op::SidebarLayout);

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
                // R57: refresh the Redis key browser's TTL countdown locally once
                // a second, so `…s` counts down without a server round-trip.
                if app.backend_kind == Backend::Redis {
                    let now = Instant::now();
                    let secs = now
                        .saturating_duration_since(app.redis_ttl_clock)
                        .as_secs() as i64;
                    if secs > 0 {
                        app.redis_ttl_clock = now;
                        for k in app.redis_scan.all.iter_mut() {
                            k.ttl = redis_ttl_advance(k.ttl, secs);
                        }
                        for k in app.redis_scan.keys.iter_mut() {
                            k.ttl = redis_ttl_advance(k.ttl, secs);
                        }
                        if let Some(v) = app.redis_value.as_mut() {
                            v.ttl = redis_ttl_advance(v.ttl, secs);
                        }
                    }
                }
                // A digit typed without a following motion becomes a direct list
                // jump once the short grace window closes.
                if app
                    .count_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    flush_count(&mut app, &tx);
                }
                // First landing in a data grid: one-shot discovery hint. Fades
                // out after a few seconds, or on the next key.
                maybe_show_row_hint(&mut app);
                if app
                    .row_hint_until
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    if app.status == row_hint_text() {
                        app.status.clear();
                    }
                    app.row_hint_until = None;
                }
                // R75: an `Esc` flash ("关闭 X" / "已清除 Y") clears itself once
                // its short TTL lapses, unless a newer message replaced it.
                expire_flash(&mut app);
            }
        }
    }
    // R98: remember where this run left off on a graceful exit. A run that never
    // opened a connection writes nothing (`save_last_session` gates on it).
    if let Some(path) = last_session_path() {
        save_last_session(&app, &path);
    }
    Ok(())
}

/// R99: the [`QueryTag`] carried by a query reply, if it is one. Used to drop a
/// soft-cancelled / superseded run's late result before it renders.
fn query_result_tag(res: &OpResult) -> Option<&QueryTag> {
    match res {
        OpResult::Query(_, _, _, tag) => Some(tag),
        OpResult::Script(_, tag) => Some(tag),
        OpResult::QueryFailed { tag, .. } => Some(tag),
        _ => None,
    }
}

fn apply_op_result(app: &mut App, res: OpResult, tx: &Tx) {
    // R99: a query reply that belongs to a soft-cancelled or superseded run is
    // dropped silently. Its op still counts as finished for the spinner, but the
    // slot was already released at cancel time, so do not double-decrement.
    if let Some(tag) = query_result_tag(&res) {
        if app.query_reply_is_stale(tag) {
            let had_release = app
                .query_slots_released
                .get(&tag.conn_id)
                .copied()
                .unwrap_or(0)
                > 0;
            if had_release {
                let now_zero = if let Some(n) = app.query_slots_released.get_mut(&tag.conn_id) {
                    *n -= 1;
                    *n == 0
                } else {
                    false
                };
                if now_zero {
                    app.query_slots_released.remove(&tag.conn_id);
                }
                // Only a reply whose slot a soft cancel released earns the
                // "discarded" notice; a merely superseded run is dropped
                // silently. The announce map keeps it once per generation.
                let tag = tag.clone();
                app.announce_discarded(&tag);
            } else {
                app.pending_ops = app.pending_ops.saturating_sub(1);
                if app.pending_ops == 0 {
                    app.loading = false;
                    app.loading_since = None;
                }
            }
            return;
        }
    }
    // R99: a live query failure clears its in-flight marker, then flows through
    // the generic error path (humanized, cleanup, popup).
    let res = match res {
        OpResult::QueryFailed { tag, msg } => {
            app.queries_running.remove(&tag.conn_id);
            OpResult::Error(msg)
        }
        other => other,
    };
    // Intermediate / side-channel messages do not count as an op finishing, so
    // they are handled before the spinner accounting.
    if matches!(
        res,
        OpResult::ImportProgress { .. }
            | OpResult::QueryProgress { .. }
            | OpResult::SshPrompt(_)
            | OpResult::SshNotice(_)
            | OpResult::SearchProgress { .. }
            | OpResult::DictProgress { .. }
            | OpResult::DataDiffProgress { .. }
            | OpResult::TransferProgress { .. }
            | OpResult::ConnStatus(_)
            | OpResult::SessionRun(_)
            | OpResult::ProbeAllPartial { .. }
    ) {
        match res {
            // R47b: a background liveness refresh. The kernel is the source of
            // truth, so an open pool clears the local "connecting" mark.
            OpResult::ConnStatus(list) => {
                for (id, open) in list {
                    app.conn_live.insert(id.clone(), open);
                    if open {
                        app.conn_connecting.remove(&id);
                    }
                }
            }
            // R96: one connection answered during the full-list probe. Refresh
            // the RTT cache / failure set in place and advance the progress
            // line, so the tree tail is correct the moment the packet lands.
            OpResult::ProbeAllPartial { id, rtt } => {
                match rtt {
                    Some(d) => {
                        app.server_rtts.insert(id.clone(), d);
                        app.conn_probe_failed.remove(&id);
                    }
                    None => {
                        app.server_rtts.remove(&id);
                        app.conn_probe_failed.insert(id);
                    }
                }
                let p = app.probe_all.get_or_insert_with(ProbeAll::default);
                p.done = p.done.saturating_add(1);
                if rtt.is_some() {
                    p.ok = p.ok.saturating_add(1);
                } else {
                    p.failed = p.failed.saturating_add(1);
                }
                app.status = tf("探测中 {}/{}", &[&(p.done), &(p.total)]);
            }
            OpResult::SearchProgress { gen, done, total } => {
                if gen == app.search_gen {
                    app.search_progress = Some((done, total));
                    app.status = tf(
                        "全库搜索「{}」· {}/{} 表…",
                        &[&(app.search_query), &(done), &(total)],
                    );
                }
            }
            OpResult::DictProgress { gen, done, total } => {
                if gen == app.dict_gen {
                    app.dict_progress = Some((done, total));
                    app.status = tf("字典生成中 {}/{}", &[&done, &total]);
                }
            }
            OpResult::DataDiffProgress { gen, done, total } => {
                if gen == app.data_diff_gen {
                    app.data_progress = Some((done, total));
                    app.status = if total > 0 {
                        tf("数据对比中… · {}/{} 块", &[&done, &total])
                    } else {
                        tf("数据对比中… · {} 块", &[&done])
                    };
                }
            }
            OpResult::ImportProgress { done, total } => {
                app.import_progress = Some((done, total));
                app.status = tf("导入 {} / {} 行…", &[&done, &total]);
            }
            OpResult::QueryProgress { done, total } => {
                app.status = tf("执行中… {}/{}", &[&done, &total]);
            }
            OpResult::SessionRun(run) => {
                // R84: in-memory session log (Alt-H panel, zero query). Rebuild
                // the display rows so an open panel reflects the run at once.
                app.push_session_run(*run);
                if app.history_open {
                    recompute_history_view(app);
                }
            }
            OpResult::TransferProgress {
                gen,
                rows,
                chunks,
                elapsed_ms,
                total,
            } => {
                if gen == app.transfer_gen {
                    app.transfer_progress = Some((rows, chunks, elapsed_ms, total));
                    let rate = (rows as u128 * 1000)
                        .checked_div(elapsed_ms)
                        .unwrap_or(rows as u128) as u64;
                    app.status = match total.filter(|t| *t > 0) {
                        Some(t) => tf(
                            "搬运中… {} 行 / 已完成 {} 块 / ~{} 行 ({} 行/秒)",
                            &[&rows, &chunks, &t, &rate],
                        ),
                        None => tf(
                            "搬运中… {} 行 / 已完成 {} 块 ({} 行/秒)",
                            &[&rows, &chunks, &rate],
                        ),
                    };
                }
            }
            OpResult::SshPrompt(env) => {
                let kind = env.request.kind;
                // A generic user-input prompt may ship a suggested default;
                // prefill it so the user can accept with one Enter.
                let input = env.request.default_value.clone().unwrap_or_default();
                app.ssh_prompt = Some(SshPromptState {
                    request: env.request,
                    responder: Some(env.responder),
                    input,
                });
                app.status = match kind {
                    SshPromptKind::HostKeyVerify => {
                        t("SSH 主机密钥待确认（y 接受 / n 拒绝）").into()
                    }
                    SshPromptKind::HostKeyChanged => t("SSH 主机密钥已变化，请确认").into(),
                    SshPromptKind::SecretInput => t("SSH 服务器要求额外验证").into(),
                    SshPromptKind::WorkerUploadConsent => t("SSH 请求确认").into(),
                    SshPromptKind::UserInput => t("需要用户输入").into(),
                };
            }
            OpResult::SshNotice(notice) => {
                app.ssh_notice = Some(ssh_notice_text(&notice));
                app.status = ssh_notice_text(&notice);
            }
            _ => unreachable!(),
        }
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
            // Keep the highlight on the same connection across a reload.
            let keep = app
                .conn_list
                .selected()
                .and_then(|i| app.connections.get(i))
                .map(|c| c.id.clone());
            app.connections = cs;
            // R83: re-attach this session's temporary SQLite quick-open
            // connections (they are deliberately never written to the store)
            // and make sure the kernel's runtime cache still knows them (a
            // `load_connections` sync drops any id the store does not have).
            merge_temp_connections(&mut app.connections, &app.temp_conns);
            let temps = app.temp_conns.clone();
            for cfg in temps {
                app.spawn(tx, Op::RegisterTempConn(Box::new(cfg), false));
            }
            sort_connection_list(&mut app.connections, app.conn_sort);
            let sel = keep
                .and_then(|id| app.connections.iter().position(|c| c.id == id))
                .or_else(|| {
                    // R98: on the initial picker load, drop the cursor on the last
                    // session's connection. Highlight only — nothing connects
                    // until the user presses Enter.
                    if app.selected.is_none() {
                        last_session_conn_index(app)
                    } else {
                        None
                    }
                })
                .or_else(|| (!app.connections.is_empty()).then_some(0));
            app.conn_list.select(sel);
            app.picker_open = app.selected.is_none();
            app.status = tf("{} 个连接 · ↑↓+Enter 选择 · c 新建 · s 排序", &[&(n)]);
            // R47b: seed the tree's liveness cache from the kernel now that the
            // connection list is known.
            refresh_conn_status(app, tx);
            // R98: `--last` — auto-resume the previous session's connection (and
            // its database / schema / table) once the list is known.
            if app.want_last && app.selected.is_none() {
                app.want_last = false;
                resume_last_session(app, tx);
            }
        }
        OpResult::SidebarLayout { layout, raw } => {
            // R48: groups come from DBX Desktop's own store. Rebuilding the tree
            // here means a desktop regrouping shows up the next time dbxt opens
            // (the session keeps its own collapse memory).
            app.sidebar_layout = *layout;
            // R55: keep the raw tree so a group rename / row move can patch just
            // the affected entry and save the rest verbatim.
            app.sidebar_layout_raw = raw.map(|v| *v);
            rebuild_side_rows(app);
        }
        // R55: a sidebar tree write finished. A failure must not look saved, so
        // it is surfaced and the tree is reloaded from the store.
        OpResult::SidebarLayoutSaved(err) => match err {
            None => app.status = t("✓ 已保存侧栏布局").into(),
            Some(e) => {
                app.status = tf("✗ 保存侧栏布局失败：{}", &[&e]);
                app.spawn(tx, Op::SidebarLayout);
            }
        },
        // R55: a connection rename landed. Merge it in place (like a copy) so
        // the browse view and the tree keep their position, and update the
        // active config when the renamed row is the one in use.
        OpResult::ConnRenamed(cfg) => {
            let id = cfg.id.clone();
            let name = cfg.name.clone();
            // Remember the node under the cursor so a name-driven re-sort keeps
            // the cursor on the renamed root.
            let hit = app
                .side_rows
                .get(app.side_sel)
                .and_then(|r| side_row_hit(app, r));
            match app.connections.iter().position(|c| c.id == id) {
                Some(i) => app.connections[i] = *cfg,
                None => app.connections.push(*cfg),
            }
            sort_connection_list(&mut app.connections, app.conn_sort);
            if app.selected.as_ref().is_some_and(|c| c.id == id) {
                app.selected = app.connections.iter().find(|c| c.id == id).cloned();
            }
            rebuild_side_rows(app);
            if let Some(h) = hit {
                if let Some(pos) = find_side_hit(app, &app.side_rows, &h) {
                    app.side_sel = pos;
                    side_mirror_table(app);
                }
            }
            app.status = tf("✓ 已重命名连接 {}", &[&name]);
        }
        // R68: a read-only toggle was persisted. Merge the updated config so the
        // tree 🔒, the editor badge and every write guard see the new policy.
        OpResult::ConnReadOnlySet(cfg) => {
            let id = cfg.id.clone();
            let name = cfg.name.clone();
            let ro = cfg.read_only;
            match app.connections.iter().position(|c| c.id == id) {
                Some(i) => app.connections[i] = *cfg,
                None => app.connections.push(*cfg),
            }
            if app.selected.as_ref().is_some_and(|c| c.id == id) {
                app.selected = app.connections.iter().find(|c| c.id == id).cloned();
            }
            rebuild_side_rows(app);
            app.status = if ro {
                tf("✓ 连接 {} 已设为只读（写操作将被拦截）", &[&name])
            } else {
                tf("✓ 连接 {} 已恢复为可写", &[&name])
            };
        }
        OpResult::ConnDeleted { id, name } => {
            app.connections.retain(|c| c.id != id);
            // R47b: forget the removed connection's cached liveness.
            app.conn_live.remove(&id);
            app.conn_connecting.remove(&id);
            let n = app.connections.len();
            let sel = app
                .conn_list
                .selected()
                .unwrap_or(0)
                .min(n.saturating_sub(1));
            app.conn_list.select((n > 0).then_some(sel));
            app.status = format!("✓ {}", tf("已删除连接 {}", &[&name]));
        }
        // R52: cache the server version under the connection it was read for. A
        // failed read (`None`) leaves the cache empty, so a later switch retries.
        OpResult::ServerVersion { id, version, rtt } => {
            if let Some(v) = version {
                if let Some(d) = rtt {
                    app.server_rtts.insert(id.clone(), d);
                }
                app.server_versions.insert(id, v);
            }
        }
        // R87: the explicit `P` probe reply. A success refreshes the R63 latency
        // cache in place and reports the RTT; a failure is only ever a status
        // line — never a popup — so a flaky link cannot hijack the UI.
        OpResult::HealthProbe {
            id,
            name,
            rtt,
            error,
        } => match rtt {
            Some(d) => {
                app.server_rtts.insert(id.clone(), d);
                app.conn_probe_failed.remove(&id);
                app.status = tf("✓ {} 探测成功 · RTT {}", &[&name, &format_rtt(d)]);
            }
            None => {
                app.server_rtts.remove(&id);
                app.conn_probe_failed.insert(id);
                app.status = tf("⚠ {} 探测失败：{}", &[&name, &error.unwrap_or_default()]);
            }
        },
        // R96: the full-list probe finished — summarise it (and point at the
        // temporary latency ordering now that there is something to order by).
        // Handled in the side-channel block above; unreachable here, but the
        // exhaustive match needs the arm.
        OpResult::ProbeAllPartial { .. } => {}
        OpResult::ProbeAllDone { total, ok, failed } => {
            app.probe_all = None;
            app.status = if failed == 0 {
                tf("{} 条 · {} 通", &[&total, &ok])
            } else {
                tf("{} 条 · {} 通 · {} 超时", &[&total, &ok, &failed])
            };
            // Point at the latency view only when the tree is on screen — that
            // is the one surface `O` reorders, so the picker never advertises a
            // key it would ignore.
            if app.selected.is_some() && app.server_rtts.len() > 1 {
                app.status.push_str(" · ");
                app.status.push_str(t("O 按延迟排序"));
            }
        }
        OpResult::Databases {
            databases: dbs,
            warning,
            gen,
        } => {
            // A slow enumeration for the connection the user already left must
            // not clobber the new connection's database list.
            if gen != app.conn_gen {
                return;
            }
            let configured = app.selected.as_ref().and_then(|c| c.database.clone());
            app.databases = dbs;
            // Keep the tree's per-connection cache in step with the active
            // connection's list, so collapsing and re-expanding it is instant.
            if let Some(cfg) = app.selected.clone() {
                app.tree_dbs.insert(cfg.id.clone(), app.databases.clone());
                app.tree_db_state.remove(&cfg.id);
                app.tree_conn_open.insert(cfg.id.clone());
                // R47b: the active pool answered, so it is live. Confirm the
                // other connections' state with a background registry read.
                app.conn_connecting.remove(&cfg.id);
                app.conn_live.insert(cfg.id, true);
                // R98: the pool answered, so this run may remember its session.
                app.session_opened = true;
                refresh_conn_status(app, tx);
            }
            app.db_index = configured
                .as_deref()
                .and_then(|db| app.databases.iter().position(|d| d == db))
                .unwrap_or(0);
            // R41 smart restore: prefer the database the switch wanted to return
            // to, but only when this connection actually exposes it.
            let mut resume_db_missing = false;
            if let Some(p) = &app.pending_restore {
                if !p.db.is_empty() {
                    match app.databases.iter().position(|d| d == &p.db) {
                        Some(i) => app.db_index = i,
                        // R98: a `--last` restore whose database is gone degrades
                        // to the connection's first screen rather than opening a
                        // same-named table in some other database.
                        None if app.resume_last => resume_db_missing = true,
                        None => {}
                    }
                }
            }
            if resume_db_missing {
                let db = app
                    .pending_restore
                    .as_ref()
                    .map(|p| p.db.clone())
                    .unwrap_or_default();
                if let Some(p) = app.pending_restore.as_mut() {
                    p.table = None;
                    p.schema.clear();
                }
                app.resume_last = false;
                app.resume_note = Some(tf(
                    "上次会话的库 {} 已不存在 · 回到连接首屏",
                    &[&(fix_double_encoding(&db))],
                ));
            }
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
            // R98: surface a database-level degradation now; the table landing
            // re-applies the note after its own list arrives.
            if let Some(note) = app.resume_note.clone() {
                app.status = note;
            }
        }
        OpResult::TreeDatabases {
            conn_id,
            databases,
            error,
            gen,
        } => {
            // Only the newest request for this root may land.
            if app.tree_gen.get(&conn_id).copied() != Some(gen) {
                return;
            }
            // R47b: the lazy fetch finished; the dot leaves the connecting
            // state and reflects whether the pool actually came up.
            app.conn_connecting.remove(&conn_id);
            match error {
                Some(msg) => {
                    app.conn_live.insert(conn_id.clone(), false);
                    app.tree_db_state.insert(conn_id, TreeDbState::Error(msg));
                }
                None => {
                    app.conn_live.insert(conn_id.clone(), true);
                    app.tree_dbs.insert(conn_id.clone(), databases);
                    app.tree_db_state.remove(&conn_id);
                }
            }
            refresh_conn_status(app, tx);
            rebuild_side_rows(app);
        }
        // R47b: a manual disconnect finished. The pool is gone, the cached
        // database list stays (muted), and a disconnected *active* connection
        // collapses to a grey root with the cursor moved to the nearest other
        // root — never a silent drop to an empty screen.
        OpResult::ConnDisconnected { id, name, error } => {
            app.conn_connecting.remove(&id);
            if let Some(msg) = error {
                app.status = tf("✗ 断开 {} 失败：{}", &[&name, &msg]);
                refresh_conn_status(app, tx);
                return;
            }
            app.conn_live.insert(id.clone(), false);
            // A stale error row would be misleading once disconnected; the
            // cached database list is kept so the tree keeps its shape.
            app.tree_db_state.remove(&id);
            // R109: the outline cache is keyed by connection id, so a torn-down
            // connection's columns must not survive it.
            clear_outline_cache(app);
            let was_active = app.selected.as_ref().is_some_and(|c| c.id == id);
            if was_active {
                // Collapse the root and drop the browse state that belonged to
                // the dead pool. `selected` stays put so the sidebar keeps its
                // tree and the picker never flashes.
                app.tree_conn_closed.insert(id.clone());
                app.tree_conn_open.remove(&id);
                app.databases.clear();
                app.db_index = 0;
                app.tables.clear();
                app.tables_all.clear();
                app.table_list = ListState::default();
                app.clear_grid();
                app.script = None;
                app.ddl = None;
                app.page_state = None;
                app.set_placeholder();
                app.loading = false;
            }
            app.status = tf("已断开 {} · 展开该根可重连", &[&name]);
            // R101: the pool is gone, so its result snapshot can never be
            // compared again. Set after the status so the flash is visible.
            if invalidate_result_snapshot(app, &id, false) && was_active {
                app.flash(t("快照已失效").into());
            }
            rebuild_side_rows(app);
            if was_active {
                side_focus_nearest_root(app, &id);
            }
            refresh_conn_status(app, tx);
        }
        // R47b: handled as an intermediate message above; this arm keeps the
        // main match exhaustive without treating it as a finished op.
        OpResult::ConnStatus(_) => {}
        OpResult::DbSize {
            db,
            info,
            error,
            gen,
        } => {
            // Only the newest request for this database may land.
            if app.db_size_gen.get(&db).copied() != Some(gen) {
                return;
            }
            match error {
                Some(msg) => {
                    app.db_size_state
                        .insert(db.clone(), TreeDbState::Error(msg.clone()));
                    app.status = tf("✗ {} 尺寸查询失败: {}", &[&db, &msg]);
                }
                None => {
                    let total = info.total_bytes;
                    let tables = info.rows.len();
                    app.db_sizes.insert(db.clone(), *info);
                    app.db_size_state.remove(&db);
                    rebuild_side_rows(app);
                    app.status = match total {
                        Some(b) => tf(
                            "✓ {} 尺寸 {} · {} 张表行数估计（会话缓存，s 重查）",
                            &[&db, &human_bytes(b), &tables],
                        ),
                        None => tf("✓ {} 尺寸已获取", &[&db]),
                    };
                }
            }
        }
        OpResult::Schemas {
            db,
            schemas,
            warning,
        } => {
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
            // R41 smart restore: a switch may have asked for a specific schema;
            // honour it when this connection has one by that name.
            if let Some(p) = &app.pending_restore {
                if !p.schema.is_empty() && app.schemas.contains(&p.schema) {
                    app.schema = p.schema.clone();
                }
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
            // R41 smart restore: a connection switch wants to land on the same
            // database / table when this connection has one by that name.
            let mut notice = app.switch_notice.take();
            if let Some(p) = app.pending_restore.take() {
                let db = fix_double_encoding(&app.current_db());
                if let Some(name) = p.table.clone() {
                    if let Some(pos) = focus_table_in_sidebar(app, &name) {
                        app.table_list.select(Some(pos));
                        app.nav_landing =
                            Some(tf("→ {}.{}", &[&db, &(fix_double_encoding(&name))]));
                        open_table_data(app, tx);
                        // R98: a `--last` resume that found its table names the
                        // restored target; the first page lands right after.
                        if app.resume_last {
                            app.resume_last = false;
                            app.status = tf(
                                "✓ 已恢复上次会话 · {}.{}",
                                &[&db, &(fix_double_encoding(&name))],
                            );
                        }
                        if let Some(nt) = notice {
                            app.status = format!("{} · ⚠ {nt}", app.status);
                        }
                        return;
                    }
                    // R98: the remembered table is gone — degrade to the
                    // database's first screen (table → database).
                    if app.resume_last {
                        app.resume_last = false;
                        app.nav_landing = Some(tf(
                            "→ {} 首屏（无 {}）",
                            &[&db, &(fix_double_encoding(&name))],
                        ));
                        app.status = tf(
                            "上次会话的表 {} 已不存在 · 回到库 {}",
                            &[&(fix_double_encoding(&name)), &db],
                        );
                        return;
                    }
                    app.nav_landing = Some(tf(
                        "→ {} 首屏（无 {}）",
                        &[&db, &(fix_double_encoding(&name))],
                    ));
                } else {
                    // R98: the last session had no table open — landing on the
                    // restored database is the successful end of the chain.
                    if app.resume_last {
                        app.resume_last = false;
                        app.nav_landing = Some(tf("→ {}", &[&db]));
                        app.status = tf("✓ 已恢复上次会话 · {}", &[&db]);
                        return;
                    }
                    app.nav_landing = Some(tf("→ {}", &[&db]));
                }
            }
            // A recent-table jump that had to switch database / schema first:
            // open the requested table now that the list has arrived.
            if let Some((schema, name)) = app.pending_open_table.take() {
                if let Some(pos) = focus_table_in_sidebar(app, &name) {
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
            let mut status = if app.table_filter.is_empty() {
                tf(
                    "{} 个表/视图 · Enter 数据 · r 结构 · / 过滤 · Tab 编辑SQL",
                    &[&(n)],
                )
            } else {
                tf(
                    "过滤「{}」· {}/{} 个表 · Esc 清除",
                    &[&(app.table_filter), &(app.tables.len()), &(n)],
                )
            };
            if let Some(nt) = notice.take() {
                status = format!("{status} · ⚠ {nt}");
            }
            // R98: a missing remembered database left its degradation note for
            // this landing (the table list is what finally replaces the status).
            if let Some(note) = app.resume_note.take() {
                status = note;
            }
            // R103: a materialize's `已物化 …` status must survive the sidebar
            // table-list refresh that ran right after the CTAS.
            if let Some(msg) = app.pending_materialize_msg.take() {
                status = msg;
            }
            app.status = status;
        }
        OpResult::Columns {
            table,
            schema,
            columns: cols,
        } => {
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
            app.status = tf(
                "{} 结构 · {} 字段 · t 切换 DDL · Esc 返回",
                &[&(fix_double_encoding(&table)), &(n)],
            );
        }
        OpResult::Ddl {
            table,
            schema,
            text,
        } => {
            if app.selected_table().map(|t| t.name.clone()).as_deref() == Some(table.as_str())
                && app.schema == schema
            {
                app.ddl = Some(text);
                app.ddl_scroll = 0;
            }
        }
        OpResult::TableDdl {
            table,
            schema,
            text,
            error,
        } => apply_ddl_popup_result(app, table, schema, text, error),
        OpResult::TableComment {
            table,
            schema,
            comment,
        } => {
            // Only keep a comment that belongs to the table on screen.
            if app.selected_table().map(|t| t.name.as_str()) == Some(table.as_str())
                && app.schema == schema
            {
                app.table_comment = comment;
                app.table_comment_loaded = true;
            }
        }
        OpResult::TableData {
            grid,
            total,
            total_lower_bound,
            has_next,
            page,
            table,
            schema,
            table_type,
            filter,
            order_by,
            keyset,
            gen,
        } => {
            // Discard any reply that is not for the latest request: a slow page
            // load must not clobber a newer filter / sort / table view.
            if gen != app.page_gen {
                return;
            }
            // R47b: a page arrived, so the pool is live (a query re-opens a
            // disconnected connection silently).
            mark_active_live(app);
            app.page_pending = false;
            let rows = grid.rows.len();
            if let Some(t) = total {
                app.remember_count(
                    &app.current_db(),
                    &schema,
                    &table,
                    &filter,
                    t,
                    total_lower_bound,
                );
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
                total_lower_bound,
                has_next,
                filter,
                order_by,
                keyset,
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
            let ps = app.page_state.as_ref().unwrap();
            let total_txt = total_label(ps);
            let extra = page_state_extra(ps);
            app.status = tf(
                "{}.{} · 第 {} 页 · {} 行 · {}{}",
                &[
                    &(fix_double_encoding(&app.current_db())),
                    &(fix_double_encoding(&schema_label)),
                    &(page + 1),
                    &(rows),
                    &(total_txt),
                    &(extra),
                ],
            );
            // A deep OFFSET page (no primary key to seek by) is slow; say so
            // once instead of silently taking seconds.
            if app.pending_deep_hint {
                app.pending_deep_hint = false;
                app.status = format!(
                    "{} · {}",
                    app.status,
                    t("深翻页较慢（无主键或自定义排序）；加过滤可提速")
                );
            }
            if let Some(msg) = app.pending_write_msg.take() {
                app.status = tf("{} · 已刷新（第 {} 页）", &[&(msg), &(page + 1)]);
            }
            // R97: a FK jump seeds the target browse; when its first page lands
            // the status names the jump instead of being lost behind the page
            // load that ran in between.
            if let Some(msg) = app.pending_fk_msg.take() {
                app.status = msg;
            }
        }
        OpResult::TableColumns {
            table,
            schema,
            columns,
            indexes,
            foreign_keys,
        } => {
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
                    table: table.clone(),
                    schema: schema.clone(),
                    columns,
                    indexes,
                    foreign_keys,
                });
            }
            // The initial page load waits for this so keyset-vs-OFFSET is chosen
            // once, with the primary key known, for every page including the first.
            if active && app.pending_open_page {
                app.pending_open_page = false;
                spawn_table_page(app, tx, 0);
            }
        }
        // R109: one sidebar outline fetch landed. Cache it, then redraw the tree
        // so the columns appear under the table (or report the failure and close
        // the outline). A late reply for a table the user already collapsed is
        // still cached for a later, free re-expand.
        OpResult::OutlineColumns { key, table, result } => {
            if app.outline_pending.as_deref() == Some(key.as_str()) {
                app.outline_pending = None;
            }
            match result {
                Ok(columns) => {
                    let n = columns.len();
                    app.outline_cache.insert(key.clone(), columns);
                    if app.outline_open.as_deref() == Some(key.as_str()) {
                        app.status = tf(
                            "列 {} · {} 列 · < 收起",
                            &[&(fix_double_encoding(&table)), &n],
                        );
                    }
                }
                Err(e) => {
                    if app.outline_open.as_deref() == Some(key.as_str()) {
                        app.outline_open = None;
                    }
                    app.status = tf(
                        "✗ 加载列 {} 失败：{}",
                        &[
                            &(fix_double_encoding(&table)),
                            &(humanize_backend_error(&e)),
                        ],
                    );
                }
            }
            rebuild_side_rows(app);
        }
        OpResult::Query(r, sql, cap, tag) => {
            // R99: this run is the current one; clear its in-flight marker.
            app.queries_running.remove(&tag.conn_id);
            // R47b: a query answered, so the pool is live.
            mark_active_live(app);
            // A `Ctrl-Enter` history direct run lands with a distinct status that
            // names its elapsed time (R45).
            let direct = std::mem::take(&mut app.direct_run);
            // Remember the SQL so `y` can guess a table name for a query result.
            app.last_sql = Some(sql.clone());
            // A fresh run starts a new result, so drop any previous row search
            // (a `Ctrl-N` load-more keeps it, since it is the same result).
            if cap <= QUERY_MAX_ROWS {
                app.result_needle.clear();
                app.result_filter = None;
                app.clear_col_filter();
                app.clear_cell_find();
                // R91: a fresh run is a new result set; the pinned reference row
                // does not carry over (a Ctrl-N load-more keeps it).
                app.ref_row = None;
            }
            // A statement that returned no columns is a write/DDL, and one that
            // reports affected rows (e.g. `INSERT … RETURNING`) changed data too:
            // any cached COUNT(*) may be stale now.
            let is_write = r.columns.is_empty() || r.affected_rows > 0;
            if is_write {
                app.count_cache.clear();
            }
            // R103: a confirmed CTAS landed. Keep the source grid on screen (no
            // auto-jump), name the new table and refresh the sidebar list. Match
            // on the exact statement so an unrelated in-flight run (an F5 during
            // the CTAS) is never mistaken for it.
            if app
                .materialize_write
                .as_ref()
                .is_some_and(|p| p.ctas_sql == sql)
            {
                apply_materialize_success(app, tx, r.affected_rows);
                return;
            }
            // A write launched from the edit dialog refreshes the current page
            // instead of replacing the grid with the DML result.
            if app.pending_write && is_write {
                app.pending_write = false;
                let affected = r.affected_rows;
                let note = format!("{}ms", r.execution_time_ms);
                // R102: a comment write invalidates the cached comment; re-read
                // the metadata so the structure view / `gc` popup show the new
                // text (the page reload below already refreshes a data view).
                let comment_refresh = std::mem::take(&mut app.comment_refresh);
                if comment_refresh {
                    app.table_comment = None;
                    app.table_comment_loaded = false;
                }
                if app.page_state.is_some() && app.grid_kind == GridKind::TableData {
                    let sel = app.sel;
                    let ps = app.page_state.clone().unwrap();
                    let msg = tf("✓ 影响 {} 行 · {}", &[&(affected), &(note)]);
                    app.pending_write_msg = Some(msg.clone());
                    reload_table_view(app, tx, ps.filter.clone(), ps.order_by.clone(), ps.page);
                    app.pending_sel = Some(sel);
                    app.status = tf("{} · 已刷新当前页", &[&(msg)]);
                    // R102: refresh the `gc` popup's cached column metadata too.
                    if comment_refresh {
                        refresh_after_comment_write(app, tx);
                    }
                } else if comment_refresh && app.grid_kind == GridKind::Columns {
                    // The structure view is on screen: re-read its metadata (and
                    // the table comment) in place.
                    refresh_after_comment_write(app, tx);
                } else {
                    app.status = tf("✓ 影响 {} 行 · {}", &[&(affected), &(note)]);
                }
                return;
            }
            app.pending_write = false;
            let note = note_of(&r);
            let truncated = r.truncated;
            let grid = Grid::from_query(
                r.columns.clone(),
                r.column_types.clone(),
                &r.rows,
                note.clone(),
            );
            let base = if direct {
                tf(
                    "直跑历史 · {} · {} 行 · {}",
                    &[&app.selected_name(), &(grid.rows.len()), &note],
                )
            } else {
                format!("{} · {} · {}", app.selected_name(), grid.rows.len(), note)
            };
            // A large `LIMIT` (> 10000) is only a heads-up: the run is never
            // blocked, the yellow `⚠` prefix just makes a slow result expected.
            app.status = match large_limit_hint(&sql) {
                Some(n) => format!("{} {}", tf("⚠ 大结果集 · LIMIT {} · 可能较慢", &[&n]), base),
                None => base,
            };
            // R88: a scoped run names the statement it executed.
            if let Some(l) = take_scope_label(app, &sql) {
                app.status = format!("{} · {}", app.status, l);
            }
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
            // R103: remember the statement behind this tab so `g m` materializes
            // the result the user is actually looking at (including after `[`).
            if let Some(tab) = app.result_tabs.get_mut(app.result_tab) {
                tab.sql = Some(sql.clone());
            }
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.focus = Focus::Preview;
            // A truncated result can be extended with Ctrl-N.
            app.query_more = if truncated { Some((sql, cap)) } else { None };
        }
        OpResult::Script(outcomes, tag) => {
            // R99: this run is the current one; clear its in-flight marker.
            app.queries_running.remove(&tag.conn_id);
            app.count_cache.clear();
            // R47b: the script ran, so the pool is live.
            mark_active_live(app);
            // A `Ctrl-Enter` history direct run lands with a distinct status that
            // names its total elapsed time (R45).
            let direct = std::mem::take(&mut app.direct_run);
            // A script result replaces any grid on screen; a result-row search
            // (which only applies to a data grid) must not leak into it.
            app.result_needle.clear();
            app.clear_cell_find();
            app.result_filter = None;
            app.clear_col_filter();
            let n = outcomes.len();
            let errors = outcomes.iter().filter(|o| o.error.is_some()).count();
            let affected: u64 = outcomes.iter().map(|o| o.affected).sum();
            let total_ms: u64 = outcomes.iter().map(|o| o.ms as u64).sum();
            // R77: capture the per-statement SQL + failures before the outcomes
            // are moved into the script view, so a failure can be located back in
            // the editor buffer (text only, zero queries).
            let stmt_sqls: Vec<String> = outcomes.iter().map(|o| o.sql.clone()).collect();
            let error_list: Vec<(usize, String)> = outcomes
                .iter()
                .enumerate()
                .filter_map(|(i, o)| o.error.clone().map(|e| (i, e)))
                .collect();
            let was_batch = app.pending_write;
            app.pending_write = false;
            app.query_more = None;
            let script = ScriptView {
                outcomes,
                sel: 0,
                drilled: None,
            };
            push_result_tab(
                app,
                tf("脚本 {} 条", &[&(n)]),
                None,
                Some(script),
                GridKind::Query,
            );
            app.ddl = None;
            app.struct_view = StructView::Fields;
            app.focus = Focus::Preview;
            if was_batch {
                app.status = if errors == 0 {
                    tf(
                        "✓ 批量提交成功 · {} 条语句 · 影响 {} 行",
                        &[&(n), &(affected)],
                    )
                } else {
                    tf(
                        "✗ 批量提交失败 · {} 错误 · 影响 {} 行（事务可能已回滚）· Enter 看详情",
                        &[&(errors), &(affected)],
                    )
                };
            } else if direct {
                app.status = tf(
                    "直跑历史 · {} 条语句 · 影响 {} 行 · {} 错误 · {}",
                    &[
                        &(n),
                        &(affected),
                        &(errors),
                        &history_duration_label(total_ms),
                    ],
                );
            } else {
                app.status = tf(
                    "脚本 · {} 条语句 · 影响 {} 行 · {} 错误 · {} · Enter 看结果",
                    &[
                        &(n),
                        &(affected),
                        &(errors),
                        &history_duration_label(total_ms),
                    ],
                );
            }
            // R88: a scoped selection run names the statement range it ran.
            // The scope is left intact for `record_editor_errors` below, which
            // consumes it to localize a failure to exactly these statements.
            if let Some(l) = app.pending_scope.as_ref().map(|s| s.label.clone()) {
                app.status = format!("{} · {}", app.status, l);
            }
            // R77: locate the failing statements back in the editor (when it
            // still holds exactly what ran) and append `第 N 条语句` to the
            // status line. `F8` / `Shift-F8` and `Alt-E` then cycle them.
            let base = app.status.clone();
            if record_editor_errors(app, &stmt_sqls, &error_list, &base) {
                apply_editor_error_status(app);
            } else {
                // No located error: drop any half-consumed scope.
                app.pending_scope = None;
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
            // R47b: Redis shares the LocalBackend pool, so a scan proves liveness.
            mark_active_live(app);
            app.redis_scan.pending = false;
            if append {
                app.redis_scan.all.extend(keys);
            } else {
                app.redis_scan.all = keys;
            }
            app.redis_scan.cursor = cursor;
            app.redis_scan.total = total;
            app.redis_scan.exhausted = cursor == 0;
            // Drop selections whose key is no longer in the loaded window (a
            // rescan can remove keys); keep them across a load-more append.
            if !app.redis_selected.is_empty() {
                let present: HashSet<String> = app
                    .redis_scan
                    .all
                    .iter()
                    .map(|k| k.key_raw.clone())
                    .collect();
                app.redis_selected.retain(|k| present.contains(k));
                if app.redis_selected.is_empty() {
                    app.redis_anchor = None;
                }
            }
            // Re-apply the client-side filter over the freshly loaded window.
            apply_redis_filter(app);
            // A history step (`Alt-←` / `Alt-→`) may be waiting for its key to
            // appear in the reloaded list; open it now.
            if let Some(raw) = app.pending_open_redis_key.take() {
                if let Some(i) = app.redis_scan.keys.iter().position(|k| k.key_raw == raw) {
                    app.redis_list.select(Some(i));
                    open_redis_value(app, tx);
                    return;
                }
            }
            let n = app.redis_scan.keys.len();
            let all = app.redis_scan.all.len();
            let sel = app
                .redis_list
                .selected()
                .unwrap_or(0)
                .min(n.saturating_sub(1));
            app.redis_list.select((n > 0).then_some(sel));
            app.status = if !app.redis_filter.is_empty() {
                tf(
                    "过滤「{}」· {} 命中 / {} 个 key",
                    &[&(app.redis_filter), &(n), &(all)],
                )
            } else if app.redis_scan.exhausted {
                tf("{} 个 key · 已全部加载", &[&(n)])
            } else {
                tf("{} 个 key · 已加载 {} · n 加载更多", &[&(total), &(n)])
            };
        }
        OpResult::RedisMemPartial { gen, key_raw, mem } => {
            // Drop a partial from a sample the user already replaced.
            let (done, total) = match app.redis_mem_probe.as_mut() {
                Some(p) if p.gen == gen => {
                    p.done += 1;
                    (p.done, p.total)
                }
                _ => return,
            };
            app.redis_mem.insert(key_raw, mem);
            app.status = tf("采样中 {}/{}", &[&done, &total]);
        }
        OpResult::RedisMemDone { gen, truncated } => {
            let probe = match app.redis_mem_probe.take() {
                Some(p) if p.gen == gen => p,
                _ => return,
            };
            // A live memory ordering follows the freshly filled cache.
            if app.redis_mem_sort {
                apply_redis_filter(app);
            }
            let base = redis_mem_summary(&app.redis_mem, &app.redis_scan.keys)
                .unwrap_or_else(|| tf("采样完成 · {} 键无数据", &[&probe.total]));
            app.status = if truncated {
                format!("{base} · {}", t("已截断（仅前 500）"))
            } else {
                base
            };
        }
        OpResult::RedisValue(view) => {
            let view = *view;
            app.col_hidden.clear();
            app.result_needle.clear();
            app.result_filter = None;
            app.clear_col_filter();
            app.clear_cell_find();
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
                &[
                    &(fix_double_encoding(&view.key_display)),
                    &(view.redis_type),
                    &(ttl),
                ],
            );
        }
        OpResult::RedisWritten {
            cmd,
            summary,
            reload_value,
            reload_list,
        } => {
            app.cmd_output
                .push(format!("redis[{}]> {cmd}", app.redis_db));
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
                Some(e) => tf(
                    "⚠ 批量完成 {}/{}：{}",
                    &[&executed, &total, &truncate_disp(&one_line(&e), 60)],
                ),
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
            // R47b: documents arrived, so the pool is live.
            mark_active_live(app);
            app.col_hidden.clear();
            app.result_needle.clear();
            app.result_filter = None;
            app.clear_col_filter();
            app.clear_cell_find();
            app.result_tabs.clear();
            app.result_tab = 0;
            app.grid_kind = GridKind::MongoDocs;
            let rows = grid.rows.len();
            app.set_grid(*grid);
            app.mongo_page = page;
            app.mongo_filter = filter.clone();
            // R82: keep the arrival order for `Ctrl-S`, and start each page in
            // that order with no stale prompt/ordering from the previous one.
            app.mongo_docs_base = docs.clone();
            app.mongo_docs = docs;
            app.mongo_size_sort = MongoSizeSort::Natural;
            app.mongo_field_prompt = None;
            app.mongo_path_prompt = None;
            app.page_state = Some(PageState {
                table: collection.clone(),
                schema: String::new(),
                table_type: None,
                page,
                page_size: MONGO_PAGE,
                total: Some(total),
                total_lower_bound: false,
                has_next,
                filter,
                order_by: None,
                keyset: None,
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
                &[
                    &(fix_double_encoding(&app.current_db())),
                    &(fix_double_encoding(&collection)),
                    &(page + 1),
                    &(rows),
                    &(total),
                ],
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
            app.status = tf(
                "{} 索引 · {} · Esc 返回",
                &[&(fix_double_encoding(&collection)), &(n)],
            );
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
        OpResult::HistoryPanel { rows, favorites } => {
            app.history_persisted = rows;
            app.rebuild_history_rows();
            app.history_favorites = favorites.into_iter().collect();
            // The panel may have been closed before the reply landed; only
            // refresh the visible state when it is still open. The needle is
            // *not* cleared here: the user may already be typing a `/` filter.
            if app.history_open {
                recompute_history_view(app);
                let n = app.history_rows.len();
                let session = app.session_runs.len();
                app.status = if n == 0 {
                    t("没有查询历史（执行一条 SQL 后再按 Alt-H）").into()
                } else if session > 0 {
                    tf("查询历史 · {} 条（含本次会话 {}）· Enter 回填 · Ctrl-↵ 直跑 · f 收藏 · Del 删除 · y/Y 复制 · / 搜索", &[&n, &session])
                } else {
                    tf("查询历史 · {} 条 · Enter 回填 · Ctrl-↵ 直跑 · f 收藏 · Del 删除 · y/Y 复制 · / 搜索", &[&n])
                };
            }
        }
        OpResult::HistoryDeleted { id, error } => {
            if let Some(e) = error {
                app.status = tf("✗ 删除历史失败: {}", &[&e]);
            } else {
                app.history_rows.retain(|r| r.id != id);
                recompute_history_view(app);
                app.status = t("✓ 已删除该条历史（数据库数据未受影响）").into();
            }
        }
        OpResult::HistoryFavorite {
            sql,
            favorited,
            error,
        } => {
            if let Some(e) = error {
                app.status = tf("✗ 收藏操作失败: {}", &[&e]);
            } else {
                if favorited {
                    app.history_favorites.insert(sql.clone());
                    app.status = t("✓ 已收藏该条 SQL（DBX saved_sql_files）").into();
                } else {
                    app.history_favorites.remove(&sql);
                    app.status = t("已取消收藏").into();
                }
                // The entry just moved between the 收藏 / 时间序 sections;
                // keep the cursor on it (R45).
                recompute_history_view_keep(app, &sql);
            }
        }
        OpResult::Snippets(items) => {
            let total = items.len();
            app.snippets = items;
            app.snippet_open = true;
            recompute_snippet_view(app);
            // A just-saved confirmation must survive the refresh that follows it.
            if !app.status.starts_with('✓') {
                app.status = if total == 0 {
                    t("暂无收藏 · 编辑器内 Ctrl-O 后按 s 或 Alt-S 收藏").into()
                } else {
                    tf(
                        "{} 个 SQL 收藏 · Enter 插入 · / 过滤 · d 删除 · s 收藏当前 · r 刷新",
                        &[&(total)],
                    )
                };
            }
        }
        OpResult::SnippetSaved(name) => {
            app.status = tf("✓ 已收藏 SQL 片段「{}」（DBX saved_sql_files）", &[&(name)]);
            // Refresh the list only when it is on screen; a save straight from
            // the editor (Alt-S) should not pop the overlay open.
            if app.snippet_open {
                if let Some(cfg) = app.selected.clone() {
                    app.spawn(tx, Op::Snippets(Box::new(cfg)));
                }
            }
        }
        OpResult::SnippetDeleted { id, error } => {
            if let Some(e) = error {
                app.status = tf("✗ 删除收藏失败: {}", &[&e]);
            } else {
                app.snippets.retain(|s| s.id != id);
                recompute_snippet_view(app);
                app.status = t("✓ 已删除该条收藏（只删本地配置，不影响数据库）").into();
            }
        }
        OpResult::SnippetRejected(msg) => {
            app.status = format!("⚠ {msg}");
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
            app.spawn(tx, Op::SidebarLayout);
        }
        OpResult::ConnCopied(cfg) => {
            // Merge the twin in place (like an import) so the browse view and
            // its tree stay put; the new root shows immediately and can be
            // expanded with `l` / `→` (R45).
            let name = cfg.name.clone();
            let id = cfg.id.clone();
            match app.connections.iter().position(|c| c.id == id) {
                Some(i) => app.connections[i] = *cfg,
                None => app.connections.push(*cfg),
            }
            sort_connection_list(&mut app.connections, app.conn_sort);
            rebuild_side_rows(app);
            if let Some(pos) = app.side_rows.iter().position(|r| {
                matches!(r, SideRow::Conn { idx, .. }
                    if side_root_cfg(app, *idx).is_some_and(|c| c.id == id))
            }) {
                app.side_sel = pos;
                side_mirror_table(app);
            }
            app.status = tf("✓ 已复制连接 {} · 树中新根 · l/→ 展开", &[&name]);
        }
        OpResult::ConnsImported {
            saved,
            skipped,
            needs_password,
            failed,
        } => {
            // Merge the saved configs in place (an overwrite keeps the same id,
            // so it replaces its old row) rather than re-listing, so the import
            // summary is not clobbered by the list status message.
            let added = saved.len();
            for cfg in saved {
                match app.connections.iter().position(|c| c.id == cfg.id) {
                    Some(i) => app.connections[i] = cfg,
                    None => app.connections.push(cfg),
                }
            }
            sort_connection_list(&mut app.connections, app.conn_sort);
            app.picker_open = app.selected.is_none();
            let n = app.connections.len();
            let sel = app
                .conn_list
                .selected()
                .unwrap_or(0)
                .min(n.saturating_sub(1));
            app.conn_list.select((n > 0).then_some(sel));
            app.status = conn_import_status(added, skipped, needs_password, &failed);
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
        OpResult::QueryProgress { .. } => {}
        // R84: handled as a side-channel above; unreachable here.
        OpResult::SessionRun(_) => {}
        OpResult::TransferProgress { .. } => {}
        OpResult::ExportDone {
            format,
            path,
            rows,
            bytes,
            elapsed_ms,
            error,
        } => {
            let label = format.label();
            let ok = error.is_none();
            // R114: only a file that actually landed is remembered; a failed or
            // cancelled export leaves the previous directory untouched.
            if ok {
                app.last_export_path = Some(path.clone());
            }
            app.status = match error {
                Some(e) => tf("✗ {} 导出失败: {}", &[&label, &e]),
                None => tf(
                    "✓ 已导出 {} · {} 行 · {} → {}",
                    &[&label, &rows, &human_size(bytes), &(path.display())],
                ),
            };
            // A slow export reports its own timing so the win is visible.
            if ok && elapsed_ms >= 250 {
                app.status = tf(
                    "✓ 已导出 {} · {} 行 · {} · {}ms → {}",
                    &[
                        &label,
                        &rows,
                        &human_size(bytes),
                        &elapsed_ms,
                        &(path.display()),
                    ],
                );
            }
        }
        OpResult::BatchExportDone {
            kind,
            path,
            tabs,
            rows,
            bytes,
            elapsed_ms,
            truncated,
            error,
        } => {
            let label = kind.label();
            app.status = match error {
                Some(e) => tf("✗ {} 导出失败: {}", &[&label, &e]),
                None => {
                    let mut line = tf(
                        "✓ 已导出 {} · {} 个 Tab · {} 行 · {} → {}",
                        &[&label, &tabs, &rows, &human_size(bytes), &(path.display())],
                    );
                    if !truncated.is_empty() {
                        line.push_str(&tf(
                            " · {} 个 Tab 已截断至 100K 行",
                            &[&truncated.len()],
                        ));
                    }
                    if elapsed_ms >= 250 {
                        line.push_str(&tf(" · {}ms", &[&elapsed_ms]));
                    }
                    line
                }
            };
        }
        OpResult::SearchDone {
            gen,
            hits,
            skipped,
            tables,
            truncated,
        } => {
            // A reply for a superseded scan (the user re-ran or the connection
            // changed) must not overwrite the current results.
            if gen != app.search_gen {
                return;
            }
            app.search_running = false;
            app.search_progress = None;
            let n = hits.len();
            app.search_hits = hits;
            app.search_skipped = skipped;
            app.search_truncated = truncated;
            if n > 0 {
                app.search_list.select(Some(0));
            } else {
                app.search_list.select(None);
            }
            let skip_n = app.search_skipped.len();
            let cap = if truncated {
                tf(" · 已达上限 {}", &[&SEARCH_MAX_HITS])
            } else {
                String::new()
            };
            let skip = if skip_n > 0 {
                tf(" · 跳过 {} 张大表", &[&skip_n])
            } else {
                String::new()
            };
            if n == 0 {
                app.status = tf(
                    "全库搜索「{}」· 无命中（扫描 {} 表{}）",
                    &[&(app.search_query), &(tables), &(skip)],
                );
            } else {
                app.status = tf(
                    "全库搜索「{}」· {} 命中{}{} · Enter 定位 · y 复制",
                    &[&(app.search_query), &(n), &(cap), &(skip)],
                );
            }
        }
        OpResult::SearchCancelled { gen } => {
            if gen != app.search_gen {
                return;
            }
            app.search_running = false;
            app.search_progress = None;
            app.status = t("已中止全库搜索（保留已扫描的部分结果）").into();
        }
        OpResult::DictReady {
            gen,
            db,
            tables,
            content,
        } => {
            // A reply for a superseded walk must not open a stale prompt.
            if gen != app.dict_gen {
                return;
            }
            app.dict_running = false;
            app.dict_progress = None;
            app.dict_db = db.clone();
            app.dict_content = Some(content);
            // Prefill the `Ctrl-Y`-style destination prompt with the default
            // filename; clearing it copies to the clipboard instead.
            let mut ta = TextArea::default();
            ta.insert_str(dict_default_filename(&db));
            ta.set_placeholder_text(t("留空 = 复制到剪贴板 · 输入路径 = 写入文件"));
            app.dict_prompt = Some(ta);
            app.status = tf("数据字典已生成（{} 表）· Enter 写入 · Esc 取消", &[&tables]);
        }
        OpResult::DictCancelled { gen } => {
            if gen != app.dict_gen {
                return;
            }
            app.dict_running = false;
            app.dict_progress = None;
            app.dict_content = None;
            app.status = t("已中止数据字典（未生成任何文件）").into();
        }
        OpResult::DiffReady { gen, diff } => {
            // A reply for a superseded request must not replace the open overlay.
            if gen != app.diff_gen {
                return;
            }
            let changed = diff.changed();
            let equal = diff.equal();
            let cross = diff.cross;
            let src = diff.src.label();
            let tgt = diff.tgt.label();
            app.diff_picker = None;
            app.db_diff = None;
            app.diff = Some(Box::new(SchemaDiffState {
                diff: *diff,
                tab: DiffTab::Columns,
                list: ListState::default(),
                scroll: 0,
                alter: String::new(),
            }));
            if let Some(state) = app.diff.as_mut() {
                if !state.diff.cols.is_empty() {
                    state.list.select(Some(0));
                }
            }
            let cross_note = if cross {
                tf(" · {}", &[&t("⚠ 跨方言")])
            } else {
                String::new()
            };
            app.status = if equal {
                tf(
                    "结构对比 {} → {} · {} · Tab 切换 · Esc 关{}",
                    &[&src, &tgt, &t("结构一致"), &cross_note],
                )
            } else {
                tf(
                    "结构对比 {} → {} · {} 处差异 · y 摘要 · g ALTER{}",
                    &[&src, &tgt, &changed, &cross_note],
                )
            };
        }
        OpResult::DataDiffDone { gen, result } => {
            if gen != app.data_diff_gen {
                return;
            }
            app.data_progress = None;
            if let Some(p) = app.diff_picker.as_mut() {
                p.comparing = false;
                p.loading = false;
            }
            app.diff_picker = None;
            let cancelled = result.cancelled;
            let truncated = result.truncated;
            let src = result.src_label.clone();
            let tgt = result.tgt_label.clone();
            let mut state = DataDiffState {
                result: *result,
                tab: DataTab::Summary,
                list: ListState::default(),
                scroll: 0,
                sync_sql: String::new(),
            };
            if data_tab_rows(&state) > 0 {
                state.list.select(Some(0));
            }
            let verdict = data_diff_status_line(&state.result);
            app.data_diff = Some(Box::new(state));
            let tail = if truncated {
                tf(" · ⚠ 已截断（仅前 {} 行）", &[&DATA_MAX_DIFF_ROWS])
            } else {
                String::new()
            };
            let stop = if cancelled {
                tf(" · {}", &[&t("已中止（保留已比结果）")])
            } else {
                String::new()
            };
            app.status = tf(
                "数据对比 {} → {} · {} · Tab 切换 · Esc 关{}{}",
                &[&src, &tgt, &verdict, &tail, &stop],
            );
        }
        OpResult::TransferNeedsConfirm { gen, estimated } => {
            if gen != app.transfer_gen {
                return;
            }
            app.transfer_progress = None;
            if let Some(w) = app.transfer.as_mut() {
                w.large_warn = Some(estimated);
                w.error = None;
                w.submitted = false;
                // On the options step, put the cursor on “start” so the
                // advertised “press Enter again” works with one key.
                if w.step == TransferStep::Options {
                    w.opt_list.select(Some(TRANSFER_OPTION_ROWS - 1));
                }
            }
            app.status = tf(
                "⚠ 源表预估至少 {} 行，再按 Enter 确认开始搬运（Esc 取消）",
                &[&estimated],
            );
        }
        OpResult::TransferDone { gen, report } => {
            if gen != app.transfer_gen {
                return;
            }
            app.transfer = None;
            app.transfer_progress = None;
            if report.moved > 0 || report.created {
                // Rows were written, or the target table was (re)created (an
                // overwrite / create-only leaves a freshly empty table), so any
                // cached COUNT(*) for it is stale.
                app.count_cache.clear();
            }
            let moved = report.moved;
            let skipped = report.skipped.len();
            let aborted = report.aborted.clone();
            let cancelled = report.cancelled;
            let created = report.created;
            let tgt = report.tgt_label.clone();
            app.status = if let Some((row, err)) = &aborted {
                tf(
                    "✗ 搬运中止于源行 {}: {} · 已搬 {} 行{}",
                    &[
                        &row,
                        &err,
                        &moved,
                        &(if created { " · 已建表" } else { "" }),
                    ],
                )
            } else if cancelled {
                tf(
                    "⚠ 搬运已中止 · 已搬 {} 行（已提交批次保留）· 目标 {}",
                    &[&moved, &tgt],
                )
            } else {
                tf(
                    "✓ 搬运完成 · 已搬 {} 行 · 跳过 {} 行 · 目标 {}{}",
                    &[
                        &moved,
                        &skipped,
                        &tgt,
                        &(if created { " · 已建表" } else { "" }),
                    ],
                )
            };
            app.transfer_report = Some(report);
        }
        OpResult::DbDiffReady { gen, diff } => {
            if gen != app.diff_gen {
                return;
            }
            let only_src = diff.count(DbTableMark::OnlySrc);
            let only_tgt = diff.count(DbTableMark::OnlyTgt);
            let both = diff.count(DbTableMark::Both);
            let src = diff.src_label.clone();
            let tgt = diff.tgt_label.clone();
            app.diff_picker = None;
            app.diff = None;
            app.db_diff = Some(Box::new(DbDiffState {
                diff: *diff,
                list: ListState::default(),
            }));
            if let Some(state) = app.db_diff.as_mut() {
                if !state.diff.entries.is_empty() {
                    state.list.select(Some(0));
                }
            }
            app.status = tf(
                "库结构对比 {} → {} · 仅源 {} · 仅目标 {} · 共有 {} · Enter 对比两库都有的表",
                &[&src, &tgt, &only_src, &only_tgt, &both],
            );
        }
        OpResult::DiffTablesFor {
            gen,
            db,
            schema,
            tables,
        } => {
            let Some(p) = app.diff_picker.as_mut() else {
                return;
            };
            if p.gen != gen || p.stage != DiffPickStage::Lists {
                return;
            }
            p.loading = false;
            p.target_db = db;
            p.target_schema = schema;
            p.entries = tables;
            p.list
                .select(if p.entries.is_empty() { None } else { Some(0) });
            let empty = p.entries.is_empty();
            let n = p.entries.len();
            app.status = if empty {
                t("目标连接里没有表").into()
            } else {
                tf("{} 张表 · Enter 对比 · c 换连接 · Esc 返回", &[&n])
            };
        }
        // Handled before the spinner accounting above; unreachable here.
        OpResult::SshPrompt(_)
        | OpResult::SshNotice(_)
        | OpResult::SearchProgress { .. }
        | OpResult::DictProgress { .. }
        | OpResult::DataDiffProgress { .. } => {}
        // R83: the temporary connection is in the kernel cache now. Only the
        // initial quick-open activates it; a cache refresh leaves the UI alone.
        OpResult::TempConnRegistered { cfg, activate } => {
            if activate {
                activate_connection(app, tx, *cfg, None, None);
            }
        }
        OpResult::TempConnUnregistered => {}
        // R99: `QueryFailed` is rewritten to `Error` at the top of this
        // function, so this arm is unreachable — it exists only so the match
        // stays exhaustive.
        OpResult::QueryFailed { .. } => {}
        OpResult::Error(e) => {
            // v0.6.27+ secret-store failures get a human hint (key missing /
            // migration pending); every other error passes through unchanged.
            let e = humanize_backend_error(&e);
            app.direct_run = false;
            app.import_progress = None;
            app.page_pending = false;
            app.pending_sel = None;
            app.pending_focus = None;
            app.pending_write = false;
            app.pending_write_msg = None;
            // R102: a failed comment write must not refresh on a later write.
            app.comment_refresh = false;
            // R103: a failed CTAS must not label / refresh a later write.
            app.materialize_write = None;
            app.pending_materialize_msg = None;
            app.search_running = false;
            app.search_progress = None;
            // R100: a failed / timed-out dictionary walk must not leave its
            // progress marker stuck with no job behind it.
            app.dict_running = false;
            app.dict_progress = None;
            app.data_progress = None;
            // A watchdog timeout or any unexpected failure of a transfer must not
            // leave its progress overlay stuck with no job behind it.
            if app.transfer.as_ref().is_some_and(|w| w.submitted) {
                app.transfer = None;
                app.transfer_progress = None;
            }
            // A failed cross-connection table fetch — or a rejected data compare
            // (no primary key) — must not leave the picker stuck on its spinner.
            if let Some(p) = app.diff_picker.as_mut() {
                p.loading = false;
                p.comparing = false;
            }
            // A failed scan must not leave the key list permanently unable to
            // load another page.
            app.redis_scan.pending = false;
            // A failed column fetch must not strand a deferred first page: load
            // it now without a primary-key plan (plain OFFSET ordering).
            if app.pending_open_page {
                app.pending_open_page = false;
                app.pending_sel.get_or_insert(0);
                spawn_table_page(app, tx, 0);
            }
            // R41: on a small terminal a long SQL execution error is compressed
            // into the compact error box (first line + line count); `Enter` shows
            // the whole message. Other failures stay on the status line.
            if app.term_h > 0
                && app.term_h <= 24
                && (e.starts_with("query:") || e.starts_with("script:"))
            {
                open_error_popup(app, &e);
            }
            let status = format!("✗ {e}");
            // R77: a failed single statement is located back in the editor too
            // (a multi-statement script lands in `Script`). When the editor no
            // longer holds what ran, `record_editor_errors` clears the highlight
            // and the plain error status stands.
            let mut located = false;
            if e.starts_with("query:") {
                if let Some(exec) = app.last_executed.clone() {
                    located = record_editor_errors(
                        app,
                        std::slice::from_ref(&exec),
                        &[(0, e.clone())],
                        &status,
                    );
                }
            }
            if located {
                apply_editor_error_status(app);
            } else {
                app.status = status;
            }
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

/// Row-count sample cap. `None` means "always run the exact `COUNT(*)`"; a value
/// means "count at most this many rows and report a lower bound past it".
/// `DBXT_COUNT_SAMPLE_LIMIT=0` (or `off`/`none`) disables the cap.
fn count_sample_limit() -> Option<u64> {
    match std::env::var("DBXT_COUNT_SAMPLE_LIMIT")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("0") | Some("off") | Some("none") | Some("") => None,
        Some(v) => Some(v.parse().unwrap_or(COUNT_SAMPLE_LIMIT_DEFAULT)),
        None => Some(COUNT_SAMPLE_LIMIT_DEFAULT),
    }
}

/// Turn a sampled `COUNT(*)` (capped at `limit + 1` rows) into `(value, lower)`:
/// within the cap the value is exact, past it we only know `value > limit`.
fn classify_sample(n: u64, limit: u64) -> (u64, bool) {
    if n > limit {
        (limit, true)
    } else {
        (n, false)
    }
}

/// Render the total for the status line / grid title, honouring the lower bound.
fn total_label(ps: &PageState) -> String {
    match ps.total {
        Some(t) if ps.total_lower_bound => tf(">{} 行", &[&t]),
        Some(t) => tf("共 {} 行", &[&t]),
        None => t("总数未知").into(),
    }
}

/// A SQL literal for one primary-key value. `None` (a NULL key) or a value that
/// cannot be rendered safely aborts the keyset path in favour of OFFSET.
fn pk_value_literal(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(if *b { "TRUE".into() } else { "FALSE".into() }),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(sql_literal(s)),
        other => Some(sql_literal(&other.to_string())),
    }
}

/// `pk > v` (single key) or `(a, b) > (va, vb)` (composite) — the keyset seek
/// predicate for a table-browser page. Row-value comparison is understood by
/// MySQL, PostgreSQL and SQLite. Returns `None` when the tuple is incomplete or
/// holds a NULL. (The data-compare feature has its own `keyset_predicate` that
/// expands to `(k1 > v1) OR (k1 = v1 AND k2 > v2) …`.)
fn table_data_keyset_predicate(
    db_type: Option<DatabaseType>,
    pk: &[String],
    values: &[serde_json::Value],
    op: &str,
) -> Option<String> {
    if pk.is_empty() || pk.len() != values.len() {
        return None;
    }
    let lits: Vec<String> = values.iter().map(pk_value_literal).collect::<Option<_>>()?;
    if pk.len() == 1 {
        Some(format!(
            "{} {} {}",
            quote_table_identifier(db_type, &pk[0]),
            op,
            lits[0]
        ))
    } else {
        let cols = pk
            .iter()
            .map(|c| quote_table_identifier(db_type, c))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("({cols}) {op} ({})", lits.join(", ")))
    }
}

/// Whether a primary-key column's type cannot round-trip through a text SQL
/// literal (binary keys fall back to OFFSET).
fn is_binary_pk_type(t: &str) -> bool {
    let t = t.to_ascii_lowercase();
    t.contains("blob") || t.contains("bytea") || t.contains("binary") || t.contains("image")
}

/// Can this view be browsed by primary-key seek? Returns the key columns and the
/// display direction when the effective order is exactly the table's primary
/// key — either the implicit default (no ORDER BY, so we impose `pk ASC`) or an
/// explicit order over precisely those columns in that order, one direction for
/// all of them. Anything else (a custom sort) keeps the classic OFFSET path.
fn keyset_plan(meta: Option<&TableMeta>, ps: &PageState) -> Option<(Vec<String>, bool)> {
    let meta = meta?;
    if meta.table != ps.table || meta.schema != ps.schema {
        return None;
    }
    let mut pk: Vec<String> = Vec::new();
    for c in &meta.columns {
        if c.is_primary_key {
            if is_binary_pk_type(&c.data_type) {
                return None;
            }
            pk.push(c.name.clone());
        }
    }
    if pk.is_empty() || pk.len() > 6 {
        return None;
    }
    let explicit = ps
        .order_by
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty());
    match explicit {
        None => Some((pk, true)),
        Some(o) => {
            let keys = parse_order_by(Some(o));
            if keys.len() != pk.len() {
                return None;
            }
            let mut desc: Option<bool> = None;
            for (i, (name, d)) in keys.iter().enumerate() {
                if !name.eq_ignore_ascii_case(&pk[i]) {
                    return None;
                }
                match desc {
                    None => desc = Some(*d),
                    Some(prev) if prev == *d => {}
                    _ => return None,
                }
            }
            Some((pk, !desc.unwrap_or(false)))
        }
    }
}

/// Pick the seek for `target`, given the page we are on and the cursor of the
/// current page. Only the immediately adjacent pages can use keyset; a jump of
/// any other size stays on OFFSET (there is nowhere to seek from).
fn keyset_seek_for(
    plan: Option<&(Vec<String>, bool)>,
    cur: Option<&KeysetCursor>,
    from_page: usize,
    target: usize,
) -> PageSeek {
    let (Some((pk, asc)), Some(cur)) = (plan, cur) else {
        return PageSeek::Offset;
    };
    if cur.pk != *pk || cur.ascending != *asc {
        return PageSeek::Offset;
    }
    if target == from_page + 1 && !cur.last.is_empty() {
        PageSeek::After(cur.last.clone())
    } else if from_page == target + 1 && !cur.first.is_empty() {
        PageSeek::Before(cur.first.clone())
    } else {
        PageSeek::Offset
    }
}

/// Human-readable `· 过滤: … · 排序: …` suffix for the status line and grid title.
fn page_state_extra(ps: &PageState) -> String {
    let mut s = String::new();
    if !ps.filter.trim().is_empty() {
        s.push_str(&tf(
            " · 过滤: {}",
            &[&(truncate_disp(&one_line(&ps.filter), 48))],
        ));
    }
    if let Some(o) = ps.order_by.as_deref().filter(|o| !o.trim().is_empty()) {
        s.push_str(&tf(" · 排序: {}", &[&(truncate_disp(o, 32))]));
    }
    s
}

fn columns_grid(cols: &[ColumnInfo]) -> Grid {
    let columns = [
        t("字段"),
        t("类型"),
        t("键"),
        t("可空"),
        t("默认值"),
        t("注释"),
    ]
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
        types: Vec::new(),
        rows,
        note: tf("{} 字段", &[&(cols.len())]),
    }
}
