use crate::prelude::*;
use crate::*;

// ─── schema diff (Alt-D) ─────────────────────────────────────────────────────
//
// Compares the structure of two tables (or the table lists of two databases)
// using the same `get_columns` / `list_indexes` / `list_tables` metadata the
// sidebar already fetches. The diff itself is pure data: the async op only
// fills the two sides and everything below is unit-testable without a backend.

/// How a diff row differs, always relative to the **target** side — the side the
/// generated `ALTER` rewrites to match the source.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffMark {
    /// The source has it, the target does not → `ALTER … ADD`.
    Add,
    /// The target has it, the source does not → `ALTER … DROP`.
    Drop,
    /// Present on both sides but with different attributes → `ALTER … MODIFY`.
    Modify,
    /// Identical on both sides (rendered dimmed for context).
    Same,
}

impl DiffMark {
    pub(crate) fn sign(self) -> &'static str {
        match self {
            DiffMark::Add => "+",
            DiffMark::Drop => "-",
            DiffMark::Modify => "~",
            DiffMark::Same => " ",
        }
    }
    pub(crate) fn color(self) -> Color {
        match self {
            DiffMark::Add => Color::Green,
            DiffMark::Drop => Color::Red,
            DiffMark::Modify => Color::Yellow,
            DiffMark::Same => Color::DarkGray,
        }
    }
}

/// One side of a schema comparison, reduced to what the diff needs.
#[derive(Clone, Debug)]
pub(crate) struct DiffSide {
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) db_type: DatabaseType,
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) indexes: Vec<IndexInfo>,
}

impl DiffSide {
    /// `db.schema.table` for the overlay title and the copied summary.
    pub(crate) fn label(&self) -> String {
        let rel = qualified_display(&self.schema, &self.table);
        let db = if self.db.trim().is_empty() {
            String::new()
        } else {
            format!("{}.", fix_double_encoding(&self.db))
        };
        format!("{}{}", db, fix_double_encoding(&rel))
    }
}

/// One column's place in the diff.
#[derive(Clone, Debug)]
pub(crate) struct ColDiffRow {
    pub(crate) name: String,
    pub(crate) mark: DiffMark,
    /// Source-side rendering (`type NOT NULL DEFAULT …`).
    pub(crate) src: String,
    /// Target-side rendering.
    pub(crate) tgt: String,
    /// Attribute summary for a `~` row (and the whole row in the narrow layout).
    pub(crate) detail: String,
}

/// Minimal index shape kept alongside a diff row so the `ALTER` generator can
/// emit `CREATE` / `DROP INDEX` without re-fetching metadata.
#[derive(Clone, Debug, Default)]
pub(crate) struct IndexShape {
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
    pub(crate) is_unique: bool,
    pub(crate) is_primary: bool,
}

/// One index's place in the diff.
#[derive(Clone, Debug)]
pub(crate) struct IndexDiffRow {
    pub(crate) mark: DiffMark,
    /// Target rendering.
    pub(crate) tgt: String,
    pub(crate) detail: String,
    pub(crate) src_shape: Option<IndexShape>,
    pub(crate) tgt_shape: Option<IndexShape>,
}

/// A full two-table comparison. `cols` / `idx` include `Same` rows so the view
/// shows the whole structure with the differences highlighted.
#[derive(Clone, Debug)]
pub(crate) struct TableDiff {
    pub(crate) src: DiffSide,
    pub(crate) tgt: DiffSide,
    pub(crate) cols: Vec<ColDiffRow>,
    pub(crate) idx: Vec<IndexDiffRow>,
    /// The two sides use different dialects (type mapping applies).
    pub(crate) cross: bool,
}

impl TableDiff {
    /// True when no column or index differs.
    pub(crate) fn equal(&self) -> bool {
        self.cols.iter().all(|c| c.mark == DiffMark::Same)
            && self.idx.iter().all(|i| i.mark == DiffMark::Same)
    }
    /// Number of changed rows (columns + indexes), for the title badge.
    pub(crate) fn changed(&self) -> usize {
        self.cols
            .iter()
            .filter(|c| c.mark != DiffMark::Same)
            .count()
            + self.idx.iter().filter(|i| i.mark != DiffMark::Same).count()
    }
}

/// Verdict of comparing two declared types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TypeVerdict {
    Same,
    /// Both sides map to canonical types and they differ.
    Diff,
    /// At least one side is not in the mapping table (shown with `?`).
    Unknown,
}

/// What a database-level diff found for one table name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DbTableMark {
    /// Only in the source database.
    OnlySrc,
    /// Only in the target database.
    OnlyTgt,
    /// Present in both (and openable as a table diff).
    Both,
}

#[derive(Clone, Debug)]
pub(crate) struct DbDiffEntry {
    pub(crate) table: String,
    pub(crate) mark: DbTableMark,
}

/// A database-to-database table-list comparison.
#[derive(Clone, Debug)]
pub(crate) struct DbDiff {
    pub(crate) src_label: String,
    pub(crate) tgt_label: String,
    pub(crate) entries: Vec<DbDiffEntry>,
    pub(crate) src_db: String,
    pub(crate) tgt_db: String,
    pub(crate) src_schema: String,
    pub(crate) tgt_schema: String,
}

impl DbDiff {
    pub(crate) fn count(&self, mark: DbTableMark) -> usize {
        self.entries.iter().filter(|e| e.mark == mark).count()
    }
}

/// Which tab of the diff overlay is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffTab {
    Columns,
    Indexes,
    Alter,
}

impl DiffTab {
    pub(crate) fn next(self) -> Self {
        match self {
            DiffTab::Columns => DiffTab::Indexes,
            DiffTab::Indexes => DiffTab::Alter,
            DiffTab::Alter => DiffTab::Columns,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            DiffTab::Columns => "列",
            DiffTab::Indexes => "索引",
            DiffTab::Alter => "ALTER",
        }
    }
}

/// The open table-diff overlay.
#[derive(Clone, Debug)]
pub(crate) struct SchemaDiffState {
    pub(crate) diff: TableDiff,
    pub(crate) tab: DiffTab,
    pub(crate) list: ListState,
    /// Scroll offset for the wrapped `ALTER` preview.
    pub(crate) scroll: u16,
    /// Generated sync SQL, filled lazily by `g`.
    pub(crate) alter: String,
}

/// The database-level diff overlay (table lists of two databases).
#[derive(Clone, Debug)]
pub(crate) struct DbDiffState {
    pub(crate) diff: DbDiff,
    pub(crate) list: ListState,
}

/// Which list the `Alt-D` target picker is browsing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffPickMode {
    Table,
    Database,
}

/// What the picker compares: table *structure* (R32 `Alt-D`) or table *data*
/// (`Alt-K`). `m` toggles it inside the picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffKind {
    Schema,
    Data,
}

/// Which step of the picker is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffPickStage {
    /// Browsing the target's tables / databases.
    Lists,
    /// Browsing other connections, to diff across connections (cross-dialect).
    Connections,
}

/// The `Alt-D` / `Alt-K` target picker (source is the focused table / current
/// database).
#[derive(Clone, Debug)]
pub(crate) struct DiffPicker {
    pub(crate) mode: DiffPickMode,
    /// Structure or data compare; `m` toggles it.
    pub(crate) kind: DiffKind,
    pub(crate) stage: DiffPickStage,
    pub(crate) list: ListState,
    pub(crate) entries: Vec<String>,
    /// The target connection when diffing across connections (`None` = the
    /// source connection itself).
    pub(crate) target_conn: Option<Box<ConnectionConfig>>,
    /// The target database / schema on `target_conn`.
    pub(crate) target_db: String,
    pub(crate) target_schema: String,
    /// A cross-connection table-list fetch is in flight.
    pub(crate) loading: bool,
    /// A data compare is running in the background; Esc aborts it.
    pub(crate) comparing: bool,
    /// The optional `WHERE` predicate applied to both sides of a data compare.
    pub(crate) where_input: String,
    /// Monotonic id of the latest target table fetch; a stale reply is dropped.
    pub(crate) gen: u64,
    /// The same-connection table list, restored when backing out of the
    /// connection step.
    pub(crate) src_entries: Vec<String>,
}

// ─── data compare (Alt-K) ────────────────────────────────────────────────────
//
// Compares the *rows* of two tables aligned by primary key. Everything below
// the async op is pure data, so key alignment, chunk classification, cell
// comparison and sync-SQL generation are unit-testable without a backend.

/// Which tab of the data-diff overlay is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DataTab {
    Summary,
    OnlySrc,
    OnlyTgt,
    Diff,
    /// Generated `INSERT` / `UPDATE` / `DELETE`, reached by `g` (Tab skips it).
    Sync,
}

impl DataTab {
    /// The Tab cycle: the four data views; `Sync` is reached with `g`.
    pub(crate) fn next(self) -> Self {
        match self {
            DataTab::Summary => DataTab::OnlySrc,
            DataTab::OnlySrc => DataTab::OnlyTgt,
            DataTab::OnlyTgt => DataTab::Diff,
            DataTab::Diff | DataTab::Sync => DataTab::Summary,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            DataTab::Summary => "汇总",
            DataTab::OnlySrc => "仅源",
            DataTab::OnlyTgt => "仅目标",
            DataTab::Diff => "差异",
            DataTab::Sync => "同步 SQL",
        }
    }
}

/// Which side a difference row sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowMark {
    /// `<` — the key exists only in the source.
    OnlySrc,
    /// `>` — the key exists only in the target.
    OnlyTgt,
    /// `≠` — the key exists on both sides but the rows differ.
    Diff,
}

impl RowMark {
    pub(crate) fn sign(self) -> &'static str {
        match self {
            RowMark::OnlySrc => "<",
            RowMark::OnlyTgt => ">",
            RowMark::Diff => "≠",
        }
    }
    pub(crate) fn color(self) -> Color {
        match self {
            RowMark::OnlySrc => Color::Green,
            RowMark::OnlyTgt => Color::Red,
            RowMark::Diff => Color::Yellow,
        }
    }
}

/// How a primary-key cell is compared during the ordered merge join.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PkCmp {
    Numeric,
    Text,
}

/// One aligned column pair (source name/type ↔ target name/type).
#[derive(Clone, Debug)]
pub(crate) struct DataCol {
    /// Display name (the source spelling).
    pub(crate) name: String,
    pub(crate) src_name: String,
    pub(crate) tgt_name: String,
    pub(crate) src_type: String,
    pub(crate) tgt_type: String,
    /// Canonical signature when the two sides' types map to the same family and
    /// the comparison is cross-dialect; value normalisation keys off it.
    pub(crate) canon: Option<String>,
    /// At least one side's type is outside the mapping table (shown with `?`).
    pub(crate) unknown_type: bool,
}

/// The aligned-column plan for a data compare: primary-key columns first, then
/// the remaining name-intersection columns in source order.
#[derive(Clone, Debug)]
pub(crate) struct DataAlign {
    pub(crate) cols: Vec<DataCol>,
    pub(crate) pk_len: usize,
    pub(crate) cross: bool,
}

