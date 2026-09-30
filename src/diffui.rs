use crate::prelude::*;
use crate::*;

/// The table `Alt-D` uses as the source: the table open in the results pane
/// when it is focused, else the sidebar selection.
pub(crate) fn diff_source(app: &App) -> Option<(String, String, String)> {
    if app.focus == Focus::Preview {
        if let Some(ps) = &app.page_state {
            if !ps.table.is_empty() {
                return Some((app.current_db(), ps.schema.clone(), ps.table.clone()));
            }
        }
    }
    app.selected_table()
        .map(|t| (app.current_db(), app.schema.clone(), t.name.clone()))
}

/// Open the target picker. The source is the focused table (table mode) or the
/// current database (database mode).
/// Other SQL connections that can serve as a cross-connection diff target.
pub(crate) fn diff_other_connections(app: &App) -> Vec<ConnectionConfig> {
    let cur = app.selected.as_ref().map(|c| c.id.clone());
    app.connections
        .iter()
        .filter(|c| Some(c.id.clone()) != cur && backend_for_connection(c) == Backend::Sql)
        .cloned()
        .collect()
}

/// The schema a target connection's tables are listed under: only schema-aware
/// engines get one (the source schema when set, otherwise `public`), so a MySQL
/// or SQLite target is never handed a bogus schema name.
pub(crate) fn diff_target_schema(cfg: &ConnectionConfig, src_schema: &str) -> String {
    if !schema_picker_engine(cfg.db_type) {
        return String::new();
    }
    if !src_schema.trim().is_empty() {
        return src_schema.to_string();
    }
    "public".to_string()
}

pub(crate) fn open_diff_picker(app: &mut App, mode: DiffPickMode, kind: DiffKind) {
    if app.backend_kind != Backend::Sql || app.selected.is_none() {
        app.status = t("对比仅支持 SQL 连接").into();
        return;
    }
    // Data compare is inherently table-to-table; the database list is a
    // structure-only view.
    let mode = if kind == DiffKind::Data {
        DiffPickMode::Table
    } else {
        mode
    };
    let entries: Vec<String> = match mode {
        DiffPickMode::Table => {
            let Some((_, _, src)) = diff_source(app) else {
                app.status = match kind {
                    DiffKind::Schema => t("先选中一张表再按 Alt-D").into(),
                    DiffKind::Data => t("先选中一张表再按 Alt-K").into(),
                };
                return;
            };
            app.tables
                .iter()
                .filter(|t| t.name != src)
                .map(|t| t.name.clone())
                .collect()
        }
        DiffPickMode::Database => {
            let cur = app.current_db();
            app.databases
                .iter()
                .filter(|d| **d != cur)
                .cloned()
                .collect()
        }
    };
    if entries.is_empty() {
        app.status = match mode {
            DiffPickMode::Table => t("当前库里没有别的表可对比").into(),
            DiffPickMode::Database => t("没有别的数据库可对比").into(),
        };
        return;
    }
    app.diff_gen += 1;
    let gen = app.diff_gen;
    let mut list = ListState::default();
    list.select(Some(0));
    app.diff_picker = Some(DiffPicker {
        mode,
        kind,
        stage: DiffPickStage::Lists,
        list,
        src_entries: entries.clone(),
        entries,
        target_conn: None,
        target_db: String::new(),
        target_schema: String::new(),
        loading: false,
        comparing: false,
        where_input: String::new(),
        gen,
    });
    app.status = match (kind, mode) {
        (DiffKind::Schema, DiffPickMode::Table) => {
            t("选择目标表（源 = 当前表；c 换连接做跨库/跨方言对比）").into()
        }
        (DiffKind::Schema, DiffPickMode::Database) => t("选择目标库（源 = 当前库）").into(),
        (DiffKind::Data, _) => t("选择目标表做数据对比（按主键对齐；c 换连接；w 加 WHERE）").into(),
    };
}

