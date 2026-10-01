use crate::prelude::*;
use crate::*;

pub(crate) fn preview_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // R62: half-page scroll in the results pane. `Ctrl-D` yields to the
    // delete-row key, so only `Ctrl-U` fires here (see `half_page_key`). Handled
    // before the per-grid keymaps so the SQL / Redis / Mongo grids share the one
    // rule, and `Ctrl-D` still reaches each grid's own delete untouched.
    // R95: row-select mode owns `Ctrl-U` (batch set-value), so the half-page
    // motion yields to the selection just like `Ctrl-D` yields to delete-row.
    if app.row_sel_anchor.is_none() {
        if let Some(dir) = half_page_key(
            Focus::Preview,
            k.modifiers.contains(KeyModifiers::CONTROL),
            k.code,
        ) {
            half_screen_move(app, tx, if dir == HalfPage::Down { 1 } else { -1 });
            return;
        }
    }
    // A Redis value / Mongo document grid has its own keymap (edit, delete, TTL,
    // rename, JSON filter) that must not fall through to the SQL row actions.
    if app.backend_kind == Backend::Redis && app.grid_kind == GridKind::RedisValue {
        redis_value_key(app, tx, k);
        return;
    }
    if app.backend_kind == Backend::Mongo && app.grid_kind == GridKind::MongoDocs {
        mongo_docs_key(app, tx, k);
        return;
    }
    // R101: the open snapshot diff owns the keyboard (Esc closes it); with no
    // diff open, Ctrl-Shift-D stores / compares and Ctrl-Shift-X clears.
    if app.result_diff.is_some() {
        result_diff_key(app, k);
        return;
    }
    if result_snapshot_key(app, k) {
        return;
    }
    // R57: row-select mode owns the keyboard until Esc / an action; a key the
    // mode does not use exits it and falls through to the normal grid keymap.
    if app.row_sel_anchor.is_some() && row_select_key(app, tx, k) {
        return;
    }
    // Every results-pane command that needs a modifier is a Ctrl combo, so no
    // bare uppercase letter is required. Handle them here and swallow any other
    // Ctrl combo so it can never fall through to a plain-key action.
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('f') => page_turn(app, tx, true),
            KeyCode::Char('b') => page_turn(app, tx, false),
            KeyCode::Char('e') => app.focus = Focus::Editor,
            KeyCode::Char('k') => sort_column(app, tx, true),
            KeyCode::Char('r') => clear_filter(app, tx),
            KeyCode::Char('d') => delete_row(app),
            // Ctrl-Y: export the focused grid to CSV; Ctrl-N: load more rows when
            // the previous query hit the row cap.
            KeyCode::Char('y') => open_export(app),
            KeyCode::Char('n') => load_more_rows(app, tx),
            _ => {}
        }
        return;
    }
    // R48: Alt-F pins / unpins the results pane. It is free in the grid — the
    // editor owns Alt-F for SQL formatting, so the two never collide.
    if k.modifiers.contains(KeyModifiers::ALT)
        && matches!(k.code, KeyCode::Char('f') | KeyCode::Char('F'))
    {
        toggle_pin_results(app);
        return;
    }
    // R73: Alt-W closes the active result tab (the last one is kept). Ctrl-W is
    // already the pane-collapse toggle, so the tab close takes the free Alt
    // mnemonic; the close is purely client-side and never runs a query.
    if k.modifiers.contains(KeyModifiers::ALT)
        && matches!(k.code, KeyCode::Char('w') | KeyCode::Char('W'))
    {
        close_result_tab(app);
        return;
    }
    // R72: Alt-0 forgets every remembered column width for the current browsed
    // table (session + tui.json). `Ctrl-0` was rejected: most terminals — tmux
    // included — collapse it to a plain `0`, which is the single-column reset.
    if k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Char('0') {
        clear_table_col_widths(app);
        return;
    }
    let screen = viewport_rows(app) as u16;
    let ddl = app.struct_view == StructView::Ddl && app.ddl.is_some();
    // Vim-style count prefix and the `g` chord are handled before the search /
    // scroll layers so `5n` pages five times and `gd` / `gt` switch views.
    if count_pre(
        app,
        tx,
        k,
        count_motion(k.code) || matches!(k.code, KeyCode::Char('n') | KeyCode::Char('p')),
    ) {
        return;
    }
    if app.pending_g {
        match k.code {
            KeyCode::Char('d') if k.modifiers.is_empty() => {
                app.pending_g = false;
                if app.selected_table().is_none() {
                    app.status = t("先选中一张表").into();
                } else {
                    app.status = t("g d → 表结构").into();
                    load_structure(app, tx);
                }
                return;
            }
            KeyCode::Char('t') if k.modifiers.is_empty() => {
                app.pending_g = false;
                if app.selected_table().is_none() {
                    app.status = t("先选中一张表").into();
                } else {
                    app.status = t("g t → 表数据").into();
                    open_table_data(app, tx);
                }
                return;
            }
            KeyCode::Char('v') if k.modifiers.is_empty() => {
                app.pending_g = false;
                open_locate(app);
                return;
            }
            // `gc` — R48: a quick column-structure popup (name / type /
            // nullable / comment) from the cached metadata, without leaving the
            // grid for the full `gd` structure view. R65: Enter inside it jumps
            // the cell cursor to that column.
            KeyCode::Char('c') if k.modifiers.is_empty() => {
                app.pending_g = false;
                open_cols_popup(app);
                return;
            }
            // `gb` — R65: switch to another table in the *same* database. A
            // type-to-filter list of the cached table names; Enter opens the
            // highlighted one in the data view. Never a query.
            KeyCode::Char('b') if k.modifiers.is_empty() => {
                app.pending_g = false;
                open_table_jump(app);
                return;
            }
            // `gg` — vim's "go to top" (R42): a real console motion for the
            // result / statement list.
            KeyCode::Char('g') if k.modifiers.is_empty() => {
                app.pending_g = false;
                preview_home(app);
                return;
            }
            // `g w` — R85: fit the focused column to its loaded content (95th
            // percentile width, clamped [6, 40]), remembered like a manual `<`
            // / `>`. Pure client-side scan of the page already in memory.
            KeyCode::Char('w') if k.modifiers.is_empty() => {
                app.pending_g = false;
                fit_col_width(app);
                return;
            }
            // `g W` — R85: fit every visible column; on a narrow (<80 col)
            // terminal only the leading visible columns (see `auto_fit_columns`).
            KeyCode::Char('W')
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                app.pending_g = false;
                fit_all_col_widths(app);
                return;
            }
            // R91: `gf` pins / unpins the focused data column at the left edge
            // (up to two columns). In the MongoDB document grid `gf` stays the
            // R82 field jump — that grid resolves the chord before this block.
            KeyCode::Char('f') if k.modifiers.is_empty() => {
                app.pending_g = false;
                toggle_freeze_col(app);
                return;
            }
            // R91: `gs` pins / unpins the focused row as the *reference* row, so
            // a wide grid can be read against a fixed baseline (status bar `Δ`).
            KeyCode::Char('s') if k.modifiers.is_empty() => {
                app.pending_g = false;
                toggle_ref_row(app);
                return;
            }
            // R103: `g m` materializes the current result set as a new table
            // (CTAS) through the red confirmation layer.
            KeyCode::Char('m') if k.modifiers.is_empty() => {
                app.pending_g = false;
                open_materialize_prompt(app);
                return;
            }
            KeyCode::Esc => {
                app.pending_g = false;
                return;
            }
            _ => app.pending_g = false,
        }
    }
    // R102: `c` in the structure view edits the open table's comment. The bare
    // key is otherwise the column picker, which is a no-op on the structure view
    // (its grid has no hidden columns), so the gesture is reclaimed here; `g c`
    // still opens the column popup above.
    if k.code == KeyCode::Char('c') && k.modifiers.is_empty() && app.grid_kind == GridKind::Columns
    {
        open_table_comment_edit(app);
        return;
    }
    // Esc clears an active value locate, then an active column filter, then an
    // active result search, then an active cell find, before it does anything
    // else. This applies to the top-level grid and to a drilled script result;
    // only the script *list* has no search to clear.
    if k.code == KeyCode::Esc
        && !app.locate_needle.is_empty()
        && !ddl
        && app.script.as_ref().is_none_or(|s| s.drilled.is_some())
    {
        clear_locate(app);
        app.flash(t("已清除定位").into());
        return;
    }
    if k.code == KeyCode::Esc
        && !app.cell_find_needle.is_empty()
        && !ddl
        && app.script.as_ref().is_none_or(|s| s.drilled.is_some())
    {
        app.clear_cell_find();
        app.flash(t("已清除单元格查找").into());
        return;
    }
    if k.code == KeyCode::Esc
        && app
            .col_filter_spec()
            .is_some_and(|(_, n)| !n.trim().is_empty())
        && !ddl
        && app.script.as_ref().is_none_or(|s| s.drilled.is_some())
    {
        app.clear_col_filter();
        app.rebuild_view();
        app.sel = 0;
        app.flash(t("已清除列过滤").into());
        return;
    }
    if k.code == KeyCode::Esc
        && !app.result_needle.is_empty()
        && !ddl
        && app.script.as_ref().is_none_or(|s| s.drilled.is_some())
    {
        app.result_needle.clear();
        app.rebuild_view();
        app.sel = 0;
        app.flash(t("已清除结果搜索").into());
        return;
    }
    match k.code {
        KeyCode::Esc => {
            if let Some(s) = &mut app.script {
                if s.drilled.is_some() {
                    s.drilled = None;
                    app.sel = 0;
                    app.col_offset = 0;
                    app.col_cursor = 0;
                    // A result search does not apply to the statement list.
                    app.result_needle.clear();
                    app.result_filter = None;
                    app.clear_col_filter();
                    app.flash(t("已返回语句列表").into());
                    return;
                }
            }
            if ddl {
                app.struct_view = StructView::Fields;
                app.flash(t("已返回字段视图").into());
                return;
            }
            app.show_first_grid();
            app.focus = Focus::Sidebar;
            // R101: collapsing the results pane is the "clear screen" event —
            // the snapshot belonged to the grid that just left the screen.
            if current_conn_id(app).is_some_and(|c| app.result_snapshot.contains_key(&c)) {
                invalidate_current_snapshot(app, true);
            } else {
                app.flash(t("已回到侧栏").into());
            }
        }
        KeyCode::Char('e') => edit_cell(app),
        KeyCode::Char('i') => quick_insert(app),
        KeyCode::Char('o') => open_row_popup(app),
        // Result tabs: flip between successive query results.
        KeyCode::Char('[') => switch_result_tab(app, -1),
        KeyCode::Char(']') => switch_result_tab(app, 1),
        KeyCode::Char('t') => {
            // `gt` is the goto-data chord; bare `t` keeps toggling fields / DDL.
            if app.ddl.is_some() {
                app.struct_view = match app.struct_view {
                    StructView::Fields => StructView::Ddl,
                    StructView::Ddl => StructView::Fields,
                };
                app.ddl_scroll = 0;
            }
        }
        // R107: `D` fetches the current table's complete DDL and opens the
        // modal popup (`y` copy · `Ctrl-Y` save `{table}.sql`). An explicit
        // action: exactly one dialect source statement is issued here.
        KeyCode::Char('D') if k.modifiers.is_empty() => open_ddl_popup(app, tx),
        // `g` starts the `gd` (goto structure) / `gt` (goto data) / `gb`
        // (switch table) / `gw` (fit width) chord.
        KeyCode::Char('g') => {
            app.pending_g = true;
            app.status =
                t("g… d=表结构 t=表数据 v=定位值 c=列结构 b=切换表 f=冻结列 s=钉行 w=适配列宽 W=全列适配 m=物化成表").into();
        }
        KeyCode::Char('s') => sort_column(app, tx, false),
        // R94: `S` toggles the status-bar numeric summary (min / max / avg of the
        // focused column's loaded window). Off by default; `s` stays the sort.
        KeyCode::Char('S') => toggle_num_summary(app),
        KeyCode::Char('f') => open_filter_prompt(app),
        // `/` searches the visible result rows (filter-as-you-type).
        KeyCode::Char('/') => open_result_filter(app),
        // R64: `\` finds a substring in *any* cell of the loaded page. Every
        // match is highlighted and `n`/`N` step through them; client-side only,
        // never a query.
        KeyCode::Char('\\') => open_cell_find(app),
        // `*` filters to the focused column: type a value (pre-filled from the
        // cell under the cursor) to keep only rows whose cell contains it.
        KeyCode::Char('*') => open_col_filter(app),
        // R76: `#` cycles the big-number display mode for this result set.
        // Display-only: `Y` / edit still see the driver's original value.
        KeyCode::Char('#') => cycle_num_fmt(app),
        // `|` jumps straight to a column by number or name prefix (wide tables).
        KeyCode::Char('|') => open_col_jump(app),
        // R56: `:` jumps straight to a row by number (or `:$` for the last), the
        // vim `:<n>` gesture for a long grid. `gg` / `G` stay first / last.
        KeyCode::Char(':') => open_goto_row(app),
        // R59: `}` / `{` walk to the next / previous row whose cell in the
        // focused column is non-blank (NULL or empty string skipped), the vim
        // paragraph motion applied to a sparse column. `n` / `p` stay page turns.
        KeyCode::Char('}') => jump_nonblank_row(app, 1),
        KeyCode::Char('{') => jump_nonblank_row(app, -1),
        // R55: `<` / `>` narrow / widen the focused column, remembered per table
        // (persisted to tui.json since R72; a query result stays session-only).
        // The terminal twin of dragging a column border.
        KeyCode::Char('<') => adjust_col_width(app, -2),
        KeyCode::Char('>') => adjust_col_width(app, 2),
        // R72: `0` drops the focused column's remembered width, back to natural.
        KeyCode::Char('0') => reset_col_width(app),
        // `y` in the script *list* copies the focused statement's whole result
        // as CSV (the same format Ctrl-Y export leads with, R22); in a grid it
        // keeps copying the focused row as an INSERT statement.
        KeyCode::Char('y') => {
            if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
                copy_stmt_result(app);
            } else {
                copy_row_sql(app);
            }
        }
        // `Y` (R53): copy just the focused cell's value — the row-level `y`
        // above keeps copying the whole row as INSERT, and the row popup's `y`
        // is the same "copy value" gesture one level down.
        KeyCode::Char('Y') => copy_cell_value(app),
        // Bare-key aliases for the two view commands (mobile reachability).
        KeyCode::Char('w') => toggle_compact(app),
        // R76: `%` toggles alternate-row banding (zebra stripes) and remembers
        // it in tui.json. Free in the results pane (the editor keeps `%` for
        // its bracket jump), so it never shadows a data action.
        KeyCode::Char('%') => toggle_stripe(app),
        KeyCode::Char('c') => open_col_picker(app),
        // Delete the focused row: builds a bound `DELETE … WHERE …` and routes it
        // through the same red confirmation layer as every other write.
        KeyCode::Delete => delete_row(app),
        KeyCode::Char('z') => toggle_freeze_first(app),
        KeyCode::Up | KeyCode::Char('k') => {
            let times = take_count(app);
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(times as u16);
            } else {
                for _ in 0..times {
                    move_cursor(app, tx, -1);
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let times = take_count(app);
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_add(times as u16);
            } else {
                for _ in 0..times {
                    move_cursor(app, tx, 1);
                }
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if !ddl {
                move_col_cursor(app, -1);
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if !ddl {
                move_col_cursor(app, 1);
            }
        }
        KeyCode::PageUp => {
            let times = take_count(app) as u16;
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_sub(screen * times);
            } else {
                for _ in 0..times {
                    screen_move(app, tx, -1);
                }
            }
        }
        KeyCode::PageDown => {
            let times = take_count(app) as u16;
            if ddl {
                app.ddl_scroll = app.ddl_scroll.saturating_add(screen * times);
            } else {
                for _ in 0..times {
                    screen_move(app, tx, 1);
                }
            }
        }
        KeyCode::Home => {
            take_count(app);
            preview_home(app);
        }
        KeyCode::End => {
            take_count(app);
            preview_end(app);
        }
        // `G` — vim's "go to bottom" (R42), for the grid and the statement list.
        KeyCode::Char('G') => preview_end(app),
        KeyCode::Char('n') => {
            let times = take_count(app);
            if !app.result_needle.trim().is_empty() {
                for _ in 0..times {
                    search_move(app, 1);
                }
            } else if !app.locate_needle.trim().is_empty() {
                for _ in 0..times {
                    locate_move(app, 1);
                }
            } else if !app.cell_find_needle.trim().is_empty() {
                for _ in 0..times {
                    cell_find_step(app, 1);
                }
            } else {
                page_turn_by(app, tx, true, times);
            }
        }
        KeyCode::Char('N') => {
            take_count(app);
            if !app.result_needle.trim().is_empty() {
                search_move(app, -1);
            } else if !app.locate_needle.trim().is_empty() {
                locate_move(app, -1);
            } else if !app.cell_find_needle.trim().is_empty() {
                cell_find_step(app, -1);
            } else {
                app.status = t("先按 / 或 gv 搜索，再用 n/N 跳转命中").into();
            }
        }
        KeyCode::Char('p') => {
            let times = take_count(app);
            page_turn_by(app, tx, false, times);
        }
        KeyCode::Enter => {
            if let Some(s) = &app.script {
                if s.drilled.is_none() {
                    let idx = s.sel;
                    // R41: an errored statement opens the compact error box
                    // (first line + line count) instead of drilling into an
                    // empty grid; Enter again widens it to the full text.
                    if let Some(err) = s.outcomes.get(idx).and_then(|o| o.error.clone()) {
                        open_error_popup(app, &err);
                        return;
                    }
                    drill_script(app, idx);
                    return;
                }
            }
            // Enter always opens the whole row (the higher-frequency intent);
            // the cell's full value is one more key away — move within the row
            // popup and press Enter/v to drill in. `v` still opens the cell
            // popup directly from the grid.
            open_row_popup(app);
        }
        // R57: `V` enters row-select mode (vim's linewise visual). `v` is taken
        // by the cell popup, so the uppercase twin carries the bulk-row gesture.
        KeyCode::Char('V') => enter_row_select(app),
        // `v`: full cell value, straight from the grid (the shortcut path that
        // skips the row popup).
        KeyCode::Char('v') => open_cell_popup(app),
        _ => {}
    }
}