impl DataAlign {
    pub(crate) fn pk(&self) -> &[DataCol] {
        &self.cols[..self.pk_len]
    }
    pub(crate) fn src_select(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.src_name.clone()).collect()
    }
    pub(crate) fn tgt_select(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.tgt_name.clone()).collect()
    }
    pub(crate) fn src_types(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.src_type.clone()).collect()
    }
    pub(crate) fn tgt_types(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.tgt_type.clone()).collect()
    }
    pub(crate) fn src_pk_names(&self) -> Vec<String> {
        self.pk().iter().map(|c| c.src_name.clone()).collect()
    }
    pub(crate) fn tgt_pk_names(&self) -> Vec<String> {
        self.pk().iter().map(|c| c.tgt_name.clone()).collect()
    }
}

/// A cell-level difference inside a `≠` row.
#[derive(Clone, Debug)]
pub(crate) struct DataCellDiff {
    pub(crate) col: String,
    /// Index into [`DataAlign::cols`], so the sync generator can find the
    /// target name / type without a second lookup.
    pub(crate) idx: usize,
    pub(crate) src_val: Val,
    pub(crate) tgt_val: Val,
    pub(crate) unknown_type: bool,
}

/// One retained difference row.
#[derive(Clone, Debug)]
pub(crate) struct DataDiffRow {
    pub(crate) mark: RowMark,
    /// Primary-key display (`1, 42`).
    pub(crate) key: String,
    /// Raw primary-key values, for the generated `WHERE`.
    pub(crate) pk_vals: Vec<Val>,
    /// `≠` only: the columns whose values differ.
    pub(crate) cells: Vec<DataCellDiff>,
    /// `<` / `>` only: the whole row in aligned column order.
    pub(crate) vals: Vec<Val>,
}

/// The result of a two-table data compare.
#[derive(Clone, Debug)]
pub(crate) struct DataCompare {
    pub(crate) src_label: String,
    pub(crate) tgt_label: String,
    pub(crate) src_db_type: DatabaseType,
    pub(crate) tgt_schema: String,
    pub(crate) tgt_table: String,
    pub(crate) tgt_db_type: DatabaseType,
    /// `COUNT(*)` forecasts (a hint for scale, not part of the compare).
    pub(crate) src_count: Option<u64>,
    pub(crate) tgt_count: Option<u64>,
    pub(crate) filter: String,
    pub(crate) align: DataAlign,
    pub(crate) rows: Vec<DataDiffRow>,
    pub(crate) only_src: usize,
    pub(crate) only_tgt: usize,
    pub(crate) differing: usize,
    /// Rows inspected (both sides): the `已比 N 行` half of the R90 status
    /// summary.
    pub(crate) compared: usize,
    /// No primary key on either side: rows were aligned by position (R90).
    pub(crate) positional: bool,
    /// The row cap was hit; `rows` holds only the first `DATA_MAX_DIFF_ROWS`.
    pub(crate) truncated: bool,
    /// The user aborted the compare; the rows found so far are kept.
    pub(crate) cancelled: bool,
}

impl DataCompare {
    pub(crate) fn cross(&self) -> bool {
        self.align.cross
    }
    pub(crate) fn equal(&self) -> bool {
        self.rows.is_empty()
    }
    pub(crate) fn changed(&self) -> usize {
        self.only_src + self.only_tgt + self.differing
    }
}

/// The open data-diff overlay.
#[derive(Clone, Debug)]
pub(crate) struct DataDiffState {
    pub(crate) result: DataCompare,
    pub(crate) tab: DataTab,
    pub(crate) list: ListState,
    /// Scroll offset for the wrapped sync-SQL preview.
    pub(crate) scroll: u16,
    /// Generated sync SQL, filled lazily by `g`.
    pub(crate) sync_sql: String,
}

// ─── type normalisation (cross-dialect) ──────────────────────────────────────

/// Collapse runs of whitespace to single spaces.
pub(crate) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split `varchar(255)` into (`varchar`, `(255)`); `int` into (`int`, ``).
pub(crate) fn split_type_params(s: &str) -> (String, String) {
    match s.find('(') {
        Some(i) => (s[..i].trim().to_string(), s[i..].trim().to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

/// Normalise a declared type for a **same-dialect** comparison: lowercase,
/// collapse whitespace and drop the cosmetic MySQL integer display width
/// (`int(11)` == `int`).
pub(crate) fn norm_type_text(raw: &str) -> String {
    let s = collapse_ws(&raw.trim().to_ascii_lowercase());
    let (base, params) = split_type_params(&s);
    if matches!(
        base.as_str(),
        "int"
            | "integer"
            | "bigint"
            | "smallint"
            | "tinyint"
            | "mediumint"
            | "int4"
            | "int8"
            | "int2"
    ) {
        return base;
    }
    let params = params.replace(' ', "");
    if params.is_empty() {
        base
    } else {
        format!("{base}{params}")
    }
}

/// Canonical, dialect-neutral signature of a SQL type, used only for
/// **cross-dialect** comparison: `varchar(255)`, `character varying(255)` →
/// `varchar(255)`; `int`, `integer`, `int4` → `int`. `None` for a base type
/// outside the common table (the diff then shows `?` and both raw spellings).
pub(crate) fn canonical_type(raw: &str) -> Option<String> {
    let s = collapse_ws(&raw.trim().to_ascii_lowercase());
    let (head, params) = split_type_params(&s);
    let params = params.replace(' ', "");
    let mut words = head.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    let (family, extra): (&str, &str) = match first {
        "varchar" | "nvarchar" | "varchar2" => ("varchar", ""),
        "char" | "nchar" => ("char", ""),
        "character" => {
            if rest.first() == Some(&"varying") {
                ("varchar", "")
            } else {
                ("char", "")
            }
        }
        "int" | "integer" | "int4" | "mediumint" | "int32" => ("int", ""),
        "bigint" | "int8" | "int64" => ("bigint", ""),
        "smallint" | "int2" | "int16" => ("smallint", ""),
        "tinyint" => ("tinyint", ""),
        "text" | "clob" | "longtext" | "mediumtext" | "tinytext" | "string" => ("text", ""),
        "bool" | "boolean" => ("boolean", ""),
        "decimal" | "numeric" | "number" => ("decimal", ""),
        "float" | "real" | "float4" => ("float", ""),
        "double" => ("double", ""),
        "timestamp" | "datetime" => (
            "timestamp",
            if rest.first() == Some(&"with") {
                " tz"
            } else {
                ""
            },
        ),
        "timestamptz" => ("timestamp", " tz"),
        "date" => ("date", ""),
        "time" => ("time", ""),
        "json" | "jsonb" => ("json", ""),
        "blob" | "bytea" | "varbinary" | "binary" | "longblob" | "mediumblob" | "tinyblob" => {
            ("blob", "")
        }
        _ => return None,
    };
    // `unsigned` / `zerofill` is a real difference, so it must stay in the
    // signature rather than being flattened away.
    let mut sig = String::from(family);
    sig.push_str(extra);
    if rest.contains(&"unsigned") {
        sig.push_str(" unsigned");
    }
    if rest.contains(&"zerofill") {
        sig.push_str(" zerofill");
    }
    if !params.is_empty() {
        sig.push_str(&params);
    }
    Some(sig)
}

/// Compare two declared types. Same-dialect comparisons only normalise
/// whitespace/display width; cross-dialect comparisons go through
/// [`canonical_type`] so `varchar(255)` and `character varying(255)` match.
pub(crate) fn compare_types(src: &str, tgt: &str, cross: bool) -> TypeVerdict {
    if !cross {
        return if norm_type_text(src) == norm_type_text(tgt) {
            TypeVerdict::Same
        } else {
            TypeVerdict::Diff
        };
    }
    match (canonical_type(src), canonical_type(tgt)) {
        (Some(a), Some(b)) => {
            if a == b {
                TypeVerdict::Same
            } else {
                TypeVerdict::Diff
            }
        }
        // An unmapped base type on both sides is only "unknown" when the raw
        // spellings differ; an identical verbatim type is still equal.
        _ => {
            if norm_type_text(src) == norm_type_text(tgt) {
                TypeVerdict::Same
            } else {
                TypeVerdict::Unknown
            }
        }
    }
}

/// Map a source type spelling onto the target dialect, for a generated `ALTER`.
/// Returns `None` when the type is outside the common table.
pub(crate) fn map_type_to_dialect(raw: &str, target: DatabaseType) -> Option<String> {
    let canon = canonical_type(raw)?;
    let (head, params) = split_type_params(&canon);
    let params = params.replace(' ', "");
    let mut words = head.split_whitespace();
    let family = words.next().unwrap_or("");
    let rest: Vec<&str> = words.collect();
    let unsigned = rest.contains(&"unsigned");
    let pg = is_postgres_family(target.as_str());
    let mysql = is_mysql_family(target.as_str());
    let mut suffix = "";
    if unsigned {
        suffix = " unsigned";
    }
    let mapped = match family {
        "varchar" => {
            if pg {
                format!("character varying{params}")
            } else {
                format!("varchar{params}")
            }
        }
        "char" => {
            if pg {
                format!("character{params}")
            } else {
                format!("char{params}")
            }
        }
        "int" => {
            if pg {
                "integer".to_string()
            } else {
                format!("int{suffix}")
            }
        }
        "bigint" => format!("bigint{suffix}"),
        "smallint" => format!("smallint{suffix}"),
        "tinyint" => {
            if pg {
                "smallint".to_string()
            } else {
                "tinyint".to_string()
            }
        }
        "text" => "text".to_string(),
        "boolean" => {
            if mysql {
                "tinyint(1)".to_string()
            } else {
                "boolean".to_string()
            }
        }
        "decimal" => {
            if pg {
                format!("numeric{params}")
            } else {
                format!("decimal{params}")
            }
        }
        "float" => {
            if pg {
                "real".to_string()
            } else {
                "float".to_string()
            }
        }
        "double" => {
            if pg {
                "double precision".to_string()
            } else {
                "double".to_string()
            }
        }
        "timestamp" => {
            if rest.contains(&"tz") {
                if mysql {
                    "timestamp".to_string()
                } else {
                    "timestamp with time zone".to_string()
                }
            } else if mysql {
                "datetime".to_string()
            } else {
                "timestamp".to_string()
            }
        }
        "date" => "date".to_string(),
        "time" => "time".to_string(),
        "json" => {
            if pg {
                "jsonb".to_string()
            } else {
                "json".to_string()
            }
        }
        "blob" => {
            if pg {
                "bytea".to_string()
            } else {
                "blob".to_string()
            }
        }
        _ => return None,
    };
    Some(mapped)
}

/// `Y` / `N` marker used in the copied summary and the `~` detail lines.
pub(crate) fn yn(b: bool) -> &'static str {
    if b {
        "Y"
    } else {
        "N"
    }
}

/// Normalise an optional string for equality (trimmed; `None` == empty).
pub(crate) fn norm_opt(v: &Option<String>) -> String {
    v.as_deref().map(str::trim).unwrap_or("").to_string()
}

/// Display an optional string, `-` when absent/blank.
pub(crate) fn opt_or_dash(v: &Option<String>) -> String {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => "-".to_string(),
    }
}

/// Find a column by exact name, falling back to a case-insensitive match.
pub(crate) fn find_column<'a>(cols: &'a [ColumnInfo], name: &str) -> Option<&'a ColumnInfo> {
    cols.iter()
        .find(|c| c.name == name)
        .or_else(|| cols.iter().find(|c| c.name.eq_ignore_ascii_case(name)))
}