pub(crate) fn diff_picker_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some((mode, stage, kind)) = app.diff_picker.as_ref().map(|p| (p.mode, p.stage, p.kind))
    else {
        return;
    };
    let n = app
        .diff_picker
        .as_ref()
        .map(|p| p.entries.len())
        .unwrap_or(0);
    let comparing = app
        .diff_picker
        .as_ref()
        .map(|p| p.comparing)
        .unwrap_or(false);
    let step = |app: &mut App, delta: i32| {
        if n == 0 {
            return;
        }
        if let Some(p) = app.diff_picker.as_mut() {
            let cur = p.list.selected().unwrap_or(0) as i32;
            let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
            p.list.select(Some(next));
        }
    };
    let back_to_lists = |app: &mut App| {
        // Invalidate any in-flight cross-connection table fetch so its reply is
        // dropped instead of replacing the restored list.
        app.diff_gen += 1;
        let gen = app.diff_gen;
        if let Some(p) = app.diff_picker.as_mut() {
            p.stage = DiffPickStage::Lists;
            p.entries = p.src_entries.clone();
            p.target_conn = None;
            p.target_db.clear();
            p.target_schema.clear();
            p.loading = false;
            p.gen = gen;
            p.list
                .select(if p.entries.is_empty() { None } else { Some(0) });
        }
        app.status = match kind {
            DiffKind::Schema => t("选择目标表（源 = 当前表；c 换连接做跨库/跨方言对比）").into(),
            DiffKind::Data => t("选择目标表做数据对比（按主键对齐；c 换连接；w 加 WHERE）").into(),
        };
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // A running data compare aborts on Esc, keeping the rows found so far.
            if app
                .diff_picker
                .as_ref()
                .map(|p| p.comparing)
                .unwrap_or(false)
            {
                app.data_cancel.store(true, Ordering::Relaxed);
                app.status = t("正在中止数据对比…（保留已比结果）").into();
                return;
            }
            if stage == DiffPickStage::Connections {
                back_to_lists(app);
                return;
            }
            app.diff_picker = None;
            app.flash(t("已取消对比").into());
        }
        // Toggle target kind: table (Alt-D) ↔ database (Shift+Alt-D). Structure only.
        KeyCode::Char('d') | KeyCode::Char('D')
            if kind == DiffKind::Schema && stage == DiffPickStage::Lists && !comparing =>
        {
            let next = if mode == DiffPickMode::Table {
                DiffPickMode::Database
            } else {
                DiffPickMode::Table
            };
            open_diff_picker(app, next, DiffKind::Schema);
        }
        // `m`: switch the picker between structure and data compare.
        KeyCode::Char('m') | KeyCode::Char('M') if stage == DiffPickStage::Lists && !comparing => {
            let next = match kind {
                DiffKind::Schema => DiffKind::Data,
                DiffKind::Data => DiffKind::Schema,
            };
            open_diff_picker(app, mode, next);
        }
        // `w`: type an optional WHERE applied to both sides of a data compare.
        KeyCode::Char('w') | KeyCode::Char('W')
            if kind == DiffKind::Data && stage == DiffPickStage::Lists && !comparing =>
        {
            open_data_where(app);
        }
        // `c`: pick another connection as the diff target (cross-dialect).
        KeyCode::Char('c')
            if mode == DiffPickMode::Table && stage == DiffPickStage::Lists && !comparing =>
        {
            let conns = diff_other_connections(app);
            if conns.is_empty() {
                app.status = t("没有别的 SQL 连接可做跨连接对比").into();
                return;
            }
            let names: Vec<String> = conns
                .iter()
                .map(|c| format!("{} ({})", c.name, c.db_type.as_str()))
                .collect();
            if let Some(p) = app.diff_picker.as_mut() {
                p.stage = DiffPickStage::Connections;
                p.entries = names;
                p.list.select(Some(0));
            }
            app.status = t("选择目标连接（Enter 进入其表列表 · Esc 返回）").into();
        }
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home => {
            if let Some(p) = app.diff_picker.as_mut() {
                if n > 0 {
                    p.list.select(Some(0));
                }
            }
        }
        KeyCode::End => {
            if let Some(p) = app.diff_picker.as_mut() {
                if n > 0 {
                    p.list.select(Some(n - 1));
                }
            }
        }
        KeyCode::Enter => {
            // A running compare ignores Enter (Esc aborts it).
            if app
                .diff_picker
                .as_ref()
                .map(|p| p.comparing)
                .unwrap_or(false)
            {
                return;
            }
            let Some(p) = app.diff_picker.as_ref() else {
                return;
            };
            let Some(idx) = p.list.selected() else {
                return;
            };
            if stage == DiffPickStage::Connections {
                let Some(cfg) = diff_other_connections(app).into_iter().nth(idx) else {
                    return;
                };
                let db = cfg.database.clone().unwrap_or_default();
                let schema = diff_target_schema(&cfg, &app.schema);
                app.diff_gen += 1;
                let gen = app.diff_gen;
                if let Some(p) = app.diff_picker.as_mut() {
                    p.target_conn = Some(Box::new(cfg.clone()));
                    p.target_db = db.clone();
                    p.target_schema = schema.clone();
                    p.loading = true;
                    p.gen = gen;
                    p.stage = DiffPickStage::Lists;
                }
                app.loading = true;
                app.status = tf("加载 {} 的表…", &[&cfg.name]);
                app.spawn(
                    tx,
                    Op::DiffTablesFor {
                        cfg: Box::new(cfg),
                        db,
                        schema,
                        gen,
                    },
                );
                return;
            }
            let Some(choice) = p.entries.get(idx).cloned() else {
                return;
            };
            match (kind, mode) {
                (DiffKind::Schema, DiffPickMode::Table) => {
                    let Some((db, schema, src)) = diff_source(app) else {
                        return;
                    };
                    let Some(src_cfg) = app.selected.clone() else {
                        return;
                    };
                    let (tgt_cfg, tgt_db, tgt_schema) = {
                        let p = app.diff_picker.as_ref().unwrap();
                        match &p.target_conn {
                            Some(c) => {
                                ((**c).clone(), p.target_db.clone(), p.target_schema.clone())
                            }
                            None => (src_cfg.clone(), db.clone(), schema.clone()),
                        }
                    };
                    start_table_diff(
                        app, tx, src_cfg, db, schema, src, tgt_cfg, tgt_db, tgt_schema, choice,
                    );
                }
                (DiffKind::Schema, DiffPickMode::Database) => {
                    let src_db = app.current_db();
                    let schema = app.schema.clone();
                    let tgt_schema = schema.clone();
                    start_db_diff(app, tx, src_db, schema, choice, tgt_schema);
                }
                (DiffKind::Data, _) => {
                    start_selected_data_diff(app, tx);
                }
            }
        }
        _ => {}
    }
}

/// Start a two-table diff (source is the desired structure). The two sides may
/// be on different connections and dialects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_table_diff(
    app: &mut App,
    tx: &Tx,
    src_cfg: ConnectionConfig,
    src_db: String,
    src_schema: String,
    src_table: String,
    tgt_cfg: ConnectionConfig,
    tgt_db: String,
    tgt_schema: String,
    tgt_table: String,
) {
    app.diff_picker = None;
    app.diff = None;
    app.db_diff = None;
    app.diff_gen += 1;
    let gen = app.diff_gen;
    app.loading = true;
    app.status = tf(
        "对比 {} → {}…",
        &[
            &fix_double_encoding(&qualified_display(&src_schema, &src_table)),
            &fix_double_encoding(&qualified_display(&tgt_schema, &tgt_table)),
        ],
    );
    app.spawn(
        tx,
        Op::DiffTable {
            src_cfg: Box::new(src_cfg),
            src_db,
            src_schema,
            src_table,
            tgt_cfg: Box::new(tgt_cfg),
            tgt_db,
            tgt_schema,
            tgt_table,
            gen,
        },
    );
}

/// Start a two-database table-list diff.
pub(crate) fn start_db_diff(
    app: &mut App,
    tx: &Tx,
    src_db: String,
    src_schema: String,
    tgt_db: String,
    tgt_schema: String,
) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.diff_picker = None;
    app.diff = None;
    app.db_diff = None;
    app.diff_gen += 1;
    let gen = app.diff_gen;
    app.loading = true;
    app.status = tf(
        "对比库 {} → {}…",
        &[&fix_double_encoding(&src_db), &fix_double_encoding(&tgt_db)],
    );
    app.spawn(
        tx,
        Op::DiffDatabase {
            src_cfg: Box::new(cfg.clone()),
            src_db,
            src_schema,
            tgt_cfg: Box::new(cfg),
            tgt_db,
            tgt_schema,
            gen,
        },
    );
}

/// Number of selectable rows in the active tab (`Alter` is scroll-only).
pub(crate) fn diff_row_count(state: &SchemaDiffState) -> usize {
    match state.tab {
        DiffTab::Columns => state.diff.cols.len(),
        DiffTab::Indexes => state.diff.idx.len(),
        DiffTab::Alter => 0,
    }
}