/// `Home` / `gg` in the results pane: top of the DDL, the statement list or the
/// grid, whichever owns the pane (R42).
pub(crate) fn preview_home(app: &mut App) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        app.ddl_scroll = 0;
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            s.sel = 0;
            return;
        }
    }
    app.sel = 0;
    // R51: jumping to the first row also parks the cell cursor back on the
    // first column, so a Home/End sweep starts from a known corner.
    app.col_cursor = 0;
    app.col_offset = 0;
}

/// `End` / `G` in the results pane: bottom of the DDL, the statement list or the
/// grid (R42).
pub(crate) fn preview_end(app: &mut App) {
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        app.ddl_scroll = u16::MAX;
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            s.sel = s.outcomes.len().saturating_sub(1);
            return;
        }
    }
    let n = result_row_count(app);
    if n > 0 {
        app.sel = n - 1;
        // R51: the last row also resets the cell cursor to the first column.
        app.col_cursor = 0;
        app.col_offset = 0;
    }
}

/// `y` in the script list: copy the focused statement's result grid as CSV (the
/// R22 export default), so a multi-statement run can be pasted into a ticket or
/// a spreadsheet straight from the console.
pub(crate) fn copy_stmt_result(app: &mut App) {
    let (idx, grid) = match app
        .script
        .as_ref()
        .and_then(|s| s.outcomes.get(s.sel).map(|o| (s.sel, o.grid.clone())))
    {
        Some(v) => v,
        None => {
            app.status = t("没有可复制的结果").into();
            return;
        }
    };
    if grid.columns.is_empty() && grid.rows.is_empty() {
        app.status = tf("第 {} 条语句没有结果集", &[&(idx + 1)]);
        return;
    }
    let csv = grid_to_csv(&grid);
    let n = csv.chars().count();
    match clipboard_copy(&csv) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制第 {} 条结果 CSV（{} 字符）· 兜底 {}",
                &[&(idx + 1), &n, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制第 {} 条结果 CSV（{} 字符）", &[&(idx + 1), &n]),
    }
}

/// Key handling for the full-cell popup: Esc / q / Enter close it (returning to
/// the row popup underneath when it was drilled from one), the arrows scroll the
/// wrapped value, `J` toggles the pretty-JSON view when the value is a JSON
/// object/array, and `y`/`Y` copy the **original** value (never the pretty form).
pub(crate) fn cell_popup_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if matches!(k.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
        app.cell_popup = None;
        app.flash(t("已关闭单元格").into());
        return;
    }
    // R97: `f` follows the cell's foreign key when the popup offers one; with no
    // FK it only says so and leaves the popup untouched.
    if k.code == KeyCode::Char('f') {
        if app.cell_popup.as_ref().is_some_and(|p| p.fk_jump.is_some()) {
            fk_jump(app, tx);
        } else {
            app.status = t("该单元格没有外键可跳转").into();
        }
        return;
    }
    // `U`: cycle the Unicode view — raw → escape-decoded → whole-value
    // re-escaped (non-ASCII as `\uXXXX`) → raw. A pure-ASCII value has no third
    // state, so it toggles raw ↔ decoded; a malformed `\u` escape reports an
    // error and leaves the view alone. Display state only — `y`/`Y` still copy
    // the raw value.
    if k.code == KeyCode::Char('U') {
        let Some(popup) = app.cell_popup.as_mut() else {
            return;
        };
        match advance_u_mode(popup) {
            Ok(UMode::Raw) => {
                app.popup_cache = None;
                app.status = t("Unicode 原值视图 · U 循环 解码 / 重新转义").into();
            }
            Ok(UMode::Decoded) => {
                app.popup_cache = None;
                app.status = t("Unicode 转义解码视图 · U 下一个").into();
            }
            Ok(UMode::Escaped) => {
                app.popup_cache = None;
                app.status = t("Unicode 重新转义视图（非 ASCII → \\uXXXX）· U 回原值").into();
            }
            Err(()) => {
                app.status =
                    t("✗ Unicode 解码失败（孤立代理对 / \\u 转义不完整）· 原值未变且只读").into();
            }
        }
        return;
    }
    // `J`: switch between the pretty and raw JSON views. A non-JSON value says
    // so instead of silently doing nothing.
    if k.code == KeyCode::Char('J') {
        let status = match &mut app.cell_popup {
            Some(p) if p.pretty.is_some() => {
                // R89: `J` always lands on the JSON view, so clear any active
                // Unicode `U` view first (otherwise the `U` body would keep
                // taking precedence and `J` would look dead).
                p.u_mode = UMode::Raw;
                p.show_pretty = !p.show_pretty;
                p.scroll = 0;
                Some(if p.show_pretty {
                    t("JSON 美化视图 · J 切回原值")
                } else {
                    t("原值视图 · J 切换 JSON 美化")
                })
            }
            _ => Some(t("该单元格不是 JSON 对象/数组")),
        };
        if let Some(s) = status {
            app.popup_cache = None;
            app.status = s.into();
        }
        return;
    }
    // `y` / `Y`: copy the original value, so a pretty view never changes what
    // lands on the clipboard.
    if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
        if let Some(p) = app.cell_popup.as_ref() {
            let col = p.col.clone();
            let raw = p.raw.clone();
            copy_named_value(app, &col, &raw);
        }
        return;
    }
    let delta: i32 = match k.code {
        KeyCode::Up | KeyCode::Char('k') => -1,
        KeyCode::Down | KeyCode::Char('j') => 1,
        KeyCode::PageUp => -5,
        KeyCode::PageDown => 5,
        _ => 0,
    };
    if delta == 0 {
        return;
    }
    if let Some(p) = &mut app.cell_popup {
        p.scroll = (p.scroll as i32 + delta).max(0) as u16;
    }
}

