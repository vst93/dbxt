//! R102: table / column comment view + edit.
//!
//! The structure view reads a table's comment best-effort (alongside the columns
//! and DDL it already fetches) and shows it in its header. A dedicated modal
//! editor (`c` in the structure view, `n` inside the `gc` column popup) prefills
//! the current comment and turns Enter into a write statement that always goes
//! through the existing red confirmation layer — dbxt never runs a comment write
//! silently.
//!
//! SQL is generated per dialect: PostgreSQL / the generic `COMMENT ON` form for
//! tables and columns, MySQL's inline `ALTER TABLE … COMMENT` for tables only
//! (its per-column comment needs the whole column definition, which is too
//! brittle to rewrite in place), and a read-only degradation for SQLite and the
//! other comment-less engines.

use crate::prelude::*;
use crate::*;

/// How a comment write is expressed for an engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CommentDialect {
    /// PostgreSQL, compatible forks, and the generic `COMMENT ON` form.
    CommentOn,
    /// MySQL / MariaDB: an inline `ALTER TABLE … COMMENT = '…'` (table only).
    MySqlTable,
    /// SQLite and the other comment-less engines: nothing to write.
    Unsupported,
}

/// SQLite and its wire-compatible siblings: no comment support at all.
pub(crate) fn is_sqlite_family(db_type: DatabaseType) -> bool {
    matches!(
        db_type,
        DatabaseType::Sqlite
            | DatabaseType::Rqlite
            | DatabaseType::Turso
            | DatabaseType::CloudflareD1
    )
}

/// The comment dialect for an engine.
pub(crate) fn comment_dialect(db_type: DatabaseType) -> CommentDialect {
    if is_sqlite_family(db_type) {
        CommentDialect::Unsupported
    } else if is_mysql_family(db_type.as_str()) {
        CommentDialect::MySqlTable
    } else {
        CommentDialect::CommentOn
    }
}

/// Whether `target` can be edited in place on this engine. MySQL columns cannot
/// (see the module docs); SQLite has no comments at all.
pub(crate) fn comment_target_editable(db_type: DatabaseType, target: &CommentTarget) -> bool {
    match comment_dialect(db_type) {
        CommentDialect::Unsupported => false,
        CommentDialect::MySqlTable => matches!(target, CommentTarget::Table),
        CommentDialect::CommentOn => true,
    }
}

/// The read-only status for an engine that cannot edit the given target.
pub(crate) fn comment_readonly_status(db_type: DatabaseType) -> &'static str {
    if is_sqlite_family(db_type) {
        t("该引擎无注释，注释只读")
    } else {
        t("该引擎列注释暂不支持就地编辑")
    }
}

/// Generate the write statement for a comment, or `None` when the engine cannot
/// express it. `value` is the typed comment; a blank / whitespace-only value is
/// the clear gesture (`COMMENT … IS NULL` on PostgreSQL, `COMMENT = ''` on
/// MySQL).
pub(crate) fn comment_sql(
    db_type: DatabaseType,
    schema: &str,
    table: &str,
    target: &CommentTarget,
    value: Option<&str>,
) -> Option<String> {
    let set = value.filter(|v| !v.trim().is_empty());
    let table_ref = table_ref(db_type, schema, table);
    match comment_dialect(db_type) {
        CommentDialect::Unsupported => None,
        CommentDialect::MySqlTable => match target {
            CommentTarget::Table => {
                let lit = set.map(sql_literal).unwrap_or_else(|| "''".to_string());
                Some(format!("ALTER TABLE {table_ref} COMMENT = {lit};"))
            }
            CommentTarget::Column(_) => None,
        },
        CommentDialect::CommentOn => match target {
            CommentTarget::Table => Some(match set {
                Some(v) => format!("COMMENT ON TABLE {table_ref} IS {};", sql_literal(v)),
                None => format!("COMMENT ON TABLE {table_ref} IS NULL;"),
            }),
            CommentTarget::Column(col) => {
                let q = quote_table_identifier(Some(db_type), col);
                Some(match set {
                    Some(v) => format!("COMMENT ON COLUMN {table_ref}.{q} IS {};", sql_literal(v)),
                    None => format!("COMMENT ON COLUMN {table_ref}.{q} IS NULL;"),
                })
            }
        },
    }
}

/// The one-line clear-semantics hint for the editor (the exact clear SQL depends
/// on the dialect).
pub(crate) fn comment_clear_hint(db_type: DatabaseType, target: &CommentTarget) -> &'static str {
    match comment_dialect(db_type) {
        CommentDialect::MySqlTable if matches!(target, CommentTarget::Table) => {
            t("留空 = 清除（COMMENT = ''）")
        }
        _ => t("留空 = 清除（COMMENT … IS NULL）"),
    }
}

/// True when the open connection can edit the table comment in place (a write
/// gesture, so a read-only connection hides it).
pub(crate) fn table_comment_editable(app: &App) -> bool {
    app.selected
        .as_ref()
        .is_some_and(|c| !c.read_only && comment_target_editable(c.db_type, &CommentTarget::Table))
}

/// True when a column comment can be edited in place: the engine supports it,
/// the connection is writable, and the popup has cached table metadata to
/// resolve the column against.
pub(crate) fn column_comment_editable(app: &App) -> bool {
    app.table_meta.is_some()
        && app.selected.as_ref().is_some_and(|c| {
            !c.read_only
                && comment_target_editable(c.db_type, &CommentTarget::Column(String::new()))
        })
}