pub(crate) fn diff_move(app: &mut App, delta: i32) {
    let Some(state) = app.diff.as_mut() else {
        return;
    };
    if state.tab == DiffTab::Alter {
        state.scroll = (state.scroll as i32 + delta).max(0) as u16;
        return;
    }
    let n = diff_row_count(state);
    if n == 0 {
        return;
    }
    let cur = state.list.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
    state.list.select(Some(next));
}

pub(crate) fn diff_key(app: &mut App, _tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.diff = None;
            app.flash(t("已关闭结构对比").into());
        }
        KeyCode::Char('y') => copy_diff_summary(app),
        KeyCode::Char('g') => {
            if let Some(state) = app.diff.as_mut() {
                if state.alter.is_empty() {
                    state.alter = generate_alter(&state.diff);
                }
                state.tab = DiffTab::Alter;
                state.scroll = 0;
            }
            app.status = t("已生成 ALTER 同步语句（只生成不执行）· Tab 回差异 · Esc 关").into();
        }
        KeyCode::Tab | KeyCode::Char('t') => {
            if let Some(state) = app.diff.as_mut() {
                state.tab = state.tab.next();
                state.scroll = 0;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => diff_move(app, -1),
        KeyCode::Down | KeyCode::Char('j') => diff_move(app, 1),
        KeyCode::PageUp => diff_move(app, -10),
        KeyCode::PageDown => diff_move(app, 10),
        KeyCode::Home => {
            if let Some(state) = app.diff.as_mut() {
                if state.tab == DiffTab::Alter {
                    state.scroll = 0;
                } else if diff_row_count(state) > 0 {
                    state.list.select(Some(0));
                }
            }
        }
        KeyCode::End => {
            if let Some(state) = app.diff.as_mut() {
                let n = diff_row_count(state);
                if state.tab != DiffTab::Alter && n > 0 {
                    state.list.select(Some(n - 1));
                }
            }
        }
        _ => {}
    }
}

/// `y` in the table-diff overlay: copy the plain-text summary.
pub(crate) fn copy_diff_summary(app: &mut App) {
    let Some(state) = app.diff.as_ref() else {
        return;
    };
    let text = diff_summary_text(&state.diff);
    let lines = text.lines().count();
    match clipboard_copy(&text) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制差异摘要（{} 行）· 兜底 {}",
                &[&lines, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制差异摘要（{} 行）· OSC52 剪贴板", &[&lines]),
    }
}

pub(crate) fn db_diff_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = app
        .db_diff
        .as_ref()
        .map(|s| s.diff.entries.len())
        .unwrap_or(0);
    let step = |app: &mut App, delta: i32| {
        if n == 0 {
            return;
        }
        if let Some(state) = app.db_diff.as_mut() {
            let cur = state.list.selected().unwrap_or(0) as i32;
            let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
            state.list.select(Some(next));
        }
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.db_diff = None;
            app.flash(t("已关闭库结构对比").into());
        }
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home => {
            if let Some(state) = app.db_diff.as_mut() {
                if n > 0 {
                    state.list.select(Some(0));
                }
            }
        }
        KeyCode::End => {
            if let Some(state) = app.db_diff.as_mut() {
                if n > 0 {
                    state.list.select(Some(n - 1));
                }
            }
        }
        KeyCode::Enter => {
            // Only a table present on both sides can be opened as a table diff.
            let picked = app.db_diff.as_ref().and_then(|s| {
                let idx = s.list.selected()?;
                let entry = s.diff.entries.get(idx)?.clone();
                Some((
                    entry,
                    s.diff.src_db.clone(),
                    s.diff.src_schema.clone(),
                    s.diff.tgt_db.clone(),
                    s.diff.tgt_schema.clone(),
                ))
            });
            let Some((entry, src_db, src_schema, tgt_db, tgt_schema)) = picked else {
                return;
            };
            match entry.mark {
                DbTableMark::Both => {
                    let Some(cfg) = app.selected.clone() else {
                        return;
                    };
                    start_table_diff(
                        app,
                        tx,
                        cfg.clone(),
                        src_db,
                        src_schema,
                        entry.table.clone(),
                        cfg,
                        tgt_db,
                        tgt_schema,
                        entry.table,
                    )
                }
                DbTableMark::OnlySrc => {
                    app.status = tf("{} 只在源库", &[&fix_double_encoding(&entry.table)])
                }
                DbTableMark::OnlyTgt => {
                    app.status = tf("{} 只在目标库", &[&fix_double_encoding(&entry.table)])
                }
            }
        }
        _ => {}
    }
}

// ── data transfer (Alt-T) ──

/// All SQL connections, with the source connection moved to the front so the
/// same-connection (and same-dialect) case is the default selection.
pub(crate) fn transfer_connections(app: &App, src_id: &str) -> Vec<ConnectionConfig> {
    let mut conns: Vec<ConnectionConfig> = app
        .connections
        .iter()
        .filter(|c| backend_for_connection(c) == Backend::Sql)
        .cloned()
        .collect();
    if let Some(pos) = conns.iter().position(|c| c.id == src_id) {
        conns.swap(0, pos);
    }
    conns
}

/// Open the `Alt-T` wizard for the focused table.
pub(crate) fn open_transfer_wizard(app: &mut App) {
    if app.backend_kind != Backend::Sql || app.selected.is_none() {
        app.status = t("数据搬运仅支持 SQL 连接").into();
        return;
    }
    let Some((src_db, src_schema, src_table)) = diff_source(app) else {
        app.status = t("先选中一张表再按 Alt-T").into();
        return;
    };
    let Some(src_conn) = app.selected.clone() else {
        return;
    };
    let conns = transfer_connections(app, &src_conn.id);
    if conns.is_empty() {
        app.status = t("没有可用的 SQL 连接").into();
        return;
    }
    let mut conn_list = ListState::default();
    conn_list.select(Some(0));
    let mut opt_list = ListState::default();
    opt_list.select(Some(0));
    let mut name_input = TextArea::from(vec![src_table.clone()]);
    name_input.move_cursor(CursorMove::End);
    app.transfer_gen += 1;
    app.transfer = Some(Box::new(TransferWizard {
        step: TransferStep::Connection,
        conn_list,
        conns,
        target_conn: src_conn.clone(),
        name_focus: NameFocus::Table,
        name_values: [src_db.clone(), src_schema.clone(), src_table.clone()],
        name_input,
        opt_list,
        mode: TransferMode::CreateAndCopy,
        conflict: TransferConflict::Stop,
        on_error: TransferOnError::Stop,
        where_input: String::new(),
        limit_input: String::new(),
        with_indexes: true,
        with_auto_increment: true,
        large_warn: None,
        error: None,
        prompt: None,
        submitted: false,
        src_conn,
        src_db,
        src_schema,
        src_table,
    }));
    app.transfer_report = None;
    app.status = t("数据搬运 ① 选择目标连接（源 = 当前表；Enter 下一步 · Esc 取消）").into();
}