/// Open the compact execution-error box. `Enter` widens it to the full text.
pub(crate) fn open_error_popup(app: &mut App, err: &str) {
    let lines: Vec<String> = err.lines().map(|l| l.to_string()).collect();
    let lines = if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    };
    app.popup_cache = None;
    app.error_popup = Some(ErrorPopup {
        lines,
        expanded: false,
        scroll: 0,
    });
}

/// Key handling for the compact error box: `Esc`/`q` close, `Enter` widens a
/// compact box to the full scrollable text (and closes an expanded one), and the
/// usual scroll keys page the expanded view.
pub(crate) fn error_popup_key(app: &mut App, k: KeyEvent) {
    if app.error_popup.is_none() {
        return;
    }
    let mut scroll_delta: i32 = 0;
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.error_popup = None;
            app.flash(t("已关闭错误框").into());
            return;
        }
        KeyCode::Enter => {
            let expanded = app.error_popup.as_ref().is_some_and(|p| p.expanded);
            if expanded {
                app.error_popup = None;
            } else if let Some(p) = app.error_popup.as_mut() {
                p.expanded = true;
            }
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => scroll_delta = -1,
        KeyCode::Down | KeyCode::Char('j') => scroll_delta = 1,
        KeyCode::PageUp => scroll_delta = -5,
        KeyCode::PageDown => scroll_delta = 5,
        _ => {}
    }
    if scroll_delta != 0 {
        if let Some(p) = app.error_popup.as_mut() {
            p.scroll = (p.scroll as i32 + scroll_delta).max(0) as u16;
        }
    }
}

pub(crate) fn open_help(app: &mut App) {
    // Progressive discovery: the first `?` shows a context mini cheat-sheet that
    // always fits one screen; a second `?` (or Enter) widens it to the full
    // scrollable help. This is the cheap path on a small terminal where the full
    // overlay needs scrolling.
    app.help_mini = true;
    app.help_open = false;
    app.help_scroll = 0;
    app.help_filter = None;
}

/// R110: open the About dialog. The uptime is computed exactly once, here, and
/// frozen into the snapshot — the render path never reads a clock, so there is
/// no refresh and no polling. Opening it leaves any help layer untouched, so
/// `Esc` closes About and reveals the help underneath.
pub(crate) fn open_about(app: &mut App) {
    let uptime = format_uptime(uptime_between(app.session_start, Instant::now()));
    app.about = Some(AboutInfo::capture(&app.session_started_wall, uptime));
}

/// R110: the About overlay's key handler — `Esc` / `q` closes it (and flashes
/// the same transient status every other overlay uses).
pub(crate) fn about_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.about = None;
            app.flash(t("已关闭关于").into());
        }
        _ => {}
    }
}

pub(crate) fn help_key(app: &mut App, k: KeyEvent) {
    // While the `/` filter input owns the keyboard, every key is text (or the
    // two ways out: Enter keeps the filter, Esc clears it).
    if app.help_filter.is_some() {
        help_filter_key(app, k);
        return;
    }
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
            app.help_open = false;
            app.help_mini = false;
            app.help_needle.clear();
            app.help_scroll = 0;
            app.flash(t("已关闭帮助").into());
        }
        KeyCode::Char('/') => {
            app.help_filter = Some(TextArea::from([app.help_needle.clone()]));
        }
        // R110: `V` inside the full cheat-sheet opens About (F10 is the global
        // key; some terminals eat F10, so this is the documented fallback).
        KeyCode::Char('V') => open_about(app),
        KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.help_scroll = app.help_scroll.saturating_add(1),
        KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(8),
        KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(8),
        KeyCode::Home => app.help_scroll = 0,
        _ => {}
    }
}

/// The `/` filter input inside the full help overlay. Enter keeps the needle
/// (the list stays filtered); Esc clears it and restores the full sheet.
pub(crate) fn help_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.help_filter = None;
            app.help_scroll = 0;
        }
        KeyCode::Esc => {
            app.help_filter = None;
            app.help_needle.clear();
            app.help_scroll = 0;
            app.flash(t("已清除帮助过滤").into());
        }
        _ => {
            if let Some(ta) = app.help_filter.as_mut() {
                ta.input(k);
            }
            app.help_needle = app
                .help_filter
                .as_ref()
                .and_then(|ta| ta.lines().first().cloned())
                .unwrap_or_default();
            app.help_scroll = 0;
        }
    }
}

/// The mini cheat-sheet's key handler: `?` / Enter promotes to the full overlay,
/// Esc (or `q`) closes.
pub(crate) fn help_mini_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Char('?') | KeyCode::Enter | KeyCode::F(1) => {
            app.help_mini = false;
            app.help_open = true;
            app.help_scroll = 0;
            app.help_needle.clear();
            app.help_filter = None;
        }
        // R110: F10 opens About even from the mini cheat-sheet.
        KeyCode::F(10) => open_about(app),
        KeyCode::Esc | KeyCode::Char('q') => {
            app.help_mini = false;
            app.flash(t("已关闭帮助").into());
        }
        _ => {}
    }
}

pub(crate) fn filter_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let filter = app
                .filter_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.filter_prompt = None;
            if app.grid_kind == GridKind::MongoDocs {
                app.mongo_filter = filter.clone();
                app.pending_sel = Some(0);
                if filter.is_empty() {
                    app.status = t("过滤已清除").into();
                } else {
                    app.status = tf("过滤: {}", &[&(filter)]);
                }
                reload_mongo_docs(app, tx, 0);
                return;
            }
            let order_by = app.page_state.as_ref().and_then(|p| p.order_by.clone());
            if filter.is_empty() {
                app.status = t("过滤已清除").into();
            } else {
                app.status = tf("过滤: {}", &[&(filter)]);
            }
            reload_table_view(app, tx, filter, order_by, 0);
        }
        KeyCode::Esc => {
            app.filter_prompt = None;
            app.flash(t("已取消过滤").into());
        }
        _ => {
            if let Some(t) = &mut app.filter_prompt {
                t.input(k);
            }
        }
    }
}

// ── result-grid search (`/` in the results pane) ──

/// `/` in the results pane: filter visible rows as you type. The current needle
/// is loaded for editing, so `/` again refines an existing search.
pub(crate) fn open_result_filter(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图不支持搜索").into();
        return;
    }
    // The script *list* has no data grid to search; once a statement is drilled
    // into, its result is a normal grid and search applies.
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表不支持搜索（先 Enter 进入某条语句的结果）").into();
        return;
    }
    if active_grid(app).is_none() {
        app.status = t("没有可搜索的结果").into();
        return;
    }
    // `/` and `gv` are mutually exclusive: a row filter would hide the rows a
    // value locate wants to step through, so starting one drops the other.
    clear_locate(app);
    app.clear_cell_find();
    let mut ta = TextArea::from([app.result_needle.clone()]);
    ta.set_placeholder_text(t("搜索本页结果行…"));
    ta.move_cursor(CursorMove::End);
    app.result_filter = Some(ta);
}

/// Filter-as-you-type handler for the result search prompt.
pub(crate) fn result_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.result_filter = None;
            let n = result_row_count(app);
            app.status = if app.result_needle.trim().is_empty() {
                t("结果搜索已清除").into()
            } else {
                tf(
                    "搜索「{}」· {} 行命中 · n/N 跳转 · Esc 清除",
                    &[&(app.result_needle), &(n)],
                )
            };
        }
        KeyCode::Esc => {
            app.result_filter = None;
            app.result_needle.clear();
            app.rebuild_view();
            app.sel = 0;
            app.flash(t("已清除结果搜索").into());
        }
        _ => {
            if let Some(t) = &mut app.result_filter {
                t.input(k);
            }
            app.result_needle = app
                .result_filter
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.rebuild_view();
            app.sel = 0;
            let n = result_row_count(app);
            app.status = if app.result_needle.trim().is_empty() {
                t("输入以搜索结果行…").into()
            } else {
                tf("搜索「{}」· {} 行命中", &[&(app.result_needle), &(n)])
            };
        }
    }
}

/// `n` / `N` while a result search is active: cycle through the matching rows
/// (the filter already hides every non-match, so every visible row is a hit).
pub(crate) fn search_move(app: &mut App, dir: i32) {
    let n = result_row_count(app);
    if n == 0 {
        app.status = tf("搜索「{}」· 0 行命中", &[&(app.result_needle)]);
        return;
    }
    if dir > 0 {
        app.sel = (app.sel + 1) % n;
    } else {
        app.sel = (app.sel + n - 1) % n;
    }
    app.status = tf(
        "搜索「{}」· 命中 {}/{}",
        &[&(app.result_needle), &(app.sel + 1), &(n)],
    );
}

// ── results-pane column filter (`*`) ──

/// Display name of the column the `*` filter is scoped to.
pub(crate) fn col_filter_label(app: &App) -> String {
    app.col_filter_name
        .as_deref()
        .map(fix_double_encoding)
        .unwrap_or_default()
}

/// `*` in the results pane: filter the grid to the rows whose cell in the
/// focused column contains a typed value. The prompt is pre-filled with the
/// focused cell (truncated), so "show me the rows like this one" is one
/// keystroke. Client-side only — the page is never re-fetched.
pub(crate) fn open_col_filter(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图不支持列过滤").into();
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表不支持列过滤（先 Enter 进入某条语句的结果）").into();
        return;
    }
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可过滤的结果").into();
        return;
    };
    let Some(name) = grid.columns.get(app.col_cursor).cloned() else {
        app.status = t("没有可过滤的结果").into();
        return;
    };
    // `*` and `gv` are mutually exclusive for the same reason `/` and `gv` are:
    // a value locate wants every row on screen.
    clear_locate(app);
    app.clear_cell_find();
    // Seed from the focused cell (a huge / multi-line cell would be useless as a
    // pre-typed needle), so Enter alone re-runs "find rows like this one".
    let seed = grid
        .rows
        .get(app.sel)
        .and_then(|r| r.get(app.col_cursor))
        .map(|v| match v {
            Val::Null => String::new(),
            Val::Text(s) => truncate_disp(s.trim(), 48),
        })
        .unwrap_or_default();
    app.col_filter_name = Some(name.clone());
    app.col_filter_needle = seed.clone();
    let mut ta = TextArea::from([seed]);
    ta.set_placeholder_text(t("只显示该列含此值的行…"));
    ta.move_cursor(CursorMove::End);
    app.col_filter_prompt = Some(ta);
    app.rebuild_view();
    app.sel = 0;
    app.status = tf(
        "列过滤「{}」含「{}」· {} 行 · Enter 保留 · Esc 清除",
        &[
            &(col_filter_label(app)),
            &(app.col_filter_needle),
            &(result_row_count(app)),
        ],
    );
}

