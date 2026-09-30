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

/// The whole on-disk config. Parsing is deliberately forgiving: a missing file,
/// an unknown version, a truncated body or a single malformed table entry all
/// fall back to defaults instead of failing to start.
#[derive(Clone, Default)]
pub(crate) struct TuiConfig {
    /// Global compact-column default, used when a table has no stored choice.
    pub(crate) compact: Option<bool>,
    pub(crate) tables: HashMap<(String, String), TablePrefs>,
    /// Whether this session changed the global compact default. Untouched
    /// globals are left to whatever another session last wrote.
    pub(crate) dirty_global: bool,
    /// `(database, table)` entries this session actually changed. Saving merges
    /// only these into the on-disk file, so two dbxt sessions (or a hand-edit)
    /// no longer clobber each other's tables; an entry reset to defaults is
    /// removed instead of silently surviving.
    pub(crate) dirty: HashSet<(String, String)>,
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
    pub(crate) fn save(&self, path: &std::path::Path) {
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
        root.insert("tables".into(), serde_json::Value::Object(tables));
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
