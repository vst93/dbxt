//! R107: single-table complete DDL export (dialect `SHOW CREATE` family).
//!
//! The kernel already knows how to ask every dialect for a table definition.
//! For the two dialects whose source statement is a plain one-liner this module
//! issues it directly and picks the DDL cell out of the reply — MySQL's
//! `SHOW CREATE TABLE` returns `(Table, Create Table)` (DDL is column 2), while
//! SQLite's `sqlite_master` query returns a single column. Every other dialect
//! goes through the kernel's single-table DDL entry point
//! (`get_table_ddl_core`: PostgreSQL `pg_catalog`, SQL Server `sys.sql_modules`,
//! Oracle `DBMS_METADATA`, …). dbxt never reconstructs a `CREATE TABLE` from
//! column metadata.
//!
//! The fetch is an explicit action only: `D` (or `D` inside the `g c` popup)
//! issues the source statement; nothing here runs on browse.

use crate::prelude::*;
use crate::*;

/// The dialect's source statement for a *table* definition, when it is a plain
/// one-liner. `None` means "let the kernel fetch it" via
/// [`dbx_core::schema::get_table_ddl_core`], which is the case for PostgreSQL,
/// SQL Server and Oracle (catalog-driven, multi-statement source SQL).
pub(crate) fn table_source_sql(
    db_type: DatabaseType,
    database: &str,
    schema: &str,
    table: &str,
) -> Option<String> {
    match db_type {
        // MySQL / MariaDB: the database is the qualifier when no schema layer
        // produced one (`qualified_table_name` backtick-quotes it).
        DatabaseType::Mysql => {
            let catalog = if schema.trim().is_empty() {
                database
            } else {
                schema
            };
            Some(format!(
                "SHOW CREATE TABLE {}",
                qualified_table_name(Some(db_type), Some(catalog), table)
            ))
        }
        // SQLite and its wire-compatible siblings read the stored `CREATE`
        // statement from `sqlite_master` (single column).
        DatabaseType::Sqlite => {
            let schema = if schema.trim().is_empty() {
                "main"
            } else {
                schema
            };
            Some(format!(
                "SELECT sql FROM {}.sqlite_master WHERE type = 'table' AND name = {}",
                dbx_core::db::sqlite::sqlite_quote_schema_ident(schema),
                sql_string_literal(table),
            ))
        }
        _ => None,
    }
}

/// The column that carries the DDL text in the source statement's reply.
///
/// MySQL's `SHOW CREATE TABLE` answers `(Table, Create Table)` so the DDL is
/// the second column; SQLite's `sqlite_master` query answers one column.
pub(crate) fn ddl_column_index(db_type: DatabaseType) -> usize {
    if matches!(db_type, DatabaseType::Mysql) {
        1
    } else {
        0
    }
}