/// Prompt handler for `*`. Filters as you type (live hit count); Enter keeps the
/// filter and closes the prompt, Esc clears it.
pub(crate) fn col_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.col_filter_prompt = None;
            if app.col_filter_needle.trim().is_empty() {
                app.clear_col_filter();
                app.rebuild_view();
                app.sel = 0;
                app.status = t("列过滤已清除").into();
                return;
            }
            app.status = tf(
                "列过滤「{}」含「{}」· {} 行 · Esc 清除",
                &[
                    &(col_filter_label(app)),
                    &(app.col_filter_needle),
                    &(result_row_count(app)),
                ],
            );
        }
        KeyCode::Esc => {
            app.clear_col_filter();
            app.rebuild_view();
            app.sel = 0;
            app.flash(t("已清除列过滤").into());
        }
        _ => {
            if let Some(ta) = &mut app.col_filter_prompt {
                ta.input(k);
            }
            app.col_filter_needle = app
                .col_filter_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.rebuild_view();
            app.sel = 0;
            let label = col_filter_label(app);
            app.status = if app.col_filter_needle.trim().is_empty() {
                tf("列过滤「{}」· 输入以筛选 · Esc 清除", &[&label])
            } else {
                tf(
                    "列过滤「{}」含「{}」· {} 行",
                    &[&label, &(app.col_filter_needle), &(result_row_count(app))],
                )
            };
        }
    }
}

// ── grid value locate (`gv`) + column jump (`|`) ──

/// The column a `gv` locate searches: an explicit sort column when one is set,
/// otherwise the primary-key column, otherwise the first column. Derived from
/// the *rendered* grid so a hidden column never becomes a phantom target.
pub(crate) fn locate_target_col(app: &App) -> Option<usize> {
    let grid = active_grid(app)?;
    if grid.columns.is_empty() {
        return None;
    }
    if let Some(ps) = &app.page_state {
        if let Some((name, _)) = parse_order_by(ps.order_by.as_deref()).first() {
            if let Some(i) = grid
                .columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(name))
            {
                return Some(i);
            }
        }
    }
    if let Some(meta) = &app.table_meta {
        for c in &meta.columns {
            if c.is_primary_key {
                if let Some(i) = grid
                    .columns
                    .iter()
                    .position(|gc| gc.eq_ignore_ascii_case(&c.name))
                {
                    return Some(i);
                }
            }
        }
    }
    Some(0)
}

/// Loaded rows whose `col` cell contains `needle` (case-insensitive substring).
/// Pure so the locate state machine can be unit tested without a backend.
pub(crate) fn locate_matches(grid: &Grid, col: usize, needle: &str) -> Vec<usize> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return Vec::new();
    }
    grid.rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.get(col)
                .is_some_and(|v| v.text().to_lowercase().contains(&n))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The locate hit rows for the current needle / target column.
pub(crate) fn locate_hits(app: &App) -> Vec<usize> {
    let Some(col) = app.locate_col else {
        return Vec::new();
    };
    let Some(grid) = active_grid(app) else {
        return Vec::new();
    };
    locate_matches(&grid, col, &app.locate_needle)
}

/// `gv` — locate a value in the sort / primary-key column. Unlike `/` (which
/// hides non-matching rows) this only moves the cursor, so paging and the row
/// positions stay intact while you hunt for one key. `n`/`N` cycle the hits.
pub(crate) fn open_locate(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图不支持定位").into();
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表不支持定位（先 Enter 进入某条语句的结果）").into();
        return;
    }
    if active_grid(app).is_none() {
        app.status = t("没有可定位的结果").into();
        return;
    }
    let Some(col) = locate_target_col(app) else {
        app.status = t("没有可定位的结果").into();
        return;
    };
    // `/` and `gv` are mutually exclusive: a value locate wants every row on
    // screen, an active row filter would hide the very rows it searches for.
    if !app.result_needle.is_empty() {
        app.result_needle.clear();
        app.result_filter = None;
        app.rebuild_view();
        app.sel = 0;
    }
    // A column filter hides rows too, so `gv` clears it for the same reason.
    if app.clear_col_filter() {
        app.rebuild_view();
        app.sel = 0;
    }
    // A cell find highlights cells in place, but its hit list is keyed to the
    // row/column layout, so a locate drops it too (they share `n`/`N`).
    app.clear_cell_find();
    app.locate_col = Some(col);
    let mut ta = TextArea::from([app.locate_needle.clone()]);
    ta.set_placeholder_text(t("定位值（排序列 / 主键列）…"));
    ta.move_cursor(CursorMove::End);
    app.locate_prompt = Some(ta);
}

/// Prompt handler for `gv`. Filters as you type so the hit count is live; Enter
/// jumps to the first hit and keeps the needle for `n`/`N`, Esc clears it.
pub(crate) fn locate_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.locate_needle = app
                .locate_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.locate_prompt = None;
            let hits = locate_hits(app);
            if hits.is_empty() {
                app.status = tf("未找到匹配值「{}」", &[&(app.locate_needle)]);
                app.locate_needle.clear();
                return;
            }
            app.sel = hits[0];
            let label = locate_col_label(app);
            app.status = tf(
                "定位 {}「{}」· {} 命中 · n/N 跳转 · Esc 清除",
                &[&(label), &(app.locate_needle), &(hits.len())],
            );
        }
        KeyCode::Esc => {
            clear_locate(app);
            app.flash(t("已清除定位").into());
        }
        _ => {
            if let Some(t) = &mut app.locate_prompt {
                t.input(k);
            }
            app.locate_needle = app
                .locate_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            let n = locate_hits(app).len();
            app.status = if app.locate_needle.trim().is_empty() {
                t("输入以定位值…").into()
            } else {
                tf("定位「{}」· {} 命中", &[&(app.locate_needle), &(n)])
            };
        }
    }
}

/// Drop the locate needle and prompt (but leave the cursor where it is).
pub(crate) fn clear_locate(app: &mut App) {
    app.locate_prompt = None;
    app.locate_needle.clear();
    app.locate_col = None;
}

/// The target column's display name, for the status line.
pub(crate) fn locate_col_label(app: &App) -> String {
    app.locate_col
        .and_then(|c| active_grid(app).and_then(|g| g.columns.get(c).cloned()))
        .unwrap_or_else(|| t("列").to_string())
}

/// `n` / `N` while a value locate is active: step to the next / previous hit,
/// wrapping and anchoring on the cursor when it is not itself a hit.
pub(crate) fn locate_move(app: &mut App, dir: i32) {
    let hits = locate_hits(app);
    if hits.is_empty() {
        app.status = tf("定位「{}」· 0 命中", &[&(app.locate_needle)]);
        return;
    }
    let pos = hits.iter().position(|&r| r == app.sel);
    app.sel = if dir > 0 {
        match pos {
            Some(i) => hits[(i + 1) % hits.len()],
            None => *hits.iter().find(|&&r| r > app.sel).unwrap_or(&hits[0]),
        }
    } else {
        match pos {
            Some(i) => hits[(i + hits.len() - 1) % hits.len()],
            None => *hits
                .iter()
                .rev()
                .find(|&&r| r < app.sel)
                .unwrap_or(&hits[hits.len() - 1]),
        }
    };
    let label = locate_col_label(app);
    app.status = tf(
        "定位 {}「{}」· 命中 {}/{}",
        &[
            &(label),
            &(app.locate_needle),
            &(app.sel + 1),
            &(hits.len()),
        ],
    );
}

// ── result-set cell find (`\` in the results pane, R64) ──

/// Every `(row, col)` in `grid` whose cell contains `needle` (case-insensitive
/// substring, NULL matched as the text `null`), in reading order (row-major),
/// capped at `limit`. Pure, so the hit set is unit-testable without a backend.
/// Returns `(hits, capped)`: `capped` is `true` when the grid held more matches
/// than the ceiling allowed.
pub(crate) fn cell_find_hits(
    grid: &Grid,
    needle: &str,
    limit: usize,
) -> (Vec<(usize, usize)>, bool) {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return (Vec::new(), false);
    }
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (ri, row) in grid.rows.iter().enumerate() {
        for ci in 0..grid.columns.len() {
            if cell_matches(row, ci, &n) {
                if hits.len() >= limit {
                    return (hits, true);
                }
                hits.push((ri, ci));
            }
        }
    }
    (hits, false)
}

/// Recompute [`App::cell_find_hits`] from the grid on screen. Client-side over
/// the already-loaded page and capped — never a query.
pub(crate) fn compute_cell_find(app: &mut App) {
    let limit = cell_find_limit();
    let needle = app.cell_find_needle.trim().to_lowercase();
    let (hits, capped) = if needle.is_empty() {
        (Vec::new(), false)
    } else {
        match active_grid(app) {
            Some(grid) => cell_find_hits(&grid, &needle, limit),
            None => (Vec::new(), false),
        }
    };
    app.cell_find_hits = hits;
    app.cell_find_idx = 0;
    app.cell_find_capped = capped;
}

/// `\` in the results pane: find a substring in *any* cell of the loaded page.
/// Every match is highlighted; Enter jumps to the first and `n`/`N` cycle. The
/// page is never re-fetched (zero-query), and unlike `/` no row is hidden.
pub(crate) fn open_cell_find(app: &mut App) {
    if app.grid_kind == GridKind::Columns {
        app.status = t("表结构视图不支持单元格查找").into();
        return;
    }
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("脚本列表不支持单元格查找（先 Enter 进入某条语句的结果）").into();
        return;
    }
    if active_grid(app).is_none() {
        app.status = t("没有可查找的结果").into();
        return;
    }
    // A cell find wants every row on screen, so an active row / column filter
    // (which hides rows) is dropped first — the same rule `/` and `gv` follow.
    if !app.result_needle.is_empty() {
        app.result_needle.clear();
        app.result_filter = None;
        app.rebuild_view();
        app.sel = 0;
    }
    if app.clear_col_filter() {
        app.rebuild_view();
        app.sel = 0;
    }
    clear_locate(app);
    let mut ta = TextArea::from([app.cell_find_needle.clone()]);
    ta.set_placeholder_text(t("在结果单元格中查找…"));
    ta.move_cursor(CursorMove::End);
    app.cell_find_prompt = Some(ta);
}

/// Prompt handler for `\`. The hits recompute as you type (live highlight, no
/// cursor jump); Enter lands on the first match and keeps the needle for `n`/`N`,
/// Esc clears everything.
pub(crate) fn cell_find_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.cell_find_needle = app
                .cell_find_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.cell_find_prompt = None;
            compute_cell_find(app);
            if app.cell_find_hits.is_empty() {
                app.status = tf("单元格查找「{}」· 无命中", &[&(app.cell_find_needle)]);
                app.cell_find_needle.clear();
                return;
            }
            let (r, c) = app.cell_find_hits[0];
            app.sel = r;
            app.col_cursor = c;
            app.cell_find_idx = 0;
            // `≥n` flags a hit list that reached the scan cap (more matches were
            // truncated), so the count is never mistaken for the exact total.
            let shown = if app.cell_find_capped {
                format!("≥{}", app.cell_find_hits.len())
            } else {
                app.cell_find_hits.len().to_string()
            };
            app.status = tf(
                "查找「{}」· {} 命中 · n/N 跳转 · Esc 清除",
                &[&(app.cell_find_needle), &(shown)],
            );
        }
        KeyCode::Esc => {
            app.clear_cell_find();
            app.flash(t("已清除单元格查找").into());
        }
        _ => {
            if let Some(t) = &mut app.cell_find_prompt {
                t.input(k);
            }
            app.cell_find_needle = app
                .cell_find_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            compute_cell_find(app);
            let n = app.cell_find_hits.len();
            app.status = if app.cell_find_needle.trim().is_empty() {
                t("输入以查找单元格…").into()
            } else {
                let shown = if app.cell_find_capped {
                    format!("≥{n}")
                } else {
                    n.to_string()
                };
                tf("查找「{}」· {} 命中", &[&(app.cell_find_needle), &(shown)])
            };
        }
    }
}

