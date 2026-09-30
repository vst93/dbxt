use crate::prelude::*;

// ─── persistent TUI config (~/.config/dbxt/tui.json) ─────────────────────────

/// Per `(database, table)` preferences restored when the table is reopened.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct TablePrefs {
    /// Columns hidden for this table (DBX column-visibility equivalent).
    pub(crate) hidden: HashSet<String>,
    /// Compact-column choice; `None` = follow the global default.
    pub(crate) compact: Option<bool>,
    /// Last ORDER BY expression (without the `ORDER BY` keyword).
    pub(crate) order_by: Option<String>,
}

/// Cap on persisted per-column width overrides (R72). The list is LRU-ordered
/// (most recently adjusted last); a save past the cap drops the oldest entries
/// so a long-lived config can never grow without bound.
pub(crate) const COL_WIDTH_MEM_MAX: usize = 200;

/// R83: how many SQLite files the `L` quick-open picker remembers (in
/// `tui.json`). Kept small — the list sits at the top of the picker.
pub(crate) const SQLITE_RECENT_MAX: usize = 5;

/// One persisted column-width override (R72). The identity is
/// `(conn, db, schema, table, col)` — the same scope the session memory uses,
/// minus the query bucket (a plain query result is never persisted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColWidthEntry {
    pub(crate) conn: String,
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) col: String,
    pub(crate) width: usize,
}

impl ColWidthEntry {
    /// Canonical identity string, used for the in-memory dirty / removed sets.
    pub(crate) fn key(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            self.conn, self.db, self.schema, self.table, self.col
        )
    }
}

/// Canonical identity for a width override, matching [`ColWidthEntry::key`].
pub(crate) fn col_width_key(conn: &str, db: &str, schema: &str, table: &str, col: &str) -> String {
    format!("{conn}\u{0}{db}\u{0}{schema}\u{0}{table}\u{0}{col}")
}

/// The whole on-disk config. Parsing is deliberately forgiving: a missing file,
/// an unknown version, a truncated body or a single malformed table entry all
/// fall back to defaults instead of failing to start.
#[derive(Clone, Default)]
pub(crate) struct TuiConfig {
    /// Global compact-column default, used when a table has no stored choice.
    pub(crate) compact: Option<bool>,
    /// R76: global big-number display mode for result cells (`None` = default).
    pub(crate) num_fmt: Option<NumFmt>,
    /// R76: alternate-row banding in the result grid (`None` = default on).
    pub(crate) stripe: Option<bool>,
    /// R79: editor auto-indent on `Enter` (`None` = default on). An input
    /// behaviour, not a visual one, so it ships on unless `tui.json` says
    /// otherwise.
    pub(crate) editor_indent: Option<bool>,
    /// R79: editor bracket auto-pairing (`None` = default on).
    pub(crate) editor_pairs: Option<bool>,
    /// R83: the last few SQLite files opened through the `L` quick-open picker,
    /// most-recent first. These are plain file paths, never connections — a
    /// quick-open stays out of the connection store entirely.
    pub(crate) sqlite_recent: Vec<PathBuf>,
    /// R87: sidebar group ids the user collapsed (R48 fold state). Persisted so
    /// a long tree keeps its fold state across restarts; group ids are short and
    /// few, so the whole set rides `tui.json`.
    pub(crate) group_closed: HashSet<String>,
    pub(crate) tables: HashMap<(String, String), TablePrefs>,
    /// R72: persisted per-column widths, LRU-ordered (oldest first). Capped at
    /// [`COL_WIDTH_MEM_MAX`] on load and on save.
    pub(crate) col_widths: Vec<ColWidthEntry>,
    /// Width identities this session set / changed (merged into the file on
    /// save).
    pub(crate) dirty_widths: HashSet<String>,
    /// Width identities this session cleared (removed from the file on save).
    pub(crate) removed_widths: HashSet<String>,
    /// Whether this session changed the global compact default. Untouched
    /// globals are left to whatever another session last wrote.
    pub(crate) dirty_global: bool,
    /// R76: whether this session changed a global *display* pref (`num_fmt` /
    /// `stripe`). Kept separate from `dirty_global` so toggling one never
    /// clobbers the other.
    pub(crate) dirty_display: bool,
    /// `(database, table)` entries this session actually changed. Saving merges
    /// only these into the on-disk file, so two dbxt sessions (or a hand-edit)
    /// no longer clobber each other's tables; an entry reset to defaults is
    /// removed instead of silently surviving.
    pub(crate) dirty: HashSet<(String, String)>,
    /// R83: whether this session changed the SQLite recent-files list. Kept
    /// separate so a session that never used the picker cannot clobber another
    /// session's list.
    pub(crate) dirty_sqlite_recent: bool,
    /// R87: whether this session changed the sidebar fold state. Kept separate
    /// so a session that never collapsed a group cannot clobber another
    /// session's fold state.
    pub(crate) dirty_group_closed: bool,
}

