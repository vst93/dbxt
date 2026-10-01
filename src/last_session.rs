//! R98: last-session memory (`~/.config/dbxt/last-session.json`).
//!
//! A tiny, deliberately independent file: where the previous run left off
//! (connection + database / schema / table). It is written on a graceful exit
//! **only after a connection actually opened** this run, read once at startup
//! to highlight the connection in the picker, and read again for `dbxt --last`
//! to auto-resume the previous table. It never touches the DBX store and never
//! panics on a missing / corrupt file.

use crate::prelude::*;
use crate::*;

/// R98: the file name, sitting beside `tui.json` in the config directory.
pub(crate) const LAST_SESSION_FILE: &str = "last-session.json";

/// R98: the minimal last-session record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LastSession {
    pub(crate) conn_id: String,
    pub(crate) conn_name: String,
    pub(crate) database: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) saved_at: i64,
}

impl LastSession {
    /// Parse a `last-session.json` body. Every corruption — invalid JSON, a
    /// non-object root, a missing / blank `conn_id` — means "no session"
    /// (`None`), never an error and never a panic. The database / schema / table
    /// fields are optional and default to empty.
    pub(crate) fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let o = v.as_object()?;
        let conn_id = o
            .get("conn_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if conn_id.is_empty() {
            return None;
        }
        let s = |k: &str| o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Some(LastSession {
            conn_id: conn_id.to_string(),
            conn_name: s("conn_name"),
            database: s("database"),
            schema: s("schema"),
            table: s("table"),
            saved_at: o.get("saved_at").and_then(|x| x.as_i64()).unwrap_or(0),
        })
    }

    /// Serialize to the exact on-disk shape. A blank optional field is still
    /// written (so the file is a stable, self-describing record).
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "conn_id": self.conn_id,
            "conn_name": self.conn_name,
            "database": self.database,
            "schema": self.schema,
            "table": self.table,
            "saved_at": self.saved_at,
        })
    }

    /// Read `path`, tolerating a missing / unreadable / corrupt file (`None`).
    pub(crate) fn load(path: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        Self::from_json(&text)
    }

    /// Best-effort atomic write: a sibling temp file then a rename, so a crash
    /// never leaves a half-written session behind. Silent on failure (a
    /// read-only config dir must never interrupt shutdown).
    pub(crate) fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = self.to_json().to_string();
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Current unix time in seconds (`0` if the clock is before the epoch).
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// R98: resolve `last-session.json`. It sits beside `tui.json`, so `DBXT_CONFIG`
/// (tests / portable setups) relocates both and `DBXT_NO_PERSIST=1` disables
/// both. `DBXT_LAST_SESSION` overrides the file directly.
pub(crate) fn last_session_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DBXT_LAST_SESSION").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    config_path().map(|p| p.with_file_name(LAST_SESSION_FILE))
}

/// R98: the picker index of the remembered connection, when it still exists.
pub(crate) fn last_session_conn_index(app: &App) -> Option<usize> {
    let s = app.last_session.as_ref()?;
    app.connections.iter().position(|c| c.id == s.conn_id)
}

/// R98: `--last` — reconnect the remembered connection and hand the database /
/// schema / table to the normal smart-restore path. Any level that no longer
/// exists degrades with a one-line status: table → database → connection → the
/// connection list.
///
/// Only called once the connection list has arrived and nothing is active.
pub(crate) fn resume_last_session(app: &mut App, tx: &Tx) {
    let Some(s) = app.last_session.clone() else {
        app.status = t("没有可恢复的上次会话 · 回到连接列表").into();
        return;
    };
    let Some(pos) = app.connections.iter().position(|c| c.id == s.conn_id) else {
        app.status = t("上次会话的连接已不存在 · 回到连接列表").into();
        return;
    };
    app.conn_list.select(Some(pos));
    let cfg = app.connections[pos].clone();
    let restore = ConnPointer {
        db: s.database.clone(),
        schema: s.schema.clone(),
        table: (!s.table.is_empty()).then(|| s.table.clone()),
    };
    activate_connection(app, tx, cfg, Some(restore), None);
    // Arm the chain *after* activation, which clears it for ordinary switches.
    app.resume_last = true;
    // Redis has no database / table layer, so the resume ends at the connection.
    if app.backend_kind == Backend::Redis {
        app.resume_last = false;
        app.status = t("✓ 已恢复上次会话").into();
    }
}

/// R98: write the current position to `path` on a graceful exit. A no-op unless
/// a connection actually opened this run (`session_opened`) and one is still
/// active — a failed / never-attempted connection never leaves a file behind.
pub(crate) fn save_last_session(app: &App, path: &std::path::Path) {
    if !app.session_opened {
        return;
    }
    let Some(cfg) = app.selected.as_ref() else {
        return;
    };
    let ptr = snapshot_pointer(app);
    let session = LastSession {
        conn_id: cfg.id.clone(),
        conn_name: cfg.name.clone(),
        database: ptr.db,
        schema: ptr.schema,
        table: ptr.table.unwrap_or_default(),
        saved_at: now_unix(),
    };
    session.save(path);
}
