use crate::prelude::*;
use crate::*;

// ─── input handling ──────────────────────────────────────────────────────────

pub(crate) fn handle_event(app: &mut App, tx: &Tx, ev: Event) {
    trace_event(app, &ev);
    match ev {
        Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            key(app, tx, k);
            // R61: an edit through any path (paste, snippet insert, history
            // recall, completion accept, …) invalidates the editor find
            // highlight, not only the keys routed through `editor_key`.
            sync_editor_find(app);
            // R77: the same hook retires the located execution-error highlight
            // once the buffer no longer matches what ran.
            sync_editor_errors(app);
            // R71: the same hook retires the template placeholder mode once the
            // last `{{…}}` has been filled in.
            sync_editor_template(app);
        }
        Event::Paste(s) => match app.focus {
            Focus::Editor => {
                app.editor.insert_str(s);
                sync_editor_find(app);
                sync_editor_errors(app);
                sync_editor_template(app);
            }
            Focus::CmdInput => {
                app.cmd_input.insert_str(s);
            }
            _ => {}
        },
        Event::Mouse(m) => mouse(app, tx, m),
        _ => {}
    }
}

/// Close every modal overlay and cancel / invalidate the background tasks that
/// belonged to the outgoing backend. Called by Ctrl-L: the backend line switch
/// must not leave a diff / transfer / search surface owning the keyboard over a
/// different backend. Background tasks tied to the old backend are asked to stop
/// and their reply generation is bumped so a late result cannot reopen a panel.
pub(crate) fn reset_overlays_for_backend_switch(app: &mut App) {
    // Global search.
    app.search_cancel.store(true, Ordering::Relaxed);
    app.search_gen = app.search_gen.wrapping_add(1);
    app.search_open = false;
    app.search_input = None;
    app.search_running = false;
    app.search_progress = None;
    // Data compare.
    app.data_cancel.store(true, Ordering::Relaxed);
    app.data_diff_gen = app.data_diff_gen.wrapping_add(1);
    app.data_diff = None;
    app.data_where = None;
    app.data_progress = None;
    // Schema / database diff.
    app.diff_gen = app.diff_gen.wrapping_add(1);
    app.diff_picker = None;
    app.diff = None;
    app.db_diff = None;
    // Data transfer (a running copy is aborted; committed batches stay).
    if app.transfer.as_ref().is_some_and(|w| w.submitted) {
        app.transfer_cancel.store(true, Ordering::Relaxed);
    }
    app.transfer_gen = app.transfer_gen.wrapping_add(1);
    app.transfer = None;
    app.transfer_progress = None;
    app.transfer_report = None;
    // Import / export / everything else that can own the keyboard.
    app.import_prompt = None;
    app.import_plan = None;
    app.import_report = None;
    app.export_open = false;
    app.export_path = None;
    app.export_pending = None;
    app.recent_open = false;
    app.col_picker_open = false;
    app.cols_popup_open = false;
    app.cols_popup_needle.clear();
    app.cols_popup_filter = None;
    app.cols_popup_sel = 0;
    // R65: drop the in-data-view table switcher with the rest of the overlays.
    app.table_jump_open = false;
    app.table_jump_needle.clear();
    app.snippet_open = false;
    app.snippet_name = None;
    app.snippet_needle.clear();
    app.snippet_filter = None;
    app.snippet_view.clear();
    app.snippet_confirm = None;
    app.template_open = false;
    app.template_filter = None;
    app.template_needle.clear();
    app.template_view.clear();
    app.template_active = false;
    app.template_ph_start = None;
    app.completion = None;
    app.table_prompt = None;
    app.tree_search_prompt = None;
    app.tree_search.clear();
    app.result_filter = None;
    // R52: a backend switch drops the grid, so the column filter (and its
    // prompt) must not linger as a bogus marker on whatever renders next.
    app.clear_col_filter();
    app.locate_prompt = None;
    app.col_jump = None;
    app.goto_prompt = None;
    // R61: drop the editor find highlight with the rest of the overlays.
    clear_editor_find(app);
    // R77: the located execution-error highlight is tied to the connection too.
    clear_editor_errors(app);
    app.mongo_dialog = None;
    app.redis_prompt = None;
    app.help_open = false;
    app.help_mini = false;
    app.pending_g = false;
    app.clear_count();
    app.db_picker_open = false;
    app.history_open = false;
    app.history_filter = None;
    app.file_load_prompt = None;
    app.file_load_plan = None;
    app.filter_prompt = None;
    app.cell_popup = None;
    app.row_popup = None;
    app.error_popup = None;
    app.pinned_result = None;
    // R55: cancel an in-place tree rename on a backend switch.
    app.rename_edit = None;
    // R57: drop a results row selection with the grid it belonged to.
    app.row_sel_anchor = None;
}

/// True when quitting would discard unrun work: the editor holds SQL that was
/// never executed, or a sidebar / result filter is active.
pub(crate) fn quit_has_unsaved(app: &App) -> bool {
    let sql = app.editor_sql();
    let sql = sql.trim();
    let last = app.last_executed.as_deref().unwrap_or("").trim();
    let editor_dirty = !sql.is_empty() && sql != last;
    let filter_dirty = !app.table_filter.trim().is_empty() || !app.result_needle.trim().is_empty();
    editor_dirty || filter_dirty
}

/// The single quit entry point. With unrun work the first press only arms the
/// quit (a status hint, no overlay); a second press quits. Esc / any other key
/// disarms it in [`key`].
pub(crate) fn request_quit(app: &mut App) {
    if app.quit_armed {
        app.quit = true;
        return;
    }
    if quit_has_unsaved(app) {
        app.quit_armed = true;
        app.status = t("⚠ 编辑器有未执行语句 · 再按 q / Ctrl-C 退出 · Esc 留下").into();
        return;
    }
    app.quit = true;
}

pub(crate) fn key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The back/forward landing hint is transient: it survives until the next
    // key press (the history keys themselves refresh it).
    let is_nav_key =
        k.modifiers.contains(KeyModifiers::ALT) && matches!(k.code, KeyCode::Left | KeyCode::Right);
    if !is_nav_key {
        app.nav_landing = None;
    }
    // The one-shot table-view discovery hint also yields to any key press.
    if app.row_hint_until.take().is_some() && app.status == row_hint_text() {
        app.status.clear();
    }
    // Two-stage quit: any key other than a quit key disarms the pending quit
    // (so `Esc` — or simply carrying on — leaves the app running).
    let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::SHIFT)
        && k.code == KeyCode::Char('c');
    let bare_q = k.modifiers.is_empty() && k.code == KeyCode::Char('q');
    if app.quit_armed && !(ctrl_c || bare_q) {
        app.quit_armed = false;
    }
    // global: quit. Ctrl-Shift-C is a *view* toggle (compact columns), so the
    // quit must not swallow it on terminals that report Shift as a modifier.
    if ctrl_c {
        request_quit(app);
        return;
    }

    // A blocking SSH prompt (host-key TOFU / keyboard-interactive) is the
    // topmost modal: the tunnel handshake is suspended on our answer.
    if app.ssh_prompt.is_some() {
        ssh_prompt_key(app, k);
        return;
    }

    // confirmation overlay swallows everything else
    if app.confirm.is_some() {
        confirm_key(app, tx, k);
        return;
    }

    // diff-style edit confirmation layer is modal too
    if app.edit_dialog.is_some() {
        edit_dialog_key(app, tx, k);
        return;
    }

    // deleting a single history entry is confirmed in its own red layer
    if app.history_confirm.is_some() {
        history_confirm_key(app, tx, k);
        return;
    }

    // global: cycle backend line sql → redis → mongo
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('l') {
        app.backend_kind = match app.backend_kind {
            Backend::Sql => Backend::Redis,
            Backend::Redis => Backend::Mongo,
            Backend::Mongo => Backend::Sql,
        };
        app.cmd_input = TextArea::default();
        app.clear_grid();
        app.script = None;
        app.ddl = None;
        app.page_state = None;
        app.struct_view = StructView::Fields;
        app.col_offset = 0;
        app.col_cursor = 0;
        // Close every overlay and cancel the background tasks that belonged to
        // the old backend; otherwise a diff / transfer / search surface opened
        // on the SQL side kept owning the keyboard over the new backend.
        reset_overlays_for_backend_switch(app);
        app.set_placeholder();
        app.status = tf(
            "命令模式：{}",
            &[&match app.backend_kind {
                Backend::Sql => "SQL",
                Backend::Redis => "Redis",
                Backend::Mongo => "MongoDB",
            }],
        );
        return;
    }

    match app.page {
        Page::NewConn => form_key(app, tx, k),
        Page::Browse => browse_key(app, tx, k),
    }
}

pub(crate) fn confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(c) = app.confirm.take() {
                // A read-only connection refuses Redis / MongoDB writes too, so
                // the red layer's Enter cannot slip a mutation past the guard.
                if (c.redis.is_some() || c.mongo.is_some()) && readonly_conn_block(app) {
                    return;
                }
                if let Some(cc) = c.conn {
                    // R68: a read-only toggle is a config write, so it runs
                    // before the delete / disconnect branches.
                    if let Some(new_ro) = cc.readonly {
                        apply_conn_readonly(app, tx, &cc, new_ro);
                        return;
                    }
                    if cc.disconnect {
                        // R47b: drain the pools (manual transactions roll back)
                        // and let the reply collapse the root to a grey dot.
                        app.status = tf("断开连接 {}…", &[&cc.name]);
                        app.spawn(
                            tx,
                            Op::Disconnect {
                                id: cc.id,
                                name: cc.name,
                            },
                        );
                        return;
                    }
                    app.status = tf("删除连接 {}…", &[&cc.name]);
                    app.spawn(
                        tx,
                        Op::DeleteConn {
                            id: cc.id,
                            name: cc.name,
                        },
                    );
                    return;
                }
                if let Some(mc) = c.mongo {
                    run_mongo_action(app, tx, mc);
                    return;
                }
                if let Some(rc) = c.redis {
                    // A select-all delete demands a typed re-confirmation first.
                    if rc.typed_confirm.is_some() {
                        open_redis_typed_confirm(app, rc);
                        return;
                    }
                    // R81: a confirmed single-key TTL write updates the loaded
                    // list in place (no rescan, cursor kept) and re-applies the
                    // active filter / order so a TTL sort stays truthful.
                    if !rc.set_ttl_in_place.is_empty() {
                        redis_apply_ttl_in_place(app, &rc.set_ttl_in_place);
                        apply_redis_filter(app);
                    }
                    if !rc.batch.is_empty() {
                        // R57: a single-key delete prunes the loaded list in
                        // place before the command runs, keeping the cursor.
                        if !rc.remove_in_place.is_empty() {
                            redis_remove_keys_in_place(app, &rc.remove_in_place);
                        }
                        run_redis_batch(app, tx, rc.db, rc.batch, rc.reload_list);
                        return;
                    }
                    run_redis_write(app, tx, rc.db, &rc.cmd, rc.reload_value, rc.reload_list);
                    return;
                }
                app.push_history(&c.sql);
                if c.clear_batch {
                    app.batch.clear();
                }
                // Row edits / deletes / batches refresh the current page on success.
                if c.refresh {
                    app.pending_write = true;
                }
                execute_sql(app, tx, c.sql, app.pending_run_origin);
            }
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.confirm = None;
            app.pending_write = false;
            app.flash(t("已取消").into());
        }
        _ => {}
    }
}

/// Open the typed re-confirmation prompt for a dangerous batch (repeat the key
/// count or `YES`). The pending batch is stashed so the prompt can run it.
pub(crate) fn open_redis_typed_confirm(app: &mut App, rc: RedisConfirm) {
    let need = rc.typed_confirm.unwrap_or(0);
    let title = tf("二次确认 · {}", &[&rc.summary]);
    let batch = rc.batch_keys.clone();
    app.redis_pending_batch = Some(rc);
    let mut ta = TextArea::default();
    ta.set_placeholder_text(tf("输入 {} 或 YES", &[&need]));
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchConfirm,
        title,
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch,
        input: ta,
    });
    app.status = tf("二次确认：输入 {} 或 YES", &[&need]);
}