/// Render a column's attributes as one line (`varchar(20) NOT NULL DEFAULT ''`).
pub(crate) fn render_column_attrs(c: &ColumnInfo) -> String {
    let mut s = c.data_type.clone();
    s.push_str(if c.is_nullable { " NULL" } else { " NOT NULL" });
    if let Some(d) = c
        .column_default
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        s.push_str(&format!(" DEFAULT {d}"));
    }
    if let Some(cs) = c
        .character_set
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        s.push_str(&format!(" {cs}"));
    }
    if let Some(col) = c
        .collation
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        s.push_str(&format!(" {col}"));
    }
    if let Some(cm) = c
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        s.push_str(&format!(" COMMENT {cm}"));
    }
    s
}

/// The attribute deltas between two columns of the same name. Empty means the
/// columns are identical. Tokens are deliberately language-neutral so the same
/// text can be pasted into a ticket in either language.
pub(crate) fn column_changes(src: &ColumnInfo, tgt: &ColumnInfo, cross: bool) -> Vec<String> {
    let mut out = Vec::new();
    match compare_types(&src.data_type, &tgt.data_type, cross) {
        TypeVerdict::Same => {}
        TypeVerdict::Diff => out.push(format!("type {}→{}", src.data_type, tgt.data_type)),
        TypeVerdict::Unknown => out.push(format!("type ? {}→{}", src.data_type, tgt.data_type)),
    }
    if src.is_nullable != tgt.is_nullable {
        out.push(format!(
            "null {}→{}",
            yn(src.is_nullable),
            yn(tgt.is_nullable)
        ));
    }
    if norm_opt(&src.column_default) != norm_opt(&tgt.column_default) {
        out.push(format!(
            "default {}→{}",
            opt_or_dash(&src.column_default),
            opt_or_dash(&tgt.column_default)
        ));
    }
    if norm_opt(&src.comment) != norm_opt(&tgt.comment) {
        out.push(format!(
            "comment {}→{}",
            opt_or_dash(&src.comment),
            opt_or_dash(&tgt.comment)
        ));
    }
    if norm_opt(&src.character_set) != norm_opt(&tgt.character_set) {
        out.push(format!(
            "charset {}→{}",
            opt_or_dash(&src.character_set),
            opt_or_dash(&tgt.character_set)
        ));
    }
    if norm_opt(&src.collation) != norm_opt(&tgt.collation) {
        out.push(format!(
            "collation {}→{}",
            opt_or_dash(&src.collation),
            opt_or_dash(&tgt.collation)
        ));
    }
    if norm_opt(&src.extra) != norm_opt(&tgt.extra) {
        out.push(format!(
            "extra {}→{}",
            opt_or_dash(&src.extra),
            opt_or_dash(&tgt.extra)
        ));
    }
    if src.is_primary_key != tgt.is_primary_key {
        out.push(format!(
            "pk {}→{}",
            yn(src.is_primary_key),
            yn(tgt.is_primary_key)
        ));
    }
    if src.is_unique != tgt.is_unique {
        out.push(format!(
            "unique {}→{}",
            yn(src.is_unique),
            yn(tgt.is_unique)
        ));
    }
    out
}

/// Canonical key an index is matched on: primary indexes collapse to one name
/// (`PRIMARY`), everything else is compared case-insensitively by name.
pub(crate) fn index_key(ix: &IndexInfo) -> String {
    if ix.is_primary {
        "primary".to_string()
    } else {
        ix.name.to_ascii_lowercase()
    }
}

/// Human-readable index signature (`UNIQUE (a, b) WHERE …`).
pub(crate) fn index_signature(ix: &IndexInfo) -> String {
    let mut s = String::new();
    if ix.is_primary {
        s.push_str("PRIMARY ");
    } else if ix.is_unique {
        s.push_str("UNIQUE ");
    }
    s.push('(');
    s.push_str(&ix.columns.join(", "));
    s.push(')');
    if let Some(f) = ix
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        s.push_str(&format!(" WHERE {f}"));
    }
    s
}

pub(crate) fn index_shape(ix: &IndexInfo) -> IndexShape {
    IndexShape {
        name: ix.name.clone(),
        columns: ix.columns.clone(),
        is_unique: ix.is_unique,
        is_primary: ix.is_primary,
    }
}

/// Build the full column + index diff between two sides.
pub(crate) fn build_table_diff(src: DiffSide, tgt: DiffSide) -> TableDiff {
    let cross = src.db_type != tgt.db_type;

    let mut cols: Vec<ColDiffRow> = Vec::new();
    let mut used_tgt = vec![false; tgt.columns.len()];
    for sc in &src.columns {
        match tgt
            .columns
            .iter()
            .position(|c| c.name == sc.name || c.name.eq_ignore_ascii_case(&sc.name))
        {
            Some(ti) => {
                used_tgt[ti] = true;
                let tc = &tgt.columns[ti];
                let changes = column_changes(sc, tc, cross);
                let mark = if changes.is_empty() {
                    DiffMark::Same
                } else {
                    DiffMark::Modify
                };
                cols.push(ColDiffRow {
                    name: sc.name.clone(),
                    mark,
                    src: render_column_attrs(sc),
                    tgt: render_column_attrs(tc),
                    detail: changes.join(" · "),
                });
            }
            None => {
                let attrs = render_column_attrs(sc);
                cols.push(ColDiffRow {
                    name: sc.name.clone(),
                    mark: DiffMark::Add,
                    src: attrs.clone(),
                    tgt: String::new(),
                    detail: attrs,
                });
            }
        }
    }
    for (ti, tc) in tgt.columns.iter().enumerate() {
        if used_tgt[ti] {
            continue;
        }
        let attrs = render_column_attrs(tc);
        cols.push(ColDiffRow {
            name: tc.name.clone(),
            mark: DiffMark::Drop,
            src: String::new(),
            tgt: attrs.clone(),
            detail: attrs,
        });
    }

    let mut idx: Vec<IndexDiffRow> = Vec::new();
    let mut used_tgt_ix = vec![false; tgt.indexes.len()];
    for si in &src.indexes {
        let key = index_key(si);
        match tgt.indexes.iter().position(|i| index_key(i) == key) {
            Some(ti) => {
                used_tgt_ix[ti] = true;
                let ti_ix = &tgt.indexes[ti];
                let ss = index_signature(si);
                let ts = index_signature(ti_ix);
                let mark = if ss == ts {
                    DiffMark::Same
                } else {
                    DiffMark::Modify
                };
                let detail = if mark == DiffMark::Same {
                    ss.clone()
                } else {
                    format!("{} → {}", ss, ts)
                };
                idx.push(IndexDiffRow {
                    mark,
                    tgt: ts,
                    detail,
                    src_shape: Some(index_shape(si)),
                    tgt_shape: Some(index_shape(ti_ix)),
                });
            }
            None => {
                let ss = index_signature(si);
                idx.push(IndexDiffRow {
                    mark: DiffMark::Add,
                    tgt: String::new(),
                    detail: ss,
                    src_shape: Some(index_shape(si)),
                    tgt_shape: None,
                });
            }
        }
    }
    for (ti, ix) in tgt.indexes.iter().enumerate() {
        if used_tgt_ix[ti] {
            continue;
        }
        let ts = index_signature(ix);
        idx.push(IndexDiffRow {
            mark: DiffMark::Drop,
            tgt: ts.clone(),
            detail: ts,
            src_shape: None,
            tgt_shape: Some(index_shape(ix)),
        });
    }

    TableDiff {
        src,
        tgt,
        cols,
        idx,
        cross,
    }
}

/// Coerce a column default onto the target dialect. A cross-dialect default may
/// carry a PostgreSQL `::type` cast (`''::character varying`) that MySQL would
/// reject, so the cast is dropped.
pub(crate) fn render_default(d: &str, tgt_dt: DatabaseType, cross: bool) -> String {
    let d = d.trim();
    if cross && is_mysql_family(tgt_dt.as_str()) {
        if let Some((base, _cast)) = d.split_once("::") {
            return base.trim().to_string();
        }
    }
    d.to_string()
}

/// Render a column definition in the **target** dialect for a generated
/// `ALTER`. A cross-dialect type is mapped when possible, otherwise the source
/// spelling is kept so the statement stays inspectable.
pub(crate) fn render_column_def(c: &ColumnInfo, tgt_dt: DatabaseType, cross: bool) -> String {
    let mut parts = vec![quote_table_identifier(Some(tgt_dt), &c.name)];
    let ty = if cross {
        map_type_to_dialect(&c.data_type, tgt_dt).unwrap_or_else(|| c.data_type.clone())
    } else {
        c.data_type.clone()
    };
    parts.push(ty);
    let mysql = is_mysql_family(tgt_dt.as_str());
    if mysql {
        if let Some(cs) = c
            .character_set
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(format!("CHARACTER SET {cs}"));
        }
        if let Some(col) = c
            .collation
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(format!("COLLATE {col}"));
        }
    }
    if !c.is_nullable {
        parts.push("NOT NULL".to_string());
    }
    if let Some(d) = c
        .column_default
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        parts.push(format!("DEFAULT {}", render_default(d, tgt_dt, cross)));
    }
    if mysql {
        if let Some(extra) = c.extra.as_deref() {
            if extra.to_ascii_lowercase().contains("auto_increment") {
                parts.push("AUTO_INCREMENT".to_string());
            }
        }
        if let Some(cm) = c
            .comment
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(format!("COMMENT {}", sql_literal(cm)));
        }
    }
    parts.join(" ")
}