impl TuiConfig {
    /// Read `path`, tolerating every kind of corruption (returns defaults).
    pub(crate) fn load(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let mut cfg = Self {
            compact: v.get("compact").and_then(|b| b.as_bool()),
            num_fmt: v
                .get("num_fmt")
                .and_then(|s| s.as_str())
                .and_then(NumFmt::from_key),
            stripe: v.get("stripe").and_then(|b| b.as_bool()),
            editor_indent: v.get("editor_indent").and_then(|b| b.as_bool()),
            editor_pairs: v.get("editor_pairs").and_then(|b| b.as_bool()),
            ..Self::default()
        };
        if let Some(arr) = v.get("sqlite_recent").and_then(|a| a.as_array()) {
            let mut paths: Vec<PathBuf> = arr
                .iter()
                .filter_map(|x| x.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .collect();
            // Keep the most recent five; a hand-edited overflow is truncated.
            paths.truncate(SQLITE_RECENT_MAX);
            cfg.sqlite_recent = paths;
        }
        if let Some(arr) = v.get("group_closed").and_then(|a| a.as_array()) {
            cfg.group_closed = arr
                .iter()
                .filter_map(|x| x.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .collect();
        }
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
        if let Some(arr) = v.get("col_widths").and_then(|a| a.as_array()) {
            let mut entries: Vec<ColWidthEntry> = Vec::new();
            for e in arr {
                let Some(o) = e.as_object() else {
                    continue;
                };
                let s = |k: &str| o.get(k).and_then(|x| x.as_str()).map(str::to_string);
                let (Some(conn), Some(db), Some(schema), Some(table), Some(col)) =
                    (s("conn"), s("db"), s("schema"), s("table"), s("col"))
                else {
                    continue;
                };
                let Some(w) = o.get("w").and_then(|x| x.as_u64()) else {
                    continue;
                };
                entries.push(ColWidthEntry {
                    conn,
                    db,
                    schema,
                    table,
                    col,
                    width: (w as usize).clamp(MIN_CELL_WIDTH, COL_W_MAX),
                });
            }
            // Keep the LRU tail (most recent) when a hand-edited file overflows.
            if entries.len() > COL_WIDTH_MEM_MAX {
                let drop = entries.len() - COL_WIDTH_MEM_MAX;
                entries.drain(0..drop);
            }
            cfg.col_widths = entries;
        }
        cfg
    }

    /// The remembered width for one `(conn, db, schema, table, col)`, if any.
    pub(crate) fn col_width(
        &self,
        conn: &str,
        db: &str,
        schema: &str,
        table: &str,
        col: &str,
    ) -> Option<usize> {
        let key = col_width_key(conn, db, schema, table, col);
        self.col_widths
            .iter()
            .find(|e| e.key() == key)
            .map(|e| e.width)
    }

    /// Remember / update one column's width, moving it to the LRU head.
    pub(crate) fn set_col_width(
        &mut self,
        conn: &str,
        db: &str,
        schema: &str,
        table: &str,
        col: &str,
        width: usize,
    ) {
        let key = col_width_key(conn, db, schema, table, col);
        self.col_widths.retain(|e| e.key() != key);
        self.col_widths.push(ColWidthEntry {
            conn: conn.to_string(),
            db: db.to_string(),
            schema: schema.to_string(),
            table: table.to_string(),
            col: col.to_string(),
            width: width.clamp(MIN_CELL_WIDTH, COL_W_MAX),
        });
        self.removed_widths.remove(&key);
        self.dirty_widths.insert(key);
    }

    /// Forget one column's remembered width.
    pub(crate) fn clear_col_width(
        &mut self,
        conn: &str,
        db: &str,
        schema: &str,
        table: &str,
        col: &str,
    ) {
        let key = col_width_key(conn, db, schema, table, col);
        self.col_widths.retain(|e| e.key() != key);
        self.dirty_widths.remove(&key);
        self.removed_widths.insert(key);
    }

    /// Forget every remembered width for one table. Returns how many entries
    /// were dropped.
    pub(crate) fn clear_table_col_widths(
        &mut self,
        conn: &str,
        db: &str,
        schema: &str,
        table: &str,
    ) -> usize {
        let prefix = format!("{conn}\u{0}{db}\u{0}{schema}\u{0}{table}\u{0}");
        let mut keys: Vec<String> = Vec::new();
        for e in &self.col_widths {
            let key = e.key();
            if key.starts_with(&prefix) {
                keys.push(key);
            }
        }
        for key in &keys {
            self.col_widths.retain(|e| &e.key() != key);
            self.dirty_widths.remove(key);
            self.removed_widths.insert(key.clone());
        }
        keys.len()
    }

    /// Write the config back, merging with whatever is on disk so two dbxt
    /// sessions (or an external editor) do not clobber each other: only the
    /// `(database, table)` entries this session actually changed are applied,
    /// and a cleared entry is removed. Best-effort: a read-only config dir must
    /// never interrupt the TUI.
    pub(crate) fn save(&self, path: &std::path::Path) {
        // Start from the current on-disk state so another session's tables are
        // preserved; a missing / corrupt file simply means we start from scratch.
        let mut merged = TuiConfig::load(path);
        if self.dirty_global {
            merged.compact = self.compact;
        }
        if self.dirty_display {
            merged.num_fmt = self.num_fmt;
            merged.stripe = self.stripe;
        }
        if self.dirty_sqlite_recent {
            merged.sqlite_recent = self.sqlite_recent.clone();
        }
        if self.dirty_group_closed {
            merged.group_closed = self.group_closed.clone();
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
        // Column widths: drop the ones this session cleared, then re-apply the
        // ones it changed (each moved to the LRU head), then cap the list.
        if !self.removed_widths.is_empty() {
            merged
                .col_widths
                .retain(|e| !self.removed_widths.contains(&e.key()));
        }
        for key in &self.dirty_widths {
            merged.col_widths.retain(|e| &e.key() != key);
        }
        // Re-append this session's entries in their own LRU order (most recent
        // last), so a HashSet iteration never scrambles the eviction order.
        for e in &self.col_widths {
            if self.dirty_widths.contains(&e.key()) {
                merged.col_widths.push(e.clone());
            }
        }
        if merged.col_widths.len() > COL_WIDTH_MEM_MAX {
            let drop = merged.col_widths.len() - COL_WIDTH_MEM_MAX;
            merged.col_widths.drain(0..drop);
        }
        merged.write(path);
    }

    /// Serialize `self` (the already-merged state) to `path`, creating the parent
    /// directory. Best-effort and silent on failure.
    pub(crate) fn write(&self, path: &std::path::Path) {
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
        if let Some(m) = self.num_fmt {
            root.insert(
                "num_fmt".into(),
                serde_json::Value::String(m.key().to_string()),
            );
        }
        if let Some(s) = self.stripe {
            root.insert("stripe".into(), serde_json::Value::Bool(s));
        }
        if let Some(e) = self.editor_indent {
            root.insert("editor_indent".into(), serde_json::Value::Bool(e));
        }
        if let Some(e) = self.editor_pairs {
            root.insert("editor_pairs".into(), serde_json::Value::Bool(e));
        }
        if !self.sqlite_recent.is_empty() {
            let arr: Vec<serde_json::Value> = self
                .sqlite_recent
                .iter()
                .map(|p| serde_json::Value::String(p.to_string_lossy().into_owned()))
                .collect();
            root.insert("sqlite_recent".into(), serde_json::Value::Array(arr));
        }
        if !self.group_closed.is_empty() {
            let mut ids: Vec<&String> = self.group_closed.iter().collect();
            ids.sort();
            root.insert(
                "group_closed".into(),
                serde_json::Value::Array(
                    ids.into_iter()
                        .map(|id| serde_json::Value::String(id.clone()))
                        .collect(),
                ),
            );
        }
        root.insert("tables".into(), serde_json::Value::Object(tables));
        if !self.col_widths.is_empty() {
            let arr: Vec<serde_json::Value> = self
                .col_widths
                .iter()
                .map(|e| {
                    let mut m = serde_json::Map::new();
                    m.insert("conn".into(), serde_json::Value::String(e.conn.clone()));
                    m.insert("db".into(), serde_json::Value::String(e.db.clone()));
                    m.insert("schema".into(), serde_json::Value::String(e.schema.clone()));
                    m.insert("table".into(), serde_json::Value::String(e.table.clone()));
                    m.insert("col".into(), serde_json::Value::String(e.col.clone()));
                    m.insert("w".into(), serde_json::Value::from(e.width as u64));
                    serde_json::Value::Object(m)
                })
                .collect();
            root.insert("col_widths".into(), serde_json::Value::Array(arr));
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::Value::Object(root).to_string();
        // Atomic-ish: write a sibling temp file then rename so a crash never
        // leaves a half-written config behind. The pid keeps two concurrent
        // dbxt processes from fighting over the same temp path.
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub(crate) fn table(&self, db: &str, schema: &str, table: &str) -> Option<&TablePrefs> {
        self.tables
            .get(&(db.to_string(), table_pref_key(schema, table)))
    }

    /// Mutable access that records the entry as changed by this session.
    pub(crate) fn entry(&mut self, db: &str, schema: &str, table: &str) -> &mut TablePrefs {
        let key = (db.to_string(), table_pref_key(schema, table));
        self.dirty.insert(key.clone());
        self.tables.entry(key).or_default()
    }

    /// Set the global compact default and mark it changed by this session.
    pub(crate) fn set_compact(&mut self, value: Option<bool>) {
        self.compact = value;
        self.dirty_global = true;
    }

    /// R76: set the global big-number display mode.
    pub(crate) fn set_num_fmt(&mut self, value: NumFmt) {
        self.num_fmt = Some(value);
        self.dirty_display = true;
    }

    /// R76: set the global alternate-row banding switch.
    pub(crate) fn set_stripe(&mut self, value: bool) {
        self.stripe = Some(value);
        self.dirty_display = true;
    }

    /// R83: remember a SQLite file as the most recent quick-open, dropping any
    /// older occurrence and capping the list at [`SQLITE_RECENT_MAX`].
    pub(crate) fn push_sqlite_recent(&mut self, path: &std::path::Path) {
        self.sqlite_recent.retain(|p| p != path);
        self.sqlite_recent.insert(0, path.to_path_buf());
        self.sqlite_recent.truncate(SQLITE_RECENT_MAX);
        self.dirty_sqlite_recent = true;
    }

    /// R87: record one sidebar group's fold state, marking it changed only when
    /// the state actually moves (so merely walking the tree never writes the
    /// file). `closed` = the group should be collapsed.
    pub(crate) fn set_group_closed(&mut self, id: &str, closed: bool) {
        let changed = if closed {
            self.group_closed.insert(id.to_string())
        } else {
            self.group_closed.remove(id)
        };
        if changed {
            self.dirty_group_closed = true;
        }
    }

    /// R83: forget one SQLite file from the recent list.
    pub(crate) fn remove_sqlite_recent(&mut self, path: &std::path::Path) {
        let before = self.sqlite_recent.len();
        self.sqlite_recent.retain(|p| p != path);
        if self.sqlite_recent.len() != before {
            self.dirty_sqlite_recent = true;
        }
    }
}

/// Resolve the config file path. `DBXT_CONFIG` overrides it (tests / portable
/// setups), `DBXT_NO_PERSIST=1` disables persistence entirely.
pub(crate) fn config_path() -> Option<PathBuf> {
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