/// Run a confirmed MongoDB document write, then reload the current page.
pub(crate) fn run_mongo_action(app: &mut App, tx: &Tx, mc: MongoConfirm) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.loading = true;
    let op = match mc.action {
        MongoAction::Insert { doc_json } => {
            app.status = tf("插入文档到 {}…", &[&fix_double_encoding(&mc.collection)]);
            Op::MongoInsert {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                doc_json,
            }
        }
        MongoAction::Update { id, doc_json } => {
            app.status = tf("更新文档 {}…", &[&truncate_disp(&id, 40)]);
            Op::MongoUpdate {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                id,
                doc_json,
            }
        }
        MongoAction::Delete { id } => {
            app.status = tf("删除文档 {}…", &[&truncate_disp(&id, 40)]);
            Op::MongoDelete {
                cfg: Box::new(cfg),
                db: mc.db,
                collection: mc.collection,
                id,
            }
        }
    };
    app.spawn(tx, op);
}

pub(crate) fn browse_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // R55: while a tree row is being renamed in place the field owns the
    // keyboard — a plain text field, so no global shortcut may steal a letter
    // (pressing `q` must type `q`, not quit).
    if app.rename_edit.is_some() {
        rename_edit_key(app, tx, k);
        return;
    }
    // Overlays are modal, most-specific first. Esc always closes the current one.
    if app.help_open {
        help_key(app, k);
        return;
    }
    if app.help_mini {
        help_mini_key(app, k);
        return;
    }
    // CSV import and result export overlays (newest, so checked before the rest).
    if app.import_report.is_some() {
        import_report_key(app, k);
        return;
    }
    if app.import_plan.is_some() {
        import_plan_key(app, tx, k);
        return;
    }
    if app.import_prompt.is_some() {
        import_prompt_key(app, tx, k);
        return;
    }
    // SQL file execution (Alt-L): path prompt then preview are modal.
    if app.file_load_prompt.is_some() {
        file_load_prompt_key(app, k);
        return;
    }
    if app.file_load_plan.is_some() {
        file_load_plan_key(app, tx, k);
        return;
    }
    if app.export_path.is_some() {
        export_path_key(app, tx, k);
        return;
    }
    if app.export_open {
        export_key(app, k);
        return;
    }
    // Connection bundle import / export (Alt-E / Alt-I) are modal overlays.
    if app.conn_export.is_some() {
        conn_export_key(app, k);
        return;
    }
    if app.conn_import_path.is_some() {
        conn_import_path_key(app, k);
        return;
    }
    if app.conn_import_plan.is_some() {
        conn_import_plan_key(app, tx, k);
        return;
    }
    if app.filter_prompt.is_some() {
        filter_prompt_key(app, tx, k);
        return;
    }
    if app.error_popup.is_some() {
        error_popup_key(app, k);
        return;
    }
    // The cell popup sits on top of a drilled row popup, so it owns the keyboard
    // first; Esc closes it and reveals the row underneath.
    if app.cell_popup.is_some() {
        cell_popup_key(app, k);
        return;
    }
    if app.row_popup.is_some() {
        row_popup_key(app, k);
        return;
    }

    // Result-row search prompt (`/` in the results pane) is modal while typing.
    if app.result_filter.is_some() {
        result_filter_key(app, k);
        return;
    }

    // Results-specific column filter prompt (`*`) is modal too.
    if app.col_filter_prompt.is_some() {
        col_filter_key(app, k);
        return;
    }

    // Result-set cell find prompt (`\` in the results pane) is modal too.
    if app.cell_find_prompt.is_some() {
        cell_find_key(app, k);
        return;
    }

    // Grid value locate (`gv`) and column jump (`|`) prompts are modal too.
    if app.locate_prompt.is_some() {
        locate_key(app, k);
        return;
    }
    if app.col_jump.is_some() {
        col_jump_key(app, k);
        return;
    }

    // R56: the `:` row-number jump prompt is modal too.
    if app.goto_prompt.is_some() {
        goto_row_key(app, k);
        return;
    }

    // R61: the editor find prompt (Ctrl-F) is modal while it is open.
    if app.editor_find.is_some() {
        editor_find_key(app, k);
        return;
    }

    // SQL completion popup (editor): must be handled before the global Tab
    // handler, otherwise Tab would switch panes instead of accepting.
    if app.completion.is_some() {
        completion_key(app, k);
        return;
    }

    // Saved-SQL snippet overlay (Ctrl-O) is modal; the name prompt is on top.
    if app.snippet_name.is_some() {
        snippet_name_key(app, tx, k);
        return;
    }
    if app.snippet_open {
        snippet_key(app, tx, k);
        return;
    }

    // R71: the built-in SQL template panel (`Alt-T` in the editor) is modal too.
    if app.template_open {
        template_key(app, k);
        return;
    }

    // Database switcher overlay (`d`) is modal.
    if app.db_picker_open {
        db_picker_key(app, tx, k);
        return;
    }

    // Redis key filter (client-side type-to-filter) is modal while typing.
    if app.redis_filter_prompt.is_some() {
        redis_filter_key(app, tx, k);
        return;
    }

    // Redis input dialogs (pattern / TTL / rename / value / batch) are modal.
    if app.redis_prompt.is_some() {
        redis_prompt_key(app, tx, k);
        return;
    }

    // MongoDB document JSON editor is modal.
    if app.mongo_dialog.is_some() {
        mongo_dialog_key(app, k);
        return;
    }

    // Sidebar tree quick search (`f`) is modal while it is being typed; it
    // checks before the `/` filter prompt because only one can be open.
    if app.tree_search_prompt.is_some() {
        tree_search_key(app, k);
        return;
    }

    // Sidebar table filter (`/`) is modal while it is being typed.
    if app.table_prompt.is_some() {
        table_filter_key(app, tx, k);
        return;
    }

    // Query-history overlay (Alt-H): the `/` filter input sits on top of it.
    if app.history_filter.is_some() {
        history_filter_key(app, k);
        return;
    }
    if app.history_open {
        history_key(app, tx, k);
        return;
    }

    // Global database search (Alt-G): the term input and results overlay are
    // both modal.
    if app.search_input.is_some() {
        search_input_key(app, tx, k);
        return;
    }
    if app.search_open {
        search_key(app, tx, k);
        return;
    }

    // Schema diff overlays (Alt-D / Shift+Alt-D) are modal: the target picker,
    // the two-table diff and the two-database diff. The data-compare overlays
    // (Alt-K) share the same picker.
    if app.data_where.is_some() {
        data_where_key(app, tx, k);
        return;
    }
    if app.diff_picker.is_some() {
        diff_picker_key(app, tx, k);
        return;
    }
    if app.data_diff.is_some() {
        data_diff_key(app, tx, k);
        return;
    }
    // Data transfer (Alt-T): the wizard, its WHERE/LIMIT prompt and the report
    // are all modal.
    if app.transfer.as_ref().is_some_and(|w| w.prompt.is_some()) {
        transfer_prompt_key(app, tx, k);
        return;
    }
    if app.transfer.is_some() {
        transfer_key(app, tx, k);
        return;
    }
    if app.transfer_report.is_some() {
        transfer_report_key(app, tx, k);
        return;
    }
    if app.diff.is_some() {
        diff_key(app, tx, k);
        return;
    }
    if app.db_diff.is_some() {
        db_diff_key(app, tx, k);
        return;
    }

    // Recent-table overlay (Ctrl-Shift-R) is modal.
    if app.recent_open {
        recent_key(app, tx, k);
        return;
    }

    // R65: the in-data-view table switcher (`g b`) is modal too.
    if app.table_jump_open {
        table_jump_key(app, tx, k);
        return;
    }

    // Column-visibility overlay (Ctrl-Shift-H) is modal.
    if app.col_picker_open {
        col_picker_key(app, k);
        return;
    }

    // R48: the `gc` column-structure popup is modal too.
    if app.cols_popup_open {
        cols_popup_key(app, k);
        return;
    }

    // R75: the sidebar table-node info card (`i`) is modal too.
    if app.table_info_open {
        table_info_key(app, k);
        return;
    }

    // Ctrl-Shift view controls (mobile efficiency). Handled before the pane
    // handlers so a Shift is never dropped by the Ctrl-letter blocks below.
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.modifiers.contains(KeyModifiers::SHIFT) {
        match k.code {
            KeyCode::Char('c') | KeyCode::Char('C') => {
                toggle_compact(app);
                return;
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                open_col_picker(app);
                return;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                open_recent_tables(app);
                return;
            }
            _ => {}
        }
    }

    // The `g` chord (`gd` / `gt` / `gv` / `gc`) is resolved before the global `?`
    // / `d` shortcuts: `gd` reaches the results pane instead of opening the
    // database picker, and any other key clears a stale pending `g`.
    if app.pending_g {
        match k.code {
            // `gg` (go to top, R42) is resolved in the results pane too, so the
            // chord must reach `preview_key` instead of being cleared here.
            KeyCode::Char('d')
            | KeyCode::Char('t')
            | KeyCode::Char('v')
            | KeyCode::Char('c')
            | KeyCode::Char('b')
            | KeyCode::Char('g')
                if k.modifiers.is_empty() =>
            {
                preview_key(app, tx, k);
                return;
            }
            KeyCode::Esc => {
                app.pending_g = false;
                return;
            }
            _ => app.pending_g = false,
        }
    }

    // Help works from anywhere except the text inputs (where `?` is a character).
    // `F1` opens the very same cheat-sheet in *every* context, so the editor and
    // the command input — where `?` must stay a literal character — get a way in
    // too. (`Alt-H` was the spec's first pick, but it is already the query-history
    // panel, so the keybinding iron law keeps it there and this free key is the
    // fallback.)
    if (k.code == KeyCode::Char('?') && !matches!(app.focus, Focus::Editor | Focus::CmdInput))
        || k.code == KeyCode::F(1)
    {
        open_help(app);
        return;
    }

    // run from anywhere (browse page)
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('j'))
        || k.code == KeyCode::F(5)
    {
        run_current(app, tx);
        return;
    }

    // R77: step through the statements that failed in the last editor run.
    // `F8` / `Shift-F8` is the free, industry-standard "next / previous problem"
    // key and works from any pane (it focuses the editor). `Alt-E` shares the
    // connection-export mnemonic but only intercepts while a located error
    // exists and the focus is not the connection sidebar — the two contexts are
    // mutually exclusive, the same rule that lets `Alt-T` mean the template
    // panel in the editor and the transfer wizard elsewhere.
    if k.code == KeyCode::F(8) {
        let dir = if k.modifiers.contains(KeyModifiers::SHIFT) {
            -1
        } else {
            1
        };
        if !cycle_editor_error(app, dir) {
            app.status = t("没有可定位的执行错误").into();
        }
        return;
    }
    if k.modifiers.contains(KeyModifiers::ALT)
        && matches!(k.code, KeyCode::Char('e') | KeyCode::Char('E'))
        && !app.editor_error_spans.is_empty()
        && !matches!(app.focus, Focus::Sidebar | Focus::CmdInput)
    {
        cycle_editor_error(app, 1);
        return;
    }

    // Ctrl-O: DBX's saved SQL snippets, insertable into the editor. Allowed from
    // the editor too (that is where the snippet lands).
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && k.code == KeyCode::Char('o')
        && app.selected.is_some()
    {
        open_snippets(app, tx);
        return;
    }

    // Ctrl-P: run the editor's SQL through the dialect's EXPLAIN.
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && k.code == KeyCode::Char('p')
        && app.selected.is_some()
        && app.backend_kind == Backend::Sql
    {
        explain_current(app, tx);
        return;
    }

    // `I` imports a CSV into the focused table (sidebar) or the table open in
    // the data browser. Uppercase on purpose: lowercase `i` is quick-insert in
    // the results pane, and an import is a rare, deliberate action.
    if k.code == KeyCode::Char('I')
        && !k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && app.selected.is_some()
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        open_import_prompt(app);
        return;
    }

    // `d` opens the database list from any non-text area: one uniform gesture for
    // SQL databases, MongoDB databases and Redis logical DBs. R57: while the
    // results grid is in row-select mode, `d` is the batch-DELETE gesture, so the
    // picker yields and the key reaches the grid handler below.
    if k.code == KeyCode::Char('d')
        && k.modifiers.is_empty()
        && !(app.row_sel_anchor.is_some() && app.focus == Focus::Preview)
        && app.selected.is_some()
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        open_db_picker(app);
        return;
    }

    // transactional batch queue: Ctrl-S commits, Ctrl-X discards
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('s') {
        commit_batch(app);
        return;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('x') {
        if app.batch.is_empty() {
            app.status = t("批量队列为空（编辑时按 Ctrl-T 加入）").into();
        } else {
            let n = app.batch.len();
            app.batch.clear();
            app.status = tf("已清空批量队列（{} 条）", &[&(n)]);
        }
        return;
    }

    // R41: Alt-<digit> jumps straight to the Nth saved connection (in the same
    // order the picker shows), Alt-Tab / Alt-` toggles with the previous
    // connection, and Alt-Enter runs the statement under the cursor (or the
    // selection). Pane focus moves to Alt-Shift-1/2/3 — most terminals report
    // that as Alt-! / Alt-@ / Alt-#, which is accepted too — so it no longer
    // collides with the connection keys; Tab / Shift-Tab still cycle panes.
    if k.modifiers.contains(KeyModifiers::ALT) {
        match k.code {
            KeyCode::Char('1') if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                quick_switch_connection(app, tx, 1);
                return;
            }
            KeyCode::Char('2') if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                quick_switch_connection(app, tx, 2);
                return;
            }
            KeyCode::Char('3') if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                quick_switch_connection(app, tx, 3);
                return;
            }
            KeyCode::Char(c @ '4'..='9') if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                quick_switch_connection(app, tx, (c as u8 - b'0') as usize);
                return;
            }
            KeyCode::Char('!') | KeyCode::Char('1') => {
                app.focus = Focus::Sidebar;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Char('@') | KeyCode::Char('2') => {
                app.focus = Focus::Editor;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Char('#') | KeyCode::Char('3') => {
                app.focus = Focus::Preview;
                app.pane_override = [None; 3];
                return;
            }
            KeyCode::Tab => {
                toggle_last_connection(app, tx);
                return;
            }
            KeyCode::Char('`') => {
                toggle_last_connection(app, tx);
                return;
            }
            KeyCode::Enter => {
                run_current_scoped(app, tx, RunScope::CurrentStatement);
                return;
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                toggle_compact(app);
                return;
            }
            // Alt-H is the query-history panel; column visibility keeps its
            // `c` (results pane) and `Ctrl-Shift-H` bindings plus Alt-V here.
            KeyCode::Char('v') | KeyCode::Char('V') => {
                open_col_picker(app);
                return;
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                open_recent_tables(app);
                return;
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                open_history(app, tx);
                return;
            }
            // Alt-G: scan every table's text columns for a term.
            KeyCode::Char('g') | KeyCode::Char('G') => {
                open_global_search(app);
                return;
            }
            // Alt-D: two-table structure diff; Shift+Alt-D: two-database diff.
            KeyCode::Char('d') => {
                open_diff_picker(app, DiffPickMode::Table, DiffKind::Schema);
                return;
            }
            KeyCode::Char('D') => {
                open_diff_picker(app, DiffPickMode::Database, DiffKind::Schema);
                return;
            }
            // Alt-K: two-table data compare (primary-key aligned).
            KeyCode::Char('k') | KeyCode::Char('K') => {
                open_diff_picker(app, DiffPickMode::Table, DiffKind::Data);
                return;
            }
            // Alt-T: with the editor focused it opens the built-in SQL template
            // panel (R71); everywhere else it is the data-transfer wizard, which
            // needs a focused table and is useless from the editor. Both share
            // the mnemonic because only one of them can apply at a time.
            KeyCode::Char('t') | KeyCode::Char('T') => {
                if app.focus == Focus::Editor {
                    open_template_panel(app);
                } else {
                    open_transfer_wizard(app);
                }
                return;
            }
            // Alt-E: export every saved connection as a JSON bundle.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                open_conn_export(app);
                return;
            }
            // Alt-I: import connections from a dbxt / DBeaver / Navicat file.
            KeyCode::Char('i') | KeyCode::Char('I') => {
                open_conn_import(app);
                return;
            }
            // Alt-P: pick a saved SQL snippet and paste it at the cursor — the
            // one-step version of the Ctrl-O panel (which appends to the end).
            KeyCode::Char('p') | KeyCode::Char('P') => {
                open_snippets_at_cursor(app, tx);
                return;
            }
            // Alt-O: toggle the script-output console feel — a separator line
            // and a `12.3ms` prefix per statement (off by default). Alt-T is
            // already the data-transfer wizard, so the timing toggle takes the
            // free Alt-O mnemonic.
            KeyCode::Char('o') | KeyCode::Char('O') => {
                app.show_stmt_timing = !app.show_stmt_timing;
                app.status = if app.show_stmt_timing {
                    t("语句分隔 + 耗时 开（Alt-O 关）").into()
                } else {
                    t("语句分隔 + 耗时 关（Alt-O 开）").into()
                };
                return;
            }
            // Alt-← / Alt-→: browser-style back / forward through the tables you
            // have browsed (max 50). The text panes keep the arrows local so
            // editing SQL is never yanked into another table; the history keys
            // are for the sidebar and results panes, where the A→B→A workflow
            // lives.
            KeyCode::Left if !matches!(app.focus, Focus::Editor | Focus::CmdInput) => {
                nav_back(app, tx);
                return;
            }
            KeyCode::Right if !matches!(app.focus, Focus::Editor | Focus::CmdInput) => {
                nav_forward(app, tx);
                return;
            }
            _ => {}
        }
    }

    // Shift-← / Shift-→ pan the column window from any pane — the keyboard twin of
    // a horizontal swipe, for terminals that report neither a horizontal wheel nor
    // a drag. Holding the key repeats (the terminal auto-repeats), so a long press
    // scrolls continuously. The text inputs keep Shift-←/→ for selection.
    if k.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(k.code, KeyCode::Left | KeyCode::Right)
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        pan_columns(app, if k.code == KeyCode::Left { -1 } else { 1 });
        return;
    }

    // Tab / Shift-Tab cycle panes; B toggles the focused pane's collapse state.
    // R71: while a just-inserted template still has `{{…}}` placeholders, Tab
    // walks them instead — the editor keeps Tab for the placeholder jump, and
    // falls through to the pane switch once the last one is filled.
    if k.code == KeyCode::Tab && k.modifiers.is_empty() {
        if app.focus == Focus::Editor && app.template_active && jump_next_placeholder(app) {
            return;
        }
        cycle_focus(app, true);
        return;
    }
    if k.code == KeyCode::BackTab {
        cycle_focus(app, false);
        return;
    }

    // Layout toggles are Ctrl-combos so no bare uppercase key is needed, and they
    // are kept out of the text inputs so typing is never hijacked.
    //   Ctrl-A = auto-collapse master switch
    //   Ctrl-W = collapse / expand just the focused pane
    if k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT)
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        match k.code {
            KeyCode::Char('a') => {
                toggle_auto_collapse(app);
                return;
            }
            KeyCode::Char('w') => {
                toggle_pane_collapse(app);
                return;
            }
            // Ctrl-G: fallback for phone terminals that never send a horizontal
            // wheel — the vertical wheel (the one gesture every terminal has)
            // pans columns instead of rows.
            KeyCode::Char('g') => {
                app.pan_mode = !app.pan_mode;
                app.status = if app.pan_mode {
                    t("横滚 开 · 滚轮横滚列").into()
                } else {
                    t("横滚 关 · 滚轮纵向").into()
                };
                return;
            }
            _ => {}
        }
    }

    // Bare `q` quits (two-stage when unrun work exists), but only with an
    // active connection and no text surface focused: in the editor / command
    // line `q` stays a literal character, and every overlay above already owns
    // `q` to close itself.
    if k.modifiers.is_empty()
        && k.code == KeyCode::Char('q')
        && app.selected.is_some()
        && !matches!(app.focus, Focus::Editor | Focus::CmdInput)
    {
        request_quit(app);
        return;
    }

    match app.focus {
        Focus::Sidebar => sidebar_key(app, tx, k),
        Focus::Editor => editor_key(app, tx, k),
        Focus::CmdInput => cmd_input_key(app, tx, k),
        Focus::Preview => preview_key(app, tx, k),
    }
}