pub(crate) fn transfer_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if app.transfer.as_ref().is_some_and(|w| w.submitted) {
        transfer_running_key(app, k);
        return;
    }
    let Some(step) = app.transfer.as_ref().map(|w| w.step) else {
        return;
    };
    match step {
        TransferStep::Connection => transfer_conn_key(app, k),
        TransferStep::Name => transfer_name_key(app, k),
        TransferStep::Options => transfer_options_key(app, tx, k),
        TransferStep::Confirm => transfer_confirm_key(app, tx, k),
    }
}

/// While a transfer runs the wizard is read-only: Esc (or q) requests an abort
/// and everything else is ignored. The committed batches are always kept.
pub(crate) fn transfer_running_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.transfer_cancel.store(true, Ordering::Relaxed);
            app.status = t("正在中止搬运…（已提交批次保留）").into();
        }
        _ => {}
    }
}

pub(crate) fn transfer_conn_key(app: &mut App, k: KeyEvent) {
    let n = app.transfer.as_ref().map(|w| w.conns.len()).unwrap_or(0);
    let step = |app: &mut App, delta: i32| {
        if n == 0 {
            return;
        }
        if let Some(w) = app.transfer.as_mut() {
            let cur = w.conn_list.selected().unwrap_or(0) as i32;
            w.conn_list
                .select(Some((cur + delta).clamp(0, n as i32 - 1) as usize));
        }
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.transfer = None;
            app.flash(t("已取消数据搬运").into());
        }
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home => {
            if let Some(w) = app.transfer.as_mut() {
                if n > 0 {
                    w.conn_list.select(Some(0));
                }
            }
        }
        KeyCode::End => {
            if let Some(w) = app.transfer.as_mut() {
                if n > 0 {
                    w.conn_list.select(Some(n - 1));
                }
            }
        }
        KeyCode::Enter => {
            let Some(idx) = app.transfer.as_ref().and_then(|w| w.conn_list.selected()) else {
                return;
            };
            let Some(cfg) = app
                .transfer
                .as_ref()
                .and_then(|w| w.conns.get(idx).cloned())
            else {
                return;
            };
            let src_schema = app
                .transfer
                .as_ref()
                .map(|w| w.src_schema.clone())
                .unwrap_or_default();
            let db = cfg
                .database
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_default();
            let schema = diff_target_schema(&cfg, &src_schema);
            if let Some(w) = app.transfer.as_mut() {
                w.target_conn = cfg.clone();
                w.name_values[0] = db;
                w.name_values[1] = schema;
                w.error = None;
                w.focus_name(NameFocus::Table);
                w.step = TransferStep::Name;
            }
            app.status = tf(
                "数据搬运 ② 目标库/表（{}）· Tab 切换字段 · Enter 下一步 · Esc 返回",
                &[&format!("{} ({})", cfg.name, cfg.db_type.as_str())],
            );
        }
        _ => {}
    }
}

pub(crate) fn transfer_name_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc => {
            if let Some(w) = app.transfer.as_mut() {
                w.step = TransferStep::Connection;
                w.error = None;
            }
            app.status = t("数据搬运 ① 选择目标连接").into();
        }
        KeyCode::Tab => {
            let next = app
                .transfer
                .as_ref()
                .map(|w| NameFocus::from_index((w.name_focus.index() + 1) % 3))
                .unwrap_or(NameFocus::Db);
            if let Some(w) = app.transfer.as_mut() {
                w.focus_name(next);
            }
        }
        KeyCode::BackTab => {
            let next = app
                .transfer
                .as_ref()
                .map(|w| NameFocus::from_index((w.name_focus.index() + 2) % 3))
                .unwrap_or(NameFocus::Db);
            if let Some(w) = app.transfer.as_mut() {
                w.focus_name(next);
            }
        }
        KeyCode::Enter => {
            let Some(w) = app.transfer.as_mut() else {
                return;
            };
            w.stash_name();
            if w.name_values[2].trim().is_empty() {
                w.error = Some(t("目标表名不能为空").to_string());
                return;
            }
            w.error = None;
            w.opt_list.select(Some(0));
            w.step = TransferStep::Options;
            app.status =
                t("数据搬运 ③ 模式与选项 · ↑↓ 选择 · Space/Enter 切换 · Enter 开搬 · Esc 取消")
                    .into();
        }
        _ => {
            if let Some(w) = app.transfer.as_mut() {
                w.name_input.input(k);
            }
        }
    }
}

pub(crate) fn transfer_cycle_conflict(app: &mut App) {
    let mut entered_confirm = false;
    if let Some(w) = app.transfer.as_mut() {
        w.conflict = w.conflict.next();
        if w.conflict == TransferConflict::Drop {
            w.step = TransferStep::Confirm;
            w.error = None;
            entered_confirm = true;
        }
    }
    if entered_confirm {
        app.status = t("⚠ 覆盖会先 DROP 目标表（数据不可恢复）· Enter 确认 · Esc 返回").into();
    }
}

pub(crate) fn transfer_open_prompt(app: &mut App, field: TransferField) {
    let Some(w) = app.transfer.as_mut() else {
        return;
    };
    let initial = match field {
        TransferField::Where => w.where_input.clone(),
        TransferField::Limit => w.limit_input.clone(),
    };
    let mut input = TextArea::from(vec![initial]);
    input.move_cursor(CursorMove::End);
    w.prompt = Some(TransferPrompt { field, input });
    app.status = match field {
        TransferField::Where => t("输入 WHERE 过滤（只搬子集，留空 = 全表）").into(),
        TransferField::Limit => t("输入 LIMIT 上限（留空 = 不限）").into(),
    };
}