/// Quote an identifier list for `CREATE INDEX … (a, b)`.
pub(crate) fn quoted_cols(cols: &[String], dt: DatabaseType) -> String {
    cols.iter()
        .map(|c| quote_table_identifier(Some(dt), c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate the `ALTER` script that rewrites the **target** to match the source.
/// Never executed by dbxt: the user copies it into another client or the editor.
pub(crate) fn generate_alter(diff: &TableDiff) -> String {
    let tgt_dt = diff.tgt.db_type;
    let pg = is_postgres_family(tgt_dt.as_str());
    let mysql = is_mysql_family(tgt_dt.as_str());
    let table = table_ref(tgt_dt, &diff.tgt.schema, &diff.tgt.table);
    let mut out = String::new();
    out.push_str(&format!(
        "-- {}\n",
        t("-- dbxt 结构对比（源 → 目标，对目标执行）")
    ));
    out.push_str(&format!("-- source: {}\n", diff.src.label()));
    out.push_str(&format!(
        "-- target: {} ({})\n",
        diff.tgt.label(),
        tgt_dt.as_str()
    ));
    if diff.cross {
        out.push_str(&format!(
            "-- {}\n",
            t("⚠ 跨方言：类型按常见映射转换，映射不了的请人工确认")
        ));
    }
    out.push('\n');

    let mut any = false;
    for row in &diff.cols {
        let q = quote_table_identifier(Some(tgt_dt), &row.name);
        match row.mark {
            DiffMark::Add => {
                let Some(sc) = find_column(&diff.src.columns, &row.name) else {
                    continue;
                };
                let def = render_column_def(sc, tgt_dt, diff.cross);
                out.push_str(&format!("ALTER TABLE {table} ADD COLUMN {def};\n"));
                if pg {
                    if let Some(cm) = sc
                        .comment
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        out.push_str(&format!(
                            "COMMENT ON COLUMN {table}.{q} IS {};\n",
                            sql_literal(cm)
                        ));
                    }
                }
                any = true;
            }
            DiffMark::Drop => {
                out.push_str(&format!("{}\n", t("-- ⚠ DROP COLUMN 会丢弃目标列的数据")));
                out.push_str(&format!("ALTER TABLE {table} DROP COLUMN {q};\n"));
                any = true;
            }
            DiffMark::Modify => {
                let (Some(sc), Some(tc)) = (
                    find_column(&diff.src.columns, &row.name),
                    find_column(&diff.tgt.columns, &row.name),
                ) else {
                    continue;
                };
                if mysql {
                    let def = render_column_def(sc, tgt_dt, diff.cross);
                    out.push_str(&format!("ALTER TABLE {table} MODIFY COLUMN {def};\n"));
                } else {
                    if compare_types(&sc.data_type, &tc.data_type, diff.cross) != TypeVerdict::Same
                    {
                        let ty = if diff.cross {
                            map_type_to_dialect(&sc.data_type, tgt_dt)
                        } else {
                            Some(sc.data_type.clone())
                        };
                        match ty {
                            Some(ty) => out.push_str(&format!(
                                "ALTER TABLE {table} ALTER COLUMN {q} TYPE {ty};\n"
                            )),
                            None => out.push_str(&format!(
                                "{}\n",
                                tf(
                                    "-- TODO 类型需人工确认: {q} {} → {}",
                                    &[&sc.data_type, &tc.data_type]
                                )
                            )),
                        }
                    }
                    if sc.is_nullable != tc.is_nullable {
                        if sc.is_nullable {
                            out.push_str(&format!(
                                "ALTER TABLE {table} ALTER COLUMN {q} DROP NOT NULL;\n"
                            ));
                        } else {
                            out.push_str(&format!(
                                "ALTER TABLE {table} ALTER COLUMN {q} SET NOT NULL;\n"
                            ));
                        }
                    }
                    if norm_opt(&sc.column_default) != norm_opt(&tc.column_default) {
                        match sc
                            .column_default
                            .as_deref()
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                        {
                            Some(d) => out.push_str(&format!(
                                "ALTER TABLE {table} ALTER COLUMN {q} SET DEFAULT {d};\n"
                            )),
                            None => out.push_str(&format!(
                                "ALTER TABLE {table} ALTER COLUMN {q} DROP DEFAULT;\n"
                            )),
                        }
                    }
                    if pg && norm_opt(&sc.comment) != norm_opt(&tc.comment) {
                        match sc
                            .comment
                            .as_deref()
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                        {
                            Some(cm) => out.push_str(&format!(
                                "COMMENT ON COLUMN {table}.{q} IS {};\n",
                                sql_literal(cm)
                            )),
                            None => {
                                out.push_str(&format!("COMMENT ON COLUMN {table}.{q} IS NULL;\n"))
                            }
                        }
                    }
                }
                any = true;
            }
            DiffMark::Same => {}
        }
    }

    for row in &diff.idx {
        match row.mark {
            DiffMark::Add => {
                let Some(sh) = &row.src_shape else { continue };
                if sh.is_primary {
                    out.push_str(&format!("{}\n", t("-- TODO 目标缺少主键，请手工添加")));
                } else {
                    out.push_str(&format!(
                        "CREATE {}INDEX {} ON {table} ({});\n",
                        if sh.is_unique { "UNIQUE " } else { "" },
                        quote_table_identifier(Some(tgt_dt), &sh.name),
                        quoted_cols(&sh.columns, tgt_dt)
                    ));
                }
                any = true;
            }
            DiffMark::Drop => {
                let Some(th) = &row.tgt_shape else { continue };
                if th.is_primary {
                    out.push_str(&format!("{}\n", t("-- TODO 目标主键多余，请手工删除")));
                } else if mysql {
                    out.push_str(&format!(
                        "DROP INDEX {} ON {table};\n",
                        quote_table_identifier(Some(tgt_dt), &th.name)
                    ));
                } else {
                    let qualified = if diff.tgt.schema.trim().is_empty() {
                        quote_table_identifier(Some(tgt_dt), &th.name)
                    } else {
                        format!(
                            "{}.{}",
                            quote_table_identifier(Some(tgt_dt), &diff.tgt.schema),
                            quote_table_identifier(Some(tgt_dt), &th.name)
                        )
                    };
                    out.push_str(&format!("DROP INDEX {qualified};\n"));
                }
                any = true;
            }
            DiffMark::Modify => {
                if let Some(th) = &row.tgt_shape {
                    if !th.is_primary {
                        if mysql {
                            out.push_str(&format!(
                                "DROP INDEX {} ON {table};\n",
                                quote_table_identifier(Some(tgt_dt), &th.name)
                            ));
                        } else {
                            out.push_str(&format!(
                                "DROP INDEX {};\n",
                                quote_table_identifier(Some(tgt_dt), &th.name)
                            ));
                        }
                    }
                }
                if let Some(sh) = &row.src_shape {
                    if !sh.is_primary {
                        out.push_str(&format!(
                            "CREATE {}INDEX {} ON {table} ({});\n",
                            if sh.is_unique { "UNIQUE " } else { "" },
                            quote_table_identifier(Some(tgt_dt), &sh.name),
                            quoted_cols(&sh.columns, tgt_dt)
                        ));
                    }
                }
                any = true;
            }
            DiffMark::Same => {}
        }
    }

    if !any {
        out.push_str(&format!("-- {}\n", t("结构一致，无需同步")));
    }
    out
}

/// Plain-text summary of the differences, for `y` (paste into a ticket).
pub(crate) fn diff_summary_text(diff: &TableDiff) -> String {
    let mut out = String::new();
    out.push_str(&format!("-- {}\n", t("-- dbxt 结构对比")));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("源"),
        diff.src.label(),
        diff.src.db_type.as_str()
    ));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("目标"),
        diff.tgt.label(),
        diff.tgt.db_type.as_str()
    ));
    out.push_str(&format!("-- {}\n", t("方向：源 → 目标")));
    if diff.equal() {
        out.push_str(&format!("-- {}\n", t("结构一致，无差异")));
        return out;
    }
    out.push('\n');
    for row in diff.cols.iter().filter(|r| r.mark != DiffMark::Same) {
        let text = if row.detail.is_empty() {
            row.src.clone()
        } else {
            row.detail.clone()
        };
        out.push_str(&format!(
            "{} {} {}  {}\n",
            row.mark.sign(),
            t("列"),
            row.name,
            text
        ));
    }
    for row in diff.idx.iter().filter(|r| r.mark != DiffMark::Same) {
        out.push_str(&format!(
            "{} {}  {}\n",
            row.mark.sign(),
            t("索引"),
            row.detail
        ));
    }
    out
}

// ─── data compare pure logic ─────────────────────────────────────────────────

/// Case-insensitive key for matching a column by name across two tables.
pub(crate) fn col_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Primary-key columns in index order: prefer the primary index (its column
/// order is the one `ORDER BY` must follow), falling back to the
/// `is_primary_key` flags in table-column order.
pub(crate) fn pk_from_metadata(columns: &[ColumnInfo], indexes: &[IndexInfo]) -> Vec<String> {
    if let Some(ix) = indexes
        .iter()
        .find(|i| i.is_primary && !i.columns.is_empty())
    {
        return ix.columns.clone();
    }
    columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.clone())
        .collect()
}

pub(crate) fn make_data_col(sc: &ColumnInfo, tc: &ColumnInfo, cross: bool) -> DataCol {
    let canon = if cross && canonical_type(&sc.data_type) == canonical_type(&tc.data_type) {
        canonical_type(&sc.data_type)
    } else {
        None
    };
    let unknown_type = cross
        && (canonical_type(&sc.data_type).is_none() || canonical_type(&tc.data_type).is_none());
    DataCol {
        name: sc.name.clone(),
        src_name: sc.name.clone(),
        tgt_name: tc.name.clone(),
        src_type: sc.data_type.clone(),
        tgt_type: tc.data_type.clone(),
        canon,
        unknown_type,
    }
}

/// Find a column by (trimmed, case-insensitive) name.
pub(crate) fn find_col<'a>(cols: &'a [ColumnInfo], name: &str) -> Option<&'a ColumnInfo> {
    cols.iter().find(|c| col_key(&c.name) == col_key(name))
}

/// Build the aligned-column plan. Rejects a pair that cannot be aligned by
/// primary key (the reason is surfaced to the user verbatim).
pub(crate) fn build_data_align(
    src_cols: &[ColumnInfo],
    tgt_cols: &[ColumnInfo],
    src_pk: &[String],
    tgt_pk: &[String],
    cross: bool,
) -> Result<DataAlign, String> {
    if src_pk.is_empty() && tgt_pk.is_empty() {
        return Err(t("数据对比需要主键：两表都没有主键，无法按行对齐").into());
    }
    if src_pk.is_empty() {
        return Err(t("数据对比需要主键：源表没有主键，无法按行对齐").into());
    }
    if tgt_pk.is_empty() {
        return Err(t("数据对比需要主键：目标表没有主键，无法按行对齐").into());
    }
    if src_pk.len() != tgt_pk.len() {
        return Err(tf(
            "主键列数不一致（源 {} / 目标 {}），无法对齐",
            &[&src_pk.len(), &tgt_pk.len()],
        ));
    }
    let mut used: HashSet<String> = HashSet::new();
    let mut cols: Vec<DataCol> = Vec::new();
    for sk in src_pk {
        let sc = find_col(src_cols, sk).ok_or_else(|| tf("源表主键列 {} 不在表结构中", &[sk]))?;
        let tc =
            find_col(tgt_cols, sk).ok_or_else(|| tf("目标表缺少主键列 {}，无法对齐", &[sk]))?;
        used.insert(col_key(&tc.name));
        cols.push(make_data_col(sc, tc, cross));
    }
    for sc in src_cols {
        if src_pk.iter().any(|k| col_key(k) == col_key(&sc.name)) {
            continue;
        }
        let Some(tc) = find_col(tgt_cols, &sc.name) else {
            continue;
        };
        if used.contains(&col_key(&tc.name)) {
            continue;
        }
        used.insert(col_key(&tc.name));
        cols.push(make_data_col(sc, tc, cross));
    }
    if cols.is_empty() {
        return Err(t("两表没有可对齐的列（列名交集为空）").into());
    }
    Ok(DataAlign {
        cols,
        pk_len: src_pk.len(),
        cross,
    })
}