/// `n` / `N` while a cell find is active: step to the next / previous matching
/// cell, wrapping, anchored on the cursor when it is not itself a hit. Both the
/// row and the column cursor move, so a hit in a far-off column is revealed.
pub(crate) fn cell_find_step(app: &mut App, dir: i32) {
    let n = app.cell_find_hits.len();
    if n == 0 {
        app.status = tf("查找「{}」· 0 命中", &[&(app.cell_find_needle)]);
        return;
    }
    let pos = (app.sel, app.col_cursor);
    let idx = if dir > 0 {
        app.cell_find_hits
            .iter()
            .position(|&h| h > pos)
            .unwrap_or(0)
    } else {
        app.cell_find_hits
            .iter()
            .rposition(|&h| h < pos)
            .unwrap_or(n - 1)
    };
    let (r, c) = app.cell_find_hits[idx];
    app.cell_find_idx = idx;
    app.sel = r;
    app.col_cursor = c;
    app.status = tf(
        "查找「{}」· 命中 {}/{}",
        &[&(app.cell_find_needle), &(idx + 1), &(n)],
    );
}

/// `|` — jump the cell cursor to a column by 1-based number or name prefix.
pub(crate) fn open_col_jump(app: &mut App) {
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可跳转的列").into();
        return;
    };
    if grid.columns.is_empty() {
        app.status = t("没有可跳转的列").into();
        return;
    }
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t("列号或列名前缀…"));
    app.col_jump = Some(ta);
}

/// Parse a `|` column-jump input against the grid columns: a 1-based number, or
/// a case-insensitive name prefix (then substring). Pure so it can be tested
/// directly.
pub(crate) fn parse_col_jump(columns: &[String], input: &str) -> Result<usize, String> {
    let q = input.trim();
    if q.is_empty() {
        return Err(t("请输入列号或列名").to_string());
    }
    if let Ok(num) = q.parse::<usize>() {
        if num >= 1 && num <= columns.len() {
            return Ok(num - 1);
        }
        return Err(tf("列号超出范围（1-{}）", &[&(columns.len())]));
    }
    let lower = q.to_lowercase();
    if let Some(i) = columns
        .iter()
        .position(|c| c.to_lowercase().starts_with(&lower))
    {
        return Ok(i);
    }
    if let Some(i) = columns
        .iter()
        .position(|c| c.to_lowercase().contains(&lower))
    {
        return Ok(i);
    }
    Err(tf("找不到列「{}」", &[&q]))
}

/// Prompt handler for `|`: Enter jumps, Esc cancels, anything else is text.
pub(crate) fn col_jump_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let input = app
                .col_jump
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.col_jump = None;
            let Some(grid) = active_grid(app) else {
                app.status = t("没有可跳转的列").into();
                return;
            };
            match parse_col_jump(&grid.columns, &input) {
                Ok(i) => {
                    let name = grid.columns.get(i).cloned().unwrap_or_default();
                    app.col_cursor = i;
                    app.poke_hbar();
                    app.sel = app.sel.min(grid.rows.len().saturating_sub(1));
                    app.status = tf("跳到第 {} 列 {}", &[&(i + 1), &(name)]);
                }
                Err(msg) => app.status = msg,
            }
        }
        KeyCode::Esc => {
            app.col_jump = None;
            app.flash(t("已取消跳列").into());
        }
        _ => {
            if let Some(t) = &mut app.col_jump {
                t.input(k);
            }
        }
    }
}

// ── R56: `:` row-number jump ──

/// R83: the denominator for the row-jump prompt — the current page's row count,
/// or the whole-table total when a paginated table / document view is open. The
/// number a user types is the *absolute* row of the table, not of the page.
pub(crate) fn goto_row_total(app: &App) -> usize {
    let page_rows = result_row_count(app);
    if app.script.is_none() {
        if let Some(total) = app.page_state.as_ref().and_then(|ps| ps.total) {
            return (total as usize).max(page_rows);
        }
    }
    page_rows
}

/// R83: split an absolute 0-based row into its page and offset. Pure.
pub(crate) fn row_jump_page(index: usize, page_size: usize) -> (usize, usize) {
    let ps = page_size.max(1);
    (index / ps, index % ps)
}

/// `:` in the results pane — jump to a row by number (`:12`) or to the last row
/// (`:$`). Reuses the existing one-line prompt infrastructure; unlike the cell
/// cursor it never touches the column, so a wide row keeps its place. When a
/// paginated table view is open the number is an absolute table row and the
/// target page is loaded on demand (R83).
pub(crate) fn open_goto_row(app: &mut App) {
    let n = goto_row_total(app);
    if n == 0 {
        app.status = t("没有可跳转的行").into();
        return;
    }
    let mut ta = TextArea::default();
    ta.set_placeholder_text(tf("行号 1-{} 或 $ 末行…", &[&(n)]));
    app.goto_prompt = Some(ta);
}

/// Parse a `:` row-jump input against a row count: a 1-based number (`1` and `0`
/// both mean the first row) or `$` / `end` for the last row. R83: an
/// out-of-range number *clamps* to the last row (the earlier reject-on-typo
/// behaviour is gone) so a jump is always a jump, never an error. Pure so it can
/// be tested directly.
pub(crate) fn parse_row_jump(count: usize, input: &str) -> Result<usize, String> {
    if count == 0 {
        return Err(t("没有可跳转的行").to_string());
    }
    let q = input.trim();
    if q.is_empty() {
        return Err(t("请输入行号").to_string());
    }
    if q == "$" || q.eq_ignore_ascii_case("end") {
        return Ok(count - 1);
    }
    match q.parse::<usize>() {
        // `n` is 1-based; anything past the last row clamps to it. `0` (and `1`)
        // both mean the first row.
        Ok(n) => Ok(n.saturating_sub(1).min(count - 1)),
        Err(_) => Err(tf("无法识别的行号「{}」", &[&(q)])),
    }
}

/// Prompt handler for `:`: Enter jumps, Esc cancels, anything else is text.
pub(crate) fn goto_row_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let input = app
                .goto_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.goto_prompt = None;
            let page_rows = result_row_count(app);
            let total = goto_row_total(app).max(page_rows);
            match parse_row_jump(total, &input) {
                Ok(i) => {
                    // R83: a paginated table view takes an absolute row; load the
                    // page it lives on and park the cursor at its offset. An
                    // in-memory result set (query / drilled script) jumps
                    // directly.
                    if app.script.is_none() {
                        if let Some(ps) = app.page_state.clone() {
                            let (page, off) = row_jump_page(i, ps.page_size);
                            if page == ps.page {
                                app.sel = off.min(page_rows.saturating_sub(1));
                                app.col_cursor = 0;
                                app.col_offset = 0;
                                app.status = tf("跳到第 {} 行 / 共 {}", &[&(i + 1), &(total)]);
                            } else if app.page_pending || !goto_page(app, tx, page, Some(off)) {
                                app.status = t("正在加载，稍后再试").into();
                            } else {
                                app.col_cursor = 0;
                                app.col_offset = 0;
                                app.status =
                                    tf("跳到第 {} 行 · 第 {} 页…", &[&(i + 1), &(page + 1)]);
                            }
                            return;
                        }
                    }
                    // The column cursor parks on the first column so the jump
                    // lands on a known corner (same rule as `gg` / `G`).
                    app.sel = i;
                    app.col_cursor = 0;
                    app.col_offset = 0;
                    app.status = tf("跳到第 {} 行 / 共 {}", &[&(i + 1), &(total)]);
                }
                Err(msg) => app.status = msg,
            }
        }
        KeyCode::Esc => {
            app.goto_prompt = None;
            app.flash(t("已取消跳行").into());
        }
        _ => {
            if let Some(t) = &mut app.goto_prompt {
                t.input(k);
            }
        }
    }
}

// ── mobile efficiency: compact columns / column visibility / recents / filter ──

/// Ctrl-Shift-C — toggle the compact column-width mode. The first press always
/// flips whatever the current (possibly automatic) state is, so the user sees an
/// immediate change on any screen size.
pub(crate) fn toggle_compact(app: &mut App) {
    let now = compact_active(app.compact, app.layout_mode);
    app.compact = Some(!now);
    let on = compact_active(app.compact, app.layout_mode);
    app.status = if on {
        tf(
            "{} · 列宽≤{} 自适应，尽量一屏放下（Alt-C / w 关闭）",
            &[&(compact_label(app)), &(COMPACT_MAX_CELL)],
        )
    } else {
        tf(
            "{} · 列宽按内容（Alt-C / w 开启）",
            &[&(compact_label(app))],
        )
    };
    // Persist the choice: as the global default and, when a table is open, for
    // that exact `database.schema.table` so reopening it restores the mode.
    app.config.set_compact(app.compact);
    if let Some(ps) = app.page_state.clone() {
        let db = app.current_db();
        app.config.entry(&db, &ps.schema, &ps.table).compact = app.compact;
    }
    app.persist();
}

/// R76: `#` — cycle the big-number cell display through original → thousands
/// → abbreviated. A pure display layer: the underlying values are untouched, so
/// `Y` (copy value) and the edit dialog still see exactly what the driver sent.
/// The chosen mode persists in `tui.json` and is applied on the next launch.
pub(crate) fn cycle_num_fmt(app: &mut App) {
    app.num_fmt = app.num_fmt.next();
    // Widths depend on the mode (a `1,234,567` is wider than `1234567`), so the
    // cached natural widths must be rebuilt for the new text.
    app.width_cache = None;
    app.config.set_num_fmt(app.num_fmt);
    app.persist();
    app.flash(tf("大数字显示 · {}", &[&(app.num_fmt.label())]));
}

/// R76: `%` — toggle alternate-row banding (zebra stripes). Persisted as a
/// global display pref so it survives the next launch.
pub(crate) fn toggle_stripe(app: &mut App) {
    app.stripe = !app.stripe;
    app.config.set_stripe(app.stripe);
    app.persist();
    app.flash(if app.stripe {
        t("斑马纹 开（% 关闭）").to_string()
    } else {
        t("斑马纹 关（% 开启）").to_string()
    });
}

/// Ctrl-Shift-H — open the column-visibility picker for the grid on screen.
pub(crate) fn open_col_picker(app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        app.status = t("没有可选择的列（先打开一张表或执行查询）").into();
        return;
    };
    if grid.columns.is_empty() || app.grid_kind == GridKind::Columns {
        app.status = t("当前视图不支持列选择").into();
        return;
    }
    app.col_picker_open = true;
    app.col_picker_list.select(Some(0));
}