// ── database / schema switcher overlay ──

/// What a `d`-overlay row selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickerKind {
    Schema,
    Database,
}

/// Entries shown by the `d` overlay. Redis exposes its 16 logical databases;
/// schema-aware SQL engines list the current database's schemas first and then
/// the server's databases; everything else lists databases only.
pub(crate) fn picker_entries(app: &App) -> Vec<(PickerKind, String)> {
    if app.backend_kind == Backend::Redis {
        return (0..16)
            .map(|i| (PickerKind::Database, format!("db{i}")))
            .collect();
    }
    let mut out = Vec::with_capacity(app.schemas.len() + app.databases.len());
    for s in &app.schemas {
        out.push((PickerKind::Schema, s.clone()));
    }
    for d in &app.databases {
        out.push((PickerKind::Database, d.clone()));
    }
    out
}

/// Row label. When the schema layer is active the two kinds are prefixed so the
/// single list still reads as "schemas first, then databases".
pub(crate) fn picker_label(app: &App, kind: PickerKind, name: &str) -> String {
    if app.schemas.is_empty() {
        return name.to_string();
    }
    match kind {
        PickerKind::Schema => format!("{} · {}", t("模式"), name),
        PickerKind::Database => format!("{} · {}", t("数据库"), name),
    }
}

pub(crate) fn db_entries(app: &App) -> Vec<String> {
    picker_entries(app)
        .iter()
        .map(|(kind, name)| picker_label(app, *kind, name))
        .collect()
}

pub(crate) fn db_current_index(app: &App) -> usize {
    if app.backend_kind == Backend::Redis {
        return app.redis_db as usize;
    }
    picker_entries(app)
        .iter()
        .position(|(kind, name)| match kind {
            PickerKind::Schema => *name == app.schema,
            PickerKind::Database => *name == app.current_db(),
        })
        .unwrap_or(0)
}

pub(crate) fn open_db_picker(app: &mut App) {
    let n = db_entries(app).len();
    if n == 0 {
        app.status = t("没有可切换的数据库").into();
        return;
    }
    app.db_picker_open = true;
    let cur = db_current_index(app).min(n - 1);
    app.db_list.select(Some(cur));
}

pub(crate) fn db_picker_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = db_entries(app).len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.db_picker_open = false;
            app.flash(t("已关闭库列表").into());
        }
        KeyCode::Char('d') if k.modifiers.is_empty() => {
            app.db_picker_open = false;
        }
        KeyCode::Char('r') if k.modifiers.is_empty() => {
            // Refresh the list in place, keeping the overlay open.
            if app.backend_kind == Backend::Redis {
                app.status = t("Redis 固定 16 个逻辑库").into();
            } else if let Some(cfg) = app.selected.clone() {
                app.status = t("刷新数据库列表…").into();
                app.spawn(tx, Op::DatabasesRefresh(Box::new(cfg.clone())));
                // Schemas are cached per database; a refresh is the moment to
                // pick up a schema created outside dbxt.
                if schema_picker_engine(cfg.db_type) {
                    app.schemas.clear();
                    app.schemas_db.clear();
                    app.spawn(tx, Op::ListSchemas(Box::new(cfg), app.current_db()));
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app
                    .db_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.db_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .db_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.db_list.select(Some(i));
            }
        }
        KeyCode::Home => {
            if n > 0 {
                app.db_list.select(Some(0));
            }
        }
        KeyCode::End => {
            if n > 0 {
                app.db_list.select(Some(n - 1));
            }
        }
        KeyCode::Enter => {
            if let Some(i) = app.db_list.selected() {
                db_picker_apply(app, tx, i);
            }
        }
        _ => {}
    }
}

pub(crate) fn db_picker_apply(app: &mut App, tx: &Tx, idx: usize) {
    app.db_picker_open = false;
    if app.backend_kind == Backend::Redis {
        app.redis_db = idx as u32;
        app.redis_value = None;
        app.clear_grid();
        app.set_placeholder();
        app.redis_filter.clear();
        app.redis_filter_prompt = None;
        app.redis_jump_letter = None;
        app.status = format!("redis db → {idx}");
        start_redis_scan(app, tx, true);
        return;
    }
    let Some((kind, name)) = picker_entries(app).into_iter().nth(idx) else {
        return;
    };
    match kind {
        PickerKind::Schema => {
            app.schema = name.clone();
            app.status = tf("切换 schema → {}", &[&(fix_double_encoding(&name))]);
            reload_tables(app, tx);
        }
        PickerKind::Database => {
            let Some(pos) = app.databases.iter().position(|d| *d == name) else {
                return;
            };
            app.db_index = pos;
            // The cached schema list belongs to the old database.
            app.schemas.clear();
            app.schemas_db.clear();
            app.status = tf("切换数据库 → {}", &[&(fix_double_encoding(&name))]);
            reload_tables(app, tx);
        }
    }
}

