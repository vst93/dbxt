use crate::prelude::*;
use crate::*;

// ─── global database search (Alt-G) ───────────────────────

/// Parse a positive integer env override, falling back to `default` for a
/// missing, unparsable or non-positive value (so `DBXT_SEARCH_SCAN_LIMIT=0`
/// cannot disable the LIMIT guard).
pub(crate) fn parse_positive_usize(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

pub(crate) fn parse_positive_u64(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// The active per-table scan ceiling (env override or built-in default).
pub(crate) fn search_scan_limit() -> usize {
    parse_positive_usize(
        std::env::var("DBXT_SEARCH_SCAN_LIMIT").ok().as_deref(),
        DEFAULT_SEARCH_SCAN_LIMIT,
    )
}

/// R64: the active in-result cell-find hit ceiling (env override or default).
pub(crate) fn cell_find_limit() -> usize {
    parse_positive_usize(
        std::env::var("DBXT_CELL_FIND_LIMIT").ok().as_deref(),
        DEFAULT_CELL_FIND_LIMIT,
    )
}

/// The active table-size skip threshold (env override or built-in default).
pub(crate) fn search_max_rows() -> u64 {
    parse_positive_u64(
        std::env::var("DBXT_SEARCH_MAX_ROWS").ok().as_deref(),
        DEFAULT_SEARCH_MAX_ROWS,
    )
}

/// Engines driven by MySQL's `information_schema` (MySQL / MariaDB are the same
/// driver in dbx-core). The global search is gated on this plus PostgreSQL.
pub(crate) fn is_mysql_family(db_type: &str) -> bool {
    matches!(db_type.to_ascii_lowercase().as_str(), "mysql" | "mariadb")
}

/// Whether a declared column type holds searchable text. Only character types
/// are scanned: numbers / dates / binary never match a LIKE, and a LIKE on a
/// numeric column is a type error on PostgreSQL.
pub(crate) fn is_text_search_column(data_type: &str) -> bool {
    let base = base_type(data_type);
    // An array (`text[]`) is not scalar text; skip rather than risk a cast.
    if base.ends_with("[]") {
        return false;
    }
    matches!(
        base.as_str(),
        "char"
            | "varchar"
            | "character"
            | "nchar"
            | "nvarchar"
            | "text"
            | "tinytext"
            | "mediumtext"
            | "longtext"
            | "citext"
            | "clob"
            | "nclob"
            | "string"
            | "name"
    )
}

/// Escape a search term into a `LIKE` pattern. `!` is the escape character (not
/// a backslash) so the same pattern works under MySQL and PostgreSQL regardless
/// of their string-literal and `LIKE`-escape differences.
pub(crate) fn search_like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        if matches!(c, '!' | '%' | '_') {
            out.push('!');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// The dialect's case-insensitive text operator: PostgreSQL's `LIKE` is
/// case-sensitive so it gets `ILIKE`; MySQL's default collation and SQLite's
/// `LIKE` already fold case.
pub(crate) fn search_like_op(db_type: DatabaseType) -> &'static str {
    if is_postgres_family(db_type.as_str()) {
        "ILIKE"
    } else {
        "LIKE"
    }
}

/// One `SELECT *` per table with an `OR` of `LIKE`s over its text columns,
/// capped by `limit` so a scan never streams a whole table.
pub(crate) fn build_search_scan_sql(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    columns: &[String],
    needle: &str,
    limit: usize,
) -> String {
    let q = |name: &str| quote_table_identifier(Some(db_type), name);
    let op = search_like_op(db_type);
    let pattern = sql_literal(&search_like_pattern(needle));
    let conds: Vec<String> = columns
        .iter()
        .map(|c| format!("{} {} {} ESCAPE '!'", q(c), op, pattern))
        .collect();
    format!(
        "SELECT * FROM {} WHERE {} LIMIT {}",
        table_ref(db_type, schema, table),
        conds.join(" OR "),
        limit
    )
}

/// Cheap, approximate per-table row estimate used only to decide whether a
/// table is too big to scan. Never a `COUNT(*)`: that would itself be the full
/// scan the guard exists to avoid.
pub(crate) fn build_search_estimates_sql(db_type: DatabaseType, schema: &str) -> String {
    if is_postgres_family(db_type.as_str()) {
        let ns = if schema.trim().is_empty() {
            "public"
        } else {
            schema
        };
        let ns = sql_literal(ns);
        format!(
            "SELECT c.relname AS table_name, GREATEST(c.reltuples, 0)::bigint AS row_estimate \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {} AND c.relkind IN ('r','p')",
            ns
        )
    } else {
        // MySQL / MariaDB: `table_rows` is approximate for InnoDB but only used
        // as a skip heuristic; a NULL / unknown count scans the table.
        "SELECT table_name, table_rows AS row_estimate FROM information_schema.tables \
         WHERE table_schema = DATABASE()"
            .to_string()
    }
}

/// Turn the row-estimate result into `table → estimated rows`.
pub(crate) fn parse_search_estimates(rows: &[Vec<serde_json::Value>]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for row in rows {
        let Some(name) = row.first().and_then(|v| v.as_str()) else {
            continue;
        };
        let est = row
            .get(1)
            .and_then(|v| match v {
                serde_json::Value::Number(n) => {
                    n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
                }
                serde_json::Value::String(s) => s.trim().parse().ok(),
                _ => None,
            })
            .unwrap_or(0);
        out.insert(name.to_string(), est);
    }
    out
}

/// A table whose estimate exceeds the threshold is skipped instead of scanned.
/// `None` (an unknown count) always scans.
pub(crate) fn search_skip_reason(estimate: Option<u64>, max_rows: u64) -> Option<u64> {
    match estimate {
        Some(n) if n > max_rows => Some(n),
        _ => None,
    }
}

/// Build the `WHERE` predicate that re-locates a hit's row: the primary key when
/// the table has one, otherwise the matched column's value. Null keys are
/// dropped; `1 = 1` is the last resort.
pub(crate) fn search_hit_filter(
    db_type: DatabaseType,
    columns: &[String],
    vals: &[Val],
    dtypes: &HashMap<String, String>,
    pk: &[String],
    matched_col: &str,
) -> String {
    let q = |name: &str| quote_table_identifier(Some(db_type), name);
    let keys: Vec<&String> = if pk.is_empty() {
        columns
            .iter()
            .filter(|c| c.as_str() == matched_col)
            .collect()
    } else {
        pk.iter().collect()
    };
    let mut conds: Vec<String> = Vec::new();
    for k in keys {
        let Some(ci) = columns.iter().position(|c| c == k) else {
            continue;
        };
        let Some(v) = vals.get(ci) else {
            continue;
        };
        if matches!(v, Val::Null) {
            continue;
        }
        conds.push(format!(
            "{} = {}",
            q(k),
            val_literal(v, dtypes.get(k).map(|s| s.as_str()))
        ));
    }
    if conds.is_empty() {
        "1 = 1".to_string()
    } else {
        conds.join(" AND ")
    }
}

/// One global-search hit: enough to show it, copy the matched value, and jump
/// back to the owning row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SearchHit {
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) column: String,
    /// The full cell value that matched.
    pub(crate) matched: String,
    /// `WHERE` predicate (without the keyword) that locates the row again.
    pub(crate) filter: String,
}
