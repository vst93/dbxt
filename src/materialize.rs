//! R103: materialize the current result set as a table (CTAS).
//!
//! `g m` in the results pane turns the grid on screen into a new table with
//! `CREATE TABLE <name> AS <source SQL>`. The source is the statement behind the
//! result set — a query result's own statement (a drilled script outcome, or the
//! focused row of a multi-statement script list), or the equivalent SELECT of a
//! browsed table's current WHERE filter / ORDER BY. The write always routes
//! through the red confirmation layer: dbxt never runs a CTAS silently, and the
//! full statement plus the target table name are shown before anything runs.
//!
//! Everything up to the confirmation is pure client-side state; the only query
//! is the CTAS itself, followed by the existing sidebar table-list refresh (no
//! new query timing).

use crate::prelude::*;
use crate::*;

/// The prefilled new-table name: `result_<HHMMSS>` in local time.
pub(crate) fn default_materialize_name() -> String {
    format!("result_{}", chrono::Local::now().format("%H%M%S"))
}

/// The result grid a materialization would read: the drilled script outcome when
/// one is shown, else the focused outcome of a script list, else the top-level
/// grid. `None` for a statement with no result columns (a write) or a DDL view.
pub(crate) fn materialize_grid_rows(app: &App) -> Option<usize> {
    if let Some(s) = &app.script {
        let idx = s.drilled.unwrap_or(s.sel);
        return s
            .outcomes
            .get(idx)
            .filter(|o| o.error.is_none() && !o.grid.columns.is_empty())
            .map(|o| o.grid.rows.len());
    }
    app.grid.as_ref().map(|g| g.rows.len())
}

/// The source SQL behind the result set on screen, or `None` when there is
/// nothing to materialize. Pure: it only reads cached state (zero queries).
pub(crate) fn materialize_source_sql(app: &App) -> Option<String> {
    // A drilled (or focused) multi-statement result names its own statement —
    // the ordinal is the script outcome's index. A script list / a write outcome
    // has no materializable SELECT of its own; never fall through to a stale
    // `last_sql`.
    if let Some(s) = &app.script {
        let idx = s.drilled.unwrap_or(s.sel);
        if let Some(o) = s.outcomes.get(idx) {
            if o.error.is_none() && !o.grid.columns.is_empty() && !o.sql.trim().is_empty() {
                return Some(o.sql.trim().to_string());
            }
        }
        return None;
    }
    match app.grid_kind {
        // A browsed table with an active WHERE / ORDER BY materializes the
        // equivalent SELECT (the same view semantics the export INSERT uses).
        GridKind::TableData => {
            let ps = app.page_state.as_ref()?;
            let cfg = app.selected.as_ref()?;
            Some(browse_equivalent_select(
                cfg,
                &ps.schema,
                &ps.table,
                &ps.filter,
                ps.order_by.as_deref(),
            ))
        }
        // A query result materializes the statement that produced it. The active
        // tab's own SQL wins so a `[` / `]` flip materializes the grid on screen.
        GridKind::Query => {
            let tab_sql = app
                .result_tabs
                .get(app.result_tab)
                .and_then(|t| t.sql.clone());
            tab_sql
                .or_else(|| app.last_sql.clone())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

/// The equivalent `SELECT *` for a browsed table's current filter / sort: the
/// unbounded view the page is a window onto (no LIMIT / OFFSET, so the new table
/// carries every matching row).
pub(crate) fn browse_equivalent_select(
    cfg: &ConnectionConfig,
    schema: &str,
    table: &str,
    filter: &str,
    order_by: Option<&str>,
) -> String {
    let schema_opt = (!schema.trim().is_empty()).then_some(schema);
    let qualified = qualified_table_name(Some(cfg.db_type), schema_opt, table);
    let mut sql = format!("SELECT * FROM {qualified}");
    let predicate = normalize_where_input(Some(filter));
    if !predicate.is_empty() {
        sql.push_str(&format!(" WHERE ({predicate})"));
    }
    if let Some(o) = order_by.map(str::trim).filter(|o| !o.is_empty()) {
        sql.push_str(&format!(" ORDER BY {o}"));
    }
    sql
}

/// Build the CTAS for `name` over `source`. The identifier is quoted / escaped
/// per dialect, so a name containing the quote character is safe. `None` for a
/// blank name.
pub(crate) fn materialize_ctas_sql(
    db_type: DatabaseType,
    name: &str,
    source: &str,
) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let quoted = quote_table_identifier(Some(db_type), name);
    Some(format!("CREATE TABLE {quoted} AS {source}"))
}

/// True when `g m` is available: a SQL data grid on a writable connection with a
/// resolvable source. Redis / MongoDB grids, the structure view and read-only
/// connections hide the action.
pub(crate) fn materialize_available(app: &App) -> bool {
    if app.backend_kind != Backend::Sql {
        return false;
    }
    let Some(cfg) = app.selected.as_ref() else {
        return false;
    };
    if cfg.read_only {
        return false;
    }
    if !matches!(app.grid_kind, GridKind::Query | GridKind::TableData) {
        return false;
    }
    materialize_grid_rows(app).is_some() && materialize_source_sql(app).is_some()
}

/// Why `g m` is unavailable, for the status line (bilingual).
fn materialize_unavailable_status(app: &App) -> &'static str {
    if app.backend_kind != Backend::Sql {
        return t("✗ 物化仅支持 SQL 结果");
    }
    let Some(cfg) = app.selected.as_ref() else {
        return t("✗ 未选择连接");
    };
    if cfg.read_only {
        return t("✗ 只读连接不能物化");
    }
    if !matches!(app.grid_kind, GridKind::Query | GridKind::TableData) {
        return t("✗ 物化仅支持结果网格（结构视图不可用）");
    }
    if materialize_grid_rows(app).is_none() {
        return t("当前没有可物化的结果");
    }
    t("当前结果没有可物化的来源 SQL")
}

/// `g m` — open the table-name prompt for the current result set.
pub(crate) fn open_materialize_prompt(app: &mut App) {
    if !materialize_available(app) {
        app.status = materialize_unavailable_status(app).into();
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(rows) = materialize_grid_rows(app) else {
        return;
    };
    let Some(source) = materialize_source_sql(app) else {
        return;
    };
    let name = default_materialize_name();
    let mut input = TextArea::from([name.clone()]);
    input.set_placeholder_text(t("新表名"));
    input.move_cursor(CursorMove::End);
    app.materialize_prompt = Some(MaterializePrompt {
        input,
        source_sql: source,
        rows,
        db_type: cfg.db_type,
    });
    app.status = tf(
        "物化结果集为表 · 输入表名 · Enter 确认 · Esc 取消（预填 {}）",
        &[&name],
    );
}

/// The table-name prompt's key handler: Enter submits, Esc cancels, every other
/// key edits the buffer.
pub(crate) fn materialize_prompt_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => submit_materialize(app),
        KeyCode::Esc => {
            app.materialize_prompt = None;
            app.flash(t("已取消物化").into());
        }
        _ => {
            if let Some(mp) = app.materialize_prompt.as_mut() {
                mp.input.input(k);
            }
        }
    }
}