// ── vim-style count prefix (`5n`, `3j`) + numeric direct jump ──

/// A bare digit waits this long for a motion key before it is taken as a direct
/// list jump (`3` → third item). Kept short so a jump still feels immediate.
pub(crate) const COUNT_JUMP_TIMEOUT: Duration = Duration::from_millis(350);

/// How long the one-shot `Enter 看整行 · v 看单元格` discovery hint stays up
/// before it fades (any key clears it sooner).
pub(crate) const ROW_HINT_TTL: Duration = Duration::from_secs(3);

/// The one-shot table-view discovery hint (translated at call time).
pub(crate) fn row_hint_text() -> &'static str {
    t("Enter 看整行 · v 看单元格")
}

/// Show the one-shot table-view discovery hint the first time a data grid owns
/// the screen this session. A query / table / Redis / Mongo grid all qualify;
/// the structure list does not.
pub(crate) fn maybe_show_row_hint(app: &mut App) {
    if app.row_hint_shown {
        return;
    }
    let data_grid = app.focus == Focus::Preview
        && matches!(
            app.grid_kind,
            GridKind::Query | GridKind::TableData | GridKind::RedisValue | GridKind::MongoDocs
        )
        && active_grid(app).is_some_and(|g| !g.rows.is_empty());
    if data_grid {
        app.row_hint_shown = true;
        app.status = row_hint_text().into();
        app.row_hint_until = Some(Instant::now() + ROW_HINT_TTL);
    }
}

/// R75: clear an expired `Esc` flash from the status bar. Runs on the UI tick so
/// a "关闭 X" / "已清除 Y" message fades after [`FLASH_TTL`] without a key press.
/// The clear is guarded by `flash_text` so a newer status is never wiped.
pub(crate) fn expire_flash(app: &mut App) {
    if app
        .flash_until
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        if app.status == app.flash_text {
            app.status.clear();
        }
        app.flash_until = None;
    }
}

/// Parse a count buffer into a repetition count (`None` for empty / zero).
pub(crate) fn parse_count(buf: &str) -> Option<u32> {
    if buf.is_empty() {
        return None;
    }
    match buf.parse::<u32>() {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// Target index for a direct `N` jump in a list of `len` items: 1-based, clamped
/// to the last item. `None` when the list is empty.
pub(crate) fn count_jump_index(count: u32, len: usize) -> Option<usize> {
    if len == 0 || count == 0 {
        return None;
    }
    Some(((count as usize) - 1).min(len - 1))
}

/// True for the keys a count may prefix in every pane (the common motions).
pub(crate) fn count_motion(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('j')
            | KeyCode::Char('k')
    )
}

/// Shared pre-dispatch for the panes that accept a count. Returns `true` when the
/// key was fully consumed (a buffered digit, or Esc cancelling a pending count);
/// `false` lets the caller handle the key normally, after flushing a pending
/// count as a direct list jump when `is_motion` is false.
pub(crate) fn count_pre(app: &mut App, tx: &Tx, k: KeyEvent, is_motion: bool) -> bool {
    if let KeyCode::Char(c @ '1'..='9') = k.code {
        if k.modifiers.is_empty() && app.count_buf.len() < 4 {
            app.count_buf.push(c);
            app.count_deadline = Some(Instant::now() + COUNT_JUMP_TIMEOUT);
            app.status = format!("{}·", app.count_buf);
            return true;
        }
    }
    if app.count_active() {
        if k.code == KeyCode::Esc {
            app.clear_count();
            app.flash(t("已取消计数").into());
            return true;
        }
        if !is_motion {
            flush_count(app, tx);
        }
    }
    false
}

/// Consume a pending count as a repetition count (1 when none is buffered).
pub(crate) fn take_count(app: &mut App) -> u32 {
    let n = parse_count(&app.count_buf).unwrap_or(1);
    app.clear_count();
    n
}

/// Apply a count whose motion key never arrived: a bare `N` jumps to the Nth
/// item of the sidebar list (connections, or the tables of the current
/// connection). Called from the event-loop tick and by [`count_pre`].
pub(crate) fn flush_count(app: &mut App, tx: &Tx) {
    let Some(n) = parse_count(&app.count_buf) else {
        app.clear_count();
        return;
    };
    app.clear_count();
    let _ = tx;
    if app.focus != Focus::Sidebar {
        return;
    }
    let picker = app.selected.is_none();
    // Redis keys, SQL tables and Mongo collections all accept a direct numeric
    // jump; the KV / document lists are the R42 additions.
    let redis = !picker && app.backend_kind == Backend::Redis;
    let len = if picker {
        app.connections.len()
    } else if redis {
        app.redis_scan.keys.len()
    } else {
        app.tables.len()
    };
    if let Some(i) = count_jump_index(n, len) {
        if picker {
            app.conn_list.select(Some(i));
        } else if redis {
            app.redis_list.select(Some(i));
        } else {
            app.table_list.select(Some(i));
            // R43: the tree cursor follows the table jump.
            rebuild_side_rows(app);
        }
        app.status = tf("跳转 {}/{}", &[&(i + 1), &(len)]);
    }
}

/// Move a list cursor by a (possibly counted) number of items, clamped to the
/// list bounds.
pub(crate) fn list_step(state: &mut ListState, len: usize, step: usize, forward: bool) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0);
    let i = if forward {
        (cur + step).min(len - 1)
    } else {
        cur.saturating_sub(step)
    };
    state.select(Some(i));
}

impl App {
    /// True while digits are buffered as a count prefix.
    pub(crate) fn count_active(&self) -> bool {
        !self.count_buf.is_empty()
    }

    /// Drop any pending count prefix.
    pub(crate) fn clear_count(&mut self) {
        self.count_buf.clear();
        self.count_deadline = None;
    }
}

