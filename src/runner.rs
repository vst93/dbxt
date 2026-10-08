use crate::prelude::*;
use crate::*;

/// Ctrl-S — package every queued edit into one transaction and ask for
/// confirmation. The full `BEGIN … COMMIT` script is shown in the red layer;
/// nothing runs until Enter (Esc keeps the queue intact).
pub(crate) fn commit_batch(app: &mut App) {
    if app.batch.is_empty() {
        app.status = t("批量队列为空（编辑时按 Ctrl-T 加入）").into();
        return;
    }
    let n = app.batch.len();
    let mut script = String::from("BEGIN;\n");
    for s in &app.batch {
        let s = s.trim().trim_end_matches(';');
        script.push_str(s);
        script.push_str(";\n");
    }
    script.push_str("COMMIT;");
    if readonly_block(app, &script) {
        return;
    }
    app.confirm = Some(Confirm {
        sql: script,
        reasons: vec![
            tf(
                "批量事务：{} 条修改将在同一个 BEGIN … COMMIT 中执行",
                &[&(n)],
            ),
            t("任一语句失败则整体回滚；Enter 后立即执行").into(),
        ],
        refresh: true,
        clear_batch: true,
        conn: None,
        redis: None,
        mongo: None,
    });
    app.status = tf("批量提交确认（{} 条）· Enter 执行 · Esc 取消", &[&(n)]);
}

// ── filter / sort ──

pub(crate) fn open_filter_prompt(app: &mut App) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持过滤").into();
        return;
    }
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    // Prefill with the focused column so a filter is one `f` away; an existing
    // filter is loaded for editing instead.
    let initial = if ps.filter.trim().is_empty() {
        match (active_grid(app), app.selected.as_ref()) {
            (Some(grid), Some(cfg)) => match grid.columns.get(app.col_cursor) {
                Some(col) => format!("{} = ", quote_table_identifier(Some(cfg.db_type), col)),
                None => String::new(),
            },
            _ => String::new(),
        }
    } else {
        ps.filter.clone()
    };
    let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
    ta.set_placeholder_text(t("例: city = 'Beijing'（留空回车 = 清除）"));
    ta.move_cursor(CursorMove::End);
    app.filter_prompt = Some(ta);
}

/// Ctrl-R — drop the active filter and reload the first page.
pub(crate) fn clear_filter(app: &mut App, tx: &Tx) {
    let has_filter = app
        .page_state
        .as_ref()
        .map(|p| !p.filter.trim().is_empty())
        .unwrap_or(false);
    if !has_filter {
        app.status = t("当前无过滤条件").into();
        return;
    }
    let order_by = app.page_state.as_ref().and_then(|p| p.order_by.clone());
    reload_table_view(app, tx, String::new(), order_by, 0);
    app.status = t("过滤已清除").into();
}

/// `s` — sort by the focused column, toggling ASC ↔ DESC.
/// Parse a generated ORDER BY expression into `(column, desc)` keys.
pub(crate) fn parse_order_by(order_by: Option<&str>) -> Vec<(String, bool)> {
    let Some(o) = order_by else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in o.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, desc) = if let Some(n) = part
            .strip_suffix(" DESC")
            .or_else(|| part.strip_suffix(" desc"))
        {
            (n.trim(), true)
        } else if let Some(n) = part
            .strip_suffix(" ASC")
            .or_else(|| part.strip_suffix(" asc"))
        {
            (n.trim(), false)
        } else {
            (part, false)
        };
        out.push((unquote_ident(name), desc));
    }
    out
}