pub(crate) fn transfer_opt_activate(app: &mut App, tx: &Tx) {
    let Some(idx) = app.transfer.as_ref().and_then(|w| w.opt_list.selected()) else {
        return;
    };
    match idx {
        0 => {
            if let Some(w) = app.transfer.as_mut() {
                w.mode = w.mode.next();
            }
        }
        1 => transfer_cycle_conflict(app),
        2 => {
            if let Some(w) = app.transfer.as_mut() {
                w.on_error = w.on_error.next();
            }
        }
        3 => transfer_open_prompt(app, TransferField::Where),
        4 => transfer_open_prompt(app, TransferField::Limit),
        5 => {
            if let Some(w) = app.transfer.as_mut() {
                w.with_indexes = !w.with_indexes;
            }
        }
        6 => {
            if let Some(w) = app.transfer.as_mut() {
                w.with_auto_increment = !w.with_auto_increment;
            }
        }
        7 => start_transfer(app, tx),
        _ => {}
    }
}

pub(crate) fn transfer_options_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let step = |app: &mut App, delta: i32| {
        if let Some(w) = app.transfer.as_mut() {
            let cur = w.opt_list.selected().unwrap_or(0) as i32;
            w.opt_list.select(Some(
                (cur + delta).clamp(0, TRANSFER_OPTION_ROWS as i32 - 1) as usize,
            ));
        }
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.transfer = None;
            app.flash(t("已取消数据搬运").into());
        }
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home => {
            if let Some(w) = app.transfer.as_mut() {
                w.opt_list.select(Some(0));
            }
        }
        KeyCode::End => {
            if let Some(w) = app.transfer.as_mut() {
                w.opt_list.select(Some(TRANSFER_OPTION_ROWS - 1));
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => transfer_opt_activate(app, tx),
        // Direct shortcuts so each option is one key away from anywhere in the
        // step (mirrors the diff picker's `m` / `w`).
        KeyCode::Char('m') => {
            if let Some(w) = app.transfer.as_mut() {
                w.mode = w.mode.next();
            }
        }
        KeyCode::Char('o') => transfer_cycle_conflict(app),
        KeyCode::Char('s') => {
            if let Some(w) = app.transfer.as_mut() {
                w.on_error = w.on_error.next();
            }
        }
        KeyCode::Char('w') => transfer_open_prompt(app, TransferField::Where),
        KeyCode::Char('l') => transfer_open_prompt(app, TransferField::Limit),
        KeyCode::Char('i') => {
            if let Some(w) = app.transfer.as_mut() {
                w.with_indexes = !w.with_indexes;
            }
        }
        KeyCode::Char('a') => {
            if let Some(w) = app.transfer.as_mut() {
                w.with_auto_increment = !w.with_auto_increment;
            }
        }
        _ => {}
    }
}

pub(crate) fn transfer_prompt_key(app: &mut App, _tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let parsed = app.transfer.as_ref().and_then(|w| {
                w.prompt
                    .as_ref()
                    .map(|p| (p.field, p.input.lines().join(" ").trim().to_string()))
            });
            if let Some((field, text)) = parsed {
                if let Some(w) = app.transfer.as_mut() {
                    match field {
                        TransferField::Where => w.where_input = text,
                        TransferField::Limit => w.limit_input = text,
                    }
                    w.prompt = None;
                }
                app.status = t("已更新搬运选项").into();
            }
        }
        KeyCode::Esc => {
            if let Some(w) = app.transfer.as_mut() {
                w.prompt = None;
            }
            app.flash(t("已取消输入").into());
        }
        _ => {
            if let Some(w) = app.transfer.as_mut() {
                if let Some(p) = w.prompt.as_mut() {
                    p.input.input(k);
                }
            }
        }
    }
}

pub(crate) fn transfer_confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') => start_transfer(app, tx),
        KeyCode::Esc | KeyCode::Char('n') => {
            if let Some(w) = app.transfer.as_mut() {
                w.conflict = TransferConflict::Stop;
                w.step = TransferStep::Options;
            }
            app.flash(t("已取消覆盖，保持报错停下").into());
        }
        _ => {}
    }
}

/// Build the job from the wizard and dispatch it in the background. A >1M source
/// must pass the wizard's `large_warn` gate first.
pub(crate) fn start_transfer(app: &mut App, tx: &Tx) {
    // A read-only target connection is a hard stop: the wizard writes to it.
    let ro_target = app
        .transfer
        .as_ref()
        .filter(|w| w.target_conn.read_only)
        .map(|w| w.target_conn.name.clone());
    if let Some(name) = ro_target {
        if let Some(w) = app.transfer.as_mut() {
            w.error = Some(t("目标连接为只读，拒绝写入").to_string());
        }
        app.status = tf("✗ 只读连接「{}」：拒绝写语句", &[&name]);
        return;
    }
    app.transfer_gen += 1;
    let gen = app.transfer_gen;
    let Some(w) = app.transfer.as_mut() else {
        return;
    };
    w.stash_name();
    let table = w.name_values[2].trim().to_string();
    if table.is_empty() {
        w.error = Some(t("目标表名不能为空").to_string());
        w.step = TransferStep::Name;
        return;
    }
    let allow_large = w.large_warn.is_some();
    let cancel = Arc::new(AtomicBool::new(false));
    app.transfer_cancel = cancel.clone();
    let job = TransferJob {
        src_cfg: Box::new(w.src_conn.clone()),
        src_db: w.src_db.clone(),
        src_schema: w.src_schema.clone(),
        src_table: w.src_table.clone(),
        tgt_cfg: Box::new(w.target_conn.clone()),
        tgt_db: w.name_values[0].trim().to_string(),
        tgt_schema: w.name_values[1].trim().to_string(),
        tgt_table: table.clone(),
        mode: w.mode,
        conflict: w.conflict,
        on_error: w.on_error,
        where_input: w.where_input.clone(),
        limit: w.limit(),
        with_indexes: w.with_indexes,
        with_auto_increment: w.with_auto_increment,
        allow_large,
        gen,
        cancel,
    };
    let src_label = qualified_display(&w.src_schema, &w.src_table);
    w.submitted = true;
    app.transfer_report = None;
    app.transfer_progress = None;
    app.loading = true;
    app.status = tf(
        "开始搬运 {} → {} · Esc 中止（已提交批次保留）",
        &[
            &fix_double_encoding(&src_label),
            &fix_double_encoding(&table),
        ],
    );
    app.spawn(tx, Op::DataTransfer(Box::new(job)));
}