// ── sidebar: connection picker or table browser ──
pub(crate) fn sidebar_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // A leading digit is a count prefix: with a motion it repeats it, alone it
    // jumps to the Nth connection / table. Text inputs are handled above this
    // dispatch, so a filter prompt still receives digits as text.
    if count_pre(app, tx, k, count_motion(k.code)) {
        return;
    }
    // no connection selected yet → picker mode
    if app.selected.is_none() {
        match k.code {
            KeyCode::Char('c') => {
                app.page = Page::NewConn;
                app.form = ConnForm::default();
            }
            // Duplicate the highlighted connection into the form.
            KeyCode::Char('p') => duplicate_connection(app),
            // Edit the highlighted connection in place (form prefilled).
            KeyCode::Char('e') => edit_connection(app),
            // Cycle the picker order: name → type → colour.
            KeyCode::Char('s') => {
                app.conn_sort = app.conn_sort.next();
                app.sort_connections();
                app.status = tf(
                    "排序：{} · s 切换（名称/类型/颜色）",
                    &[&app.conn_sort.label()],
                );
            }
            // R47b: disconnect the highlighted connection. The picker is the one
            // place every backend can reach the action (the Redis browser has no
            // tree root row to put `x` on).
            KeyCode::Char('d') => {
                if let Some(idx) = app.conn_list.selected() {
                    if let Some(cfg) = app.connections.get(idx).cloned() {
                        open_disconnect_confirm(app, &cfg);
                    }
                }
            }
            // Delete the highlighted connection (red confirm; config only).
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(idx) = app.conn_list.selected() {
                    if let Some(cfg) = app.connections.get(idx) {
                        app.confirm = Some(Confirm {
                            sql: String::new(),
                            reasons: Vec::new(),
                            refresh: false,
                            clear_batch: false,
                            conn: Some(ConnConfirm {
                                id: cfg.id.clone(),
                                name: cfg.name.clone(),
                                db_type: cfg.db_type.as_str().to_string(),
                                disconnect: false,
                                readonly: None,
                            }),
                            redis: None,
                            mongo: None,
                        });
                        app.status = tf("删除连接 {} · Enter 确认 · Esc 取消", &[&cfg.name]);
                    }
                }
            }
            KeyCode::Char('q') => {
                app.picker_open = !app.picker_open;
            }
            KeyCode::Tab => {
                app.focus = Focus::Editor;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = app.connections.len();
                let step = take_count(app) as usize;
                list_step(&mut app.conn_list, n, step, false);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = app.connections.len();
                let step = take_count(app) as usize;
                list_step(&mut app.conn_list, n, step, true);
            }
            KeyCode::Home => {
                take_count(app);
                if !app.connections.is_empty() {
                    app.conn_list.select(Some(0));
                }
            }
            KeyCode::End => {
                take_count(app);
                let n = app.connections.len();
                if n > 0 {
                    app.conn_list.select(Some(n - 1));
                }
            }
            KeyCode::PageUp => {
                let n = app.connections.len();
                let step = 10 * take_count(app) as usize;
                list_step(&mut app.conn_list, n, step, false);
            }
            KeyCode::PageDown => {
                let n = app.connections.len();
                let step = 10 * take_count(app) as usize;
                list_step(&mut app.conn_list, n, step, true);
            }
            KeyCode::Enter => {
                connect_selected(app, tx);
            }
            _ => {}
        }
        return;
    }

    // Redis connection → key browser (SCAN pagination, pattern filter,
    // multi-select batch operations).
    if app.backend_kind == Backend::Redis {
        // Shift + ↑/↓/Home/End extends a multi-select range from the anchor.
        if k.modifiers.contains(KeyModifiers::SHIFT) {
            match k.code {
                KeyCode::Up => {
                    let i = app.redis_list.selected().unwrap_or(0).saturating_sub(1);
                    redis_select_range(app, i);
                    return;
                }
                KeyCode::Down => {
                    let n = app.redis_scan.keys.len();
                    let i = (app.redis_list.selected().unwrap_or(0) + 1).min(n.saturating_sub(1));
                    redis_select_range(app, i);
                    return;
                }
                KeyCode::Home => {
                    redis_select_range(app, 0);
                    return;
                }
                KeyCode::End => {
                    let n = app.redis_scan.keys.len();
                    if n > 0 {
                        redis_select_range(app, n - 1);
                    }
                    return;
                }
                _ => {}
            }
        }
        // Ctrl-D is the delete shortcut the results pane also uses.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('d') {
            redis_batch_delete(app);
            return;
        }
        // R81: Ctrl-T cycles the loaded-key ordering scan → TTL↑ → TTL↓. Pure
        // client-side re-sorting of the loaded window; never a query.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('t') {
            cycle_redis_sort(app);
            return;
        }
        // R42 first-letter jump: Alt+<letter> cycles to the next loaded key
        // starting with that letter (the KV twin of the sidebar table jump).
        if k.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = k.code {
                if c.is_alphabetic() {
                    match redis_jump_by_letter(app, c, 1) {
                        Some(i) => {
                            let name = fix_double_encoding(&app.redis_scan.keys[i].key_display);
                            app.status = tf(
                                "首字母跳「{}」→ {} · Alt+字母 循环 · ; , 前后跳",
                                &[&c, &name],
                            );
                        }
                        None => {
                            app.status = tf("没有以「{}」开头的 key", &[&c]);
                        }
                    }
                    return;
                }
            }
        }
        match k.code {
            KeyCode::Tab => app.focus = Focus::Editor,
            KeyCode::Char('c') => {
                app.page = Page::NewConn;
                app.form = ConnForm::default();
            }
            KeyCode::Char('p') => duplicate_connection(app),
            KeyCode::Char('o') => back_to_picker(app),
            // `r` rescans from cursor 0 with the current pattern.
            KeyCode::Char('r') => {
                app.status = t("重新扫描 keys…").into();
                start_redis_scan(app, tx, true);
            }
            // `/` edits the server-side MATCH pattern.
            KeyCode::Char('/') => open_redis_pattern_prompt(app),
            // `f` filters the *loaded* keys client-side (substring, live); any
            // printable character does the same in one step below.
            KeyCode::Char('f') => open_redis_filter(app),
            // `;` / `,` repeat the last first-letter jump forward / backward.
            KeyCode::Char(';') => repeat_redis_jump(app, 1),
            KeyCode::Char(',') => repeat_redis_jump(app, -1),
            // `n` fetches the next SCAN page.
            KeyCode::Char('n') => start_redis_scan(app, tx, false),
            // Space toggles one key; `a` selects every loaded key.
            KeyCode::Char(' ') => {
                redis_toggle_select(app);
                let n = app.redis_selected.len();
                app.status = if n > 0 {
                    tf("已选 {} 个 key", &[&n])
                } else {
                    t("已清除选择").into()
                };
            }
            KeyCode::Char('a') => redis_select_all(app),
            // `y` copies the selected key names (or the focused one).
            KeyCode::Char('y') => redis_copy_selection(app),
            // R81: `T` sets the focused key's TTL (red confirm, read-only
            // blocked); `t` cycles the client-side type filter over the loaded
            // keys. Both are pure-local until confirmed: no browsing query.
            KeyCode::Char('T') => open_redis_key_ttl_prompt(app),
            KeyCode::Char('t') => cycle_redis_type_filter(app),
            // Batch operations act on the selection (focused key when empty).
            KeyCode::Delete => redis_batch_delete(app),
            KeyCode::Char('x') => open_redis_batch_ttl_prompt(app),
            KeyCode::Char('m') => open_redis_batch_rename_prompt(app),
            // R68: `!` flips the connection's read-only policy from the key
            // browser too (no tree root row exists in Redis mode).
            KeyCode::Char('!') if k.modifiers.is_empty() => open_readonly_toggle_confirm(app),
            // Esc clears the client-side filter first, then the selection.
            KeyCode::Esc => {
                if !app.redis_filter.is_empty() {
                    clear_redis_filter(app);
                    app.flash(tf(
                        "已清除 key 过滤 · {} 个 key",
                        &[&(app.redis_scan.all.len())],
                    ));
                } else if !app.redis_selected.is_empty() {
                    app.redis_selected.clear();
                    app.redis_anchor = None;
                    app.flash(t("已清除选择").into());
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = app.redis_scan.keys.len();
                let step = take_count(app) as usize;
                if n > 0 {
                    let i = app.redis_list.selected().unwrap_or(0).saturating_sub(step);
                    app.redis_list.select(Some(i));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = app.redis_scan.keys.len();
                let step = take_count(app) as usize;
                if n > 0 {
                    let cur = app.redis_list.selected().unwrap_or(0);
                    if cur + step >= n && !app.redis_scan.exhausted {
                        if step == 1 {
                            // At the last loaded key: pull the next page, keeping the cursor.
                            start_redis_scan(app, tx, false);
                        } else {
                            app.redis_list.select(Some(n - 1));
                        }
                    } else {
                        app.redis_list.select(Some((cur + step).min(n - 1)));
                    }
                }
            }
            KeyCode::Home => {
                if !app.redis_scan.keys.is_empty() {
                    app.redis_list.select(Some(0));
                }
            }
            KeyCode::End => {
                let n = app.redis_scan.keys.len();
                if n > 0 {
                    app.redis_list.select(Some(n - 1));
                }
                if !app.redis_scan.exhausted {
                    start_redis_scan(app, tx, false);
                }
            }
            KeyCode::Enter => open_redis_value(app, tx),
            KeyCode::Left | KeyCode::Char('h') => cycle_redis_db(app, tx, false),
            KeyCode::Right | KeyCode::Char('l') => cycle_redis_db(app, tx, true),
            // One-step type-to-filter (R42): any printable character that is not
            // a bound shortcut starts the client-side filter with that character
            // already typed, so a key lookup is a single keystroke.
            KeyCode::Char(c)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT)
                    && !c.is_ascii_control() =>
            {
                if app.redis_scan.all.is_empty() {
                    app.status = t("还没有 key 可过滤").into();
                } else {
                    open_redis_filter_with(app, Some(c));
                    app.status = tf(
                        "过滤「{}」· {} 个命中 · Enter 查看首位",
                        &[&(app.redis_filter), &(app.redis_scan.keys.len())],
                    );
                }
            }
            _ => {}
        }
        return;
    }

    // connection selected → connection tree (R43): connection → database → table
    // under a single cursor.
    rebuild_side_rows(app);
    // R39 first-letter jump: Alt+<letter> cycles to the next table whose name
    // starts with that letter (vim `f`-style). Plain letters are reserved for
    // the one-step type-to-filter below, so the jump takes the modifier; `;` /
    // `,` then repeat it forward / backward like vim.
    if k.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c) = k.code {
            if c.is_alphabetic() {
                match table_jump_by_letter(app, c, 1) {
                    Some(i) => {
                        let name = fix_double_encoding(&app.tables[i].name);
                        app.status = tf(
                            "首字母跳「{}」→ {} · Alt+字母 循环 · ; , 前后跳",
                            &[&c, &name],
                        );
                        rebuild_side_rows(app);
                    }
                    None => {
                        app.status = tf("没有以「{}」开头的表", &[&c]);
                    }
                }
                return;
            }
        }
    }
    // R55: Shift+↑/↓ reorders the row under the cursor within its sibling list
    // — the terminal twin of dragging a row in a GUI tree. Only rows with an
    // explicit persisted order can move (a node inside a desktop group, or a
    // top-level group); the tree cursor follows the row so a held key walks it.
    if k.modifiers.contains(KeyModifiers::SHIFT)
        && !k.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(k.code, KeyCode::Up | KeyCode::Down)
    {
        let dir = if k.code == KeyCode::Up { -1 } else { 1 };
        move_side_row(app, tx, dir);
        return;
    }
    match k.code {
        KeyCode::Tab => app.focus = Focus::Editor,
        KeyCode::Char('c') => {
            app.page = Page::NewConn;
            app.form = ConnForm::default();
        }
        // Duplicate the current connection into the form (new id on save).
        KeyCode::Char('p') => duplicate_connection(app),
        // `Y` — copy the connection under the cursor as `xxx-copy` (password +
        // SSH tunnel included) and show the new root immediately (R45).
        KeyCode::Char('Y') => copy_connection_at_cursor(app, tx),
        KeyCode::Char('o') => back_to_picker(app),
        // R55: `r` on a connection root / group row renames it in place (Enter
        // saves, Esc cancels). On every other row the long-standing meaning —
        // open the selected table's structure — is unchanged.
        KeyCode::Char('r')
            if matches!(
                app.side_rows.get(app.side_sel),
                Some(SideRow::Conn { .. }) | Some(SideRow::Group { .. })
            ) =>
        {
            open_rename_edit(app);
        }
        KeyCode::Char('r') => load_structure(app, tx),
        // `s` — on a database row: lazily fetch that database's size (R45).
        // Everywhere else: cycle the sidebar order (name → type TABLE/VIEW).
        KeyCode::Char('s') => {
            if matches!(app.side_rows.get(app.side_sel), Some(SideRow::Db { .. })) {
                request_db_size(app, tx);
            } else {
                app.cycle_table_sort();
            }
        }
        // `;` / `,` repeat the last Alt+letter jump forward / backward.
        KeyCode::Char(';') => {
            repeat_table_jump(app, 1);
            rebuild_side_rows(app);
        }
        KeyCode::Char(',') => {
            repeat_table_jump(app, -1);
            rebuild_side_rows(app);
        }
        // `/` — filter-as-you-type over the tree's visible nodes (vim-style),
        // the fast way to reach a table when the sidebar is long.
        KeyCode::Char('/') => open_table_filter(app),
        // `f` — R54 quick search over the *loaded* tree cache (connection / db /
        // table names, across groups). Distinct from `/`: hit groups open
        // automatically and Enter jumps to the first hit then clears the
        // needle. Pure client-side, never a query.
        KeyCode::Char('f') => open_tree_search(app),
        // `t` — jump straight to one of the last five browsed tables.
        KeyCode::Char('t') => open_recent_tables(app),
        // Tree navigation: `j`/`k` walk the whole tree (connections, databases,
        // tables) with one cursor; a vim count prefix repeats the step.
        KeyCode::Up | KeyCode::Char('k') => {
            let step = take_count(app) as usize;
            side_step(app, step, false);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let step = take_count(app) as usize;
            side_step(app, step, true);
        }
        KeyCode::Home => {
            take_count(app);
            if !app.side_rows.is_empty() {
                app.side_sel = 0;
                side_mirror_table(app);
            }
        }
        KeyCode::End => {
            take_count(app);
            let n = app.side_rows.len();
            if n > 0 {
                app.side_sel = n - 1;
                side_mirror_table(app);
            }
        }
        KeyCode::PageUp => {
            let step = 10 * take_count(app) as usize;
            side_step(app, step, false);
        }
        KeyCode::PageDown => {
            let step = 10 * take_count(app) as usize;
            side_step(app, step, true);
        }
        KeyCode::Enter => side_activate(app, tx),
        // vim tree: `l`/`→` expands, `h`/`←` collapses (or steps to the parent).
        KeyCode::Left | KeyCode::Char('h') => side_collapse(app),
        KeyCode::Right | KeyCode::Char('l') => side_expand(app, tx),
        // `[` / `]` keep the old fast database cycle now that `h`/`l` belong to
        // the tree; `d` is still the discoverable database list.
        KeyCode::Char('[') => cycle_db(app, tx, false),
        KeyCode::Char(']') => cycle_db(app, tx, true),
        // R47b: `x` on a connection root opens the red disconnect confirmation.
        // On any other row it falls through to the type-to-filter below, so the
        // key stays free for the table filter everywhere it is not a root.
        KeyCode::Char('x')
            if matches!(app.side_rows.get(app.side_sel), Some(SideRow::Conn { .. })) =>
        {
            if let Some(SideRow::Conn { idx, .. }) = app.side_rows.get(app.side_sel).cloned() {
                request_disconnect(app, idx);
            }
        }
        // R50: a group row holds no connection pool, so `x` is a no-op; the key
        // is still *caught* so it cannot leak into the one-step type-to-filter
        // below (which is what a plain `x` did before).
        KeyCode::Char('x')
            if matches!(app.side_rows.get(app.side_sel), Some(SideRow::Group { .. })) =>
        {
            app.status = t("分组行：x 无动作（连接根上按 x 断开）").into();
        }
        // R68: `!` flips the read-only policy of the connection under the cursor
        // (the active one on a db / table row) behind the red confirmation
        // layer. Caught before the type-to-filter below so it is never text.
        KeyCode::Char('!') if k.modifiers.is_empty() => open_readonly_toggle_confirm(app),
        // R75: `i` on a *table* node opens the cached-metadata info card (row
        // estimate / size / engine / comment / created-time, never a query). On
        // any other row it falls through to the one-step type-to-filter, so a
        // filter can still be started with `i` from a connection / database row.
        KeyCode::Char('i')
            if k.modifiers.is_empty()
                && matches!(app.side_rows.get(app.side_sel), Some(SideRow::Table { .. })) =>
        {
            open_table_info(app);
        }
        // One-step type-to-filter (R39): any printable character that is not a
        // bound shortcut starts the filter with that character already typed,
        // so a lookup is a single keystroke instead of `/` then type.
        KeyCode::Char(c)
            if !k.modifiers.contains(KeyModifiers::CONTROL)
                && !k.modifiers.contains(KeyModifiers::ALT)
                && !c.is_ascii_control() =>
        {
            if app.tables_all.is_empty() {
                app.status = t("还没有表可过滤").into();
            } else {
                open_table_filter_with(app, Some(c));
                app.status = tf(
                    "过滤「{}」· {} 个命中 · Enter 打开首位",
                    &[&(app.table_filter), &(app.tables.len())],
                );
            }
        }
        _ => {}
    }
}

/// `;` / `,` in the sidebar: repeat the last first-letter jump forward / backward.
pub(crate) fn repeat_table_jump(app: &mut App, dir: i32) {
    let Some(letter) = app.table_jump_letter else {
        app.status = t("先用 Alt+字母 做首字母跳，再用 ; , 循环").into();
        return;
    };
    match table_jump_by_letter(app, letter, dir) {
        Some(i) => {
            let name = fix_double_encoding(&app.tables[i].name);
            app.status = tf("首字母跳「{}」→ {}", &[&letter, &name]);
        }
        None => app.status = tf("没有以「{}」开头的表", &[&letter]),
    }
}