pub(crate) fn col_picker_key(app: &mut App, k: KeyEvent) {
    let n = app.grid_full.as_ref().map(|g| g.columns.len()).unwrap_or(0);
    match k.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            app.col_picker_open = false;
            app.flash(t("已关闭列可见性").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app
                    .col_picker_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.col_picker_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .col_picker_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.col_picker_list.select(Some(i));
            }
        }
        KeyCode::Char(' ') => toggle_col_visible(app),
        // `a` shows every column again, `x` narrows to just the first.
        KeyCode::Char('a') => {
            app.col_hidden.clear();
            app.reapply_col_filter();
            app.persist_cols();
            app.status = t("已显示全部列（已记住）").into();
        }
        KeyCode::Char('x') => {
            if let Some(grid) = app.grid_full.clone() {
                app.col_hidden = grid.columns.iter().skip(1).cloned().collect();
                app.reapply_col_filter();
                app.persist_cols();
                app.status = t("仅保留第一列（已记住）").into();
            }
        }
        _ => {}
    }
}

/// Space in the column picker: hide / show the highlighted column. The last
/// visible column can never be hidden.
/// R48 `gc` / R56: open the column-structure popup. It lists the columns already
/// cached for the open table (`table_meta`, read from information_schema when
/// the table was loaded) — no extra query. A query result has no table metadata,
/// so its grid columns are shown by name only. R56 adds the default value and
/// the `PRI` / `UNI` / `MUL` key mark, and a fresh popup starts unfiltered.
pub(crate) fn open_cols_popup(app: &mut App) {
    if cols_popup_rows(app).is_empty() {
        app.status = t("无可显示的列（先打开一张表或执行查询）").into();
        return;
    }
    app.cols_popup_open = true;
    app.cols_popup_scroll = 0;
    app.cols_popup_sel = 0;
    app.cols_popup_needle.clear();
    app.cols_popup_filter = None;
}

/// R56: one column row for the `g c` popup. The key mark follows MySQL's
/// `information_schema.COLUMNS.COLUMN_KEY` where the pinned dbx-core exposes it
/// — `PRI` from `is_primary_key`, `UNI` from `is_unique` — and derives `MUL`
/// from the cached index metadata.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColPopupRow {
    pub(crate) name: String,
    pub(crate) data_type: String,
    pub(crate) key: &'static str,
    pub(crate) default: String,
    pub(crate) nullable: bool,
    pub(crate) comment: String,
}

/// The `COLUMN_KEY` shorthand of one column. `MUL` (the first column of a
/// non-unique index, a value may repeat) needs index metadata, so it is only
/// produced when the backend listed the indexes into `table_meta`.
pub(crate) fn column_key_mark(c: &ColumnInfo, indexes: &[IndexInfo]) -> &'static str {
    if c.is_primary_key {
        return "PRI";
    }
    if c.is_unique {
        return "UNI";
    }
    let mul = indexes.iter().any(|ix| {
        !ix.is_primary
            && !ix.is_unique
            && ix
                .columns
                .first()
                .is_some_and(|n| n.eq_ignore_ascii_case(&c.name))
    });
    if mul {
        "MUL"
    } else {
        ""
    }
}