pub(crate) fn transfer_report_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            app.transfer_report = None;
            app.flash(t("已关闭搬运汇总").into());
        }
        KeyCode::Char('g') | KeyCode::Char('y') => {
            let Some(rep) = app.transfer_report.as_ref() else {
                return;
            };
            let text = transfer_summary_text(rep);
            let lines = text.lines().count();
            match clipboard_copy(&text) {
                Some(p) => {
                    app.status = tf(
                        "✓ 已复制搬运摘要（{} 行）· 兜底 {}",
                        &[&lines, &(p.display())],
                    )
                }
                None => app.status = tf("✓ 已复制搬运摘要（{} 行）· OSC52 剪贴板", &[&lines]),
            }
        }
        KeyCode::Char('b') => open_transfer_target(app, tx),
        _ => {}
    }
}

/// `b` in the report: jump to the copied table when it lives on the connection
/// and database already open.
pub(crate) fn open_transfer_target(app: &mut App, tx: &Tx) {
    let Some(rep) = app.transfer_report.as_ref() else {
        return;
    };
    let tgt_conn_id = rep.tgt_conn_id.clone();
    let tgt_db = rep.tgt_db.clone();
    let tgt_schema = rep.tgt_schema.clone();
    let tgt_table = rep.tgt_table.clone();
    let same_conn = app.selected.as_ref().is_some_and(|c| c.id == tgt_conn_id);
    if !same_conn || app.current_db() != tgt_db || app.schema != tgt_schema {
        app.status = t("目标表在其他连接/库/模式：请切换到该连接后用 d 浏览").into();
        return;
    }
    let Some(idx) = app
        .tables
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(&tgt_table))
    else {
        // The table was just created by the transfer, so the sidebar list is
        // stale: clear any filter, refresh, and open it when the list lands.
        app.transfer_report = None;
        app.table_filter.clear();
        app.table_prompt = None;
        app.pending_open_table = Some((tgt_schema.clone(), tgt_table.clone()));
        app.status = tf("刷新表列表并打开 {} …", &[&tgt_table]);
        spawn_table_list(app, tx);
        return;
    };
    app.transfer_report = None;
    app.table_list.select(Some(idx));
    open_table_data(app, tx);
}

// ── data compare (Alt-K) ──

/// Start a two-table data compare. The picker stays open (with `comparing` set)
/// so Esc can abort; the result overlay opens when the op finishes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_data_diff(
    app: &mut App,
    tx: &Tx,
    src_cfg: ConnectionConfig,
    src_db: String,
    src_schema: String,
    src_table: String,
    tgt_cfg: ConnectionConfig,
    tgt_db: String,
    tgt_schema: String,
    tgt_table: String,
    where_input: String,
) {
    app.data_diff = None;
    app.data_where = None;
    app.data_diff_gen += 1;
    let gen = app.data_diff_gen;
    app.data_cancel = Arc::new(AtomicBool::new(false));
    let cancel = app.data_cancel.clone();
    app.data_progress = Some((0, 0));
    if let Some(p) = app.diff_picker.as_mut() {
        p.comparing = true;
        p.loading = true;
    }
    app.loading = true;
    app.status = tf(
        "数据对比 {} → {}…（按主键对齐；Esc 中止）",
        &[
            &fix_double_encoding(&qualified_display(&src_schema, &src_table)),
            &fix_double_encoding(&qualified_display(&tgt_schema, &tgt_table)),
        ],
    );
    app.spawn(
        tx,
        Op::DataDiff {
            src_cfg: Box::new(src_cfg),
            src_db,
            src_schema,
            src_table,
            tgt_cfg: Box::new(tgt_cfg),
            tgt_db,
            tgt_schema,
            tgt_table,
            where_input,
            gen,
            cancel,
        },
    );
}

/// Open the optional `WHERE` input for a data compare (prefilled with any
/// existing value). Enter starts the compare; Esc returns to the picker.
pub(crate) fn open_data_where(app: &mut App) {
    let initial = app
        .diff_picker
        .as_ref()
        .map(|p| p.where_input.clone())
        .unwrap_or_default();
    let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
    ta.set_placeholder_text(t("例: status = 'active'（留空回车 = 无过滤）"));
    ta.move_cursor(CursorMove::End);
    app.data_where = Some(ta);
    app.status = t("数据对比 WHERE 过滤（两边同时生效）· Enter 开始 · Esc 返回").into();
}

pub(crate) fn data_where_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let text = app
                .data_where
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.data_where = None;
            if let Some(p) = app.diff_picker.as_mut() {
                p.where_input = text;
            }
            start_selected_data_diff(app, tx);
        }
        KeyCode::Esc => {
            app.data_where = None;
            app.flash(t("已取消 WHERE 输入").into());
        }
        _ => {
            if let Some(t) = app.data_where.as_mut() {
                t.input(k);
            }
        }
    }
}

/// The `Enter` action shared by the picker and the WHERE prompt: start the
/// compare for the picker's current selection.
pub(crate) fn start_selected_data_diff(app: &mut App, tx: &Tx) {
    let Some(p) = app.diff_picker.as_ref() else {
        return;
    };
    let Some(idx) = p.list.selected() else {
        return;
    };
    let Some(choice) = p.entries.get(idx).cloned() else {
        return;
    };
    let where_input = p.where_input.clone();
    let Some((db, schema, src)) = diff_source(app) else {
        return;
    };
    let Some(src_cfg) = app.selected.clone() else {
        return;
    };
    let (tgt_cfg, tgt_db, tgt_schema) = match &p.target_conn {
        Some(c) => ((**c).clone(), p.target_db.clone(), p.target_schema.clone()),
        None => (src_cfg.clone(), db.clone(), schema.clone()),
    };
    start_data_diff(
        app,
        tx,
        src_cfg,
        db,
        schema,
        src,
        tgt_cfg,
        tgt_db,
        tgt_schema,
        choice,
        where_input,
    );
}

/// The Nth row of the active data tab, in list order.
pub(crate) fn data_tab_row(state: &DataDiffState, idx: usize) -> Option<&DataDiffRow> {
    let mark = match state.tab {
        DataTab::OnlySrc => RowMark::OnlySrc,
        DataTab::OnlyTgt => RowMark::OnlyTgt,
        DataTab::Diff => RowMark::Diff,
        DataTab::Summary | DataTab::Sync => return None,
    };
    state.result.rows.iter().filter(|r| r.mark == mark).nth(idx)
}