/// Pick the DDL text out of the source statement's rows. The first row wins; an
/// empty reply, a missing column, or a blank cell is an error so a failure is
/// never rendered as a half statement. Extra rows are tolerated (the source
/// statements carry `LIMIT 1` / a unique name, but a driver may still stream
/// more).
pub(crate) fn ddl_from_rows(
    db_type: DatabaseType,
    rows: &[Vec<serde_json::Value>],
) -> Result<String, String> {
    let idx = ddl_column_index(db_type);
    let row = rows.first().ok_or_else(|| t("源语句没有返回 DDL").to_string())?;
    let cell = row
        .get(idx)
        .ok_or_else(|| t("DDL 结果缺少目标列").to_string())?;
    let text = match cell {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    if text.trim().is_empty() {
        return Err(t("DDL 结果为空").to_string());
    }
    Ok(text)
}

/// A filesystem-safe `{table}.sql` default for `Ctrl-Y`.
pub(crate) fn ddl_default_filename(table: &str) -> String {
    let mut safe: String = table
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.trim_matches('_').is_empty() {
        safe = "table".to_string();
    }
    format!("{safe}.sql")
}

/// A SQL single-quoted string literal (doubling embedded quotes).
fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

// ── the modal complete-DDL popup (`D`) ──────────────────────────────────────

/// `D` (structure view) / `D` in the `g c` popup: fetch the current table's
/// complete DDL and open the modal popup. The source statement is issued here
/// and only here — the browse redline is untouched.
pub(crate) fn open_ddl_popup(app: &mut App, tx: &Tx) {
    if app.backend_kind != Backend::Sql {
        app.status = t("完整 DDL 仅支持 SQL 引擎").into();
        return;
    }
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        app.status = t("先选中一张表").into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let db = app.current_db();
    let schema = app.schema.clone();
    app.ddl_popup = None;
    app.ddl_popup_pending = Some(DdlRequest {
        table: table.clone(),
        schema: schema.clone(),
    });
    app.loading = true;
    app.status = tf("加载 {} 完整 DDL…", &[&(fix_double_encoding(&table))]);
    app.spawn(tx, Op::TableDdl(Box::new(cfg), db, schema, table));
}

/// The text the popup would copy (`y`). Split out so the copy content can be
/// asserted without touching the system clipboard.
pub(crate) fn ddl_popup_copy_text(app: &App) -> Option<String> {
    app.ddl_popup.as_ref().map(|p| p.text.clone())
}

/// `y`: copy the complete DDL verbatim (never a re-formatted view).
pub(crate) fn copy_ddl_popup(app: &mut App) {
    let Some(text) = ddl_popup_copy_text(app) else {
        return;
    };
    let n = text.chars().count();
    match clipboard_copy(&text) {
        Some(path) => {
            app.status = tf(
                "✓ 完整 DDL 已复制（{} 字符）· 兜底 {}",
                &[&n, &(path.display())],
            )
        }
        None => app.status = tf("✓ 完整 DDL 已复制（{} 字符）", &[&n]),
    }
}

/// `Ctrl-Y`: write the complete DDL to `{table}.sql` in the current directory.
pub(crate) fn save_ddl_popup(app: &mut App) {
    let Some(popup) = app.ddl_popup.as_ref() else {
        return;
    };
    let filename = ddl_default_filename(&popup.table);
    let path = std::env::current_dir()
        .unwrap_or_default()
        .join(&filename);
    let n = popup.text.chars().count();
    match std::fs::write(&path, popup.text.as_bytes()) {
        Ok(()) => {
            app.status = tf(
                "✓ 完整 DDL 已写入 {}（{} 字符）",
                &[&(path.display()), &n],
            )
        }
        Err(e) => app.status = tf("✗ DDL 写入失败：{}", &[&e]),
    }
}

/// The popup's modal keymap: `y` copy, `Ctrl-Y` save, scroll, `Esc` close.
pub(crate) fn ddl_popup_key(app: &mut App, k: KeyEvent) {
    if app.ddl_popup.is_none() {
        return;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.ddl_popup = None;
            app.flash(t("已关闭完整 DDL").into());
        }
        KeyCode::Char('y') if !ctrl => copy_ddl_popup(app),
        KeyCode::Char('y') if ctrl => save_ddl_popup(app),
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = p.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = p.scroll.saturating_add(1);
            }
        }
        KeyCode::PageUp => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = p.scroll.saturating_sub(10);
            }
        }
        KeyCode::PageDown => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = p.scroll.saturating_add(10);
            }
        }
        KeyCode::Home | KeyCode::Char('g') => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = 0;
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            if let Some(p) = app.ddl_popup.as_mut() {
                p.scroll = u16::MAX;
            }
        }
        _ => {}
    }
}

/// R107: open the popup when the fetch lands, or report the failure. The
/// pending request is the guard, so a late reply for a table the user has
/// navigated away from is dropped instead of shown.
pub(crate) fn apply_ddl_popup_result(
    app: &mut App,
    table: String,
    schema: String,
    text: Option<String>,
    error: Option<String>,
) {
    let Some(pending) = app.ddl_popup_pending.take() else {
        return;
    };
    if pending.table != table || pending.schema != schema {
        return;
    }
    match text.filter(|d| !d.trim().is_empty()) {
        Some(ddl) => {
            app.ddl_popup = Some(DdlPopup {
                table,
                schema,
                text: ddl,
                scroll: 0,
            });
            app.status = t("完整 DDL · y 复制 · Ctrl-Y 存文件 · Esc 关").into();
        }
        None => {
            let msg = error.unwrap_or_else(|| t("DDL 结果为空").to_string());
            app.status = tf("✗ 无法获取完整 DDL：{}", &[&msg]);
        }
    }
}