/// The popup's columns, from the cached `table_meta` (name / type / key /
/// default / nullability / comment) or, for a bare query result, the grid's
/// column names alone. Empty when there is neither metadata nor a grid.
pub(crate) fn cols_popup_rows(app: &App) -> Vec<ColPopupRow> {
    if let Some(meta) = &app.table_meta {
        if !meta.columns.is_empty() {
            return meta
                .columns
                .iter()
                .map(|c| ColPopupRow {
                    name: fix_double_encoding(&c.name),
                    data_type: c.data_type.clone(),
                    key: column_key_mark(c, &meta.indexes),
                    default: c
                        .column_default
                        .as_deref()
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .unwrap_or("")
                        .to_string(),
                    nullable: c.is_nullable,
                    comment: c
                        .comment
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect();
        }
    }
    match full_grid(app) {
        Some(g) if !g.columns.is_empty() => g
            .columns
            .iter()
            .map(|c| ColPopupRow {
                name: fix_double_encoding(c),
                data_type: String::new(),
                key: "",
                default: String::new(),
                nullable: true,
                comment: String::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `/` inside the popup keeps only the columns whose name contains the needle
/// (case-insensitive substring), the same rule the result search uses.
pub(crate) fn cols_popup_matches(row: &ColPopupRow, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return true;
    }
    row.name.to_lowercase().contains(&n.to_lowercase())
}

/// `(hits, total)` of the popup's column filter, for the title and the status.
pub(crate) fn cols_popup_hits(app: &App) -> (usize, usize) {
    let rows = cols_popup_rows(app);
    let total = rows.len();
    let hits = rows
        .iter()
        .filter(|r| cols_popup_matches(r, &app.cols_popup_needle))
        .count();
    (hits, total)
}

/// R66: one column's distribution over the already-loaded rows. Everything here
/// is derived in place from the page dbxt already holds — the popup never issues
/// a query. `truncated` marks the case where the page was larger than
/// [`COL_STATS_SCAN_LIMIT`] and only the first `scanned` rows were read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColStats {
    pub(crate) non_null: usize,
    pub(crate) nulls: usize,
    pub(crate) distinct: usize,
    /// True when every non-null value parsed as a finite number.
    pub(crate) numeric: bool,
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) avg: f64,
    pub(crate) scanned: usize,
    pub(crate) truncated: bool,
    /// R72: a `COL_SPARK_W`-wide block sparkline of the sampled values — numeric
    /// buckets for a numeric column, value-length buckets otherwise. Empty when
    /// there is nothing to sample.
    pub(crate) spark: String,
}

/// Parse one cell as a finite number for the stats. A blank, a non-numeric text
/// or `inf` / `nan` all return `None`, so a text column never accidentally gains
/// a `min` / `max`. R94: a thousands-separated number (`1,234` / `12,345.68`) is
/// tolerated too, but only when the comma groups are well-formed, so `1,2` or a
/// stray `,` is still rejected.
pub(crate) fn parse_stat_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = t.parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    parse_grouped_num(t).filter(|v| v.is_finite())
}

/// Parse a thousands-separated decimal (`1,234`, `-12,345.6`). Returns `None`
/// unless every comma group is well-formed: the first group 1..=3 digits, every
/// later group exactly 3, an optional sign and an optional dot fraction of
/// digits only. Deliberately strict so a normal parsed float is never altered.
fn parse_grouped_num(t: &str) -> Option<f64> {
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => ("-", r),
        None => match t.strip_prefix('+') {
            Some(r) => ("+", r),
            None => ("", t),
        },
    };
    let (int_part, frac_part) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    if !int_part.contains(',') {
        return None;
    }
    let groups: Vec<&str> = int_part.split(',').collect();
    if groups.len() < 2 {
        return None;
    }
    let first = groups[0];
    if first.is_empty() || first.len() > 3 || !first.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    for g in &groups[1..] {
        if g.len() != 3 || !g.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let mut plain = String::from(sign);
    plain.push_str(&groups.join(""));
    if let Some(f) = frac_part {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        plain.push('.');
        plain.push_str(f);
    }
    plain.parse::<f64>().ok()
}

/// R66: count the non-null / null / distinct values of one column and, when
/// every non-null value is numeric, its min / max / average. `limit` bounds the
/// scan (the loaded page is only ever a sample of a big table anyway).
pub(crate) fn col_stats(grid: &Grid, col: usize, limit: usize) -> ColStats {
    let scanned = grid.rows.len().min(limit);
    let truncated = grid.rows.len() > scanned;
    let mut non_null = 0usize;
    let mut nulls = 0usize;
    let mut distinct: HashSet<String> = HashSet::new();
    let mut num_min = f64::INFINITY;
    let mut num_max = f64::NEG_INFINITY;
    let mut num_sum = 0.0f64;
    let mut all_numeric = true;
    // R72: the sampled values / lengths feeding the distribution sparkline.
    let mut nums: Vec<f64> = Vec::new();
    let mut lens: Vec<usize> = Vec::new();
    for row in grid.rows.iter().take(scanned) {
        match row.get(col) {
            None | Some(Val::Null) => nulls += 1,
            Some(Val::Text(s)) => {
                non_null += 1;
                distinct.insert(s.clone());
                lens.push(s.chars().count());
                match parse_stat_num(s) {
                    Some(v) => {
                        num_min = num_min.min(v);
                        num_max = num_max.max(v);
                        num_sum += v;
                        nums.push(v);
                    }
                    None => all_numeric = false,
                }
            }
        }
    }
    let numeric = non_null > 0 && all_numeric;
    let spark = if numeric {
        sparkline_numeric(&nums, COL_SPARK_W)
    } else {
        sparkline_lengths(&lens, COL_SPARK_W)
    };
    ColStats {
        non_null,
        nulls,
        distinct: distinct.len(),
        numeric,
        min: if numeric { num_min } else { 0.0 },
        max: if numeric { num_max } else { 0.0 },
        avg: if numeric {
            num_sum / non_null as f64
        } else {
            0.0
        },
        scanned,
        truncated,
        spark,
    }
}

/// R72: the eight block glyphs a sparkline draws with.
const SPARK_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// R72: turn per-bucket counts into a `width`-glyph sparkline. A non-empty bucket
/// always draws at least the lowest block, and an empty bucket draws a space so a
/// gap in the distribution stays visible; an all-zero input is all spaces.
pub(crate) fn spark_from_buckets(buckets: &[usize]) -> String {
    let max = buckets.iter().copied().max().unwrap_or(0);
    let mut out = String::with_capacity(buckets.len());
    for &c in buckets {
        if c == 0 || max == 0 {
            out.push(' ');
            continue;
        }
        let lvl = ((c * 7) as f64 / max as f64).round() as usize;
        out.push(SPARK_BLOCKS[lvl.clamp(1, 7)]);
    }
    out
}

/// R72: bucket `values` into `width` equal bins across `[min, max]` and render
/// the sparkline. A single distinct value (or an empty slice) lands in one
/// bucket.
pub(crate) fn sparkline_numeric(values: &[f64], width: usize) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut buckets = vec![0usize; width];
    for &v in values {
        let idx = if max > min {
            (((v - min) / (max - min)) * (width - 1) as f64).round() as usize
        } else {
            width / 2
        };
        buckets[idx.min(width - 1)] += 1;
    }
    spark_from_buckets(&buckets)
}

/// R72: bucket string lengths into `width` equal bins across `[min, max]` and
/// render the sparkline (the text-column counterpart of [`sparkline_numeric`]).
pub(crate) fn sparkline_lengths(lens: &[usize], width: usize) -> String {
    if lens.is_empty() || width == 0 {
        return String::new();
    }
    let min = *lens.iter().min().unwrap();
    let max = *lens.iter().max().unwrap();
    let mut buckets = vec![0usize; width];
    for &l in lens {
        let idx = if max > min {
            (((l - min) as f64 / (max - min) as f64) * (width - 1) as f64).round() as usize
        } else {
            width / 2
        };
        buckets[idx.min(width - 1)] += 1;
    }
    spark_from_buckets(&buckets)
}

/// Render a float for the stats pane: an integer stays bare (`12`), a fraction
/// keeps up to four decimals with the trailing zeros trimmed (`4.5`).
pub(crate) fn fmt_stat_num(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// ── R94: focused-column numeric summary for the status bar (`S`) ──

/// The numeric snapshot of one loaded-column window: the smallest, largest and
/// mean of every finite value that parsed (NULL / blank / non-numeric cells are
/// skipped) plus how many values fed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NumSummary {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) avg: f64,
    pub(crate) count: usize,
}

/// R94: min / max / avg over the numeric values of one column in the already
/// loaded window. `None` when the column holds no numeric value at all (so a
/// pure text column never gains a summary). NULL, blanks and thousands-separated
/// text are tolerated via [`parse_stat_num`]. Pure and client-side.
pub(crate) fn numeric_summary(grid: &Grid, col: usize, limit: usize) -> Option<NumSummary> {
    let scanned = grid.rows.len().min(limit);
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for row in grid.rows.iter().take(scanned) {
        let Some(s) = row.get(col).and_then(|v| match v {
            Val::Text(s) => Some(s.as_str()),
            _ => None,
        }) else {
            continue;
        };
        if let Some(v) = parse_stat_num(s) {
            min = min.min(v);
            max = max.max(v);
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some(NumSummary {
        min,
        max,
        avg: sum / count as f64,
        count,
    })
}

/// R94: the focused column's numeric summary over the grid on screen, or `None`
/// when there is no result grid / the cursor is past the columns / the column
/// has no numeric value. Zero queries — the loaded window only.
pub(crate) fn col_numeric_summary(app: &App) -> Option<NumSummary> {
    if app.grid_kind == GridKind::Columns {
        return None;
    }
    // The displayed grid is already on hand during the status render, so borrow
    // it instead of cloning the full page every frame.
    let grid = app.grid.as_ref()?;
    if app.col_cursor >= grid.columns.len() {
        return None;
    }
    numeric_summary(grid, app.col_cursor, COL_STATS_SCAN_LIMIT)
}

/// R94: the status-bar text `min a · max b · avg c` when the `S` summary is on
/// and the focused column has at least one numeric value.
pub(crate) fn num_summary_text(app: &App) -> Option<String> {
    if !app.num_summary {
        return None;
    }
    let s = col_numeric_summary(app)?;
    Some(tf(
        "min {} · max {} · avg {}",
        &[
            &fmt_stat_num(s.min),
            &fmt_stat_num(s.max),
            &fmt_stat_num(s.avg),
        ],
    ))
}

/// R94: `S` — toggle the status-bar numeric summary. Off by default (it changes
/// persistent status-bar content, which is a visual change). The status message
/// names the column's state so a press on a text column is never a silent no-op.
pub(crate) fn toggle_num_summary(app: &mut App) {
    app.num_summary = !app.num_summary;
    if !app.num_summary {
        app.flash(t("数值摘要 关").into());
        return;
    }
    match col_numeric_summary(app) {
        Some(s) => app.flash(tf(
            "数值摘要 开 · min {} · max {} · avg {}",
            &[
                &fmt_stat_num(s.min),
                &fmt_stat_num(s.max),
                &fmt_stat_num(s.avg),
            ],
        )),
        None => app.flash(t("数值摘要 开 · 当前列无数值").into()),
    }
}

/// R66: the stats for the popup's highlighted column. The lookup is name-based
/// against the *unfiltered* grid (a column hidden by the picker still has data),
/// and the whole scan is client-side. `None` means the column is not part of the
/// loaded page at all.
pub(crate) fn cols_popup_stats(app: &App, name: &str) -> Option<ColStats> {
    let grid = full_grid(app)?;
    let idx = col_index_by_name(&grid.columns, name)?;
    Some(col_stats(&grid, idx, COL_STATS_SCAN_LIMIT))
}

/// R66: the stats pane's lines, clipped to `width`. An unknown column (no data
/// loaded, or a metadata column absent from the page) degrades to an explicit
/// hint instead of a blank pane. `show_spark` (R72) appends the value
/// distribution sparkline beside the distinct count; the caller drops it on a
/// narrow terminal.
pub(crate) fn col_stats_lines(
    name: &str,
    stats: Option<&ColStats>,
    width: usize,
    show_spark: bool,
) -> Vec<Line<'static>> {
    let title = if name.is_empty() {
        t("值分布").to_string()
    } else {
        tf("值分布 · {}", &[&name])
    };
    let mut out: Vec<Line<'static>> = vec![Line::from(Span::styled(
        truncate_disp(&title, width),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    match stats {
        None => out.push(Line::from(Span::styled(
            truncate_disp(t("打开表数据后可用"), width),
            Style::default().fg(Color::DarkGray),
        ))),
        Some(s) => {
            let counts = tf(
                "非空 {} · 空 {} · 去重 {}",
                &[&s.non_null, &s.nulls, &s.distinct],
            );
            let spark = if show_spark { s.spark.as_str() } else { "" };
            if spark.is_empty() {
                out.push(Line::from(truncate_disp(&counts, width)));
            } else {
                // Keep the counts readable: clip them to leave room for the
                // fixed-width sparkline plus a two-cell gap.
                let room = width.saturating_sub(disp_width(spark) + 2).max(4);
                out.push(Line::from(vec![
                    Span::raw(truncate_disp(&counts, room)),
                    Span::raw("  "),
                    Span::styled(spark.to_string(), Style::default().fg(Color::Cyan)),
                ]));
            }
            if s.numeric {
                out.push(Line::from(truncate_disp(
                    &tf(
                        "min {} · max {} · avg {}",
                        &[
                            &fmt_stat_num(s.min),
                            &fmt_stat_num(s.max),
                            &fmt_stat_num(s.avg),
                        ],
                    ),
                    width,
                )));
            } else {
                out.push(Line::from(Span::styled(
                    truncate_disp(t("（非数值列）"), width),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if s.truncated {
                out.push(Line::from(Span::styled(
                    truncate_disp(&tf("（按前 {} 行统计）", &[&s.scanned]), width),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }
    out
}

/// Fixed widths for the popup table: the name and type columns are padded so the
/// rows line up, while the key / default / flags / comment tail flows after them
/// and is clipped last. A narrow terminal shrinks the type before the name (and
/// the default silently clips first), so the column name always stays readable.
pub(crate) struct ColPopupLayout {
    pub(crate) name_w: usize,
    pub(crate) type_w: usize,
    pub(crate) show_key: bool,
}

pub(crate) fn cols_popup_layout(rows: &[&ColPopupRow], width: usize) -> ColPopupLayout {
    let name_w = rows
        .iter()
        .map(|r| disp_width(&r.name))
        .max()
        .unwrap_or(4)
        .min((width / 3).max(6));
    let type_w = rows
        .iter()
        .map(|r| disp_width(&r.data_type))
        .max()
        .unwrap_or(0)
        .min((width / 4).max(4));
    ColPopupLayout {
        name_w,
        type_w,
        show_key: rows.iter().any(|r| !r.key.is_empty()),
    }
}

/// Pad `s` with spaces to `w` display columns, truncating when it is longer so a
/// wide CJK name never pushes the key column off the row.
pub(crate) fn pad_disp(s: &str, w: usize) -> String {
    let d = disp_width(s);
    if d >= w {
        truncate_disp(s, w)
    } else {
        format!("{s}{}", " ".repeat(w - d))
    }
}

/// One aligned popup line: `name  type  KEY  =default  NOT NULL  · comment`,
/// clipped to `width`. Empty columns collapse, so a query result stays a clean
/// single name column.
pub(crate) fn cols_popup_line(r: &ColPopupRow, l: &ColPopupLayout, width: usize) -> String {
    let mut s = pad_disp(&r.name, l.name_w);
    if l.type_w > 0 && !r.data_type.is_empty() {
        s.push_str("  ");
        s.push_str(&pad_disp(&r.data_type, l.type_w));
    }
    if l.show_key {
        s.push_str("  ");
        s.push_str(&pad_disp(r.key, 3));
    }
    if !r.default.is_empty() {
        s.push_str("  =");
        s.push_str(&r.default);
    }
    if !r.nullable {
        s.push_str("  NOT NULL");
    }
    if !r.comment.is_empty() {
        s.push_str("  · ");
        s.push_str(&r.comment);
    }
    truncate_disp(&s, width)
}

/// `/` inside the popup opens the column-name filter (prefilled with the active
/// needle, so a second `/` refines it).
pub(crate) fn open_cols_popup_filter(app: &mut App) {
    let mut ta = TextArea::from([app.cols_popup_needle.clone()]);
    ta.set_placeholder_text(t("过滤列名…"));
    ta.move_cursor(CursorMove::End);
    app.cols_popup_filter = Some(ta);
}

pub(crate) fn cols_popup_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The filter prompt is modal on top of the popup and owns the keyboard first.
    if app.cols_popup_filter.is_some() {
        cols_popup_filter_key(app, k);
        return;
    }
    // `j` / `k` / PgUp / PgDn move the highlighted row (the render keeps it on
    // screen), clamped to the filtered row count.
    let hits = cols_popup_hits(app).0;
    let last = hits.saturating_sub(1);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => {
            app.cols_popup_open = false;
            app.cols_popup_needle.clear();
            app.cols_popup_filter = None;
            app.flash(t("已关闭列结构").into());
        }
        KeyCode::Char('/') => open_cols_popup_filter(app),
        // R107: `D` is the same complete-DDL entry as the results pane's `D`,
        // acting on the current table. The column popup closes so the DDL popup
        // is the only modal surface.
        KeyCode::Char('D') if k.modifiers.is_empty() => {
            app.cols_popup_open = false;
            app.cols_popup_needle.clear();
            app.cols_popup_filter = None;
            open_ddl_popup(app, tx);
        }
        // R102: `n` edits the highlighted column's comment (the same confirm
        // pipeline as the table comment). Read-only / unsupported engines report
        // instead of opening the editor.
        KeyCode::Char('n') => open_column_comment_edit(app),
        // R100: `y` copies the open table's structure as Markdown (the popup's
        // action row). Pure cache: no query is issued.
        KeyCode::Char('y') | KeyCode::Char('Y') => copy_table_structure_markdown(app),
        // R65: Enter jumps the cell cursor to the highlighted column.
        KeyCode::Enter => cols_popup_jump(app),
        KeyCode::Up | KeyCode::Char('k') => {
            app.cols_popup_sel = app.cols_popup_sel.saturating_sub(1)
        }
        // `hits == 0` leaves `last == 0`, so the clamp parks on the empty state.
        KeyCode::Down | KeyCode::Char('j') => {
            app.cols_popup_sel = (app.cols_popup_sel + 1).min(last)
        }
        KeyCode::PageUp => app.cols_popup_sel = app.cols_popup_sel.saturating_sub(8),
        KeyCode::PageDown => app.cols_popup_sel = (app.cols_popup_sel + 8).min(last),
        _ => {}
    }
}

/// R65: map a `gc` popup column name onto the visible grid's column index. The
/// popup names are double-encoding-fixed, so the grid names are fixed before the
/// exact (then case-insensitive) comparison. Pure, so the popup→grid mapping is
/// unit-testable without a terminal.
pub(crate) fn col_index_by_name(columns: &[String], name: &str) -> Option<usize> {
    let want = name.to_lowercase();
    columns
        .iter()
        .position(|c| fix_double_encoding(c) == name)
        .or_else(|| {
            columns
                .iter()
                .position(|c| fix_double_encoding(c).to_lowercase() == want)
        })
}

/// R65: the highlighted column row of the `gc` popup, as an index into the
/// *filtered* list. `None` when the filter matches nothing.
pub(crate) fn cols_popup_selected(app: &App) -> Option<ColPopupRow> {
    cols_popup_rows(app)
        .into_iter()
        .filter(|r| cols_popup_matches(r, &app.cols_popup_needle))
        .nth(app.cols_popup_sel)
}

/// R65: Enter in the `gc` popup. The highlighted column is matched onto the
/// *visible* grid by name and the cell cursor jumps there; the popup closes so
/// the landing is visible. A column hidden by the column picker (or absent from
/// a bare query result) reports instead of jumping somewhere wrong.
pub(crate) fn cols_popup_jump(app: &mut App) {
    let Some(row) = cols_popup_selected(app) else {
        app.status = t("没有可跳转的列").into();
        return;
    };
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可跳转的列").into();
        return;
    };
    match col_index_by_name(&grid.columns, &row.name) {
        Some(i) => {
            app.col_cursor = i;
            app.poke_hbar();
            app.cols_popup_open = false;
            app.cols_popup_needle.clear();
            app.cols_popup_filter = None;
            app.status = tf("跳到第 {} 列 {}", &[&(i + 1), &row.name]);
        }
        None => {
            app.status = tf("列 {} 不在当前视图（可能已隐藏）", &[&row.name]);
        }
    }
}

/// Filter-as-you-type handler for the popup's column-name filter (mirrors the
/// result search: Enter keeps the needle, Esc clears it).
pub(crate) fn cols_popup_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.cols_popup_filter = None;
            let (hits, total) = cols_popup_hits(app);
            app.status = if app.cols_popup_needle.trim().is_empty() {
                t("列名过滤已清除").into()
            } else {
                tf(
                    "列名过滤「{}」· {}/{} 列",
                    &[&(app.cols_popup_needle), &(hits), &(total)],
                )
            };
        }
        KeyCode::Esc => {
            app.cols_popup_filter = None;
            app.cols_popup_needle.clear();
            app.flash(t("已清除列名过滤").into());
        }
        _ => {
            if let Some(t) = &mut app.cols_popup_filter {
                t.input(k);
            }
            app.cols_popup_needle = app
                .cols_popup_filter
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.cols_popup_scroll = 0;
            app.cols_popup_sel = 0;
            let (hits, total) = cols_popup_hits(app);
            app.status = if app.cols_popup_needle.trim().is_empty() {
                t("输入以过滤列名…").into()
            } else {
                tf(
                    "列名过滤「{}」· {}/{} 列",
                    &[&(app.cols_popup_needle), &(hits), &(total)],
                )
            };
        }
    }
}

pub(crate) fn toggle_col_visible(app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        return;
    };
    let Some(i) = app.col_picker_list.selected() else {
        return;
    };
    let Some(name) = grid.columns.get(i).cloned() else {
        return;
    };
    if app.col_hidden.remove(&name) {
        app.reapply_col_filter();
        app.persist_cols();
        app.status = tf("显示列 {} · 已记住", &[&(fix_double_encoding(&name))]);
    } else {
        let visible = grid
            .columns
            .iter()
            .filter(|c| !app.col_hidden.contains(c.as_str()))
            .count();
        if visible <= 1 {
            app.status = t("至少保留一列").into();
            return;
        }
        app.col_hidden.insert(name.clone());
        app.reapply_col_filter();
        app.persist_cols();
        app.status = tf("隐藏列 {} · 已记住", &[&(fix_double_encoding(&name))]);
    }
}

/// Ctrl-Shift-R — jump straight to one of the last five browsed tables.
pub(crate) fn open_recent_tables(app: &mut App) {
    if app.recent_tables.is_empty() {
        app.status = t("还没有浏览过表").into();
        return;
    }
    app.recent_open = true;
    app.recent_list.select(Some(0));
}

pub(crate) fn recent_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = app.recent_tables.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.recent_open = false;
            app.flash(t("已关闭最近表").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if n > 0 {
                let i = app
                    .recent_list
                    .selected()
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                app.recent_list.select(Some(i));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if n > 0 {
                let i = app
                    .recent_list
                    .selected()
                    .map(|i| (i + 1).min(n - 1))
                    .unwrap_or(0);
                app.recent_list.select(Some(i));
            }
        }
        // R63: toggle the panel between most-recent-first and name order. This
        // is a pure client-side re-sort of the same five rows; the cursor
        // returns to the top so the new first row is the highlighted one.
        KeyCode::Char('s') => {
            app.recent_sort = match app.recent_sort {
                RecentSort::Recent => RecentSort::Name,
                RecentSort::Name => RecentSort::Recent,
            };
            app.recent_list.select(Some(0));
            app.status = tf("排序：{}", &[&t(recent_sort_label(app.recent_sort))]);
        }
        KeyCode::Enter => {
            if let Some(i) = app.recent_list.selected() {
                if let Some(&real) = recent_order(app).get(i) {
                    open_recent(app, tx, real);
                }
            }
        }
        _ => {}
    }
}

// ── R65: in-data-view table switcher (`g b`) ──

/// R65: open the in-data-view table switcher (`g b`). It lists the current
/// database's tables from the cached sidebar metadata (never a query) with
/// type-to-filter; Enter opens the highlighted one's data view.
pub(crate) fn open_table_jump(app: &mut App) {
    if app.tables_all.is_empty() {
        app.status = t("还没有可切换的表").into();
        return;
    }
    app.table_jump_open = true;
    app.table_jump_needle.clear();
    app.table_jump_list.select(Some(0));
    table_jump_report(app);
}

/// R85: `g t` in the sidebar opens the same in-database table switcher the data
/// view's `g b` uses — the tree's own cached table list for the current database,
/// type-to-filter, Enter opens the highlighted table. A thin named entry point so
/// the sidebar has its own status hint while both surfaces share one overlay and
/// one keymap; the list comes from the sidebar's already-cached metadata, so it is
/// **zero queries** and a workspace `/` filter never hides a table from it.
pub(crate) fn open_tree_table_jump(app: &mut App) {
    if app.tables_all.is_empty() {
        app.status = t("当前库还没有可跳转的表").into();
        return;
    }
    open_table_jump(app);
}

/// The `g b` switcher's rows: the current database's tables (the cached
/// `tables_all`, so a sidebar `/` filter never hides a table here), ordered like
/// the sidebar and narrowed by the inline needle. Matching runs on the qualified
/// `schema.table` the sidebar draws, so `inv.` keeps a whole schema. Pure — it
/// reads the cache only and can never issue a query.
pub(crate) fn table_jump_rows(app: &App) -> Vec<(String, String)> {
    let needle = app.table_jump_needle.trim().to_lowercase();
    let schema = app.schema.clone();
    let mut list = app.tables_all.clone();
    sort_table_list(&mut list, app.table_sort);
    list.into_iter()
        .filter(|t| {
            needle.is_empty()
                || qualified_display(&schema, &t.name)
                    .to_lowercase()
                    .contains(&needle)
        })
        .map(|t| (t.name, t.table_type))
        .collect()
}

/// Move the switcher's highlight by `step` rows (forward or back), clamped to
/// the filtered list. Shared by the arrow / page keys.
pub(crate) fn table_jump_step(app: &mut App, step: usize, forward: bool) {
    let n = table_jump_rows(app).len();
    if n == 0 {
        app.table_jump_list.select(None);
        return;
    }
    let cur = app.table_jump_list.selected().unwrap_or(0).min(n - 1);
    let next = if forward {
        (cur + step).min(n - 1)
    } else {
        cur.saturating_sub(step)
    };
    app.table_jump_list.select(Some(next));
}

/// The `g b` keymap. `↑`/`↓` (and `j`/`k`, the ironclad vim-up rule every list
/// panel follows) move the highlight; every *other* printable character edits
/// the needle (filter as you type), exactly like the sidebar's own
/// type-to-filter list.
pub(crate) fn table_jump_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        // Ctrl-U clears the needle (grep / less muscle memory).
        if k.code == KeyCode::Char('u') {
            app.table_jump_needle.clear();
            app.table_jump_list.select(Some(0));
            app.status = tf("切换表 · {} 张", &[&(table_jump_rows(app).len())]);
        }
        return;
    }
    if k.modifiers.contains(KeyModifiers::ALT) {
        return;
    }
    match k.code {
        KeyCode::Esc => {
            app.table_jump_open = false;
            app.table_jump_needle.clear();
            app.flash(t("已关闭切换表").into());
        }
        KeyCode::Enter => table_jump_accept(app, tx),
        KeyCode::Up | KeyCode::Char('k') => table_jump_step(app, 1, false),
        KeyCode::Down | KeyCode::Char('j') => table_jump_step(app, 1, true),
        KeyCode::PageUp => table_jump_step(app, 8, false),
        KeyCode::PageDown => table_jump_step(app, 8, true),
        KeyCode::Backspace => {
            app.table_jump_needle.pop();
            app.table_jump_list.select(Some(0));
            table_jump_report(app);
        }
        KeyCode::Char(c) if !c.is_control() => {
            app.table_jump_needle.push(c);
            app.table_jump_list.select(Some(0));
            table_jump_report(app);
        }
        _ => {}
    }
}

/// Live hit count for the switcher's status line while the needle is typed.
pub(crate) fn table_jump_report(app: &mut App) {
    let hits = table_jump_rows(app).len();
    let total = app.tables_all.len();
    let needle = app.table_jump_needle.trim();
    app.status = if needle.is_empty() {
        tf("切换表 · {} 张 · 输入即过滤", &[&total])
    } else {
        tf("切换表「{}」· {}/{} 张", &[&needle, &hits, &total])
    };
}

/// Enter in the `g b` switcher: open the highlighted table in the data view.
/// Reuses the nav path, so the same database + schema case is a pure cached
/// jump (no reload, no query beyond the page fetch the data view always does).
pub(crate) fn table_jump_accept(app: &mut App, tx: &Tx) {
    let rows = table_jump_rows(app);
    let Some(i) = app.table_jump_list.selected() else {
        return;
    };
    let Some((table, _)) = rows.get(i).cloned() else {
        return;
    };
    app.table_jump_open = false;
    app.table_jump_needle.clear();
    let db = app.current_db();
    let schema = app.schema.clone();
    open_nav_table(app, tx, &db, &schema, &table, "");
}

/// R63: the recency panel's visible order as indices into `recent_tables`. The
/// canonical list is always recency-ordered; `Name` re-sorts a copy by
/// `schema.table` then database, case-insensitively.
pub(crate) fn recent_order(app: &App) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..app.recent_tables.len()).collect();
    if app.recent_sort == RecentSort::Name {
        idx.sort_by_key(|&i| {
            let (db, schema, table) = &app.recent_tables[i];
            (
                fix_double_encoding(&qualified_display(schema, table)).to_lowercase(),
                fix_double_encoding(db).to_lowercase(),
            )
        });
    }
    idx
}