/// Selectable row count in the active data tab (`Summary` / `Sync` are not
/// lists).
pub(crate) fn data_tab_rows(state: &DataDiffState) -> usize {
    match state.tab {
        DataTab::OnlySrc => state
            .result
            .rows
            .iter()
            .filter(|r| r.mark == RowMark::OnlySrc)
            .count(),
        DataTab::OnlyTgt => state
            .result
            .rows
            .iter()
            .filter(|r| r.mark == RowMark::OnlyTgt)
            .count(),
        DataTab::Diff => state
            .result
            .rows
            .iter()
            .filter(|r| r.mark == RowMark::Diff)
            .count(),
        DataTab::Summary | DataTab::Sync => 0,
    }
}

pub(crate) fn data_move(app: &mut App, delta: i32) {
    let Some(state) = app.data_diff.as_mut() else {
        return;
    };
    if state.tab == DataTab::Sync {
        state.scroll = (state.scroll as i32 + delta).max(0) as u16;
        return;
    }
    let n = data_tab_rows(state);
    if n == 0 {
        return;
    }
    let cur = state.list.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
    state.list.select(Some(next));
}

/// `Enter` on a data row: show its column-level detail (or the whole row for an
/// only-source / only-target row) in the shared popup.
pub(crate) fn open_data_row_popup(app: &mut App) {
    let Some(state) = app.data_diff.as_ref() else {
        return;
    };
    let Some(idx) = state.list.selected() else {
        return;
    };
    let Some(row) = data_tab_row(state, idx) else {
        return;
    };
    let align = &state.result.align;
    let title = format!("{} {} {}", row.mark.sign(), t("行"), row.key);
    let mut lines: Vec<PopupLine> = Vec::new();
    match row.mark {
        RowMark::Diff => {
            for cell in &row.cells {
                let (sv, s_style) = value_display(&cell.src_val);
                let (tv, t_style) = value_display(&cell.tgt_val);
                let tag = if cell.unknown_type { " ?" } else { "" };
                lines.push(PopupLine {
                    text: format!("{} {}", cell.col, tag),
                    style: Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                });
                lines.push(PopupLine {
                    text: format!("  {}  {}", t("源"), sv),
                    style: s_style,
                });
                lines.push(PopupLine {
                    text: format!("  {}  {}", t("目标"), tv),
                    style: t_style,
                });
            }
        }
        _ => {
            for (i, val) in row.vals.iter().enumerate() {
                let name = align
                    .cols
                    .get(i)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let (v, style) = value_display(val);
                lines.push(PopupLine {
                    text: format!("{name} = {v}"),
                    style,
                });
            }
        }
    }
    if lines.is_empty() {
        lines.push(PopupLine {
            text: t("（无列差异）").into(),
            style: Style::default().fg(Color::DarkGray),
        });
    }
    let raw = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    app.cell_popup = Some(make_cell_popup(
        title.clone(),
        lines,
        title,
        raw,
        None,
        false,
        false,
    ));
}

/// `y` in the data-diff overlay: copy the plain-text summary.
pub(crate) fn copy_data_summary(app: &mut App) {
    let Some(state) = app.data_diff.as_ref() else {
        return;
    };
    let text = data_diff_summary_text(&state.result);
    let lines = text.lines().count();
    match clipboard_copy(&text) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制差异摘要（{} 行）· 兜底 {}",
                &[&lines, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制差异摘要（{} 行）· OSC52 剪贴板", &[&lines]),
    }
}

/// R90 `Y`: copy every retained difference row as CSV (one line per differing
/// cell; fields escaped). The plain-text `y` summary stays the ticket-friendly
/// twin.
pub(crate) fn copy_data_csv(app: &mut App) {
    let Some(state) = app.data_diff.as_ref() else {
        return;
    };
    let text = data_diff_csv(&state.result);
    let rows = data_diff_csv_rows(&state.result);
    match clipboard_copy(&text) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制差异 CSV（{} 行）· 兜底 {}",
                &[&rows, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制差异 CSV（{} 行）· OSC52 剪贴板", &[&rows]),
    }
}

/// R90 `Ctrl-E`: the export path — `dbxt-diff-<epoch-millis>.csv` in the system
/// temp dir (usually `/tmp`). The millisecond stamp keeps two exports inside the
/// same second apart.
pub(crate) fn diff_export_path() -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("dbxt-diff-{ms}.csv"))
}

/// R90 `Ctrl-E`: write the difference CSV to a temp file and report its path.
/// Unlike the clipboard (`Y`) this is a file the user can open in an editor or
/// pipe elsewhere; it never touches the database.
pub(crate) fn export_data_diff(app: &mut App) {
    let Some(state) = app.data_diff.as_ref() else {
        return;
    };
    let text = data_diff_csv(&state.result);
    let rows = data_diff_csv_rows(&state.result);
    let path = diff_export_path();
    match std::fs::write(&path, text.as_bytes()) {
        Ok(()) => {
            app.status = tf("✓ 已导出差异 {} 行到 {}", &[&rows, &(path.display())]);
        }
        Err(e) => app.status = tf("✗ 导出失败: {}", &[&e]),
    }
}

pub(crate) fn data_diff_key(app: &mut App, _tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.data_diff = None;
            app.flash(t("已关闭数据对比").into());
        }
        KeyCode::Char('y') => copy_data_summary(app),
        // R90: `Y` exports the difference rows as CSV; `Ctrl-E` writes them to
        // `/tmp/dbxt-diff-<ts>.csv` and reports the path.
        KeyCode::Char('Y') => copy_data_csv(app),
        KeyCode::Char('e') if k.modifiers.contains(KeyModifiers::CONTROL) => export_data_diff(app),
        KeyCode::Char('g') => {
            if let Some(state) = app.data_diff.as_mut() {
                if state.sync_sql.is_empty() {
                    state.sync_sql = generate_data_sync(&state.result);
                }
                state.tab = DataTab::Sync;
                state.scroll = 0;
            }
            app.status = t("已生成同步语句（源 → 目标，只生成不执行）· Tab 回差异 · Esc 关").into();
        }
        KeyCode::Tab | KeyCode::Char('t') => {
            if let Some(state) = app.data_diff.as_mut() {
                state.tab = state.tab.next();
                state.scroll = 0;
                state.list.select(if data_tab_rows(state) == 0 {
                    None
                } else {
                    Some(0)
                });
            }
        }
        KeyCode::Enter => open_data_row_popup(app),
        // R90: `n` / `p` step to the next / previous difference row in the
        // active tab (aliases of j / k, the gesture the spec names).
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') => data_move(app, -1),
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => data_move(app, 1),
        KeyCode::PageUp => data_move(app, -10),
        KeyCode::PageDown => data_move(app, 10),
        KeyCode::Home => {
            if let Some(state) = app.data_diff.as_mut() {
                if state.tab == DataTab::Sync {
                    state.scroll = 0;
                } else if data_tab_rows(state) > 0 {
                    state.list.select(Some(0));
                }
            }
        }
        KeyCode::End => {
            if let Some(state) = app.data_diff.as_mut() {
                let n = data_tab_rows(state);
                if state.tab != DataTab::Sync && n > 0 {
                    state.list.select(Some(n - 1));
                }
            }
        }
        _ => {}
    }
}