/// Build the row-order aligned plan for two tables that have **no** primary
/// key (R90): the name-intersection columns in source order, `pk_len = 0`.
/// Every column is compared and rows are paired by position, so the caller
/// must bound both sides with the same `LIMIT`.
pub(crate) fn build_positional_align(
    src_cols: &[ColumnInfo],
    tgt_cols: &[ColumnInfo],
    cross: bool,
) -> Result<DataAlign, String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut cols: Vec<DataCol> = Vec::new();
    for sc in src_cols {
        let Some(tc) = find_col(tgt_cols, &sc.name) else {
            continue;
        };
        if !used.insert(col_key(&tc.name)) {
            continue;
        }
        cols.push(make_data_col(sc, tc, cross));
    }
    if cols.is_empty() {
        return Err(t("两表没有可对齐的列（列名交集为空）").into());
    }
    Ok(DataAlign {
        cols,
        pk_len: 0,
        cross,
    })
}

/// Canonical boolean for a value spelling, or `None` when it is not boolean-ish.
pub(crate) fn norm_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "f" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

/// Order two numeric spellings exactly. Integers are compared as `i128` first,
/// so a `bigint` primary key beyond 2^53 — a snowflake id, say — is never
/// collapsed to “equal” by the `/f64` rounding a decimal/float fallback would
/// apply. Only a value that is not an integer (or overflows `i128`) falls back
/// to `f64` and finally to a plain string comparison.
pub(crate) fn cmp_numeric_text(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.trim(), b.trim());
    if let (Ok(x), Ok(y)) = (a.parse::<i128>(), b.parse::<i128>()) {
        return x.cmp(&y);
    }
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    }
}

/// Compare two text values under a shared canonical type, so `1` == `1.0` in a
/// numeric family and `true` == `1` for a boolean. Falls back to an exact
/// comparison when a value does not parse for the family.
pub(crate) fn canon_cell_equal(a: &str, b: &str, canon: &str) -> bool {
    if a == b {
        return true;
    }
    let family = canon.split(['(', ' ']).next().unwrap_or(canon);
    match family {
        "boolean" => match (norm_bool(a), norm_bool(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
        "int" | "bigint" | "smallint" | "tinyint" | "decimal" | "float" | "double" => {
            cmp_numeric_text(a, b) == std::cmp::Ordering::Equal
        }
        "timestamp" => a.replace('T', " ") == b.replace('T', " "),
        _ => a == b,
    }
}

/// Whether two cells are equal. NULL is distinct from the empty string; a
/// cross-dialect pair only normalises when both sides share a canonical type.
pub(crate) fn values_equal(a: &Val, b: &Val, canon: Option<&str>) -> bool {
    match (a, b) {
        (Val::Null, Val::Null) => true,
        (Val::Null, _) | (_, Val::Null) => false,
        (Val::Text(x), Val::Text(y)) => match canon {
            Some(c) => canon_cell_equal(x, y, c),
            None => x == y,
        },
    }
}

/// The comparison mode for a primary-key column, from its canonical family.
pub(crate) fn pk_cmp_mode(col: &DataCol) -> PkCmp {
    let ty = col.canon.clone().unwrap_or_else(|| col.src_type.clone());
    let family = canonical_type(&ty).unwrap_or_else(|| norm_type_text(&ty));
    let base = family.split(['(', ' ']).next().unwrap_or("");
    if matches!(
        base,
        "int" | "bigint" | "smallint" | "tinyint" | "decimal" | "float" | "double"
    ) {
        PkCmp::Numeric
    } else {
        PkCmp::Text
    }
}

/// Order two primary-key rows the way `ORDER BY pk` does, so the chunked merge
/// join advances the correct side.
pub(crate) fn cmp_pk_row(a: &[Val], b: &[Val], modes: &[PkCmp]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let n = modes.len().min(a.len()).min(b.len());
    for i in 0..n {
        let ord = match (&a[i], &b[i]) {
            (Val::Null, Val::Null) => Ordering::Equal,
            (Val::Null, _) => Ordering::Less,
            (_, Val::Null) => Ordering::Greater,
            (Val::Text(x), Val::Text(y)) => match modes[i] {
                PkCmp::Numeric => cmp_numeric_text(x, y),
                PkCmp::Text => x.cmp(y),
            },
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Primary key as `a, b` for the row list.
pub(crate) fn pk_display(align: &DataAlign, row: &[Val]) -> String {
    (0..align.pk_len)
        .map(|i| {
            let v = row.get(i).cloned().unwrap_or(Val::Null);
            value_display(&v).0
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Classify a pair of matched rows; `None` when they are identical.
pub(crate) fn compare_data_row(
    align: &DataAlign,
    src_row: &[Val],
    tgt_row: &[Val],
) -> Option<DataDiffRow> {
    let mut cells = Vec::new();
    for (i, col) in align.cols.iter().enumerate() {
        let sv = src_row.get(i).cloned().unwrap_or(Val::Null);
        let tv = tgt_row.get(i).cloned().unwrap_or(Val::Null);
        if !values_equal(&sv, &tv, col.canon.as_deref()) {
            cells.push(DataCellDiff {
                col: col.name.clone(),
                idx: i,
                src_val: sv,
                tgt_val: tv,
                unknown_type: col.unknown_type,
            });
        }
    }
    if cells.is_empty() {
        return None;
    }
    Some(DataDiffRow {
        mark: RowMark::Diff,
        key: pk_display(align, src_row),
        pk_vals: src_row[..align.pk_len.min(src_row.len())].to_vec(),
        cells,
        vals: Vec::new(),
    })
}

pub(crate) fn only_data_row(align: &DataAlign, row: &[Val], mark: RowMark) -> DataDiffRow {
    DataDiffRow {
        mark,
        key: pk_display(align, row),
        pk_vals: row[..align.pk_len.min(row.len())].to_vec(),
        cells: Vec::new(),
        vals: row.to_vec(),
    }
}

/// The `#N` label (1-based) a row-order compare uses in place of a primary key.
pub(crate) fn positional_key(pos: usize) -> String {
    format!("#{}", pos + 1)
}

/// Classify two rows paired by position in a row-order compare. The `#N` key
/// replaces the (absent) primary key so the detail popup and the lists still
/// name the row.
pub(crate) fn compare_positional_row(
    align: &DataAlign,
    pos: usize,
    src_row: &[Val],
    tgt_row: &[Val],
) -> Option<DataDiffRow> {
    let mut row = compare_data_row(align, src_row, tgt_row)?;
    row.key = positional_key(pos);
    Some(row)
}

/// A `<` / `>` row for a row-order compare, keyed `#N`.
pub(crate) fn positional_only_row(
    align: &DataAlign,
    pos: usize,
    row: &[Val],
    mark: RowMark,
) -> DataDiffRow {
    let mut r = only_data_row(align, row, mark);
    r.key = positional_key(pos);
    r
}

/// The decision for the current merge frontier of the chunked compare.
#[derive(Debug)]
pub(crate) enum MergeStep {
    /// The source row has no target match; advance the source.
    SrcOnly,
    /// The target row has no source match; advance the target.
    TgtOnly,
    /// Both sides present the same key; advance both. `Some` carries the row's
    /// column-level difference (`None` when the rows are identical).
    Both(Option<DataDiffRow>),
    /// Both sides are exhausted.
    Done,
}

/// Decide the next merge step from the two streams' front rows. Pure, so the
/// chunked state machine can be unit-tested without a backend.
pub(crate) fn merge_next(
    align: &DataAlign,
    modes: &[PkCmp],
    src: Option<&[Val]>,
    tgt: Option<&[Val]>,
) -> MergeStep {
    match (src, tgt) {
        (None, None) => MergeStep::Done,
        (Some(_), None) => MergeStep::SrcOnly,
        (None, Some(_)) => MergeStep::TgtOnly,
        (Some(s), Some(t)) => match cmp_pk_row(s, t, modes) {
            std::cmp::Ordering::Less => MergeStep::SrcOnly,
            std::cmp::Ordering::Greater => MergeStep::TgtOnly,
            std::cmp::Ordering::Equal => MergeStep::Both(compare_data_row(align, s, t)),
        },
    }
}

/// A SQL literal for one cell, dialect-aware (a binary cell → hex literal).
pub(crate) fn data_val_literal(v: &Val, data_type: Option<&str>, db_type: DatabaseType) -> String {
    // Delegate to the shared insert-literal rules so a value is escaped the same
    // way everywhere: binary columns become hex literals and PostgreSQL array
    // cells (which arrive as JSON) become `ARRAY[…]`. The data-compare sync SQL
    // and the transfer both used to fall back to a plain quoted string here,
    // which produced invalid SQL for a `bytea` / `text[]` column.
    insert_literal(v, data_type, Some(db_type.as_str()))
}

/// `(k1 > v1) OR (k1 = v1 AND k2 > v2) OR …` — portable keyset pagination that
/// avoids `OFFSET` (which is O(offset) on a large table).
pub(crate) fn keyset_predicate(
    names: &[String],
    types: &[String],
    last: &[Val],
    db_type: DatabaseType,
) -> String {
    let mut clauses = Vec::new();
    for i in 0..names.len().min(last.len()) {
        let mut parts: Vec<String> = Vec::new();
        for j in 0..i {
            parts.push(format!(
                "{} = {}",
                quote_table_identifier(Some(db_type), &names[j]),
                data_val_literal(&last[j], types.get(j).map(String::as_str), db_type)
            ));
        }
        parts.push(format!(
            "{} > {}",
            quote_table_identifier(Some(db_type), &names[i]),
            data_val_literal(&last[i], types.get(i).map(String::as_str), db_type)
        ));
        clauses.push(format!("({})", parts.join(" AND ")));
    }
    format!("({})", clauses.join(" OR "))
}

/// A chunked, primary-key-ordered `SELECT`. `select_cols` / `pk_names` are in
/// aligned order (primary key first), so the two sides' rows line up positionally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_data_select(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    select_cols: &[String],
    pk_names: &[String],
    pk_types: &[String],
    filter: &str,
    last: Option<&[Val]>,
    limit: usize,
) -> String {
    let cols = select_cols
        .iter()
        .map(|c| quote_table_identifier(Some(db_type), c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {cols} FROM {}", table_ref(db_type, schema, table));
    let mut conds: Vec<String> = Vec::new();
    let filter = filter.trim();
    if !filter.is_empty() {
        conds.push(format!("({filter})"));
    }
    if let Some(last) = last {
        conds.push(keyset_predicate(pk_names, pk_types, last, db_type));
    }
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    let order = pk_names
        .iter()
        .map(|c| quote_table_identifier(Some(db_type), c))
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(&format!(" ORDER BY {order} LIMIT {limit}"));
    sql
}

/// A plain `SELECT … LIMIT n` used by the row-order (no-primary-key) compare.
/// The engine's natural order defines the row sequence; the compare is
/// positional, so the caller caps both sides at the same `limit`.
pub(crate) fn build_positional_select(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    select_cols: &[String],
    filter: &str,
    limit: usize,
) -> String {
    let cols = select_cols
        .iter()
        .map(|c| quote_table_identifier(Some(db_type), c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {cols} FROM {}", table_ref(db_type, schema, table));
    let filter = filter.trim();
    if !filter.is_empty() {
        sql.push_str(&format!(" WHERE ({filter})"));
    }
    sql.push_str(&format!(" LIMIT {limit}"));
    sql
}

/// Chunks a `COUNT(*)` forecast, rounded up (0 rows → 0 chunks).
pub(crate) fn chunk_ceil(rows: Option<u64>) -> usize {
    match rows {
        Some(0) => 0,
        Some(n) => (n as usize).div_ceil(DATA_CHUNK),
        None => 0,
    }
}

/// Estimated total chunk count across both sides (0 when a count is unknown).
pub(crate) fn chunk_total(src: Option<u64>, tgt: Option<u64>) -> usize {
    chunk_ceil(src) + chunk_ceil(tgt)
}

/// The `WHERE` that pins one difference row on the target, from its key values.
pub(crate) fn sync_pk_where(align: &DataAlign, row: &[Val], dt: DatabaseType) -> String {
    align
        .pk()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let v = row.get(i).cloned().unwrap_or(Val::Null);
            format!(
                "{} = {}",
                quote_table_identifier(Some(dt), &col.tgt_name),
                data_val_literal(&v, Some(&col.tgt_type), dt)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Generate the sync script that rewrites the **target** to match the source.
/// Never executed by dbxt: values are escaped for the target dialect and column
/// names use the target spelling.
pub(crate) fn generate_data_sync(cmp: &DataCompare) -> String {
    let dt = cmp.tgt_db_type;
    let table = table_ref(dt, &cmp.tgt_schema, &cmp.tgt_table);
    let align = &cmp.align;
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        t("-- dbxt 数据对比（源 → 目标，对目标执行）")
    ));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("源"),
        cmp.src_label,
        cmp.src_db_type.as_str()
    ));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("目标"),
        cmp.tgt_label,
        cmp.tgt_db_type.as_str()
    ));
    out.push_str(&format!("-- {}\n", t("方向：源 → 目标（只生成不执行）")));
    if cmp.positional {
        out.push_str(&format!(
            "-- {}\n",
            t("行序对齐（无主键）不生成同步语句：缺少唯一定位键")
        ));
        return out;
    }
    if cmp.cross() {
        out.push_str(&format!(
            "-- {}\n",
            t("⚠ 跨方言：值按目标方言转义，请先核对再执行")
        ));
    }
    if cmp.truncated {
        out.push_str(&format!(
            "-- {}\n",
            tf(
                "⚠ 差异行已截断（仅前 {} 行）；请缩小范围，或导出两侧后离线比对",
                &[&DATA_MAX_DIFF_ROWS]
            )
        ));
    }
    out.push('\n');
    if cmp.equal() {
        out.push_str(&format!("-- {}\n", t("数据一致，无需同步")));
        return out;
    }
    for row in &cmp.rows {
        match row.mark {
            RowMark::OnlySrc => {
                let names = align
                    .cols
                    .iter()
                    .map(|c| quote_table_identifier(Some(dt), &c.tgt_name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let vals = align
                    .cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let v = row.vals.get(i).cloned().unwrap_or(Val::Null);
                        data_val_literal(&v, Some(&c.tgt_type), dt)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("INSERT INTO {table} ({names}) VALUES ({vals});\n"));
            }
            RowMark::OnlyTgt => {
                out.push_str(&format!(
                    "DELETE FROM {table} WHERE {};\n",
                    sync_pk_where(align, &row.pk_vals, dt)
                ));
            }
            RowMark::Diff => {
                let sets = row
                    .cells
                    .iter()
                    .map(|c| {
                        let col = &align.cols[c.idx];
                        format!(
                            "{} = {}",
                            quote_table_identifier(Some(dt), &col.tgt_name),
                            data_val_literal(&c.src_val, Some(&col.tgt_type), dt)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "UPDATE {table} SET {sets} WHERE {};\n",
                    sync_pk_where(align, &row.pk_vals, dt)
                ));
            }
        }
    }
    out
}

/// Plain-text summary for `y` (ticket-friendly).
pub(crate) fn data_diff_summary_text(cmp: &DataCompare) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", t("-- dbxt 数据对比")));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("源"),
        cmp.src_label,
        cmp.src_db_type.as_str()
    ));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("目标"),
        cmp.tgt_label,
        cmp.tgt_db_type.as_str()
    ));
    if !cmp.filter.trim().is_empty() {
        out.push_str(&format!("-- {}: {}\n", t("过滤"), cmp.filter));
    }
    out.push_str(&format!("-- {}\n", t("方向：源 → 目标")));
    let count = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    out.push_str(&format!(
        "-- {}: {} {} / {} {}\n",
        t("行数"),
        t("源"),
        count(cmp.src_count),
        t("目标"),
        count(cmp.tgt_count)
    ));
    out.push_str(&format!(
        "-- {} {} / {} {} / {} {}\n",
        t("仅源"),
        cmp.only_src,
        t("仅目标"),
        cmp.only_tgt,
        t("差异"),
        cmp.differing
    ));
    if cmp.truncated {
        out.push_str(&format!(
            "-- {}\n",
            tf("差异行已截断（仅前 {} 行）", &[&DATA_MAX_DIFF_ROWS])
        ));
    }
    if cmp.cancelled {
        out.push_str(&format!("-- {}\n", t("已中止（仅比了部分）")));
    }
    if cmp.equal() {
        out.push_str(&format!("-- {}\n", t("数据一致，无差异")));
        return out;
    }
    out.push('\n');
    for row in &cmp.rows {
        match row.mark {
            RowMark::OnlySrc => {
                out.push_str(&format!("{:<2} {}  {}\n", "<", t("仅源"), row.key));
            }
            RowMark::OnlyTgt => {
                out.push_str(&format!("{:<2} {}  {}\n", ">", t("仅目标"), row.key));
            }
            RowMark::Diff => {
                let cols = row
                    .cells
                    .iter()
                    .map(|c| c.col.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "{:<2} {}  {}  [{}]\n",
                    "≠",
                    t("差异"),
                    row.key,
                    cols
                ));
            }
        }
    }
    out
}

/// R90: the one-line compare verdict shown in the status bar / overlay header:
/// `差异 12 行 / 已比 500 行` (bilingual). `compared` counts the rows inspected
/// on both sides, so it is the scale the user actually paid for.
pub(crate) fn data_diff_status_line(cmp: &DataCompare) -> String {
    if cmp.equal() {
        return tf("差异 0 行 / 已比 {} 行", &[&cmp.compared]);
    }
    tf("差异 {} 行 / 已比 {} 行", &[&cmp.changed(), &cmp.compared])
}

/// R90 `Y` / `Ctrl-E`: the retained difference rows as CSV, one line per
/// differing cell (`mark,key,column,source,target`). A `<` / `>` row lists
/// every aligned column (the missing side is empty); a `≠` row lists only the
/// columns that differ. Fields are RFC 4180 quoted, so a value with a comma, a
/// quote or a newline survives the round trip.
pub(crate) fn data_diff_csv(cmp: &DataCompare) -> String {
    let mut out = String::from("mark,key,column,source,target\n");
    for row in &cmp.rows {
        match row.mark {
            RowMark::Diff => {
                for cell in &row.cells {
                    out.push_str(&csv_diff_line(
                        row.mark.sign(),
                        &row.key,
                        &cell.col,
                        &value_display(&cell.src_val).0,
                        &value_display(&cell.tgt_val).0,
                    ));
                }
            }
            RowMark::OnlySrc | RowMark::OnlyTgt => {
                for (i, col) in cmp.align.cols.iter().enumerate() {
                    let v = row.vals.get(i).cloned().unwrap_or(Val::Null);
                    let shown = value_display(&v).0;
                    let (src, tgt) = if row.mark == RowMark::OnlySrc {
                        (shown.as_str(), "")
                    } else {
                        ("", shown.as_str())
                    };
                    out.push_str(&csv_diff_line(
                        row.mark.sign(),
                        &row.key,
                        &col.name,
                        src,
                        tgt,
                    ));
                }
            }
        }
    }
    out
}

/// The number of CSV records [`data_diff_csv`] emits (header excluded). Counted
/// from the structure, not by splitting the text: a quoted value may itself
/// contain a newline.
pub(crate) fn data_diff_csv_rows(cmp: &DataCompare) -> usize {
    cmp.rows
        .iter()
        .map(|row| match row.mark {
            RowMark::Diff => row.cells.len(),
            RowMark::OnlySrc | RowMark::OnlyTgt => cmp.align.cols.len(),
        })
        .sum()
}

/// One `mark,key,column,source,target` CSV record with every field escaped.
fn csv_diff_line(mark: &str, key: &str, col: &str, src: &str, tgt: &str) -> String {
    format!(
        "{},{},{},{},{}\n",
        csv_field(mark),
        csv_field(key),
        csv_field(col),
        csv_field(src),
        csv_field(tgt)
    )
}

// ─── data transfer (Alt-T) ───────────────────────────────────────
//
// Copies one table's structure and/or rows from the focused connection to
// another SQL connection (possibly a different dialect). Like the diff
// features, everything below the async op is pure data so type mapping, the
// `CREATE TABLE` generator, the option state machine and the breakpoint report
// are unit-testable without a backend.

/// What the transfer creates on the target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferMode {
    /// Create the table (structure + optional indexes) and copy every row.
    CreateAndCopy,
    /// Create the table only; no rows are read.
    CreateOnly,
    /// Insert into an existing table (`INSERT` only, no DDL).
    Append,
}

impl TransferMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TransferMode::CreateAndCopy => "建表+搬数据",
            TransferMode::CreateOnly => "仅建表",
            TransferMode::Append => "插入已有表",
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            TransferMode::CreateAndCopy => TransferMode::CreateOnly,
            TransferMode::CreateOnly => TransferMode::Append,
            TransferMode::Append => TransferMode::CreateAndCopy,
        }
    }
}