/// R63: the status-bar / title label for a recency-panel ordering.
pub(crate) fn recent_sort_label(sort: RecentSort) -> &'static str {
    match sort {
        RecentSort::Recent => "按最近",
        RecentSort::Name => "按表名",
    }
}

/// Remember a table at the head of the recents list (max 5, unique) and push it
/// onto the browser-style back/forward history.
pub(crate) fn remember_recent_table(app: &mut App, db: &str, schema: &str, table: &str) {
    let entry = (db.to_string(), schema.to_string(), table.to_string());
    app.recent_tables.retain(|e| e != &entry);
    app.recent_tables.insert(0, entry.clone());
    app.recent_tables.truncate(5);
    record_nav(
        app,
        NavEntry::Table {
            db: db.to_string(),
            schema: schema.to_string(),
            table: table.to_string(),
        },
    );
}

/// R42: push a browsed Redis key value view onto the round-trip stack. Only the
/// list entry (db + key) is a node — a value *detail* view is not, so `Alt-←`
/// from a value lands on the key list, not a second value.
pub(crate) fn remember_redis_key(app: &mut App, db: u32, key_raw: &str, key_display: &str) {
    record_nav(
        app,
        NavEntry::RedisKey {
            db,
            key_raw: key_raw.to_string(),
            key_display: key_display.to_string(),
        },
    );
}