/// The `gc` popup action-row note when a column comment cannot be edited in
/// place, or `None` when `n` is available.
pub(crate) fn column_comment_hint(app: &App) -> Option<&'static str> {
    let cfg = app.selected.as_ref()?;
    if cfg.read_only {
        return Some(t("只读连接，列注释只读"));
    }
    match comment_dialect(cfg.db_type) {
        CommentDialect::Unsupported => Some(t("SQLite 无注释，列注释只读")),
        CommentDialect::MySqlTable => Some(t("该引擎列注释暂不支持就地编辑")),
        CommentDialect::CommentOn => None,
    }
}

/// Open the modal comment editor prefilled with `original`.
pub(crate) fn open_comment_edit(
    app: &mut App,
    target: CommentTarget,
    schema: String,
    table: String,
    original: Option<String>,
) {
    let seed = original.clone().unwrap_or_default();
    let mut input = TextArea::from(seed.split('\n').collect::<Vec<_>>());
    input.set_placeholder_text(t("留空 = 清除注释"));
    input.move_cursor(CursorMove::End);
    let label = match &target {
        CommentTarget::Table => tf("表 {}", &[&table]),
        CommentTarget::Column(c) => tf("列 {}.{}", &[&table, c]),
    };
    app.comment_edit = Some(CommentEdit {
        target,
        schema,
        table,
        original,
        input,
    });
    app.status = tf("编辑{}注释 · Enter 确认 · Esc 取消", &[&label]);
}

/// `c` in the structure view: edit the open table's comment.
pub(crate) fn open_table_comment_edit(app: &mut App) {
    if readonly_conn_block(app) {
        return;
    }
    let Some(db_type) = app.selected.as_ref().map(|c| c.db_type) else {
        return;
    };
    if !comment_target_editable(db_type, &CommentTarget::Table) {
        app.status = comment_readonly_status(db_type).into();
        return;
    }
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        app.status = t("先选中一张表").into();
        return;
    };
    let schema = app.schema.clone();
    let original = app.table_comment.clone();
    open_comment_edit(app, CommentTarget::Table, schema, table, original);
}

/// `n` in the `gc` column popup: edit the highlighted column's comment. Requires
/// the cached `table_meta` (a bare query result has no table to target).
pub(crate) fn open_column_comment_edit(app: &mut App) {
    if readonly_conn_block(app) {
        return;
    }
    let Some(row) = cols_popup_selected(app) else {
        app.status = t("没有可编辑注释的列").into();
        return;
    };
    let Some(meta) = app.table_meta.as_ref() else {
        app.status = t("查询结果无表元数据，无法编辑列注释").into();
        return;
    };
    let Some(col) = meta
        .columns
        .iter()
        .find(|c| fix_double_encoding(&c.name) == row.name)
    else {
        app.status = t("查询结果无表元数据，无法编辑列注释").into();
        return;
    };
    let Some(db_type) = app.selected.as_ref().map(|c| c.db_type) else {
        return;
    };
    if !comment_target_editable(db_type, &CommentTarget::Column(row.name.clone())) {
        app.status = comment_readonly_status(db_type).into();
        return;
    }
    let schema = meta.schema.clone();
    let table = meta.table.clone();
    let original = col.comment.clone();
    open_comment_edit(
        app,
        CommentTarget::Column(row.name),
        schema,
        table,
        original,
    );
}

/// The comment editor's key handler: Enter generates the write and hands it to
/// the confirmation pipeline; Esc cancels; every other key edits the buffer.
pub(crate) fn comment_edit_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => submit_comment_edit(app),
        KeyCode::Esc => {
            app.comment_edit = None;
            app.flash(t("已取消编辑注释").into());
        }
        _ => {
            if let Some(ce) = app.comment_edit.as_mut() {
                ce.input.input(k);
            }
        }
    }
}

/// Enter in the comment editor: build the dialect SQL and route it through the
/// red confirmation layer. Never executed here.
pub(crate) fn submit_comment_edit(app: &mut App) {
    let Some(ce) = app.comment_edit.take() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let value = ce.input.lines().join("\n");
    let Some(sql) = comment_sql(cfg.db_type, &ce.schema, &ce.table, &ce.target, Some(&value))
    else {
        app.status = comment_readonly_status(cfg.db_type).into();
        return;
    };
    if readonly_block(app, &sql) {
        return;
    }
    app.pending_scope = None;
    let label = match &ce.target {
        CommentTarget::Table => fix_double_encoding(&ce.table),
        CommentTarget::Column(c) => format!("{}.{}", fix_double_encoding(&ce.table), c),
    };
    let what = if value.trim().is_empty() {
        t("清除")
    } else {
        t("更新")
    };
    app.comment_refresh = true;
    app.confirm = Some(Confirm {
        sql,
        reasons: vec![tf("{}注释：{}", &[&what, &label])],
        refresh: true,
        clear_batch: false,
        conn: None,
        redis: None,
        mongo: None,
    });
    app.status = t("注释确认 · Enter 执行 · Esc 取消").into();
}

/// After a comment write succeeds, drop the cached table comment and re-read the
/// metadata so the structure view / `gc` popup show the new text. Reuses the
/// existing column / DDL fetch (zero extra queries beyond that metadata pass).
pub(crate) fn refresh_after_comment_write(app: &mut App, tx: &Tx) {
    app.table_comment = None;
    app.table_comment_loaded = false;
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    // Refresh the popup's `table_meta` (it is the comment cache the `gc` popup
    // reads), then the structure view's own grid when it is on screen.
    app.spawn(
        tx,
        Op::TableColumns(Box::new(cfg), app.current_db(), app.schema.clone(), table),
    );
    if app.grid_kind == GridKind::Columns {
        load_structure(app, tx);
    }
}