/// What to do when the target table already exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferConflict {
    /// Stop with an error — never silently overwrite (the default).
    Stop,
    /// `DROP TABLE` first, then recreate (must pass a red confirmation).
    Drop,
}

impl TransferConflict {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TransferConflict::Stop => "报错停下",
            TransferConflict::Drop => "覆盖（先 DROP）",
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            TransferConflict::Stop => TransferConflict::Drop,
            TransferConflict::Drop => TransferConflict::Stop,
        }
    }
}

/// How a failing row is handled during the copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferOnError {
    /// Stop and report the row number (the default).
    Stop,
    /// Skip the bad row and keep going.
    Skip,
}

impl TransferOnError {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TransferOnError::Stop => "停止报行号",
            TransferOnError::Skip => "跳过继续",
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            TransferOnError::Stop => TransferOnError::Skip,
            TransferOnError::Skip => TransferOnError::Stop,
        }
    }
}

/// The wizard's three content steps plus the red overwrite confirmation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferStep {
    /// ① target connection (source is the focused table's connection).
    Connection,
    /// ② target database / schema / table name (default = same name).
    Name,
    /// ③ mode and options.
    Options,
    /// Red layer shown when overwrite was chosen; Enter applies on a commit.
    Confirm,
}

/// One aligned source → target column of the copy.
#[derive(Clone, Debug)]
pub(crate) struct TransferCol {
    pub(crate) src_name: String,
    pub(crate) tgt_name: String,
    pub(crate) src_type: String,
    /// Declared type on the target, already mapped to its dialect (used for
    /// literal escaping and shown in the wizard).
    pub(crate) tgt_type: String,
}