pub(crate) fn load_structure(app: &mut App, tx: &Tx) {
    let Some(table) = app.selected_table().map(|t| t.name.clone()) else {
        app.status = t("先选中一张表").into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    if app.backend_kind == Backend::Mongo {
        app.loading = true;
        app.status = tf("加载 {} 索引…", &[&(fix_double_encoding(&table))]);
        let db = app.current_db();
        app.spawn(
            tx,
            Op::MongoIndexes {
                cfg: Box::new(cfg),
                db,
                collection: table,
            },
        );
        return;
    }
    app.loading = true;
    app.status = tf("加载 {} 结构…", &[&(fix_double_encoding(&table))]);
    let db = app.current_db();
    let schema = app.schema.clone();
    app.spawn(
        tx,
        Op::Columns(
            Box::new(cfg.clone()),
            db.clone(),
            schema.clone(),
            table.clone(),
        ),
    );
    app.spawn(tx, Op::Ddl(Box::new(cfg), db, schema, table));
}

pub(crate) fn open_table_data(app: &mut App, tx: &Tx) {
    let Some(table) = app
        .selected_table()
        .map(|t| (t.name.clone(), t.table_type.clone()))
    else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    if app.backend_kind == Backend::Mongo {
        app.clear_grid();
        app.script = None;
        app.ddl = None;
        app.struct_view = StructView::Fields;
        app.col_offset = 0;
        app.col_cursor = 0;
        app.cell_popup = None;
        app.row_popup = None;
        app.sel = 0;
        app.pending_sel = Some(0);
        app.pending_focus = Some(Focus::Preview);
        app.result_needle.clear();
        app.result_filter = None;
        app.clear_col_filter();
        app.pending_table_filter = None;
        remember_recent_table(app, &app.current_db(), "", &table.0);
        open_mongo_collection(app, tx);
        return;
    }
    app.clear_grid();
    app.script = None;
    app.ddl = None;
    app.struct_view = StructView::Fields;
    app.col_offset = 0;
    app.col_cursor = 0;
    app.cell_popup = None;
    app.row_popup = None;
    app.sel = 0;
    app.pending_sel = Some(0);
    app.pending_focus = Some(Focus::Preview);
    app.page_pending = true;
    app.table_meta = None;
    app.deep_page_hint_shown = false;
    app.pending_deep_hint = false;
    app.result_needle.clear();
    app.result_filter = None;
    app.clear_col_filter();
    // A search-hit jump stashes a pre-filter here; consume it once.
    let initial_filter = app.pending_table_filter.take().unwrap_or_default();
    let cur_db = app.current_db();
    let cur_schema = app.schema.clone();
    remember_recent_table(app, &cur_db, &cur_schema, &table.0);
    // Restore this table's persisted preferences (db.schema.table granularity).
    let db = app.current_db();
    let schema = app.schema.clone();
    let prefs = app
        .config
        .table(&db, &schema, &table.0)
        .cloned()
        .unwrap_or_default();
    app.col_hidden = prefs.hidden;
    // Fall back to the *global* default (not whatever the previously opened table
    // happened to use), so a table with no stored choice is not contaminated.
    app.compact = prefs.compact.or(app.config.compact);
    let order_by = prefs.order_by;
    app.page_state = Some(PageState {
        table: table.0.clone(),
        schema: schema.clone(),
        table_type: Some(table.1.clone()),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: initial_filter.clone(),
        order_by: order_by.clone(),
        keyset: None,
    });
    app.loading = true;
    app.status = if initial_filter.is_empty() {
        tf(
            "加载 {} 数据…",
            &[&(qualified_display(
                &fix_double_encoding(&schema),
                &fix_double_encoding(&table.0),
            ))],
        )
    } else {
        tf(
            "定位 {} · 过滤 {}",
            &[
                &(qualified_display(
                    &fix_double_encoding(&schema),
                    &fix_double_encoding(&table.0),
                )),
                &(truncate_disp(&one_line(&initial_filter), 48)),
            ],
        )
    };
    // Column metadata powers the `e`/`i` templates (primary-key detection) and
    // decides the page ordering, so the first page waits for it: the reply below
    // spawns the actual data load via `spawn_table_page`.
    app.pending_open_page = true;
    app.spawn(
        tx,
        Op::TableColumns(Box::new(cfg), app.current_db(), schema, table.0),
    );
}

impl App {
    /// Cached row count for a table view: `(value, is_lower_bound)`.
    pub(crate) fn cached_count(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        filter: &str,
    ) -> Option<(u64, bool)> {
        self.count_cache
            .get(&count_cache_key(db, schema, table, filter))
            .copied()
    }

    /// Remember a row count (exact, or a lower bound past the sample cap) for
    /// the rest of the session, so pages after the first do not recount.
    pub(crate) fn remember_count(
        &mut self,
        db: &str,
        schema: &str,
        table: &str,
        filter: &str,
        value: u64,
        lower_bound: bool,
    ) {
        self.count_cache.insert(
            count_cache_key(db, schema, table, filter),
            (value, lower_bound),
        );
    }
}

/// Spawn a table-data page for the current `page_state`, choosing keyset seek vs
/// OFFSET and the primary-key ordering. Callers must have set `app.pending_sel`.
pub(crate) fn spawn_table_page(app: &mut App, tx: &Tx, page: usize) {
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        app.page_pending = false;
        return;
    };
    let plan = keyset_plan(app.table_meta.as_ref(), &ps);
    let (keyset_pk, keyset_asc) = match &plan {
        Some((pk, asc)) => (pk.clone(), *asc),
        None => (Vec::new(), true),
    };
    let seek = keyset_seek_for(plan.as_ref(), ps.keyset.as_ref(), ps.page, page);
    // Deep OFFSET paging (no primary key to seek by) is slow; flag the hint so
    // the reply can mention it once.
    app.pending_deep_hint = false;
    if plan.is_none() && page >= DEEP_PAGE_HINT_AFTER && !app.deep_page_hint_shown {
        app.deep_page_hint_shown = true;
        app.pending_deep_hint = true;
    }
    app.page_pending = true;
    app.loading = true;
    app.status = tf(
        "加载 {} 第 {} 页…",
        &[&(fix_double_encoding(&ps.table)), &(page + 1)],
    );
    let known = app.cached_count(&app.current_db(), &ps.schema, &ps.table, &ps.filter);
    app.page_gen += 1;
    let gen = app.page_gen;
    app.spawn(
        tx,
        Op::TableData(Box::new(TableDataReq {
            cfg: Box::new(cfg),
            db: app.current_db(),
            schema: ps.schema.clone(),
            table: ps.table.clone(),
            table_type: ps.table_type.clone(),
            page,
            page_size: ps.page_size,
            filter: ps.filter.clone(),
            order_by: ps.order_by.clone(),
            known_total: known,
            keyset_pk,
            keyset_asc,
            seek,
            gen,
        })),
    );
}

/// Fetch `page`, moving the cursor to `pending_sel` when the result lands.
/// Returns false when a page load is already in flight, so a held-down key cannot
/// stack duplicate queries.
pub(crate) fn goto_page(app: &mut App, tx: &Tx, page: usize, pending_sel: Option<usize>) -> bool {
    if app.page_pending {
        return false;
    }
    // MongoDB documents paginate with skip/limit rather than SQL OFFSET.
    if app.grid_kind == GridKind::MongoDocs {
        app.page_pending = true;
        app.pending_sel = pending_sel;
        reload_mongo_docs(app, tx, page);
        return true;
    }
    if app.page_state.is_none() || app.selected.is_none() {
        return false;
    }
    app.pending_sel = pending_sel;
    spawn_table_page(app, tx, page);
    true
}

/// Re-run the current table view from page 0 with a new filter / sort. The
/// focus is intentionally left where it is (background refresh).
pub(crate) fn reload_table_view(
    app: &mut App,
    tx: &Tx,
    filter: String,
    order_by: Option<String>,
    page: usize,
) {
    let Some(ps) = app.page_state.clone() else {
        return;
    };
    if app.selected.is_none() {
        return;
    }
    app.page_state = Some(PageState {
        page,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: filter.clone(),
        order_by: order_by.clone(),
        keyset: None,
        ..ps.clone()
    });
    app.pending_sel = Some(0);
    spawn_table_page(app, tx, page);
}

/// Rows the results pane can show at once (header + borders excluded).
pub(crate) fn viewport_rows(app: &App) -> usize {
    app.rects.results.height.saturating_sub(3).max(1) as usize
}

/// R62: half a results viewport, at least one row (so even a two-row pane still
/// moves rather than turning the motion into a no-op).
pub(crate) fn half_rows(app: &App) -> usize {
    (viewport_rows(app) / 2).max(1)
}

/// R62 half-page scroll direction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HalfPage {
    Down,
    Up,
}

/// R62: which half-page motion a `Ctrl-D` / `Ctrl-U` press means in a pane, or
/// `None` when the pane already owns the key so the motion yields there.
///
/// The rule is "fire only where the key is otherwise free", so the two
/// documented bindings win and the result is deliberately asymmetric: the
/// results pane keeps `Ctrl-D` for delete-row and takes `Ctrl-U` for half-page
/// up, while the editor keeps `Ctrl-U` for undo (R51 / R57 / R58) and takes
/// `Ctrl-D` for half-page down (its forward-delete alias also lives on `Del`).
/// Redis / Mongo value grids route through the results rule, and `Ctrl-D` there
/// still reaches each grid's own delete. The status line is untouched.
pub(crate) fn half_page_key(focus: Focus, ctrl: bool, code: KeyCode) -> Option<HalfPage> {
    if !ctrl {
        return None;
    }
    match (focus, code) {
        (Focus::Editor, KeyCode::Char('d')) => Some(HalfPage::Down),
        (Focus::Preview, KeyCode::Char('u')) => Some(HalfPage::Up),
        _ => None,
    }
}

/// One-row cursor move, flipping the page when the cursor runs off an edge so
/// browsing is continuous (no "turn page, then hunt for the row").
pub(crate) fn cursor_step(app: &mut App, tx: &Tx, dir: i32) -> bool {
    let n = result_row_count(app);
    if n == 0 {
        return false;
    }
    if dir > 0 {
        if app.sel + 1 < n {
            app.sel += 1;
            return true;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.has_next {
                return goto_page(app, tx, ps.page + 1, Some(0));
            }
        }
        false
    } else {
        if app.sel > 0 {
            app.sel -= 1;
            return true;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.page > 0 {
                // usize::MAX is clamped to the last row when the page arrives.
                return goto_page(app, tx, ps.page - 1, Some(usize::MAX));
            }
        }
        false
    }
}

pub(crate) fn move_cursor(app: &mut App, tx: &Tx, delta: i32) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            let n = s.outcomes.len();
            if n == 0 {
                return;
            }
            let next = (s.sel as i32 + delta).clamp(0, n as i32 - 1);
            s.sel = next as usize;
            return;
        }
    }
    let steps = delta.unsigned_abs() as usize;
    let dir = if delta < 0 { -1 } else { 1 };
    for _ in 0..steps {
        if !cursor_step(app, tx, dir) {
            break;
        }
    }
}

/// Screen-at-a-time scroll that carries over the page boundary.
pub(crate) fn screen_move(app: &mut App, tx: &Tx, dir: i32) {
    scroll_rows(app, tx, dir, viewport_rows(app));
}

/// R62: half-page scroll in the results pane, reusing the full-page arithmetic
/// with half a viewport as the step. The DDL view scrolls its own preview buffer
/// (`ddl_scroll`), matching how `PageUp` / `PageDown` treat it.
pub(crate) fn half_screen_move(app: &mut App, tx: &Tx, dir: i32) {
    let step = half_rows(app);
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        if dir > 0 {
            app.ddl_scroll = app.ddl_scroll.saturating_add(step as u16);
        } else {
            app.ddl_scroll = app.ddl_scroll.saturating_sub(step as u16);
        }
        return;
    }
    scroll_rows(app, tx, dir, step);
}

/// Screen-at-a-time scroll that carries over the page boundary. `screen` is the
/// row step, so the same code backs the full-page (`PageUp` / `PageDown`) and
/// half-page (`Ctrl-D` / `Ctrl-U`) motions and both clamp / page-flip alike.
pub(crate) fn scroll_rows(app: &mut App, tx: &Tx, dir: i32, screen: usize) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            let n = s.outcomes.len();
            if n == 0 {
                return;
            }
            let next = (s.sel as i32 + dir * screen as i32).clamp(0, n as i32 - 1);
            s.sel = next as usize;
            return;
        }
    }
    let n = result_row_count(app);
    if n == 0 {
        return;
    }
    if dir > 0 {
        if app.sel + screen < n {
            app.sel = (app.sel + screen).min(n - 1);
            return;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.has_next {
                let carry = (app.sel + screen).saturating_sub(n);
                let target = carry.min(ps.page_size.saturating_sub(1));
                goto_page(app, tx, ps.page + 1, Some(target));
                return;
            }
        }
        app.sel = n - 1;
    } else {
        if app.sel >= screen {
            app.sel -= screen;
            return;
        }
        if let Some(ps) = app.page_state.clone() {
            if ps.page > 0 {
                let carry = screen - app.sel;
                let target = ps.page_size.saturating_sub(carry);
                goto_page(app, tx, ps.page - 1, Some(target));
                return;
            }
        }
        app.sel = 0;
    }
}