/// Strip the dialect quoting from a single identifier.
pub(crate) fn unquote_ident(s: &str) -> String {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 {
        let (a, b) = (chars[0], chars[chars.len() - 1]);
        if (a == '`' && b == '`') || (a == '"' && b == '"') || (a == '[' && b == ']') {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    s.to_string()
}

pub(crate) fn build_order_by(cfg: &ConnectionConfig, keys: &[(String, bool)]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let q = |n: &str| quote_table_identifier(Some(cfg.db_type), n);
    Some(
        keys.iter()
            .map(|(c, d)| format!("{} {}", q(c), if *d { "DESC" } else { "ASC" }))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

pub(crate) fn sort_column(app: &mut App, tx: &Tx, append: bool) {
    if !in_table_data_view(app) {
        app.status = t("仅表格浏览支持排序").into();
        return;
    }
    let Some(grid) = active_grid(app) else {
        return;
    };
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(col) = grid.columns.get(app.col_cursor).cloned() else {
        return;
    };
    let mut keys = parse_order_by(ps.order_by.as_deref());
    let existing = keys.iter().position(|(c, _)| c == &col);
    if append {
        match existing {
            Some(i) => keys[i].1 = !keys[i].1,
            None => keys.push((col.clone(), false)),
        }
    } else {
        // Single-key sort: toggle direction when this column is already the
        // only sort key, otherwise replace the sort with this column ascending.
        let dir = match keys.first() {
            Some((c, d)) if c == &col && keys.len() == 1 => !*d,
            _ => false,
        };
        keys = vec![(col.clone(), dir)];
    }
    let next = build_order_by(&cfg, &keys);
    let dir = if keys.first().map(|(_, d)| *d).unwrap_or(false) {
        t("降序")
    } else {
        t("升序")
    };
    // Persist the sort for this table so reopening it restores the order.
    let db = app.current_db();
    app.config.entry(&db, &ps.schema, &ps.table).order_by = next.clone();
    app.persist();
    reload_table_view(app, tx, ps.filter.clone(), next, 0);
    app.status = tf(
        "按 {} {}{}",
        &[
            &(col),
            &(dir),
            &(if append {
                t("（附加排序键）")
            } else {
                ""
            }),
        ],
    );
}

pub(crate) fn drill_script(app: &mut App, idx: usize) {
    if let Some(s) = &mut app.script {
        if idx < s.outcomes.len() {
            s.drilled = Some(idx);
            app.sel = 0;
            app.col_offset = 0;
            app.col_cursor = 0;
        }
    }
}

/// What the run keys should execute when there is no selection.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RunScope {
    /// `F5`: the whole editor.
    All,
    /// `Ctrl-J` / `Alt-Enter`: only the statement under the cursor.
    CurrentStatement,
}

pub(crate) fn run_current(app: &mut App, tx: &Tx) {
    run_current_scoped(app, tx, RunScope::All);
}

pub(crate) fn run_current_scoped(app: &mut App, tx: &Tx, scope: RunScope) {
    match app.backend_kind {
        Backend::Sql => run_sql(app, tx, scope),
        Backend::Redis | Backend::Mongo => run_cmd_line(app, tx),
    }
}

/// R88: the editor's current selection as a trimmed char-offset span
/// `(lo, hi)` into the flattened buffer, or `None` when nothing usable is
/// selected. Shift+arrows build the selection inside tui-textarea.
pub(crate) fn editor_selection_span(app: &App) -> Option<(usize, usize)> {
    let ((r1, c1), (r2, c2)) = app.editor.selection_range()?;
    let text = app.editor_sql();
    let a = text_offset(&text, r1, c1)?;
    let b = text_offset(&text, r2, c2)?;
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let chars: Vec<char> = text.chars().collect();
    let (mut s, mut e) = (lo, hi);
    while s < e && chars.get(s).is_some_and(|c| c.is_whitespace()) {
        s += 1;
    }
    while e > s && chars.get(e - 1).is_some_and(|c| c.is_whitespace()) {
        e -= 1;
    }
    (s < e).then_some((s, e))
}

/// The editor's current selection, trimmed, or `None` when nothing (usable) is
/// selected. Shift+arrows build the selection inside tui-textarea.
pub(crate) fn editor_selection_text(app: &App) -> Option<String> {
    let (s, e) = editor_selection_span(app)?;
    let text = app.editor_sql();
    let sel: String = text.chars().skip(s).take(e - s).collect();
    (!sel.trim().is_empty()).then_some(sel)
}

/// `Alt-↓` / `Alt-↑` in the editor: move the cursor to the start of the next /
/// previous statement (semicolon-delimited, literals and comments ignored).
/// Empty statements are skipped by construction (the splitter never yields
/// one), and a leading comment block is stepped over so the caret lands on the
/// statement's first token. The status line reports `语句 i/n`.
pub(crate) fn jump_statement(app: &mut App, dir: i32) -> bool {
    let text = app.editor_sql();
    let chars: Vec<char> = text.chars().collect();
    let ranges = statement_ranges(&text);
    let n = ranges.len();
    if n == 0 {
        app.status = t("编辑器里没有语句").into();
        return true;
    }
    let (row, col) = app.editor.cursor();
    let off = text_offset(&text, row, col).unwrap_or(chars.len());
    let cur = statement_index_at(&ranges, off).unwrap_or(0);
    let target = if dir > 0 {
        if cur + 1 >= n {
            app.status = tf("已是最后一条语句（共 {} 条）", &[&n]);
            return true;
        }
        cur + 1
    } else {
        if cur == 0 {
            app.status = t("已是第一条语句").into();
            return true;
        }
        cur - 1
    };
    let (s, e) = ranges[target];
    let mask = code_mask(&chars);
    let at = statement_code_start(&chars, &mask, s, e);
    let (r, c) = offset_to_cursor(&text, at);
    app.editor.move_cursor(CursorMove::Jump(r as u16, c as u16));
    app.status = tf("语句 {}/{}", &[&(target + 1), &n]);
    true
}

/// The statement under the editor cursor as `(span, 0-based index)`, using the
/// same gap rule as [`statement_range_at`] (a caret parked on a separator or in
/// the whitespace between statements belongs to the following one).
pub(crate) fn current_statement_span(app: &App) -> Option<((usize, usize), usize)> {
    let text = app.editor_sql();
    let (row, col) = app.editor.cursor();
    let off = text_offset(&text, row, col)?;
    let ranges = statement_ranges(&text);
    let (s, e) = statement_range_at(&ranges, off)?;
    let idx = ranges.iter().position(|&r| r == (s, e))?;
    Some(((s, e), idx))
}

/// The statement under the editor cursor (semicolon-delimited, literals and
/// comments ignored), trimmed, or `None` when the cursor is not on a statement.
pub(crate) fn current_statement_text(app: &App) -> Option<String> {
    let ((s, e), _) = current_statement_span(app)?;
    let text = app.editor_sql();
    let stmt: String = text.chars().skip(s).take(e - s).collect();
    (!stmt.trim().is_empty()).then_some(stmt)
}

/// R88: the metadata for a scoped run — the buffer spans/ordinals of the
/// statements it sent, plus the `执行第 N 条` status label. `None` for a
/// whole-buffer run (`F5` with no selection). Pure; built before the query is
/// spawned so the result can name exactly what ran.
pub(crate) fn build_scoped_run(
    app: &App,
    sql: &str,
    sel_span: Option<(usize, usize)>,
    scope: RunScope,
    db_type: DatabaseType,
) -> Option<ScopedRun> {
    if sel_span.is_none() && scope == RunScope::All {
        return None;
    }
    let text = app.editor_sql();
    let statements = dbx_core::sql::split_sql_statements_for_database(sql, db_type);
    if statements.is_empty() {
        return None;
    }
    if let Some(sel) = sel_span {
        // A selection may start / end mid-statement; a statement counts only
        // when its buffer span actually overlaps what was highlighted.
        let located = locate_statement_indices(&text, &statements);
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(located.len());
        let mut ordinals: Vec<usize> = Vec::with_capacity(located.len());
        for ent in &located {
            match ent {
                Some((idx, sp)) if sp.0 < sel.1 && sp.1 > sel.0 => {
                    spans.push(Some(*sp));
                    ordinals.push(idx + 1);
                }
                _ => {
                    spans.push(None);
                    ordinals.push(0);
                }
            }
        }
        let mut ords: Vec<usize> = ordinals.iter().copied().filter(|n| *n > 0).collect();
        if ords.is_empty() {
            return Some(ScopedRun {
                sql: sql.trim().to_string(),
                spans,
                ordinals,
                label: t("执行选区").to_string(),
            });
        }
        ords.sort_unstable();
        let label = if ords.len() == 1 {
            tf("执行第 {} 条", &[&ords[0]])
        } else {
            tf("执行第 {}-{} 条", &[&ords[0], &ords[ords.len() - 1]])
        };
        Some(ScopedRun {
            sql: sql.trim().to_string(),
            spans,
            ordinals,
            label,
        })
    } else {
        let ((s, e), idx) = current_statement_span(app)?;
        Some(ScopedRun {
            sql: sql.trim().to_string(),
            spans: vec![Some((s, e))],
            ordinals: vec![idx + 1],
            label: tf("执行第 {} 条", &[&(idx + 1)]),
        })
    }
}

/// R88: consume the scoped-run label for the result status, but only when it
/// belongs to `sql` (so a `Ctrl-N` load-more or an `EXPLAIN` re-run never
/// inherits the previous run's label).
pub(crate) fn take_scope_label(app: &mut App, sql: &str) -> Option<String> {
    let matches = app
        .pending_scope
        .as_ref()
        .is_some_and(|s| s.sql == sql.trim());
    if matches {
        app.pending_scope.take().map(|s| s.label)
    } else {
        None
    }
}

pub(crate) fn run_sql(app: &mut App, tx: &Tx, scope: RunScope) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    // R41/R88: a selection always wins (run only what is highlighted); with no
    // selection, `Ctrl-J` / `Alt-Enter` narrow to the statement under the cursor
    // while `F5` keeps running the whole editor.
    let sel_span = editor_selection_span(app);
    let sql = if sel_span.is_some() {
        editor_selection_text(app).unwrap_or_default()
    } else {
        match scope {
            RunScope::All => app.editor_sql().trim().to_string(),
            RunScope::CurrentStatement => match current_statement_text(app) {
                Some(stmt) => stmt,
                None => {
                    app.status = t("光标处没有可执行的语句").into();
                    return;
                }
            },
        }
    };
    if sql.is_empty() {
        return;
    }
    // R88: capture the buffer spans / ordinals of what this run sends, so the
    // result can label `执行第 N 条` and a failure can localize back to exactly
    // these statements. Computed before any early return below.
    let scope_info = build_scoped_run(app, &sql, sel_span, scope, cfg.db_type);
    // Read-only connections refuse every write before the danger layer, so a
    // blocked statement never even reaches the red confirmation.
    if readonly_block(app, &sql) {
        return;
    }
    // Danger check runs per statement so `UPDATE a; DELETE FROM b;` is caught too.
    let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
    let mut reasons: Vec<String> = Vec::new();
    for st in &statements {
        if let Some(r) = detect_danger(st) {
            if !reasons.contains(&r) {
                reasons.push(r);
            }
        }
    }
    if !reasons.is_empty() {
        app.pending_run_origin = "editor";
        app.pending_scope = scope_info;
        app.confirm = Some(Confirm {
            sql,
            reasons,
            refresh: false,
            clear_batch: false,
            conn: None,
            redis: None,
            mongo: None,
        });
        return;
    }
    app.push_history(&sql);
    app.pending_scope = scope_info;
    execute_sql(app, tx, sql, "editor");
}

pub(crate) fn execute_sql(app: &mut App, tx: &Tx, sql: String, origin: &'static str) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    // The single write choke point: every path (editor, file script, history
    // direct run, row edit, batch commit, red-confirm accept) lands here, so a
    // read-only connection is enforced even if a caller forgot its own check.
    if readonly_block(app, &sql) {
        return;
    }
    app.last_executed = Some(sql.trim().to_string());
    app.loading = true;
    // A history direct run gets a distinct landing status that names its elapsed
    // time once the result arrives (R45).
    app.direct_run = origin == "direct";
    app.status = t("执行中…").into();
    let db = app.current_db();
    // R99: stamp the run with a fresh per-connection epoch so Esc can soft-cancel
    // it and a later reply can be recognised as stale.
    let epoch = app.register_query(&cfg, sql.clone());
    app.spawn(
        tx,
        Op::Query(Box::new(cfg), db, sql, QUERY_MAX_ROWS, origin, epoch),
    );
}

/// R99: soft-cancel the active connection's in-flight query. Returns `true`
/// when the Esc key was consumed — either because the run was soft-cancelled
/// (UI control returns at once; the late reply is dropped) or because it was a
/// write, which must never be soft-cancelled (double-write risk). Returns
/// `false` when no query is in flight, so Esc keeps its normal meaning.
pub(crate) fn soft_cancel_active_query(app: &mut App) -> bool {
    let Some(cfg) = app.selected.clone() else {
        return false;
    };
    let Some(run) = app.queries_running.get(&cfg.id).cloned() else {
        return false;
    };
    // A write (INSERT / UPDATE / DELETE / DDL / an undetermined verb) is never
    // soft-cancelled: the user must not mistake "still running" for "never ran"
    // and re-send it. The classification was done once at launch.
    if !run.cancellable {
        app.status = t("写操作执行中，不可取消").into();
        return true;
    }
    // Bump the connection's epoch so the in-flight reply is stale on arrival,
    // release the UI at once, and free this op's spinner slot (the late reply
    // must not decrement it a second time).
    let _ = app.bump_cancel_epoch(&cfg.id);
    app.queries_running.remove(&cfg.id);
    *app.query_slots_released.entry(cfg.id.clone()).or_insert(0) += 1;
    app.pending_ops = app.pending_ops.saturating_sub(1);
    app.loading = false;
    app.loading_since = None;
    app.status = t("已取消，结果将在后台丢弃").into();
    true
}

/// `Ctrl-N`: re-run the last query with a larger row cap when it was truncated.
pub(crate) fn load_more_rows(app: &mut App, tx: &Tx) {
    let Some((sql, cap)) = app.query_more.clone() else {
        app.status = t("没有可加载的更多结果（结果未截断）").into();
        return;
    };
    if cap >= QUERY_MAX_ROWS_CAP {
        app.status = tf(
            "已达上限 {} 行，请用 WHERE / LIMIT 缩小查询",
            &[&(QUERY_MAX_ROWS_CAP)],
        );
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let next = (cap + QUERY_MORE_STEP).min(QUERY_MAX_ROWS_CAP);
    app.loading = true;
    // R88: a load-more re-runs the same SQL; it must never inherit the scoped
    // run's `执行第 N 条` label.
    app.pending_scope = None;
    app.status = tf("加载更多… (上限 {} 行)", &[&(next)]);
    let db = app.current_db();
    let epoch = app.register_query(&cfg, sql.clone());
    app.spawn(tx, Op::Query(Box::new(cfg), db, sql, next, "editor", epoch));
}

pub(crate) fn run_cmd_line(app: &mut App, tx: &Tx) {
    let cmd = app.cmd_input.lines().join(" ").trim().to_string();
    if cmd.is_empty() {
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.cmd_input = TextArea::default();
    app.set_placeholder();
    app.loading = true;
    match app.backend_kind {
        Backend::Redis => {
            // Show the console output rather than a stale value grid.
            app.redis_value = None;
            app.clear_grid();
            app.cmd_output
                .push(format!("redis[{}]> {cmd}", app.redis_db));
            app.spawn(tx, Op::Redis(Box::new(cfg), app.redis_db, cmd));
        }
        Backend::Mongo => {
            // Show the console output rather than a stale document grid.
            app.clear_grid();
            app.page_state = None;
            // `use dbname` switches the mongo database locally. The selected
            // database (not a separate field) is what gets passed to every
            // command, so the shell prompt and the executed op can never drift.
            if let Some(db) = cmd.strip_prefix("use ") {
                let db = db.trim().trim_end_matches(';').trim().to_string();
                if db.is_empty() {
                    app.status = t("✗ use 需要数据库名").into();
                    app.loading = false;
                    return;
                }
                app.select_database(&db);
                app.cmd_output.push(format!("switched to db {db}"));
                app.set_placeholder();
                app.loading = false;
                reload_tables(app, tx);
                return;
            }
            app.cmd_output
                .push(format!("mongo({})> {cmd}", app.current_db()));
            app.spawn(tx, Op::Mongo(Box::new(cfg), app.current_db(), cmd));
        }
        Backend::Sql => run_sql(app, tx, RunScope::All),
    }
}

// ── new connection form ──

pub(crate) fn form_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // R119: an open option list owns the keyboard until it is committed / closed.
    if app.form.picker.is_some() {
        form_picker_key(app, k);
        return;
    }
    // The row list shrinks when the SSH tunnel is toggled off or the auth method
    // changes; keep the cursor on a real row.
    let len = form_rows(&app.form).len().max(1);
    if app.form.field >= len {
        app.form.field = len - 1;
    }
    let cur = form_rows(&app.form)
        .get(app.form.field)
        .map(|r| r.0)
        .unwrap_or(FormRow::Save);

    if app.form.editing {
        match k.code {
            KeyCode::Enter | KeyCode::Esc => {
                if k.code == KeyCode::Esc {
                    // cancel: clear (or reset) the current field
                    if let Some(s) = form_text_mut(&mut app.form, cur) {
                        if cur == FormRow::DbType {
                            *s = "mysql".into();
                        } else {
                            s.clear();
                        }
                    }
                }
                app.form.editing = false;
                if cur == FormRow::Color {
                    app.form.color_sel = color_sel_for(&app.form.color);
                }
                // R51: leaving the type field re-derives the default port.
                if cur == FormRow::DbType {
                    apply_default_port(&mut app.form);
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = form_text_mut(&mut app.form, cur) {
                    s.pop();
                }
                if cur == FormRow::Port {
                    app.form.port_touched = true;
                }
                if cur == FormRow::DbType {
                    apply_default_port(&mut app.form);
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = form_text_mut(&mut app.form, cur) {
                    match cur {
                        FormRow::Port | FormRow::SshPort if !c.is_ascii_digit() => {}
                        FormRow::QueryTimeout if !c.is_ascii_digit() => {}
                        FormRow::DbType => s.push(c.to_ascii_lowercase()),
                        // Colour row only accepts `#` and hex digits.
                        FormRow::Color if !(c.is_ascii_hexdigit() || c == '#') => {}
                        _ => s.push(c),
                    }
                }
                // R51: the type drives the port until the user edits the port.
                if cur == FormRow::Port {
                    app.form.port_touched = true;
                }
                if cur == FormRow::DbType {
                    apply_default_port(&mut app.form);
                }
            }
            _ => {}
        }
        return;
    }
    match k.code {
        KeyCode::Esc => {
            app.page = Page::Browse;
            app.focus = Focus::Sidebar;
        }
        KeyCode::Up => app.form.field = (app.form.field + len - 1) % len,
        KeyCode::Down | KeyCode::Tab => app.form.field = (app.form.field + 1) % len,
        KeyCode::Enter => match cur {
            FormRow::Ssl => app.form.ssl = !app.form.ssl,
            FormRow::ReadOnly => app.form.read_only = !app.form.read_only,
            FormRow::SshEnabled => app.form.ssh_enabled = !app.form.ssh_enabled,
            // R119: Enter opens the option list for closed sets; Space still
            // cycles the three SSH auth methods for a quick change.
            FormRow::SshAuth => open_form_picker(app, cur),
            FormRow::DbType => open_form_picker(app, cur),
            FormRow::Save => save_form(app, tx),
            _ => app.form.editing = true,
        },
        KeyCode::Char(' ') => match cur {
            FormRow::Ssl => app.form.ssl = !app.form.ssl,
            FormRow::ReadOnly => app.form.read_only = !app.form.read_only,
            FormRow::SshEnabled => app.form.ssh_enabled = !app.form.ssh_enabled,
            FormRow::SshAuth => app.form.ssh_auth = app.form.ssh_auth.next(),
            FormRow::DbType => open_form_picker(app, cur),
            // Space cycles the colour palette; the custom stop opens the hex editor.
            FormRow::Color => {
                let next = color_next_sel(app.form.color_sel);
                app.form.color_sel = next;
                app.form.color = color_value_for(next, &app.form.color);
                if next == CONN_COLOR_CUSTOM {
                    app.form.editing = true;
                }
            }
            FormRow::Save => save_form(app, tx),
            FormRow::Port | FormRow::SshPort | FormRow::QueryTimeout => {}
            _ => {
                app.form.editing = true;
                if let Some(s) = form_text_mut(&mut app.form, cur) {
                    s.push(' ');
                }
            }
        },
        KeyCode::Left | KeyCode::Char('h') => app.form.field = (app.form.field + len - 1) % len,
        KeyCode::Right | KeyCode::Char('l') => app.form.field = (app.form.field + 1) % len,
        _ => {}
    }
}

/// R119: open the option list for a picker row, highlighting the current value.
/// No-op for rows that are not closed sets.
pub(crate) fn open_form_picker(app: &mut App, row: FormRow) {
    if !form_row_is_picker(row) {
        return;
    }
    let items = form_picker_items(row);
    if items.is_empty() {
        return;
    }
    let current = form_picker_value(&app.form, row);
    let sel = items.iter().position(|(_, v)| *v == current).unwrap_or(0);
    app.form.picker = Some(FormPicker {
        row,
        items,
        sel,
        filter: String::new(),
    });
}

/// R119: keys for the modal option list — typing filters, ↑↓/Tab move, Enter
/// commits, Esc cancels. Everything else is swallowed so a stray key never
/// edits the form underneath.
pub(crate) fn form_picker_key(app: &mut App, k: KeyEvent) {
    let Some(mut picker) = app.form.picker.take() else {
        return;
    };
    let row = picker.row;
    match k.code {
        KeyCode::Esc => return,
        KeyCode::Enter => {
            let chosen = picker
                .matches()
                .get(picker.sel)
                .map(|(_, _, value)| value.to_string());
            if let Some(value) = chosen {
                form_apply_picker(&mut app.form, row, &value);
            }
            return;
        }
        KeyCode::Up => {
            let n = picker.matches().len();
            if n > 0 {
                picker.sel = (picker.sel + n - 1) % n;
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            let n = picker.matches().len();
            if n > 0 {
                picker.sel = (picker.sel + 1) % n;
            }
        }
        KeyCode::Backspace => {
            picker.filter.pop();
            picker.sel = 0;
        }
        KeyCode::Char(c) => {
            picker.filter.push(c);
            picker.sel = 0;
        }
        _ => {}
    }
    app.form.picker = Some(picker);
}

/// Serialize the SSH section of the form into a kernel [`SshTunnelConfig`].
/// Returns `Ok(None)` when the tunnel is disabled, and a user-facing message on
/// a validation failure (the form stays open).
pub(crate) fn build_ssh_layer(f: &ConnForm) -> Result<Option<SshTunnelConfig>, String> {
    if !f.ssh_enabled {
        return Ok(None);
    }
    let host = f.ssh_host.trim();
    if host.is_empty() {
        return Err(t("SSH 主机必填").into());
    }
    let user = f.ssh_user.trim();
    if user.is_empty() {
        return Err(t("SSH 用户必填").into());
    }
    let port = f.ssh_port.trim().parse::<u16>().unwrap_or(22);
    if port == 0 {
        return Err(t("SSH 端口无效（1-65535）").into());
    }
    let (password, key_path, key_passphrase, use_ssh_agent, agent_sock) = match f.ssh_auth {
        SshAuth::Password => {
            if f.ssh_password.is_empty() {
                return Err(t("SSH 密码为空（或改用密钥 / agent）").into());
            }
            (
                f.ssh_password.clone(),
                String::new(),
                String::new(),
                false,
                String::new(),
            )
        }
        SshAuth::Key => {
            if f.ssh_key_path.trim().is_empty() {
                return Err(t("SSH 密钥路径必填").into());
            }
            (
                String::new(),
                f.ssh_key_path.trim().to_string(),
                f.ssh_key_passphrase.clone(),
                false,
                String::new(),
            )
        }
        SshAuth::Agent => (
            String::new(),
            String::new(),
            String::new(),
            true,
            f.ssh_agent_sock.trim().to_string(),
        ),
    };
    Ok(Some(SshTunnelConfig {
        id: Uuid::new_v4().to_string(),
        name: if f.name.trim().is_empty() {
            "SSH".into()
        } else {
            format!("{} · SSH", f.name.trim())
        },
        enabled: true,
        host: host.to_string(),
        port,
        user: user.to_string(),
        password,
        key_path,
        key_passphrase,
        connect_timeout_secs: dbx_core::models::connection::default_ssh_connect_timeout_secs(),
        expose_lan: false,
        use_ssh_agent,
        ssh_agent_sock_path: agent_sock,
        auth_method: f.ssh_auth.as_str().to_string(),
        allow_exec_channel_proxy: false,
        proxy_command: String::new(),
        profile_id: String::new(),
    }))
}

pub(crate) fn save_form(app: &mut App, tx: &Tx) {
    let f = app.form.clone();
    if f.host.trim().is_empty() {
        app.form.err = t("host 必填").into();
        return;
    }
    let Ok(db_type) = parse_database_type(&f.db_type) else {
        app.form.err = tf(
            "未知类型: {} (mysql / postgres / redis / mongodb …)",
            &[&(f.db_type)],
        );
        return;
    };
    // R51: a blank connection name is generated as `host-db_type` instead of
    // being rejected, so the picker stays readable without a manual name.
    let name = if f.name.trim().is_empty() {
        auto_conn_name(f.host.trim(), &f.db_type)
    } else {
        f.name.trim().to_string()
    };
    let port = f
        .port
        .trim()
        .parse::<u16>()
        .ok()
        .or_else(|| dbx_core::database_manifest::default_port(&db_type))
        .unwrap_or(0);
    let ssh_layer = match build_ssh_layer(&f) {
        Ok(layer) => layer,
        Err(e) => {
            app.form.err = e;
            return;
        }
    };
    // Colour is written from the form on both create and edit, so a colour set
    // here survives and a desktop-set colour is kept when editing (prefilled).
    let color = match normalize_conn_color(&f.color) {
        Ok(c) => c,
        Err(()) => {
            app.form.err = t("颜色需为 #RRGGBB").into();
            return;
        }
    };
    // R58: query timeout. Blank keeps the kernel default (60 s); `0` is DBX's
    // "no limit"; a positive number is stored on the config and, on PostgreSQL,
    // mirrored into the connection's `statement_timeout` option.
    if let Err(()) = parse_query_timeout(&f.query_timeout) {
        app.form.err = t("查询超时需为 0-86400 秒（0=不限，留空=默认）").into();
        return;
    }
    // Editing preserves fields the form does not expose (colour, notes, visible
    // databases, driver profile, …) by starting from the saved config.
    let mut cfg = match f
        .edit_id
        .as_ref()
        .and_then(|id| app.connections.iter().find(|c| &c.id == id).cloned())
    {
        Some(existing) => existing,
        None => match new_connection_config(
            Uuid::new_v4().to_string(),
            name.clone(),
            db_type,
            f.host.trim().to_string(),
            port,
            f.username.trim().to_string(),
            f.password.clone(),
            if f.database.trim().is_empty() {
                None
            } else {
                Some(f.database.trim().to_string())
            },
            f.ssl,
            None,
        ) {
            Ok(c) => c,
            Err(e) => {
                app.form.err = e;
                return;
            }
        },
    };
    cfg.name = name;
    cfg.db_type = db_type;
    cfg.host = f.host.trim().to_string();
    cfg.port = port;
    cfg.username = f.username.trim().to_string();
    cfg.password = f.password.clone();
    cfg.database = if f.database.trim().is_empty() {
        None
    } else {
        Some(f.database.trim().to_string())
    };
    cfg.ssl = f.ssl;
    // R119: TLS material. Cleared when SSL is off so a stale path never leaks
    // into the connection URL after the toggle is switched back off.
    if f.ssl {
        cfg.ca_cert_path = f.ssl_ca_cert.trim().to_string();
        cfg.client_cert_path = f.ssl_client_cert.trim().to_string();
        cfg.client_key_path = f.ssl_client_key.trim().to_string();
    } else {
        cfg.ca_cert_path.clear();
        cfg.client_cert_path.clear();
        cfg.client_key_path.clear();
    }
    cfg.read_only = f.read_only;
    cfg.color = color;
    if let Err(()) = apply_form_query_timeout(&mut cfg, &f.query_timeout) {
        app.form.err = t("查询超时需为 0-86400 秒（0=不限，留空=默认）").into();
        return;
    }
    cfg.transport_layers = ssh_layer
        .into_iter()
        .map(TransportLayerConfig::Ssh)
        .collect();
    app.form.err.clear();
    app.loading = true;
    app.status = t("保存连接…").into();
    let op = if f.edit_id.is_some() {
        Op::UpdateConn(Box::new(cfg))
    } else {
        Op::AddConn(Box::new(cfg))
    };
    app.spawn(tx, op);
}

impl App {
    /// Re-sort the connection picker in place for the current `s` mode, keeping
    /// the highlight on the same connection (matched by id). The picker renders
    /// `connections` directly, so an in-place sort keeps `conn_list` indices valid.
    pub(crate) fn sort_connections(&mut self) {
        let keep = self
            .conn_list
            .selected()
            .and_then(|i| self.connections.get(i))
            .map(|c| c.id.clone());
        sort_connection_list(&mut self.connections, self.conn_sort);
        if let Some(id) = keep {
            if let Some(i) = self.connections.iter().position(|c| c.id == id) {
                self.conn_list.select(Some(i));
            }
        }
    }

    /// R39: `s` cycles the sidebar table order. The filter is re-applied so the
    /// sort survives an active `/` filter, and the same table stays selected by
    /// name.
    pub(crate) fn cycle_table_sort(&mut self) {
        self.table_sort = self.table_sort.next();
        apply_table_filter(self);
        let label = self.table_sort.label();
        let n = self.tables.len();
        self.status = tf("表排序：{} · s 切换（名称/类型）· {} 张", &[&label, &n]);
    }

    /// Select `db` in the database list, appending it when it is not present
    /// (MongoDB `use <db>` on a database with no collections yet).
    pub(crate) fn select_database(&mut self, db: &str) {
        match self.databases.iter().position(|d| d == db) {
            Some(i) => self.db_index = i,
            None => {
                self.databases.push(db.to_string());
                self.db_index = self.databases.len() - 1;
            }
        }
    }
    /// When leaving the DDL sub-view with Esc, make sure a field list is visible.
    pub(crate) fn show_first_grid(&mut self) {
        self.struct_view = StructView::Fields;
    }
}