/// The columns a transfer writes, plus where the primary key lives inside the
/// selected source columns (so a keyset cursor can be advanced).
#[derive(Clone, Debug)]
pub(crate) struct TransferAlign {
    pub(crate) cols: Vec<TransferCol>,
    /// Positions, in `cols`, of the primary-key columns (empty when the key is
    /// not fully present → the copy falls back to OFFSET paging).
    pub(crate) pk_idx: Vec<usize>,
    /// The primary key names in key order (source spelling).
    pub(crate) pk_names: Vec<String>,
    /// The primary key types in key order.
    pub(crate) pk_types: Vec<String>,
}

impl TransferAlign {
    pub(crate) fn src_select(&self) -> Vec<String> {
        self.cols.iter().map(|c| c.src_name.clone()).collect()
    }
    pub(crate) fn keyset(&self) -> bool {
        !self.pk_idx.is_empty() && self.pk_idx.len() == self.pk_names.len()
    }
    /// The primary-key values carried by one source row (for the keyset cursor).
    pub(crate) fn pk_of(&self, row: &[Val]) -> Option<Vec<Val>> {
        if !self.keyset() {
            return None;
        }
        Some(
            self.pk_idx
                .iter()
                .map(|i| row.get(*i).cloned().unwrap_or(Val::Null))
                .collect(),
        )
    }
}

/// Build the write alignment for a transfer.
///
/// * `create` — the table is being created, so the target columns are the source
///   columns with their types mapped to the target dialect.
/// * append — the target already exists, so only columns present on **both**
///   sides are written, matched case-insensitively by name and named with the
///   target spelling.
pub(crate) fn build_transfer_align(
    src_cols: &[ColumnInfo],
    tgt_cols: &[ColumnInfo],
    src_pk: &[String],
    src_dt: DatabaseType,
    tgt_dt: DatabaseType,
    create: bool,
) -> Result<TransferAlign, String> {
    let cross = src_dt != tgt_dt;
    let mut cols: Vec<TransferCol> = Vec::new();
    if create {
        for sc in src_cols {
            let tgt_type = transfer_target_type(sc, tgt_dt, cross, true);
            cols.push(TransferCol {
                src_name: sc.name.clone(),
                tgt_name: sc.name.clone(),
                src_type: sc.data_type.clone(),
                tgt_type,
            });
        }
    } else {
        for sc in src_cols {
            let Some(tc) = find_column(tgt_cols, &sc.name) else {
                continue;
            };
            let tgt_type = if cross {
                tc.data_type.clone()
            } else {
                sc.data_type.clone()
            };
            cols.push(TransferCol {
                src_name: sc.name.clone(),
                tgt_name: tc.name.clone(),
                src_type: sc.data_type.clone(),
                tgt_type,
            });
        }
    }
    if cols.is_empty() {
        return Err(t("没有可写入的列（源与目标没有同名列）").into());
    }
    // Locate the primary key inside the selected source columns.
    let mut pk_idx = Vec::new();
    let mut pk_names = Vec::new();
    let mut pk_types = Vec::new();
    for key in src_pk {
        if let Some(pos) = cols
            .iter()
            .position(|c| col_key(&c.src_name) == col_key(key))
        {
            pk_idx.push(pos);
            pk_names.push(cols[pos].src_name.clone());
            pk_types.push(cols[pos].src_type.clone());
        } else {
            // A key column was not selected (append alignment); disable keyset.
            pk_idx.clear();
            pk_names.clear();
            pk_types.clear();
            break;
        }
    }
    Ok(TransferAlign {
        cols,
        pk_idx,
        pk_names,
        pk_types,
    })
}

/// True for a column the server fills itself with an increasing value — MySQL
/// `AUTO_INCREMENT`, or a PostgreSQL `serial` column.
pub(crate) fn column_is_auto_increment(c: &ColumnInfo) -> bool {
    let extra = c.extra.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    if extra.contains("auto_increment") {
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

/// The declared type a source column gets on the target. A cross-dialect type is
/// mapped through the R32 table. An auto-increment column keeps its counter on a
/// **same-dialect** PostgreSQL copy (`serial` family); on a cross-dialect copy it
/// stays a plain integer, because a freshly created sequence would not know the
/// copied IDs (`AUTO_INCREMENT` on a MySQL target is still emitted by
/// [`transfer_column_def`]).
pub(crate) fn transfer_target_type(
    c: &ColumnInfo,
    tgt_dt: DatabaseType,
    cross: bool,
    with_auto_increment: bool,
) -> String {
    let src = c.data_type.trim();
    if with_auto_increment
        && !cross
        && column_is_auto_increment(c)
        && is_postgres_family(tgt_dt.as_str())
    {
        return match base_type(src).as_str() {
            "bigint" | "int8" | "int64" => "bigserial".to_string(),
            "smallint" | "int2" | "int16" => "smallserial".to_string(),
            _ => "serial".to_string(),
        };
    }
    if cross {
        map_type_to_dialect(src, tgt_dt).unwrap_or_else(|| src.to_string())
    } else {
        src.to_string()
    }
}

/// One column definition inside a generated `CREATE TABLE`.
pub(crate) fn transfer_column_def(
    c: &ColumnInfo,
    tgt_dt: DatabaseType,
    cross: bool,
    with_auto_increment: bool,
) -> String {
    let mysql = is_mysql_family(tgt_dt.as_str());
    let auto = with_auto_increment && column_is_auto_increment(c);
    let mut parts = vec![quote_table_identifier(Some(tgt_dt), &c.name)];
    parts.push(transfer_target_type(c, tgt_dt, cross, with_auto_increment));
    if mysql {
        // Charset/collation only carry meaning for a MySQL target; skip them on
        // a cross-dialect copy (the target's database default is the sane
        // choice) but keep them when both sides are MySQL.
        if !cross {
            if let Some(cs) = c
                .character_set
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                parts.push(format!("CHARACTER SET {cs}"));
            }
            if let Some(col) = c
                .collation
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                parts.push(format!("COLLATE {col}"));
            }
        }
    }
    if !c.is_nullable {
        parts.push("NOT NULL".to_string());
    }
    if let Some(d) = c
        .column_default
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        // A server-generated default (`nextval(...)`) has no meaning once the
        // column is re-created (serial / auto-increment covers it).
        let server_default = d.to_ascii_lowercase().starts_with("nextval(");
        if !(server_default && auto) {
            parts.push(format!("DEFAULT {}", render_default(d, tgt_dt, cross)));
        }
    }
    if mysql && auto {
        parts.push("AUTO_INCREMENT".to_string());
    }
    if mysql {
        if let Some(cm) = c
            .comment
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(format!("COMMENT {}", sql_literal(cm)));
        }
    }
    parts.join(" ")
}

/// Generate the `CREATE TABLE` script (structure + primary key + optional
/// secondary indexes) that mirrors the source table on the target dialect.
/// Returns the script plus non-fatal warnings for the summary overlay.
#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_transfer_create(
    src_cols: &[ColumnInfo],
    src_indexes: &[IndexInfo],
    src_pk: &[String],
    src_dt: DatabaseType,
    tgt_dt: DatabaseType,
    tgt_schema: &str,
    tgt_table: &str,
    with_indexes: bool,
    with_auto_increment: bool,
) -> Result<(String, Vec<String>), String> {
    if src_cols.is_empty() {
        return Err(t("源表没有列，无法建表").into());
    }
    let cross = src_dt != tgt_dt;
    let pg = is_postgres_family(tgt_dt.as_str());
    let tref = table_ref(tgt_dt, tgt_schema, tgt_table);
    let mut warnings: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    let mut defs: Vec<String> = src_cols
        .iter()
        .map(|c| transfer_column_def(c, tgt_dt, cross, with_auto_increment))
        .collect();
    if !src_pk.is_empty() {
        let keys: Vec<String> = src_pk
            .iter()
            .filter(|k| src_cols.iter().any(|c| col_key(&c.name) == col_key(k)))
            .map(|k| quote_table_identifier(Some(tgt_dt), k))
            .collect();
        if !keys.is_empty() {
            defs.push(format!("PRIMARY KEY ({})", keys.join(", ")));
        }
    } else {
        warnings.push(t("源表没有主键：搬运将退回 OFFSET 分页，顺序可能不稳定").to_string());
    }
    lines.push(format!(
        "CREATE TABLE {tref} (\n  {}\n);",
        defs.join(",\n  ")
    ));

    // PostgreSQL keeps comments out of the DDL, as separate statements.
    if pg {
        for c in src_cols {
            if let Some(cm) = c
                .comment
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                lines.push(format!(
                    "COMMENT ON COLUMN {tref}.{} IS {};",
                    quote_table_identifier(Some(tgt_dt), &c.name),
                    sql_literal(cm)
                ));
            }
        }
    }

    if with_indexes {
        for ix in src_indexes {
            if ix.is_primary || ix.columns.is_empty() {
                continue;
            }
            // Skip an index whose columns did not survive the copy.
            if !ix
                .columns
                .iter()
                .all(|c| src_cols.iter().any(|sc| col_key(&sc.name) == col_key(c)))
            {
                warnings.push(tf("跳过索引 {}（引用了缺失的列）", &[&ix.name]));
                continue;
            }
            lines.push(format!(
                "CREATE {}INDEX {} ON {tref} ({});",
                if ix.is_unique { "UNIQUE " } else { "" },
                quote_table_identifier(Some(tgt_dt), &ix.name),
                quoted_cols(&ix.columns, tgt_dt)
            ));
        }
    }
    Ok((lines.join("\n"), warnings))
}