// ── SQL file execution (Alt-L) ──

/// A read `.sql` file waiting for the user's confirmation before it runs.
#[derive(Clone)]
pub(crate) struct FileLoadPlan {
    pub(crate) path: PathBuf,
    pub(crate) sql: String,
    pub(crate) bytes: u64,
    pub(crate) statements: usize,
    /// Target connection name (shown so the file is never run on the wrong one).
    pub(crate) connection: String,
    pub(crate) db: String,
    /// Per-statement danger reasons (deduped): routes execution through the red
    /// confirmation layer.
    pub(crate) danger: Vec<String>,
    /// Set for a large file, as a slow-run heads-up.
    pub(crate) warning: Option<String>,
}

/// `~` / `~/…` → $HOME. A bare path is returned unchanged.
pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    let raw = raw.trim();
    let home = || {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if raw == "~" {
        if let Some(h) = home() {
            return h;
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Read a `.sql` file as text, tolerating non-UTF-8 bytes (lossy) and a UTF-8
/// BOM. Returns the raw IO error string on failure.
pub(crate) fn read_sql_file(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

/// Count the executable statements in a script using the dialect-aware splitter
/// (semicolons in strings / comments / routines do not count). A whitespace-only
/// file is zero statements.
pub(crate) fn count_sql_statements(sql: &str, db_type: DatabaseType) -> usize {
    if sql.trim().is_empty() {
        return 0;
    }
    dbx_core::sql::split_sql_statements_for_database(sql, db_type).len()
}

/// Open the `.sql` file-path prompt (editor `Alt-L`).
pub(crate) fn open_file_load(app: &mut App) {
    if app.backend_kind != Backend::Sql {
        app.status = t("SQL 文件执行仅支持 SQL 后端").into();
        return;
    }
    if app.selected.is_none() {
        app.status = t("✗ 未选择连接").into();
        return;
    }
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t("SQL 文件路径（~ 展开）"));
    app.file_load_prompt = Some(ta);
    app.status = t("加载 SQL 文件 · 输入路径 · Enter 预览 · Esc 取消").into();
}

/// Read the typed path and build the confirmation plan.
pub(crate) fn file_load_prompt_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let raw = app
                .file_load_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            if raw.is_empty() {
                app.status = t("文件路径不能为空").into();
                return;
            }
            let Some(cfg) = app.selected.clone() else {
                app.status = t("✗ 未选择连接").into();
                return;
            };
            let path = expand_tilde(&raw);
            match read_sql_file(&path) {
                Ok(sql) if sql.trim().is_empty() => {
                    app.status = tf("文件为空：{}", &[&(path.display())]);
                }
                Ok(sql) => {
                    app.file_load_prompt = None;
                    let bytes = std::fs::metadata(&path)
                        .map(|m| m.len())
                        .unwrap_or(sql.len() as u64);
                    let statements = count_sql_statements(&sql, cfg.db_type).max(1);
                    let mut danger: Vec<String> = Vec::new();
                    for st in dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type) {
                        if let Some(r) = detect_danger(&st) {
                            if !danger.contains(&r) {
                                danger.push(r);
                            }
                        }
                    }
                    let warning = (bytes > FILE_LOAD_WARN_BYTES)
                        .then(|| tf("文件较大（{}），执行可能较慢", &[&(human_size(bytes))]));
                    app.file_load_plan = Some(Box::new(FileLoadPlan {
                        path,
                        sql,
                        bytes,
                        statements,
                        connection: cfg.name.clone(),
                        db: app.current_db(),
                        danger,
                        warning,
                    }));
                    app.status = tf(
                        "已读取 · {} 条语句 · Enter 执行 · e 转编辑器 · Esc 取消",
                        &[&statements],
                    );
                }
                Err(e) => {
                    app.status = tf("✗ 无法读取文件: {}", &[&e]);
                }
            }
        }
        KeyCode::Esc => {
            app.file_load_prompt = None;
            app.flash(t("已取消加载 SQL 文件").into());
        }
        _ => {
            if let Some(ta) = app.file_load_prompt.as_mut() {
                ta.input(k);
            }
        }
    }
}

/// Confirm the file: run it through the multi-statement script pipeline, or open
/// the red layer first when it contains a destructive statement.
pub(crate) fn file_load_plan_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let Some(plan) = app.file_load_plan.take() else {
                return;
            };
            // Read-only connections refuse a file script that contains any write;
            // the preview stays open so it can still be loaded into the editor.
            if readonly_block(app, &plan.sql) {
                app.file_load_plan = Some(plan);
                return;
            }
            if !plan.danger.is_empty() {
                app.pending_run_origin = "script";
                app.confirm = Some(Confirm {
                    sql: plan.sql,
                    reasons: plan.danger,
                    refresh: false,
                    clear_batch: false,
                    conn: None,
                    redis: None,
                    mongo: None,
                });
                app.status = t("危险语句确认 · Enter 执行 · Esc 取消").into();
                return;
            }
            let n = plan.statements;
            app.push_history(&plan.sql);
            app.status = tf("执行 {} · {} 条语句…", &[&(plan.path.display()), &n]);
            execute_sql(app, tx, plan.sql, "script");
        }
        KeyCode::Esc => {
            app.file_load_plan = None;
            app.flash(t("已取消加载 SQL 文件").into());
        }
        // Load the whole file into the editor for review / tweaking.
        KeyCode::Char('e') | KeyCode::Char('v') if k.modifiers.is_empty() => {
            let Some(plan) = app.file_load_plan.take() else {
                return;
            };
            app.set_editor_text(&plan.sql);
            app.focus = Focus::Editor;
            app.status = tf(
                "已载入 {} 到编辑器（{} 条语句）",
                &[&(plan.path.display()), &(plan.statements)],
            );
        }
        _ => {}
    }
}

// ── sidebar table filter (`/`, filter-as-you-type) ──