/// Enter in the prompt: build the CTAS and route it through the red confirmation
/// layer. Never executed here.
pub(crate) fn submit_materialize(app: &mut App) {
    let Some(mp) = app.materialize_prompt.take() else {
        return;
    };
    let name = mp.input.lines().join(" ").trim().to_string();
    let Some(sql) = materialize_ctas_sql(mp.db_type, &name, &mp.source_sql) else {
        app.status = t("✗ 表名不能为空").into();
        // Keep the prompt open so the user can correct the name.
        app.materialize_prompt = Some(mp);
        return;
    };
    if readonly_block(app, &sql) {
        return;
    }
    app.pending_scope = None;
    app.materialize_write = Some(MaterializePlan {
        name: name.clone(),
        rows: mp.rows,
        ctas_sql: sql.clone(),
    });
    app.confirm = Some(Confirm {
        sql,
        reasons: vec![tf("物化结果集 → 表 {}", &[&name])],
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: None,
        mongo: None,
    });
    app.status = t("物化确认 · Enter 执行 · Esc 取消").into();
}

/// A confirmed CTAS landed: name the new table, refresh the sidebar table list
/// in place (the grid stays on screen — no auto-jump), and hold the status so
/// the table-list reply cannot overwrite it.
pub(crate) fn apply_materialize_success(app: &mut App, tx: &Tx, affected: u64) {
    let Some(plan) = app.materialize_write.take() else {
        return;
    };
    let rows = if affected > 0 {
        affected
    } else {
        plan.rows as u64
    };
    let msg = tf("已物化 {} · {} 行", &[&plan.name, &rows]);
    app.status = msg.clone();
    app.pending_materialize_msg = Some(msg);
    // Reuse the existing table-list refresh so the new table appears in the
    // sidebar; the grid on screen is left untouched.
    if let Some(cfg) = app.selected.clone() {
        let db = app.current_db();
        let schema = app.schema.clone();
        spawn_list_tables(app, tx, Box::new(cfg), db, schema);
    }
}