/// Strip a trailing timezone offset from a timestamp rendering so a value read
/// from PostgreSQL fits MySQL's `DATETIME`/`TIMESTAMP`. The wall-clock part is
/// kept (a full timezone conversion is deliberately out of scope; the wizard
/// warns on cross-dialect temporal columns).
pub(crate) fn strip_tz_suffix(s: &str) -> Option<String> {
    let t = s.trim();
    let cut = t.find(['+', 'Z']).or_else(|| {
        t.char_indices()
            .skip(11)
            .find(|(_, c)| *c == '-')
            .map(|(i, _)| i)
    })?;
    if cut == 0 {
        return None;
    }
    let head = t[..cut].trim().replace('T', " ");
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// True for the date/time families whose textual rendering may carry a timezone
/// offset MySQL cannot parse.
pub(crate) fn is_tz_prone_type(t: &str) -> bool {
    matches!(canonical_type(t).as_deref(), Some("timestamp tz"))
}

/// A SQL literal for one copied cell, escaped for the **target** dialect. On a
/// cross-dialect copy a timestamp read from the source is normalised when the
/// target is MySQL (offset stripped), so `2024-01-01 12:00:00+00` lands as a
/// valid `DATETIME`.
pub(crate) fn transfer_value_literal(
    v: &Val,
    src_type: &str,
    tgt_type: &str,
    tgt_dt: DatabaseType,
) -> String {
    if let Val::Text(s) = v {
        if is_mysql_family(tgt_dt.as_str())
            && is_tz_prone_type(src_type)
            && matches!(canonical_type(tgt_type).as_deref(), Some("timestamp"))
        {
            if let Some(norm) = strip_tz_suffix(s) {
                return sql_literal(&norm);
            }
        }
    }
    data_val_literal(v, Some(tgt_type), tgt_dt)
}

/// A bounded `COUNT(*)` used as the transfer's size forecast: at most
/// [`TRANSFER_COUNT_PROBE`] rows are examined, so a huge table still answers
/// quickly. `Some(TRANSFER_COUNT_PROBE)` means "at least this many".
pub(crate) fn build_transfer_count_sql(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    filter: &str,
    limit: Option<u64>,
) -> String {
    let probe = TRANSFER_COUNT_PROBE.min(limit.unwrap_or(u64::MAX)).max(1);
    let tref = table_ref(db_type, schema, table);
    let where_clause = if filter.trim().is_empty() {
        String::new()
    } else {
        format!(" WHERE ({})", filter.trim())
    };
    format!("SELECT COUNT(*) FROM (SELECT 1 FROM {tref}{where_clause} LIMIT {probe}) AS _dbxt_c")
}

/// A source `SELECT … LIMIT chunk OFFSET n` used when the table has no usable
/// primary key (the keyset path reuses [`build_data_select`]).
pub(crate) fn build_transfer_offset_select(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    cols: &[String],
    filter: &str,
    limit: usize,
    offset: u64,
) -> String {
    let select = cols
        .iter()
        .map(|c| quote_table_identifier(Some(db_type), c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {select} FROM {}", table_ref(db_type, schema, table));
    if !filter.trim().is_empty() {
        sql.push_str(&format!(" WHERE ({})", filter.trim()));
    }
    if let Some(first) = cols.first() {
        sql.push_str(&format!(
            " ORDER BY {}",
            quote_table_identifier(Some(db_type), first)
        ));
    }
    sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    sql
}

/// The completed (or aborted) transfer, shown in the summary overlay.
#[derive(Clone)]
pub(crate) struct TransferReport {
    pub(crate) src_label: String,
    pub(crate) tgt_label: String,
    pub(crate) src_db_type: DatabaseType,
    pub(crate) tgt_db_type: DatabaseType,
    pub(crate) mode: TransferMode,
    pub(crate) conflict: TransferConflict,
    pub(crate) on_error: TransferOnError,
    pub(crate) tgt_db: String,
    pub(crate) tgt_schema: String,
    pub(crate) tgt_table: String,
    /// The target connection id, so the report can offer to browse the table
    /// when it lives on the connection already open.
    pub(crate) tgt_conn_id: String,
    /// Source rows read (after WHERE / LIMIT).
    pub(crate) src_rows: u64,
    /// Rows successfully written to the target.
    pub(crate) moved: u64,
    /// `(1-based source row number, error)` for rows skipped in skip mode.
    pub(crate) skipped: Vec<(u64, String)>,
    /// The error that stopped the transfer: failing row number + message.
    pub(crate) aborted: Option<(u64, String)>,
    /// The user aborted with Esc (committed batches are kept).
    pub(crate) cancelled: bool,
    /// Source row estimate from the bounded `COUNT(*)`.
    pub(crate) estimated: Option<u64>,
    /// The structure was created (CREATE TABLE ran).
    pub(crate) created: bool,
    /// The primary-key cursor of the last committed row, for a resume.
    pub(crate) breakpoint: Option<String>,
    /// Non-fatal warnings from the `CREATE TABLE` generator.
    pub(crate) warnings: Vec<String>,
    pub(crate) elapsed_ms: u128,
    pub(crate) chunks_done: usize,
}

impl TransferReport {
    pub(crate) fn ok(&self) -> bool {
        self.aborted.is_none() && !self.cancelled
    }
    /// Rows per second, rounded.
    pub(crate) fn rate(&self) -> u64 {
        if self.elapsed_ms == 0 {
            return self.moved;
        }
        (self.moved as u128 * 1000 / self.elapsed_ms) as u64
    }
}

/// The transfer handed to the background worker.
pub(crate) struct TransferJob {
    pub(crate) src_cfg: Box<ConnectionConfig>,
    pub(crate) src_db: String,
    pub(crate) src_schema: String,
    pub(crate) src_table: String,
    pub(crate) tgt_cfg: Box<ConnectionConfig>,
    pub(crate) tgt_db: String,
    pub(crate) tgt_schema: String,
    pub(crate) tgt_table: String,
    pub(crate) mode: TransferMode,
    pub(crate) conflict: TransferConflict,
    pub(crate) on_error: TransferOnError,
    pub(crate) where_input: String,
    pub(crate) limit: Option<u64>,
    pub(crate) with_indexes: bool,
    pub(crate) with_auto_increment: bool,
    /// Set once the user confirmed a >[`TRANSFER_WARN_ROWS`] source.
    pub(crate) allow_large: bool,
    pub(crate) gen: u64,
    pub(crate) cancel: Arc<AtomicBool>,
}

/// A text field edited inside the transfer wizard's options step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferField {
    Where,
    Limit,
}

/// The modal input for the `WHERE` / `LIMIT` option (`Alt-T` step ③).
pub(crate) struct TransferPrompt {
    pub(crate) field: TransferField,
    pub(crate) input: TextArea<'static>,
}

/// Which of the three name fields (database / schema / table) has focus in the
/// wizard's name step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NameFocus {
    Db,
    Schema,
    Table,
}

impl NameFocus {
    pub(crate) fn index(self) -> usize {
        match self {
            NameFocus::Db => 0,
            NameFocus::Schema => 1,
            NameFocus::Table => 2,
        }
    }
    pub(crate) fn from_index(i: usize) -> Self {
        match i {
            1 => NameFocus::Schema,
            2 => NameFocus::Table,
            _ => NameFocus::Db,
        }
    }
}

/// The `Alt-T` data-transfer wizard: three steps plus the red overwrite layer.
pub(crate) struct TransferWizard {
    pub(crate) step: TransferStep,
    /// ① the selectable target connections (the source connection is first).
    pub(crate) conn_list: ListState,
    pub(crate) conns: Vec<ConnectionConfig>,
    /// The chosen target connection (starts as the source connection).
    pub(crate) target_conn: ConnectionConfig,
    /// ② the three name fields, plus the one currently being typed.
    pub(crate) name_focus: NameFocus,
    pub(crate) name_values: [String; 3],
    pub(crate) name_input: TextArea<'static>,
    /// ③ the option list cursor and its values.
    pub(crate) opt_list: ListState,
    pub(crate) mode: TransferMode,
    pub(crate) conflict: TransferConflict,
    pub(crate) on_error: TransferOnError,
    pub(crate) where_input: String,
    pub(crate) limit_input: String,
    pub(crate) with_indexes: bool,
    pub(crate) with_auto_increment: bool,
    /// Set after a >1M source triggered [`OpResult::TransferNeedsConfirm`].
    pub(crate) large_warn: Option<u64>,
    /// Inline error from a rejected start.
    pub(crate) error: Option<String>,
    /// The `WHERE` / `LIMIT` sub-prompt.
    pub(crate) prompt: Option<TransferPrompt>,
    /// A job has been dispatched and is running in the background; the wizard
    /// becomes a read-only progress overlay whose only action is Esc (abort).
    pub(crate) submitted: bool,
    /// Source identity captured when the wizard opened.
    pub(crate) src_conn: ConnectionConfig,
    pub(crate) src_db: String,
    pub(crate) src_schema: String,
    pub(crate) src_table: String,
}

impl TransferWizard {
    /// Persist the live input into `name_values` before a focus change.
    pub(crate) fn stash_name(&mut self) {
        self.name_values[self.name_focus.index()] = self.name_input.lines().join("\n");
    }
    pub(crate) fn focus_name(&mut self, focus: NameFocus) {
        self.stash_name();
        self.name_focus = focus;
        self.name_input = TextArea::from(vec![self.name_values[focus.index()].clone()]);
        self.name_input.move_cursor(CursorMove::End);
    }
    /// Parse the `LIMIT` option (`None` = no limit).
    pub(crate) fn limit(&self) -> Option<u64> {
        self.limit_input
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
    }
}

/// The number of selectable rows in the options step (the last row starts).
pub(crate) const TRANSFER_OPTION_ROWS: usize = 8;

/// Label + value for one option row (`Alt-T` step ③). The last row is the
/// start action.
pub(crate) fn transfer_option_rows(w: &TransferWizard) -> Vec<(String, String)> {
    let where_v = if w.where_input.trim().is_empty() {
        t("(无)").to_string()
    } else {
        w.where_input.trim().to_string()
    };
    let limit_v = match w.limit() {
        Some(n) => n.to_string(),
        None => t("(不限)").to_string(),
    };
    let yn = |b: bool| {
        if b {
            t("是").to_string()
        } else {
            t("否").to_string()
        }
    };
    vec![
        (t("搬运模式").to_string(), t(w.mode.label()).to_string()),
        (t("表已存在").to_string(), t(w.conflict.label()).to_string()),
        (t("出错处理").to_string(), t(w.on_error.label()).to_string()),
        (t("WHERE 过滤").to_string(), where_v),
        (t("LIMIT 上限").to_string(), limit_v),
        (t("带索引").to_string(), yn(w.with_indexes)),
        (t("自增值").to_string(), yn(w.with_auto_increment)),
        (t("开始搬运").to_string(), "".to_string()),
    ]
}

/// Plain-text transfer summary, for `g` (ticket / clipboard friendly).
pub(crate) fn transfer_summary_text(rep: &TransferReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", t("-- dbxt 数据搬运")));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("源"),
        rep.src_label,
        rep.src_db_type.as_str()
    ));
    out.push_str(&format!(
        "-- {}: {} ({})\n",
        t("目标"),
        rep.tgt_label,
        rep.tgt_db_type.as_str()
    ));
    let conflict = if rep.mode == TransferMode::Append {
        String::new()
    } else {
        format!(" / {}", t(rep.conflict.label()))
    };
    out.push_str(&format!(
        "-- {}: {}{} / {}\n",
        t("模式"),
        t(rep.mode.label()),
        conflict,
        t(rep.on_error.label())
    ));
    out.push_str(&format!(
        "-- {}: {} / {} / {}ms / {} {}\n",
        t("结果"),
        tf("已搬 {} 行", &[&rep.moved]),
        tf("跳过 {}", &[&rep.skipped.len()]),
        rep.elapsed_ms,
        rep.rate(),
        t("行/秒")
    ));
    out.push_str(&format!("-- {}: {}\n", t("已完成块"), rep.chunks_done));
    if let Some(bp) = &rep.breakpoint {
        out.push_str(&format!("-- {}: {bp}\n", t("断点主键")));
    }
    if rep.created {
        out.push_str(&format!("-- {}\n", t("已在目标建表")));
    }
    if rep.cancelled {
        out.push_str(&format!("-- {}\n", t("已中止（已提交批次保留）")));
    }
    if let Some((row, err)) = &rep.aborted {
        out.push_str(&format!("-- {} {}: {err}\n", t("中止于源行"), row));
    }
    for (row, err) in rep.skipped.iter().take(20) {
        out.push_str(&format!("-- {} {row}: {err}\n", t("跳过源行")));
    }
    if rep.skipped.len() > 20 {
        out.push_str(&format!(
            "-- … {} {}\n",
            t("其余跳过"),
            rep.skipped.len() - 20
        ));
    }
    for w in &rep.warnings {
        out.push_str(&format!("-- ⚠ {w}\n"));
    }
    out
}