/// Explicit page turn (`n`/`p`/Ctrl-F/Ctrl-B): keep the cursor at the same
/// relative row so the view does not jump back to the top.
pub(crate) fn page_turn(app: &mut App, tx: &Tx, forward: bool) {
    page_turn_by(app, tx, forward, 1);
}

/// Page-turn repeated `times` (vim counts such as `5n`). A grid without a
/// `page_state` falls back to screen scrolling, which is also repeated.
pub(crate) fn page_turn_by(app: &mut App, tx: &Tx, forward: bool, times: u32) {
    if times == 0 {
        return;
    }
    let Some(ps) = app.page_state.clone() else {
        for _ in 0..times {
            screen_move(app, tx, if forward { 1 } else { -1 });
        }
        return;
    };
    if forward {
        if !ps.has_next {
            app.status = t("已经是最后一页").into();
            return;
        }
        goto_page(app, tx, ps.page + times as usize, Some(app.sel));
    } else {
        if ps.page == 0 {
            app.status = t("已经是第一页").into();
            return;
        }
        let target = ps.page.saturating_sub(times as usize);
        goto_page(app, tx, target, Some(app.sel));
    }
}

/// A grid cell counts as blank for the `}` / `{` motion when it is SQL NULL or
/// an empty (whitespace-only) string — the two values the grid draws greyed out.
pub(crate) fn cell_blank(v: &Val) -> bool {
    match v {
        Val::Null => true,
        Val::Text(s) => s.trim().is_empty(),
    }
}

/// R59: `}` / `{` in the results grid — move the row cursor to the next /
/// previous row whose cell in the focused column is non-blank, skipping NULL and
/// empty strings (a sparse column is otherwise a wall of blanks). The status line
/// reports the absolute row number. `n` / `p` keep their page-turn meaning.
pub(crate) fn jump_nonblank_row(app: &mut App, dir: i32) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("语句列表没有单元格可跳").into();
        return;
    }
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可跳转的结果").into();
        return;
    };
    let n = grid.rows.len();
    if n == 0 {
        app.status = t("没有可跳转的结果").into();
        return;
    }
    let col = app.col_cursor.min(grid.columns.len().saturating_sub(1));
    let col_name = grid
        .columns
        .get(col)
        .map(|c| fix_double_encoding(c))
        .unwrap_or_default();
    let mut i = app.sel as i64 + dir as i64;
    let mut skipped = 0usize;
    while i >= 0 && (i as usize) < n {
        let blank = grid
            .rows
            .get(i as usize)
            .and_then(|r| r.get(col))
            .is_none_or(cell_blank);
        if !blank {
            app.sel = i as usize;
            let abs = cursor_abs_row(app);
            app.status = tf(
                "第 {} 行 · {} 非空（跳过 {} 个空单元格）",
                &[&abs, &col_name, &skipped],
            );
            return;
        }
        skipped += 1;
        i += dir as i64;
    }
    app.status = if dir > 0 {
        t("下方没有非空单元格").into()
    } else {
        t("上方没有非空单元格").into()
    };
}

pub(crate) fn reload_tables(app: &mut App, tx: &Tx) {
    if app.selected.is_some() {
        // Remember the current table so a same-named table can be re-selected in
        // the new database.
        app.pending_table = app.selected_table().map(|t| t.name.clone());
        app.tables.clear();
        app.tables_all.clear();
        app.columns.clear();
        app.ddl = None;
        app.clear_grid();
        app.script = None;
        app.page_state = None;
        app.col_offset = 0;
        app.col_cursor = 0;
        app.cell_popup = None;
        app.row_popup = None;
        app.filter_prompt = None;
        app.table_meta = None;
        app.pending_sel = None;
        app.pending_focus = None;
        app.page_pending = false;
        app.loading = true;
        let db = app.current_db();
        // R43: a newly selected database opens in the tree (it may have been
        // collapsed on an earlier visit).
        if let Some(cfg) = app.selected.clone() {
            app.tree_conn_open.insert(cfg.id.clone());
            app.tree_db_closed.remove(&db_node_key(&cfg.id, &db));
        }
        app.status = tf("切换到 {} …", &[&(fix_double_encoding(&db))]);
        app.set_placeholder();
        spawn_table_list(app, tx);
    }
}

/// Queue the table list for the current database. Schema-aware engines fetch the
/// schema list first (once per database), then reload the tables for the current
/// schema; everything else goes straight to the flat table list.
pub(crate) fn spawn_table_list(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let db = app.current_db();
    if schema_picker_engine(cfg.db_type) && app.schemas_db != db {
        app.spawn(tx, Op::ListSchemas(Box::new(cfg), db));
    } else {
        let schema = app.schema.clone();
        spawn_list_tables(app, tx, Box::new(cfg), db, schema);
    }
}

/// Queue one table-list request, bumping the generation so an earlier reply can
/// be recognised and dropped.
pub(crate) fn spawn_list_tables(
    app: &mut App,
    tx: &Tx,
    cfg: Box<ConnectionConfig>,
    db: String,
    schema: String,
) {
    app.tables_gen = app.tables_gen.wrapping_add(1);
    let gen = app.tables_gen;
    app.spawn(tx, Op::ListTables(cfg, db, schema, gen));
}

/// Default schema to browse when nothing is remembered: PostgreSQL's `public`
/// when present, else the first schema the server listed.
pub(crate) fn default_schema(schemas: &[String]) -> String {
    schemas
        .iter()
        .find(|s| s.eq_ignore_ascii_case("public"))
        .or_else(|| schemas.first())
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn cycle_db(app: &mut App, tx: &Tx, forward: bool) {
    let len = app.databases.len();
    if len <= 1 {
        return;
    }
    app.db_index = if forward {
        (app.db_index + 1) % len
    } else {
        (app.db_index + len - 1) % len
    };
    reload_tables(app, tx);
}

// ── result column window ──

pub(crate) fn max_cell_width(mode: LayoutMode) -> usize {
    match mode {
        LayoutMode::Narrow => 18,
        LayoutMode::Mid => 28,
        LayoutMode::Wide => 44,
    }
}

// ── sidebar width (side-by-side layout) ──

/// Readable floor for the sidebar: the `▸ ` marker plus a short table name.
pub(crate) const SIDEBAR_MIN_W: u16 = 14;
/// Sidebar width in the mid layout, and the ceiling the auto-width can reach.
pub(crate) const SIDEBAR_MID_W: u16 = 22;
/// Sidebar width on a wide screen — the historical value, kept unchanged.
pub(crate) const SIDEBAR_WIDE_W: u16 = 28;

/// Width of the sidebar in the side-by-side layout. On a wide screen the
/// historical 28 columns are kept. On a mid screen the sidebar shrinks toward
/// the widest `schema.table` name so the freed columns go to the data area,
/// never below the readable floor. The narrow layout stacks the sidebar, so its
/// width there is simply the terminal width (the data area is already maximal).
pub(crate) fn sidebar_width(term_w: u16, mode: LayoutMode, longest_name: usize) -> u16 {
    match mode {
        LayoutMode::Wide => SIDEBAR_WIDE_W,
        LayoutMode::Narrow => term_w.max(SIDEBAR_MIN_W),
        LayoutMode::Mid => (longest_name as u16 + 4).clamp(SIDEBAR_MIN_W, SIDEBAR_MID_W),
    }
}

/// Display width of the widest `schema.table` name in the sidebar list, so the
/// sidebar can be sized to its content.
pub(crate) fn sidebar_longest_name(tables: &[TableInfo], schema: &str) -> usize {
    tables
        .iter()
        .map(|t| disp_width(&fix_double_encoding(&qualified_display(schema, &t.name))))
        .max()
        .unwrap_or(0)
}

/// Widest row the connection tree can draw: a table name plus its indent (two
/// levels when the engine has a database layer, one otherwise) and any
/// connection name, so the mid layout can size the sidebar to its content.
pub(crate) fn sidebar_tree_longest_name(app: &App) -> usize {
    let indent = if app.databases.is_empty() { 2 } else { 4 };
    let tables = sidebar_longest_name(&app.tables, &app.schema) + indent;
    let conns = app
        .connections
        .iter()
        .map(|c| disp_width(&c.name))
        .max()
        .unwrap_or(0);
    tables.max(conns)
}

/// Truncate a table name to `max` display cells, marking the cut with a trailing
/// `~` (distinct from the generic `…` ellipsis, so a clipped identifier is
/// obvious in the sidebar).
pub(crate) fn truncate_table_name(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if disp_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('~');
    out
}

pub(crate) const MIN_CELL_WIDTH: usize = 6;

/// Narrowest a column may get in the compact (mobile) column-width mode. Below
/// this a value is no longer identifiable at a glance.
pub(crate) const COMPACT_MIN_CELL: usize = 6;
/// Widest a column gets in compact mode when the pane is too small for every
/// column to share the space equally. Keeps a phone table from showing two
/// half-screen columns.
pub(crate) const COMPACT_MAX_CELL: usize = 8;

/// Is the compact column-width mode active? `None` means "auto": on for a
/// narrow (phone) terminal, off otherwise. The toggle stores an explicit
/// on/off so a user can override the automatic choice in either direction.
pub(crate) fn compact_active(compact: Option<bool>, mode: LayoutMode) -> bool {
    compact.unwrap_or(mode == LayoutMode::Narrow)
}

/// Width cap for one grid column in compact mode.
///
/// The pane is shared equally among all columns, so a table whose columns are
/// not too numerous fits completely and needs no horizontal scrolling at all.
/// When even `COMPACT_MIN_CELL` per column does not fit, the cap stays at the
/// minimum and the grid scrolls as before.
pub(crate) fn compact_max_cell(inner_w: usize, gutter: usize, ncols: usize, base: usize) -> usize {
    if ncols == 0 {
        return COMPACT_MAX_CELL.min(base);
    }
    let avail = inner_w.saturating_sub(gutter);
    let per = avail.saturating_sub(ncols.saturating_sub(1)) / ncols;
    if per >= COMPACT_MAX_CELL {
        // Room to spare: let a wide terminal use its normal content width.
        base.min(per.max(COMPACT_MIN_CELL))
    } else {
        per.clamp(COMPACT_MIN_CELL, COMPACT_MAX_CELL).min(base)
    }
}

/// Effective per-column width cap for the current frame.
pub(crate) fn grid_max_cell(app: &App, ncols: usize, inner_w: usize, gutter: u16) -> usize {
    let base = max_cell_width(app.layout_mode);
    if compact_active(app.compact, app.layout_mode) {
        compact_max_cell(inner_w, gutter as usize, ncols, base)
    } else {
        base
    }
}

/// Human label for the compact mode, used in the status bar and messages.
pub(crate) fn compact_label(app: &App) -> String {
    let on = compact_active(app.compact, app.layout_mode);
    let auto = if app.compact.is_none() {
        t("自动")
    } else {
        t("手动")
    };
    tf(
        "紧凑列 {}{}",
        &[&(if on { t("开") } else { t("关") }), &(auto)],
    )
}

/// Drop every column the user hid with Ctrl-Shift-H. At least one column always
/// survives so a grid can never render as nothing (DBX does the same).
pub(crate) fn filter_grid(grid: &Grid, hidden: &HashSet<String>) -> Grid {
    if hidden.is_empty() {
        return grid.clone();
    }
    let mut keep: Vec<usize> = (0..grid.columns.len())
        .filter(|&i| !hidden.contains(grid.columns[i].as_str()))
        .collect();
    if keep.is_empty() {
        keep.push(0);
    }
    if keep.len() == grid.columns.len() {
        return grid.clone();
    }
    Grid {
        columns: keep.iter().map(|&i| grid.columns[i].clone()).collect(),
        types: keep
            .iter()
            .map(|&i| grid.types.get(i).cloned().unwrap_or_default())
            .collect(),
        rows: grid
            .rows
            .iter()
            .map(|r| {
                keep.iter()
                    .map(|&i| r.get(i).cloned().unwrap_or(Val::Null))
                    .collect()
            })
            .collect(),
        note: grid.note.clone(),
    }
}

/// True when any cell of `row` contains `needle` (already lower-cased). NULL is
/// matched as the text `null` so `/null` finds real NULLs.
pub(crate) fn row_matches(row: &[Val], needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    row.iter().any(|v| {
        let s = match v {
            Val::Null => "null",
            Val::Text(s) => s.as_str(),
        };
        s.to_lowercase().contains(needle)
    })
}

/// True when the cell at `col` in `row` contains `needle` (already lower-cased).
/// NULL matches the text `null`, so a column filter can find real NULLs too.
pub(crate) fn cell_matches(row: &[Val], col: usize, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    row.get(col).is_some_and(|v| {
        let s = match v {
            Val::Null => "null",
            Val::Text(s) => s.as_str(),
        };
        s.to_lowercase().contains(needle)
    })
}

/// Kept row indices for the two client-side row filters: a whole-row search
/// needle and an optional single-column filter `(column name, needle)`. Both are
/// case-insensitive substring matches; an inactive filter keeps everything, so
/// the result is the identity when neither is set. Pure, so the filter state
/// machine is unit-testable without a backend.
pub(crate) fn kept_row_indices(
    cols: &Grid,
    row_needle: &str,
    col: Option<(&str, &str)>,
) -> Vec<usize> {
    let row_needle = row_needle.trim().to_lowercase();
    let col = col.and_then(|(name, needle)| {
        let needle = needle.trim().to_lowercase();
        let idx = cols.columns.iter().position(|c| c == name)?;
        (!needle.is_empty()).then_some((idx, needle))
    });
    let row_active = !row_needle.is_empty();
    cols.rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            if let Some((idx, n)) = &col {
                if !cell_matches(r, *idx, n) {
                    return false;
                }
            }
            !row_active || row_matches(r, &row_needle)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Drop every row rejected by the active result-row search / column filter. An
/// inactive pair returns the grid unchanged, so this is a no-op with no filter.
pub(crate) fn apply_row_filters(grid: Grid, row_needle: &str, col: Option<(&str, &str)>) -> Grid {
    let keep = kept_row_indices(&grid, row_needle, col);
    if keep.len() == grid.rows.len() {
        return grid;
    }
    let Grid {
        columns,
        types,
        rows,
        note,
    } = grid;
    Grid {
        columns,
        types,
        rows: keep.into_iter().map(|i| rows[i].clone()).collect(),
        note,
    }
}

/// Natural width of one grid column: the widest of its header and cells,
/// clamped to `[MIN_CELL_WIDTH, max_cell]`.
#[cfg(test)]
pub(crate) fn natural_width(grid: &Grid, ci: usize, max_cell: usize) -> usize {
    let mut w = disp_width(grid.columns.get(ci).map(String::as_str).unwrap_or(""));
    for row in &grid.rows {
        if let Some(v) = row.get(ci) {
            let cw = cell_text_width(v);
            if cw > w {
                w = cw;
            }
        }
    }
    w.clamp(MIN_CELL_WIDTH, max_cell)
}

/// Natural width of every column in one row-major pass. One scan of the grid
/// beats one full scan per column (cache locality), and the result feeds the
/// per-grid width cache so scrolling a 20k-row result never rescans it. Uses the
/// raw value text (`NumFmt::Original`); the render path calls
/// [`natural_widths_fmt`] so a comma-separated number still fits its column.
pub(crate) fn natural_widths(grid: &Grid, max_cell: usize) -> Vec<usize> {
    natural_widths_fmt(grid, max_cell, NumFmt::Original)
}

/// [`natural_widths`] against a specific big-number display mode: the width pass
/// and the render pass must agree, or a `1,234,567` would be truncated.
pub(crate) fn natural_widths_fmt(grid: &Grid, max_cell: usize, mode: NumFmt) -> Vec<usize> {
    let mut widths: Vec<usize> = grid.columns.iter().map(|c| disp_width(c)).collect();
    for row in &grid.rows {
        for (ci, w) in widths.iter_mut().enumerate() {
            if let Some(v) = row.get(ci) {
                let cw = cell_text_width_fmt(v, grid.col_type(ci), mode);
                if cw > *w {
                    *w = cw;
                }
            }
        }
    }
    for w in &mut widths {
        *w = (*w).clamp(MIN_CELL_WIDTH, max_cell);
    }
    widths
}

/// How many columns starting at `off` fit in `avail` display columns using their
/// natural widths. Content-sized columns keep a narrow `id` narrow instead of
/// stretching it to fill the pane.
pub(crate) fn visible_cols(grid: &Grid, off: usize, avail: usize, max_cell: usize) -> usize {
    let widths = natural_widths(grid, max_cell);
    visible_cols_from_widths(&widths, off, avail)
}

/// [`visible_cols`] against precomputed widths (the render path uses the
/// cached vector so it never rebuilds it).
pub(crate) fn visible_cols_from_widths(widths: &[usize], off: usize, avail: usize) -> usize {
    let n = widths.len();
    if n == 0 || off >= n {
        return 0;
    }
    let mut used = 0usize;
    let mut count = 0usize;
    for w in widths.iter().skip(off) {
        let add = w + if count > 0 { 1 } else { 0 };
        if count > 0 && used + add > avail {
            break;
        }
        used += add;
        count += 1;
        if used >= avail {
            break;
        }
    }
    count.max(1)
}

/// Move the focused cell one column left/right. The visible window follows the
/// cursor, which is what makes horizontal browsing feel like scrolling a table.
/// Works for table data, query results and drilled script results alike.
pub(crate) fn move_col_cursor(app: &mut App, delta: i32) {
    let Some(grid) = active_grid(app) else {
        return;
    };
    let n = grid.columns.len();
    if n == 0 {
        return;
    }
    let next = (app.col_cursor as i32 + delta).clamp(0, n as i32 - 1);
    app.col_cursor = next as usize;
    // R47b: moving the cell cursor across columns can scroll the window, so the
    // progress bar reappears for a moment.
    app.poke_hbar();
}

/// The grid the cell cursor currently operates on: a drilled script result
/// takes precedence over the top-level grid (which is empty while a script is
/// shown). Hidden columns and the active result-row search are applied, so the
/// row / column cursor and CSV export see exactly what is on screen.
pub(crate) fn active_grid(app: &App) -> Option<Grid> {
    if let Some(s) = &app.script {
        if let Some(i) = s.drilled {
            let full = s.outcomes.get(i)?.grid.clone();
            let cols = filter_grid(&full, &app.col_hidden);
            return Some(apply_row_filters(
                cols,
                &app.result_needle,
                app.col_filter_spec(),
            ));
        }
    }
    app.grid.clone()
}

/// The grid as it was fetched, before the session column filter. Used by the row
/// detail popup, which must show every column even the hidden ones.
pub(crate) fn full_grid(app: &App) -> Option<Grid> {
    if let Some(s) = &app.script {
        if let Some(i) = s.drilled {
            return s.outcomes.get(i).map(|o| o.grid.clone());
        }
    }
    app.grid_full.clone()
}

/// The focused row read from the unfiltered grid. An active result search keeps
/// a display→source row map, so the row must come from `full_grid` — reading it
/// from the on-screen (filtered) grid with the full-grid index would fail or
/// copy the wrong row.
pub(crate) fn focused_full_row(app: &App) -> Option<Vec<Val>> {
    let grid = full_grid(app)?;
    let idx = app.full_row_index()?;
    grid.rows.get(idx).cloned()
}

/// `display` row index (into the on-screen, filtered grid) → index into the
/// *unfiltered* grid. `full_row_index` is the focused-row case of this; R57's
/// row-select mode needs the mapping for a whole range.
pub(crate) fn full_row_at(app: &App, display: usize) -> Option<usize> {
    if let Some(s) = &app.script {
        let i = s.drilled?;
        let full = &s.outcomes.get(i)?.grid;
        let col = app.col_filter_spec();
        let row_active = !app.result_needle.trim().is_empty();
        let col_active = col.is_some_and(|(_, n)| !n.trim().is_empty());
        if !row_active && !col_active {
            return (display < full.rows.len()).then_some(display);
        }
        // filter_grid only drops columns, so row indices still line up with
        // `full.rows`; the map translates the filtered display row back.
        let cols = filter_grid(full, &app.col_hidden);
        let map = kept_row_indices(&cols, &app.result_needle, col);
        return map.get(display).copied();
    }
    app.result_rows.get(display).copied()
}

/// The rows currently covered by the R57 row selection, read from the
/// *unfiltered* grid so hidden columns and an active row filter do not change
/// what `Y` / `d` / `c` act on.
pub(crate) fn selected_full_rows(app: &App) -> Vec<Vec<Val>> {
    let Some(anchor) = app.row_sel_anchor else {
        return Vec::new();
    };
    let Some(full) = full_grid(app) else {
        return Vec::new();
    };
    let (lo, hi) = (anchor.min(app.sel), anchor.max(app.sel));
    (lo..=hi)
        .filter_map(|d| full_row_at(app, d))
        .filter_map(|f| full.rows.get(f).cloned())
        .collect()
}

/// True when the results pane is showing a browsable table (not a query result,
/// structure list or script).
pub(crate) fn in_table_data_view(app: &App) -> bool {
    app.script.is_none() && app.grid_kind == GridKind::TableData && app.page_state.is_some()
}

/// Absolute row number (1-based) of the cursor across all pages.
pub(crate) fn abs_row(page: usize, page_size: usize, sel: usize) -> usize {
    page * page_size + sel + 1
}

/// Total page count for a known row total (at least one page).
pub(crate) fn page_count(total: u64, page_size: usize) -> usize {
    if page_size == 0 {
        return 1;
    }
    ((total as usize).div_ceil(page_size)).max(1)
}

/// How many leading data columns to pin. The row-number gutter is always pinned;
/// the first data column is pinned only when the toggle is on, the grid is wide
/// enough to still scroll, and there is room for at least one more column.
#[cfg(test)]
pub(crate) fn effective_frozen(
    freeze_first: bool,
    grid: &Grid,
    gutter: usize,
    inner_w: usize,
    max_cell: usize,
) -> usize {
    let widths = natural_widths(grid, max_cell);
    effective_frozen_widths(freeze_first, widths.len(), &widths, gutter, inner_w)
}

/// [`effective_frozen`] against precomputed widths.
pub(crate) fn effective_frozen_widths(
    freeze_first: bool,
    ncols: usize,
    widths: &[usize],
    gutter: usize,
    inner_w: usize,
) -> usize {
    if !freeze_first {
        return 0;
    }
    if ncols < 3 {
        return 0;
    }
    let w0 = widths.first().copied().unwrap_or(MIN_CELL_WIDTH);
    if gutter + 1 + w0 + 1 + MIN_CELL_WIDTH <= inner_w {
        1
    } else {
        0
    }
}

/// The scrollable window `(off, visible)` that keeps `cursor` on screen, starting
/// from the previous window origin `start` and never scrolling into the frozen
/// prefix.
#[cfg(test)]
pub(crate) fn window_for_cursor(
    grid: &Grid,
    cursor: usize,
    start: usize,
    avail: usize,
    max_cell: usize,
    frozen: usize,
) -> (usize, usize) {
    let widths = natural_widths(grid, max_cell);
    window_for_cursor_widths(&widths, cursor, start, avail, frozen)
}

/// [`window_for_cursor`] against precomputed widths.
pub(crate) fn window_for_cursor_widths(
    widths: &[usize],
    cursor: usize,
    start: usize,
    avail: usize,
    frozen: usize,
) -> (usize, usize) {
    let n = widths.len();
    if n == 0 {
        return (0, 0);
    }
    let mut off = start.max(frozen).min(n - 1);
    let mut visible = visible_cols_from_widths(widths, off, avail).max(1);
    if cursor < frozen {
        return (off, visible);
    }
    let mut guard = 0usize;
    while cursor >= off + visible && off + visible < n && guard <= n {
        off += 1;
        visible = visible_cols_from_widths(widths, off, avail).max(1);
        guard += 1;
    }
    if cursor < off {
        off = cursor;
        visible = visible_cols_from_widths(widths, off, avail).max(1);
    }
    (off, visible)
}

// ── mouse / touch (touch tap = Mouse Down, wheel = Scroll) ──
