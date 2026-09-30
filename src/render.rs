use crate::prelude::*;
use crate::*;

// ─── rendering ───────────────────────────────────────────────────────────────

pub(crate) fn ui(f: &mut Frame, app: &mut App) {
    let (w, h) = (f.area().width, f.area().height);
    app.layout_mode = layout_mode(w);
    app.term_h = h;
    app.term_w = w;
    app.rects = Rects::default();

    let header_h = if h < 14 { 0 } else { 1 };
    let status_h = if h < 10 { 0 } else { 1 };
    let footer_h = if h < 12 { 0 } else { 1 };

    let chunks = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Min(3),
        Constraint::Length(status_h),
        Constraint::Length(footer_h),
    ])
    .split(f.area());

    if header_h > 0 {
        render_header(f, chunks[0], app);
    }
    // R92: on the connection-list page (no active connection) reserve the top
    // row of the content area for the day's tip. Non-modal and one line: the
    // picker below is shifted down by that row, never covered by it.
    let mut content = chunks[1];
    if app.page == Page::Browse && app.picker_open && app.selected.is_none() && content.height > 2 {
        render_tip_bar(
            f,
            Rect {
                x: content.x,
                y: content.y,
                width: content.width,
                height: 1,
            },
            app,
        );
        content.y += 1;
        content.height -= 1;
    }
    match app.page {
        Page::Browse => render_browse(f, content, app),
        Page::NewConn => render_form(f, content, app),
    }
    if status_h > 0 {
        render_status(f, chunks[2], app);
    }
    if footer_h > 0 {
        render_footer(f, chunks[3], app);
    }

    if app.page == Page::Browse && app.picker_open && app.selected.is_none() {
        render_conn_picker(f, content, app);
    }
    // R83: the SQLite quick-open picker (`L`) draws over the base UI, under the
    // help layers (matching the key router, which checks help first).
    if app.sqlite_open.is_some() {
        render_sqlite_open(f, f.area(), app);
    }
    // Overlays are drawn lowest-precedence first so the topmost one on screen is
    // the one the key router actually owns (see `footer_ctx`, which lists the
    // same order).
    if app.cols_popup_open {
        render_cols_popup(f, f.area(), app);
    }
    // R75: the sidebar table-node info card (`i`) draws over the grid but under
    // the taller overlays below, matching its dispatch order.
    if app.table_info_open {
        render_table_info(f, f.area(), app);
    }
    if app.col_picker_open {
        render_col_picker(f, f.area(), app);
    }
    if app.recent_open {
        render_recent_tables(f, f.area(), app);
    }
    if app.conn_recent_open {
        render_conn_recent(f, f.area(), app);
    }
    if app.table_jump_open {
        render_table_jump(f, f.area(), app);
    }
    if app.history_open {
        // Confine the panel to the content area so the header, status line and
        // footer stay visible — `y`/`f`/`Del` feedback lands on the status line.
        render_history_panel(f, chunks[1], app);
    }
    if app.search_open {
        // Same content-area confinement as the history panel.
        render_search_panel(f, chunks[1], app);
    }
    if app.search_input.is_some() {
        render_search_input(f, f.area(), app);
    }
    if app.diff_picker.is_some() {
        render_diff_picker(f, f.area(), app);
    }
    if app.data_where.is_some() {
        render_data_where(f, f.area(), app);
    }
    if app.data_diff.is_some() {
        render_data_diff(f, chunks[1], app);
    }
    if app.transfer.as_ref().is_some_and(|w| w.prompt.is_some()) {
        render_transfer_prompt(f, f.area(), app);
    }
    if app.transfer.is_some() {
        render_transfer_wizard(f, chunks[1], app);
    }
    if app.transfer_report.is_some() {
        render_transfer_report(f, chunks[1], app);
    }
    if app.diff.is_some() {
        render_diff_panel(f, chunks[1], app);
    }
    if app.db_diff.is_some() {
        render_db_diff(f, chunks[1], app);
    }
    if app.file_load_plan.is_some() {
        render_file_load_plan(f, f.area(), app);
    }
    if app.file_load_prompt.is_some() {
        render_file_load_prompt(f, f.area(), app);
    }
    if app.table_prompt.is_some() {
        render_table_filter(f, f.area(), app);
    }
    if app.tree_search_prompt.is_some() {
        render_tree_search(f, f.area(), app);
    }
    if app.mongo_dialog.is_some() {
        render_mongo_dialog(f, f.area(), app);
    }
    if app.redis_prompt.is_some() {
        render_redis_prompt(f, f.area(), app);
    }
    if app.db_picker_open {
        render_db_picker(f, f.area(), app);
    }
    if app.snippet_open {
        render_snippets(f, f.area(), app);
    }
    if app.template_open {
        render_templates(f, f.area(), app);
    }
    if app.snippet_name.is_some() {
        render_snippet_name(f, f.area(), app);
    }
    // The completion popup sits just under the editor, over whatever is below.
    if app.completion.is_some() {
        render_completion(f, app);
    }
    if app.result_filter.is_some() {
        render_result_filter(f, f.area(), app);
    }
    // R64: the in-result cell-find prompt (`\`). The title carries the live hit
    // count so it is visible while typing.
    if app.cell_find_prompt.is_some() {
        let n = app.cell_find_hits.len();
        // A capped hit list is shown as `≥n` so the count never reads as exact.
        let shown = if app.cell_find_capped {
            format!("≥{n}")
        } else {
            n.to_string()
        };
        let title = if app.cell_find_needle.trim().is_empty() {
            t(" 查找单元格（当前页）· 大小写不敏感 · 纯客户端 ").to_string()
        } else if n == 0 {
            tf(
                " 查找「{}」· 无命中 · Esc 退出 ",
                &[&(app.cell_find_needle)],
            )
        } else {
            tf(
                " 查找「{}」· {} 命中 · Enter 跳转 · Esc 清除 ",
                &[&(app.cell_find_needle), &(shown)],
            )
        };
        let short = if app.cell_find_needle.trim().is_empty() || n == 0 {
            t(" 查找单元格 · Enter/Esc ").to_string()
        } else {
            tf(" 查找 {} 命中 · Enter ", &[&(shown)])
        };
        render_prompt_input(f, f.area(), app.cell_find_prompt.as_mut(), &title, &short);
    }
    // R56: the `gc` popup's column-name filter sits on top of the popup.
    if app.cols_popup_filter.is_some() {
        let (hits, total) = cols_popup_hits(app);
        render_prompt_input(
            f,
            f.area(),
            app.cols_popup_filter.as_mut(),
            &tf(
                " 过滤列名 {}/{} · Enter 保留 · Esc 清除 ",
                &[&(hits), &(total)],
            ),
            t(" 过滤列名 · Enter 保留 "),
        );
    }
    if app.col_filter_prompt.is_some() {
        let title = tf(
            " 列过滤「{}」· {} 行 · Enter 保留 · Esc 清除 ",
            &[&(col_filter_label(app)), &(result_row_count(app))],
        );
        render_prompt_input(
            f,
            f.area(),
            app.col_filter_prompt.as_mut(),
            &title,
            t(" 列过滤 · Enter/Esc "),
        );
    }
    if app.locate_prompt.is_some() {
        let hits = locate_hits(app).len();
        render_prompt_input(
            f,
            f.area(),
            app.locate_prompt.as_mut(),
            &tf(" 定位值 {} 命中 · Enter 跳转 · Esc 清除 ", &[&(hits)]),
            t(" 定位值 · Enter/Esc "),
        );
    }
    if app.col_jump.is_some() {
        render_prompt_input(
            f,
            f.area(),
            app.col_jump.as_mut(),
            t(" 跳列：列号 1-9 或列名前缀 · Enter 跳转 · Esc 取消 "),
            t(" 跳列 · Enter/Esc "),
        );
    }
    // R56: `:` row jump in the results pane. R83: a paginated table view shows
    // the whole-table total, since the number is an absolute row.
    if app.goto_prompt.is_some() {
        let n = goto_row_total(app);
        render_prompt_input(
            f,
            f.area(),
            app.goto_prompt.as_mut(),
            &tf(" 跳行 1-{} 或 $ · Enter 跳转 · Esc 取消 ", &[&(n)]),
            t(" 跳行 · Enter/Esc "),
        );
    }
    // R61: the editor find prompt (`Ctrl-F`) is a bottom-bar input. The title
    // carries the live `3/7` count so it is visible while typing.
    if app.editor_find.is_some() {
        let n = editor_find_hits(app.editor.lines(), &app.editor_find_needle).len();
        let i = app
            .editor_find_idx
            .map(|i| i + 1)
            .unwrap_or(1)
            .min(n.max(1));
        let title = if app.editor_find_needle.is_empty() {
            t(" 查找（编辑器）· 大小写不敏感 · 纯客户端 ").to_string()
        } else if n == 0 {
            tf(
                " 查找「{}」· 无命中 · Esc 退出 ",
                &[&(app.editor_find_needle)],
            )
        } else {
            tf(
                " 查找「{}」· {}/{} · Enter/F3/Alt-N 下一个 · Alt-B 上一个 ",
                &[&(app.editor_find_needle), &(i), &(n)],
            )
        };
        // The short title keeps the count too, so a 42-column phone never hides
        // it when the full title does not fit.
        let short = if app.editor_find_needle.is_empty() || n == 0 {
            t(" 查找 · Enter/Esc ").to_string()
        } else {
            tf(" 查找 {}/{} · Enter ", &[&(i), &(n)])
        };
        render_prompt_input(f, f.area(), app.editor_find.as_mut(), &title, &short);
    }
    // R82: the MongoDB document-grid prompts (`gf` field jump, `c` path copy).
    // Both are bottom-bar inputs and sit under the row popup.
    if app.mongo_field_prompt.is_some() {
        render_prompt_input(
            f,
            f.area(),
            app.mongo_field_prompt.as_mut(),
            t(" 字段跳转（已加载页）· Enter 跳转 · Esc 取消 "),
            t(" 字段跳转 · Enter/Esc "),
        );
    }
    if app.mongo_path_prompt.is_some() {
        render_prompt_input(
            f,
            f.area(),
            app.mongo_path_prompt.as_mut(),
            t(" 提取点路径 a.b.0.name · Enter 复制 · Esc 取消 "),
            t(" 路径提取 · Enter/Esc "),
        );
    }
    // The row popup draws first so a drilled cell popup sits on top of it.
    if app.row_popup.is_some() {
        render_row_popup(f, f.area(), app);
    }
    if let Some(popup) = app.cell_popup.as_ref() {
        let cache = &mut app.popup_cache;
        let (box_area, _inner, _max) = render_cell_popup(f, f.area(), popup, cache);
        app.rects.cell_popup = box_area;
    }
    if app.error_popup.is_some() {
        render_error_popup(f, f.area(), app);
    }
    if app.filter_prompt.is_some() {
        render_filter_prompt(f, f.area(), app);
    }
    if app.export_open {
        render_export(f, f.area(), app);
    }
    if app.export_path.is_some() {
        render_export_path(f, f.area(), app);
    }
    if app.conn_export.is_some() {
        render_conn_export(f, f.area(), app);
    }
    if app.conn_import_path.is_some() {
        render_conn_import_prompt(f, f.area(), app);
    }
    if app.conn_import_plan.is_some() {
        render_conn_import_plan(f, f.area(), app);
    }
    if app.import_prompt.is_some() {
        render_import_prompt(f, f.area(), app);
    }
    if app.import_plan.is_some() {
        render_import_plan(f, f.area(), app);
    }
    if app.import_report.is_some() {
        render_import_report(f, f.area(), app);
    }
    if app.help_open {
        render_help(f, f.area(), app);
    }
    if app.help_mini {
        render_help_mini(f, f.area(), app);
    }
    if app.edit_dialog.is_some() {
        render_edit_dialog(f, f.area(), app);
    }
    if let Some(confirm) = app.confirm.clone() {
        let (ok, cancel) = render_confirm(f, f.area(), &confirm);
        app.rects.confirm_ok = ok;
        app.rects.confirm_cancel = cancel;
    }
    if let Some(hc) = app.history_confirm.clone() {
        let (ok, cancel) = render_history_confirm(f, f.area(), &hc);
        app.rects.hist_ok = ok;
        app.rects.hist_cancel = cancel;
    }
    if app.ssh_prompt.is_some() {
        render_ssh_prompt(f, f.area(), app);
    }
    if app.mouse_debug {
        render_mouse_debug(f, f.area(), app);
    }
}

/// Live mouse-event readout for `DBXT_MOUSE_DEBUG`: a small floating panel drawn
/// last, over the normal UI, so a phone user can swipe and read what their
/// terminal encoded it as without the layout changing underneath.
pub(crate) fn render_mouse_debug(f: &mut Frame, area: Rect, app: &App) {
    if app.mouse_log.is_empty() || area.width < 30 || area.height < 8 {
        return;
    }
    let w = area.width.saturating_sub(4).min(78);
    let inner_w = w.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .mouse_log
        .iter()
        .map(|l| Line::from(truncate_disp(l, inner_w)))
        .collect();
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) - 2,
        y: area.y + 1,
        width: w,
        height: h,
    };
    let title = tf(
        " 鼠标事件 DBXT_MOUSE_DEBUG · 横滑={} ",
        &[&format!("{:?}", app.drag_pan)],
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(Span::styled(title, Style::default().fg(Color::Magenta))),
        ),
        rect,
    );
}

pub(crate) fn fit_status(msg: &str, width: usize) -> String {
    let n = disp_width(msg);
    if n <= width {
        return msg.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if msg.starts_with('✗') || msg.starts_with('✓') || msg.starts_with('⚠') {
        // 错误 / 成功确认 / 警告：关键信息在前，保留头部，尾部截断
        truncate_disp(msg, width)
    } else {
        // 普通消息：进度类根因常在尾部，保留尾部。
        // Keep the last `width - 1` *display cells* (not chars) so a CJK
        // message is not over-skipped into an empty `…` on a narrow screen.
        let budget = width - 1;
        let mut tail = String::new();
        let mut used = 0usize;
        for c in msg.chars().rev() {
            let cw = disp_width(&c.to_string());
            if used + cw > budget {
                break;
            }
            used += cw;
            tail.insert(0, c);
        }
        format!("…{tail}")
    }
}

pub(crate) fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let conn = app
        .selected
        .as_ref()
        .map(|c| format!("{} ({})", c.name, c.db_type.as_str()))
        .unwrap_or_else(|| t("未连接").into());
    // The current connection carries its own colour in the title bar, matching
    // the sidebar / picker (the name stays fully readable either way).
    let conn_style = app
        .selected
        .as_ref()
        .map(|c| {
            Style::default()
                .fg(connection_color(c))
                .add_modifier(Modifier::BOLD)
        })
        .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD));
    let db = if app.selected.is_some() {
        if app.backend_kind == Backend::Redis {
            format!(" · db:{}", app.redis_db)
        } else if !app.current_db().is_empty() {
            let schema = if app.schema.is_empty() {
                String::new()
            } else {
                format!(".{}", fix_double_encoding(&app.schema))
            };
            format!(" · db:{}{schema}", fix_double_encoding(&app.current_db()))
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let mode = match app.backend_kind {
        Backend::Sql => "",
        Backend::Redis => " · redis",
        Backend::Mongo => " · mongo",
    };
    let spinner = if app.loading {
        // Show elapsed seconds once a call is slow enough to be worth noticing,
        // so a long query reads as "working" rather than "hung".
        let secs = app
            .loading_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if secs >= 3 {
            format!(" {} {}s", spinner_frame(app.spinner), secs)
        } else {
            format!(" {}", spinner_frame(app.spinner))
        }
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(
            " dbxt ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(conn, conn_style),
        Span::styled(db, Style::default().fg(Color::Cyan)),
        Span::styled(mode, Style::default().fg(Color::Magenta)),
        Span::styled(spinner, Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// R92: the one-line "Tip of the day" bar shown atop the connection-list page
/// (no active connection). Muted, non-modal, and cut to a single row so a narrow
/// terminal never wraps it into two lines. The trailing `T 换一条` names the
/// rotation key when there is room; on a very narrow screen only the tip text is
/// shown (the footer still carries the key).
pub(crate) fn render_tip_bar(f: &mut Frame, area: Rect, app: &App) {
    let w = area.width as usize;
    if w == 0 || area.height == 0 {
        return;
    }
    let tip = ui_text::tip(app.tip_idx);
    let label = format!(" {} ", t("今日 Tip"));
    let tail = format!("  ·  {} ", t("T 换一条"));
    let label_w = disp_width(&label);
    let tail_w = disp_width(&tail);
    let label_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span<'static>> = Vec::new();
    // Reserve the trailing key hint only when a useful slice of the tip still
    // fits; otherwise drop it rather than clip the sentence mid-word.
    if w >= label_w + tail_w + 6 {
        spans.push(Span::styled(label, label_style));
        spans.push(Span::styled(
            truncate_disp(tip, w - label_w - tail_w),
            text_style,
        ));
        spans.push(Span::styled(tail, text_style));
    } else if w >= label_w + 4 {
        spans.push(Span::styled(label, label_style));
        spans.push(Span::styled(truncate_disp(tip, w - label_w), text_style));
    } else {
        spans.push(Span::styled(truncate_disp(tip, w), text_style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn spinner_frame(i: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[i % FRAMES.len()]
}

/// Right-hand section of the status bar: context about the current result set.
pub(crate) fn context_info(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    // R61: an active editor find leads the context block so its `3/7` count
    // survives on a 42-column status bar (the block is truncated from the tail).
    if !app.editor_find_needle.is_empty() {
        let n = editor_find_hits(app.editor.lines(), &app.editor_find_needle).len();
        if n > 0 {
            let i = app.editor_find_idx.map(|i| i + 1).unwrap_or(1).min(n);
            parts.push(tf("查找 {}/{}", &[&(i), &(n)]));
        }
    }
    // A back/forward landing goes first so even a 42-column status bar shows it
    // (the block is truncated from the tail, not the head).
    if let Some(hint) = &app.nav_landing {
        parts.push(hint.clone());
    }
    // R91: the pinned reference row's offset rides near the head of the block so
    // it survives the tail truncation on a narrow status bar. The first
    // differing column is named only when both rows are in the loaded window.
    if let Some((delta, col)) = ref_offset(app) {
        let mut s = if delta == 0 {
            "Δ0".to_string()
        } else if delta > 0 {
            format!("Δ+{delta}")
        } else {
            format!("Δ{delta}")
        };
        if let Some(col) = col {
            s.push_str(&format!(
                " ({})",
                truncate_disp(&fix_double_encoding(&col), 16)
            ));
        }
        parts.push(s);
    }
    // R65: where the open data view lives (`db.table`, table alone when narrow).
    // The connection name leads the left block, so only the location is added
    // here. Sits ahead of the other persistent fields so the identity survives
    // the status bar's tail truncation on a narrow terminal.
    if let Some(ps) = &app.page_state {
        if let Some(label) = session_label(&app.current_db(), &ps.schema, &ps.table, app.term_w) {
            parts.push(label);
        }
    }
    // R63: the connect-time latency is the next-most-useful connection fact and
    // only a few cells wide, so it sits here — before the wide fields — and
    // survives the tail truncation on a 42-column status bar. Absent when the
    // probe failed, so a failure is silent (no error, no second query).
    if let Some(d) = app
        .selected
        .as_ref()
        .and_then(|c| app.server_rtts.get(&c.id))
    {
        parts.push(tf("延迟 {}", &[&format_rtt(*d)]));
    }
    // Mobile efficiency markers go next: on a phone the status bar is narrow,
    // and whether the wide table now fits is the single most useful fact.
    let mut fits: Option<usize> = None;
    if let Some(grid) = &app.grid {
        if app.grid_kind != GridKind::Columns && !grid.columns.is_empty() {
            let ncols = grid.columns.len();
            if app.pinned_grid_cols().len() + app.vis_cols.max(1) >= ncols {
                fits = Some(ncols);
            }
        }
    }
    if let Some(ncols) = fits {
        parts.push(tf("全部 {} 列已适配", &[&(ncols)]));
    }
    if compact_active(app.compact, app.layout_mode) {
        parts.push(if app.compact.is_none() {
            t("紧凑列").into()
        } else {
            t("紧凑列 手动").into()
        });
    }
    // R52: the active connection's server version, read once at connect. A free
    // environment fact for when the server is not what you assumed. Hidden below
    // 56 columns (the same rule as the sidebar size column), so a phone status
    // bar keeps its row / column readout. `term_w == 0` (tests) counts as wide.
    if app.term_w == 0 || app.term_w >= 56 {
        if let Some(v) = app
            .selected
            .as_ref()
            .and_then(|c| app.server_versions.get(&c.id))
        {
            parts.push(tf("服务器 {}", &[&v]));
        }
    }
    // R52: statement ledger for a multi-statement editor, so Alt-↓ / Alt-↑ has a
    // persistent anchor. Parsed only while the editor is focused, and reported
    // only for ≥2 statements (a single statement needs no position).
    if app.focus == Focus::Editor {
        let text = app.editor_sql();
        let ranges = statement_ranges(&text);
        if ranges.len() > 1 {
            let (row, col) = app.editor.cursor();
            let off = text_offset(&text, row, col).unwrap_or(0);
            let cur = statement_index_at(&ranges, off).unwrap_or(0);
            parts.push(tf("语句 {}/{}", &[&(cur + 1), &(ranges.len())]));
        }
    }
    if !app.col_hidden.is_empty() {
        parts.push(tf("隐藏列 {}", &[&(app.col_hidden.len())]));
    }
    // When the grid columns are clipped, name the focused column and give its
    // absolute position (`列 1|column_3 4/8`). This is the single column readout:
    // it used to be split between a narrow-only name line and an always-on
    // scroll-window line, which duplicated `列 …` on a narrow screen. The pin
    // prefix (`1|`) is kept so a frozen prefix stays visible.
    if app.grid_kind != GridKind::Columns {
        if let Some(grid) = &app.grid {
            let ncols = grid.columns.len();
            if ncols > 0 && app.pinned_grid_cols().len() + app.vis_cols.max(1) < ncols {
                if let Some(name) = grid.columns.get(app.col_cursor) {
                    let pin = frozen_label(&app.pinned_grid_cols());
                    parts.push(tf(
                        "列 {}{} {}/{}",
                        &[
                            &pin,
                            &truncate_disp(&fix_double_encoding(name), 12),
                            &(app.col_cursor + 1),
                            &ncols,
                        ],
                    ));
                }
            }
        }
    }
    // Redis multi-select count (batch operations target the selection).
    if app.backend_kind == Backend::Redis && !app.redis_selected.is_empty() {
        parts.push(tf("已选 {}", &[&(app.redis_selected.len())]));
    }
    // Touch fallback: vertical wheel pans columns (Ctrl-G).
    if app.pan_mode {
        parts.push(t("横滚 开").into());
    }
    match app.focus {
        Focus::Sidebar => parts.push(t("焦点 侧栏").into()),
        Focus::Editor => parts.push(t("焦点 SQL").into()),
        Focus::CmdInput => parts.push(t("焦点 命令").into()),
        Focus::Preview => parts.push(t("焦点 结果").into()),
    }
    // Small marker for the responsive-collapse master switch (Ctrl-A).
    parts.push(if app.auto_collapse {
        t("自动折叠 开").into()
    } else {
        t("自动折叠 关").into()
    });
    // Result tabs (queries only).
    if app.result_tabs.len() > 1 && app.grid_kind != GridKind::TableData {
        parts.push(tf(
            "结果 {}/{}",
            &[&(app.result_tab + 1), &(app.result_tabs.len())],
        ));
    }
    // Live event readout while `DBXT_EVENT_TRACE` is set, so a user can report
    // exactly which events their terminal sends for a swipe. Pushed last so the
    // page / row / column readout survives the status-bar truncation (the event
    // text is the first thing that can go); with the `DBXT_MOUSE_DEBUG` panel on
    // screen the status bar keeps its width instead.
    let event_part = if app.trace_path.is_some() && !app.mouse_debug {
        app.last_event.as_ref().map(|ev| tf("事件 {}", &[&(ev)]))
    } else {
        None
    };
    if let Some(ps) = &app.page_state {
        // A lower-bound total cannot give an exact page count.
        let pages = match ps.total {
            Some(t) if !ps.total_lower_bound => page_count(t, ps.page_size).to_string(),
            _ => "?".into(),
        };
        parts.push(tf("第 {}/{} 页", &[&(ps.page + 1), &(pages)]));
    }
    let n = result_row_count(app);
    if n > 0 {
        let cur = cursor_abs_row(app);
        let total_abs = app
            .page_state
            .as_ref()
            .filter(|ps| !ps.total_lower_bound)
            .and_then(|ps| ps.total)
            .map(|t| t as usize)
            .unwrap_or(n);
        parts.push(tf("行 {}/{}", &[&(cur), &(total_abs)]));
    }
    // Horizontal position used to be a separate `列 1|3-8/21` readout here; it
    // is now folded into the single focused-column line above (R43).
    if !app.batch.is_empty() {
        parts.push(tf("批量 {} 待提交", &[&(app.batch.len())]));
    }
    if let Some(ev) = event_part {
        parts.push(ev);
    }
    parts.join(" · ")
}

pub(crate) fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let right = context_info(app);
    // While events are being traced the context block carries one extra field
    // (the last mouse/resize event), so let it use more of the line — otherwise
    // the row/column readout next to it would be the first thing cut off.
    let cap = if app.trace_path.is_some() && !app.mouse_debug {
        area.width.saturating_sub(24).max(area.width / 2)
    } else {
        area.width / 2
    };
    let right_w = (disp_width(&right) as u16 + 2).min(cap);
    let chunks = Layout::horizontal([Constraint::Min(10), Constraint::Length(right_w)]).split(area);
    let style = if app.status.starts_with('✗') {
        Style::default().fg(Color::Red)
    } else if app.status.starts_with('✓') {
        Style::default().fg(Color::Green)
    } else if app.status.starts_with('⚠') || app.loading {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    // A colour-coded connection badge leads the status line, so the active
    // connection is visible even while a long status message is truncated.
    // R54: a read-only connection carries its 🔒 here too (not only on the tree
    // root), so its write policy is visible while the editor is focused.
    let badge = app.selected.as_ref().map(|c| {
        let name = truncate_disp(&c.name, 18);
        let text = if c.read_only {
            format!("● 🔒 {name} ")
        } else {
            format!("● {name} ")
        };
        (text, connection_color(c))
    });
    let prefix_w = badge
        .as_ref()
        .map(|(t, _)| disp_width(t) as u16)
        .unwrap_or(0);
    let msg = fit_status(
        &app.status,
        chunks[0].width.saturating_sub(prefix_w) as usize,
    );
    let mut left: Vec<Span> = Vec::new();
    if let Some((text, color)) = badge {
        left.push(Span::styled(
            text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    left.push(Span::styled(msg, style));
    f.render_widget(Paragraph::new(Line::from(left)), chunks[0]);
    f.render_widget(
        Paragraph::new(truncate_disp(&right, chunks[1].width as usize))
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

/// One footer hint: the keycap plus what it does.
pub(crate) type Hint = (&'static str, &'static str);

/// Which surface currently owns the keyboard, from the footer's point of view.
/// Overlays take precedence over the page, exactly like the key router.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FooterView {
    Confirm,
    SshPrompt,
    EditDialog,
    HelpFilter,
    Help,
    HelpMini,
    /// R83: the `L` SQLite file quick-open picker.
    SqliteOpen,
    ImportReport,
    ImportPlan,
    ImportPrompt,
    FileLoadPrompt,
    FileLoadPlan,
    ExportPath,
    ExportPicker,
    ConnExport,
    ConnImportPrompt,
    ConnImportPlan,
    FilterPrompt,
    Popup,
    RowPopup,
    ErrorBox,
    ResultFilter,
    ColFilter,
    LocatePrompt,
    ColJump,
    /// R56: the `:` row-number jump prompt.
    GotoRow,
    /// R61: the editor find prompt (`Ctrl-F`).
    EditorFind,
    Completion,
    SnippetName,
    Snippets,
    /// R59: the `/` filter input over the favourites list.
    SnippetFilter,
    /// R59: the `d` delete confirmation over the favourites list.
    SnippetConfirm,
    /// R71: the built-in SQL template panel (`Alt-T` in the editor).
    Templates,
    /// R71: the `/` filter input over the template list.
    TemplateFilter,
    DbPicker,
    RedisPrompt,
    MongoDoc,
    /// R55: an in-place tree-row rename owns the keyboard.
    Rename,
    TablePrompt,
    HistoryFilter,
    History,
    SearchInput,
    Search,
    DiffPicker,
    SchemaDiff,
    DbDiff,
    DataDiff,
    DataWhere,
    TransferWizard,
    TransferConfirm,
    TransferRunning,
    TransferPrompt,
    TransferReport,
    Recent,
    /// R87: the session's recent-connection list (`Alt-Shift-H`).
    ConnRecent,
    /// R65: the in-data-view table switcher (`g b`).
    TableJump,
    ColPicker,
    /// R75: the sidebar table-node info card (`i`).
    TableInfo,
    ConnPicker,
    NewConn,
    RedisKeys,
    RedisValue,
    MongoDocs,
    Browse,
}

/// Everything the footer needs to know, so the group choice can be unit-tested
/// without building a whole `App`.
#[derive(Clone, Copy)]
pub(crate) struct FooterCtx {
    pub(crate) view: FooterView,
    pub(crate) focus: Focus,
    pub(crate) has_connection: bool,
}

/// Pick the footer group from the app state. The order mirrors the key router
/// exactly (`key` checks confirm / edit-dialog before `browse_key`, and
/// `browse_key` checks its overlays in the order below), so the footer can never
/// describe a different surface than the one the keyboard is actually on.
pub(crate) fn footer_ctx(app: &App) -> FooterCtx {
    footer_ctx_inner(app, true)
}

/// Build the footer context, optionally ignoring the help layers. The mini
/// cheat-sheet needs the context *under* itself (it is not a real surface), so
/// it calls this with `include_help = false` to learn what `?` was pressed over.
pub(crate) fn footer_ctx_inner(app: &App, include_help: bool) -> FooterCtx {
    let view = if app.confirm.is_some() {
        FooterView::Confirm
    } else if app.ssh_prompt.is_some() {
        FooterView::SshPrompt
    } else if app.edit_dialog.is_some() {
        FooterView::EditDialog
    } else if app.history_confirm.is_some() {
        FooterView::Confirm
    } else if app.rename_edit.is_some() {
        FooterView::Rename
    } else if include_help && app.help_open && app.help_filter.is_some() {
        FooterView::HelpFilter
    } else if include_help && app.help_open {
        FooterView::Help
    } else if include_help && app.help_mini {
        FooterView::HelpMini
    } else if app.sqlite_open.is_some() {
        FooterView::SqliteOpen
    } else if app.import_report.is_some() {
        FooterView::ImportReport
    } else if app.import_plan.is_some() {
        FooterView::ImportPlan
    } else if app.import_prompt.is_some() {
        FooterView::ImportPrompt
    } else if app.file_load_prompt.is_some() {
        FooterView::FileLoadPrompt
    } else if app.file_load_plan.is_some() {
        FooterView::FileLoadPlan
    } else if app.export_path.is_some() {
        FooterView::ExportPath
    } else if app.export_open {
        FooterView::ExportPicker
    } else if app.conn_export.is_some() {
        FooterView::ConnExport
    } else if app.conn_import_path.is_some() {
        FooterView::ConnImportPrompt
    } else if app.conn_import_plan.is_some() {
        FooterView::ConnImportPlan
    } else if app.filter_prompt.is_some() {
        FooterView::FilterPrompt
    } else if app.error_popup.is_some() {
        FooterView::ErrorBox
    } else if app.row_popup.is_some() && app.cell_popup.is_none() {
        FooterView::RowPopup
    } else if app.row_popup.is_some() || app.cell_popup.is_some() {
        FooterView::Popup
    } else if app.result_filter.is_some() {
        FooterView::ResultFilter
    } else if app.col_filter_prompt.is_some() {
        FooterView::ColFilter
    } else if app.locate_prompt.is_some() {
        FooterView::LocatePrompt
    } else if app.col_jump.is_some() {
        FooterView::ColJump
    } else if app.goto_prompt.is_some() {
        FooterView::GotoRow
    } else if app.editor_find.is_some() {
        FooterView::EditorFind
    } else if app.completion.is_some() {
        FooterView::Completion
    } else if app.snippet_name.is_some() {
        FooterView::SnippetName
    } else if app.snippet_filter.is_some() {
        FooterView::SnippetFilter
    } else if app.snippet_confirm.is_some() {
        FooterView::SnippetConfirm
    } else if app.snippet_open {
        FooterView::Snippets
    } else if app.template_filter.is_some() {
        FooterView::TemplateFilter
    } else if app.template_open {
        FooterView::Templates
    } else if app.db_picker_open {
        FooterView::DbPicker
    } else if app.redis_filter_prompt.is_some() {
        FooterView::TablePrompt
    } else if app.redis_prompt.is_some() {
        FooterView::RedisPrompt
    } else if app.mongo_dialog.is_some() {
        FooterView::MongoDoc
    } else if app.table_prompt.is_some() {
        FooterView::TablePrompt
    } else if app.tree_search_prompt.is_some() {
        FooterView::LocatePrompt
    } else if app.history_filter.is_some() {
        FooterView::HistoryFilter
    } else if app.history_open {
        FooterView::History
    } else if app.search_input.is_some() {
        FooterView::SearchInput
    } else if app.search_open {
        FooterView::Search
    } else if app.data_where.is_some() {
        FooterView::DataWhere
    } else if app.transfer.as_ref().is_some_and(|w| w.prompt.is_some()) {
        FooterView::TransferPrompt
    } else if app.transfer.as_ref().is_some_and(|w| w.submitted) {
        FooterView::TransferRunning
    } else if app
        .transfer
        .as_ref()
        .is_some_and(|w| w.step == TransferStep::Confirm)
    {
        FooterView::TransferConfirm
    } else if app.transfer.is_some() {
        FooterView::TransferWizard
    } else if app.transfer_report.is_some() {
        FooterView::TransferReport
    } else if app.diff_picker.is_some() {
        FooterView::DiffPicker
    } else if app.data_diff.is_some() {
        FooterView::DataDiff
    } else if app.diff.is_some() {
        FooterView::SchemaDiff
    } else if app.db_diff.is_some() {
        FooterView::DbDiff
    } else if app.recent_open {
        FooterView::Recent
    } else if app.conn_recent_open {
        FooterView::ConnRecent
    } else if app.table_jump_open {
        FooterView::TableJump
    } else if app.col_picker_open {
        FooterView::ColPicker
    } else if app.table_info_open {
        FooterView::TableInfo
    } else if app.page == Page::NewConn {
        FooterView::NewConn
    } else if app.picker_open && app.selected.is_none() {
        FooterView::ConnPicker
    } else if app.backend_kind == Backend::Redis
        && app.selected.is_some()
        && app.focus == Focus::Sidebar
    {
        FooterView::RedisKeys
    } else if app.backend_kind == Backend::Redis && app.grid_kind == GridKind::RedisValue {
        FooterView::RedisValue
    } else if app.backend_kind == Backend::Mongo
        && app.grid_kind == GridKind::MongoDocs
        && app.focus == Focus::Preview
    {
        FooterView::MongoDocs
    } else {
        FooterView::Browse
    };
    FooterCtx {
        view,
        focus: app.focus,
        has_connection: app.selected.is_some(),
    }
}

/// Build the footer hint list for a context, most relevant first. The last entry
/// is always the pinned `?` help key, so the escape hatch can never be dropped;
/// rendering trims lower-priority hints when the line is narrow.
pub(crate) fn footer_hints_ctx(ctx: FooterCtx) -> Vec<Hint> {
    let mut v: Vec<Hint> = match ctx.view {
        FooterView::HelpFilter => vec![("Enter", t("保留")), ("Esc", t("清除"))],
        FooterView::Help => vec![("/", t("过滤")), ("↑↓", t("滚动")), ("Esc", t("关闭"))],
        FooterView::HelpMini => vec![("Enter/?", t("全部键位")), ("Esc", t("关闭"))],
        // R83: the SQLite quick-open picker.
        FooterView::SqliteOpen => vec![
            ("↑↓", t("选择")),
            ("Enter", t("打开")),
            ("Tab", t("补全")),
            ("Del", t("移除最近")),
            ("Esc", t("取消")),
        ],
        FooterView::Rename => vec![
            ("Enter", t("保存")),
            ("Esc", t("取消")),
            ("Ctrl-U", t("清空")),
        ],
        // R20–R22 overlays: import / export / Redis input dialogs. Without
        // these arms the footer fell through to the page's group while an
        // overlay owned the keyboard.
        FooterView::ImportPrompt => vec![("Enter", t("预览")), ("Esc", t("取消"))],
        FooterView::FileLoadPrompt => vec![("Enter", t("预览")), ("Esc", t("取消"))],
        FooterView::FileLoadPlan => vec![
            ("Enter", t("执行")),
            ("e", t("转编辑器")),
            ("Esc", t("取消")),
        ],
        FooterView::ImportPlan => vec![
            ("Enter", t("导入")),
            ("m", t("追加/覆盖")),
            ("s", t("出错处理")),
            ("↑↓", t("滚动")),
            ("Esc", t("取消")),
        ],
        FooterView::ImportReport => vec![("Enter/Esc", t("关闭"))],
        FooterView::ExportPicker => vec![
            ("↑↓", t("选择")),
            ("Enter", t("确定")),
            ("1-6", t("快选")),
            ("Esc", t("取消")),
        ],
        FooterView::ExportPath => vec![("Enter", t("导出")), ("Esc", t("取消"))],
        FooterView::ConnExport => vec![
            ("Enter", t("导出")),
            ("y", t("复制 JSON")),
            ("p", t("含密码")),
            ("i", t("导入")),
            ("e", t("路径")),
            ("Esc", t("取消")),
        ],
        FooterView::ConnImportPrompt => vec![("Enter", t("预览")), ("Esc", t("取消"))],
        FooterView::ConnImportPlan => vec![
            ("↑↓", t("选择")),
            ("Space", t("勾选")),
            ("s/r/b", t("跳过/覆盖/都存")),
            ("d", t("逐条")),
            ("Enter", t("导入")),
            ("Esc", t("取消")),
        ],
        FooterView::RedisPrompt => vec![("Enter", t("确认")), ("Esc", t("取消"))],
        FooterView::TablePrompt | FooterView::ResultFilter | FooterView::ColFilter => {
            vec![("Enter", t("保留")), ("Esc", t("清除"))]
        }
        FooterView::LocatePrompt => vec![("Enter", t("跳到命中")), ("Esc", t("清除"))],
        FooterView::ColJump => vec![("Enter", t("跳列")), ("Esc", t("取消"))],
        FooterView::GotoRow => vec![("Enter", t("跳行")), ("Esc", t("取消"))],
        FooterView::EditorFind => vec![
            ("Enter/F3", t("下一个")),
            ("⇧Enter", t("上一个")),
            ("Esc", t("退出保留高亮")),
        ],
        FooterView::HistoryFilter => vec![("Enter", t("保留")), ("Esc", t("清除"))],
        FooterView::History => vec![
            ("↑↓", t("选择")),
            ("Enter", t("回填编辑器")),
            ("Ctrl-↵", t("直跑")),
            ("f", t("收藏")),
            ("y/Y", t("复制语句")),
            ("Del", t("删除")),
            ("/", t("搜索")),
            ("Esc", t("关闭")),
        ],
        FooterView::SearchInput => vec![("Enter", t("搜索")), ("Esc", t("取消"))],
        FooterView::Search => vec![
            ("↑↓", t("选择")),
            ("Enter", t("定位")),
            ("y", t("复制命中")),
            ("r", t("重搜")),
            ("Esc", t("中止/关闭")),
        ],
        FooterView::DiffPicker => vec![
            ("↑↓", t("选择")),
            ("Enter", t("对比")),
            ("m", t("结构/数据")),
            ("d", t("表/库")),
            ("Esc", t("取消")),
        ],
        FooterView::SchemaDiff => vec![
            ("Tab", t("切换")),
            ("y", t("摘要")),
            ("g", t("ALTER")),
            ("↑↓", t("滚动")),
            ("Esc", t("关闭")),
        ],
        FooterView::DbDiff => vec![
            ("↑↓", t("选择")),
            ("Enter", t("对比两库同有表")),
            ("Esc", t("关闭")),
        ],
        FooterView::DataDiff => vec![
            ("Tab", t("切换")),
            ("Enter", t("展开")),
            ("n/p", t("差异行")),
            ("y/Y", t("摘要/CSV")),
            ("^E", t("导出")),
            ("g", t("同步 SQL")),
            ("Esc", t("关闭")),
        ],
        FooterView::DataWhere => vec![("Enter", t("开始对比")), ("Esc", t("取消"))],
        FooterView::TransferWizard => vec![
            ("↑↓", t("选择")),
            ("Enter", t("下一步/切换")),
            ("m", t("模式")),
            ("w/l", t("WHERE/LIMIT")),
            ("Esc", t("取消")),
        ],
        FooterView::TransferConfirm => vec![("Enter", t("确认覆盖")), ("Esc", t("返回"))],
        FooterView::TransferRunning => vec![("Esc", t("中止搬运"))],
        FooterView::TransferPrompt => vec![("Enter", t("确定")), ("Esc", t("取消"))],
        FooterView::TransferReport => vec![
            ("g", t("复制摘要")),
            ("b", t("浏览目标表")),
            ("Esc", t("关闭")),
        ],
        FooterView::Recent => vec![("↑↓", t("选择")), ("Enter", t("直达")), ("Esc", t("关闭"))],
        FooterView::ConnRecent => vec![("↑↓", t("选择")), ("Enter", t("直连")), ("Esc", t("关闭"))],
        FooterView::TableJump => vec![
            ("a-z", t("过滤")),
            ("↑↓", t("选择")),
            ("Enter", t("切换表")),
            ("Esc", t("关闭")),
        ],
        FooterView::ColPicker => vec![
            ("Space", t("勾选")),
            ("a", t("全选")),
            ("x", t("仅首列")),
            ("Esc", t("关闭")),
        ],
        FooterView::TableInfo => vec![("↑↓", t("滚动")), ("Esc", t("关闭"))],
        FooterView::Completion => vec![
            ("↑↓", t("选择")),
            ("Tab/Enter", t("上屏")),
            ("Esc", t("取消")),
        ],
        FooterView::SnippetName => vec![("Enter", t("保存")), ("Esc", t("取消"))],
        FooterView::SnippetFilter => vec![("Enter", t("保留")), ("Esc", t("清除"))],
        FooterView::SnippetConfirm => {
            vec![("Enter", t("删除")), ("Esc", t("取消"))]
        }
        FooterView::Snippets => vec![
            ("↑↓", t("选择")),
            ("Enter", t("插入编辑器")),
            ("/", t("过滤")),
            ("d", t("删除")),
            ("s", t("收藏")),
            ("r", t("刷新")),
            ("Esc", t("关闭")),
        ],
        FooterView::Templates => vec![
            ("↑↓", t("选择")),
            ("Enter", t("插入编辑器")),
            ("/", t("过滤")),
            ("Esc", t("关闭")),
        ],
        FooterView::TemplateFilter => vec![("Enter", t("保留")), ("Esc", t("清除"))],
        FooterView::FilterPrompt => {
            vec![("Enter", t("应用")), ("Esc", t("取消")), ("⏎", t("清除"))]
        }
        FooterView::Popup => vec![
            ("↑↓", t("滚动")),
            ("J", t("JSON 美化")),
            ("U", t("Unicode")),
            ("y/Y", t("复制原值")),
            ("Esc/Enter", t("关闭")),
        ],
        FooterView::RowPopup => vec![
            ("↑↓/n p", t("移动")),
            ("Enter/v", t("看值")),
            ("y/Y", t("复制值")),
            ("/", t("过滤名/值")),
            ("Esc", t("关闭")),
        ],
        FooterView::ErrorBox => vec![
            ("Enter", t("看全量")),
            ("↑↓", t("滚动")),
            ("Esc", t("关闭")),
        ],
        FooterView::EditDialog => vec![
            ("Enter", t("执行")),
            ("Esc", t("取消")),
            ("Ctrl-V", t("转编辑器")),
            ("Ctrl-T", t("加入批量")),
        ],
        FooterView::MongoDoc => vec![("Ctrl-S", t("校验并保存")), ("Esc", t("取消"))],
        FooterView::DbPicker => vec![
            ("↑↓", t("选择")),
            ("Enter", t("切换")),
            ("r", t("刷新")),
            ("Esc", t("关闭")),
        ],
        FooterView::Confirm => vec![("Enter/y", t("执行")), ("Esc/n", t("取消"))],
        FooterView::SshPrompt => vec![
            ("y/Enter", t("接受并记住")),
            ("s", t("仅本次")),
            ("n/Esc", t("拒绝")),
        ],
        FooterView::ConnPicker => vec![
            ("↑↓", t("选择连接")),
            ("Enter", t("连接")),
            ("L", t("打开 SQLite")),
            ("Alt-1..9", t("直切")),
            ("T", t("换一条")),
            ("c", t("新建")),
            ("e", t("编辑")),
            ("p", t("复制")),
            ("P", t("探测")),
            ("s", t("排序")),
            ("x", t("删除")),
            ("q", t("显隐")),
        ],
        FooterView::NewConn => vec![
            ("↑↓/Tab", t("字段")),
            ("Enter", t("编辑/切换/保存")),
            ("Space", t("切换")),
            ("Esc", t("返回")),
        ],
        FooterView::RedisKeys => vec![
            ("↑↓", t("key")),
            ("a-z", t("过滤")),
            ("Alt+a-z", t("首字母跳")),
            ("Space", t("勾选")),
            ("a", t("全选")),
            ("Enter", t("查看值")),
            ("Del", t("批量删")),
            ("x", t("批量TTL")),
            ("m", t("批量改名")),
            ("T", t("设 TTL")),
            ("t", t("类型过滤")),
            ("Ctrl-T", t("TTL 排序")),
            ("/", t("匹配模式")),
            ("n", t("更多")),
            ("r", t("重扫")),
            ("d", t("逻辑库")),
            ("1-9", t("直跳")),
            ("Tab", t("命令台")),
        ],
        FooterView::RedisValue => vec![
            ("↑↓", t("行")),
            ("←→", t("列")),
            ("Enter", t("整行")),
            ("v", t("单元格")),
            ("y", t("复制值")),
            ("e", t("编辑")),
            ("x", t("TTL")),
            ("m", t("重命名")),
            ("n", t("更多")),
            ("Del", t("删 key")),
            ("/", t("搜索")),
            ("Esc", t("返回列表")),
        ],
        FooterView::MongoDocs => vec![
            ("↑↓", t("行")),
            ("←→", t("列")),
            ("Enter", t("整行")),
            ("v", t("单元格")),
            ("y", t("复制 JSON")),
            ("gf", t("字段跳转")),
            ("c", t("路径提取")),
            ("Ctrl-S", t("大小排序")),
            ("e", t("编辑")),
            ("i", t("插入")),
            ("Del", t("删文档")),
            ("n/p", t("翻页")),
            ("f", t("JSON 过滤")),
            ("/", t("搜索")),
            ("Esc", t("返回列表")),
        ],
        FooterView::Browse => match ctx.focus {
            Focus::Sidebar if !ctx.has_connection => vec![
                ("↑↓", t("选择连接")),
                ("Enter", t("连接")),
                ("Alt-1..9", t("直切")),
                ("c", t("新建")),
                ("p", t("复制")),
                ("P", t("探测")),
                ("d", t("断开连接")),
                ("q", t("显隐")),
            ],
            Focus::Sidebar => vec![
                ("↑↓", t("树")),
                ("h l", t("折叠/展开")),
                ("a-z", t("过滤")),
                ("f", t("搜索")),
                ("Enter", t("浏览")),
                ("r", t("结构/改名")),
                ("⇧↑↓", t("移动")),
                ("s", t("排序")),
                ("x", t("断开连接")),
                ("Alt+a-z", t("首字母跳")),
                ("Alt-1..9", t("切连接")),
                // R85: `g t` jumps to a table in the current database.
                ("gt", t("跳表")),
                // R87: `P` health probe + the session's recent-connection list.
                ("P", t("探测")),
                ("Alt-⇧H", t("最近连接")),
                ("d", t("切库")),
                ("L", t("打开 SQLite")),
                ("Tab", t("SQL")),
                ("1-9", t("直跳")),
            ],
            Focus::Editor => vec![
                // R79: order is the narrow-screen priority. `Ctrl-J` (run) and
                // `Alt-/` (complete) are the two keys a small terminal must keep,
                // so they lead and survive the 42-column tier trimming.
                ("Ctrl-J", t("运行")),
                ("Alt-/", t("补全")),
                ("Alt-Enter", t("当前句")),
                // R71: the built-in SQL template panel. R79's auto-indent and
                // bracket auto-pair are `tui.json` switches, not keys, so they
                // stay documented in the full help only.
                ("Alt-T", t("模板")),
                ("Alt-P", t("片段")),
                ("Enter", t("换行")),
                ("↑↓", t("历史")),
                ("Tab", t("下一区")),
                ("%", t("配对括号")),
                // R88: F2 toggles the optional statement-ordinal gutter.
                ("F2", t("语句序号")),
                ("Esc", t("侧栏")),
            ],
            Focus::CmdInput => vec![
                ("Enter", t("执行")),
                ("[ ]", t("切库")),
                ("Ctrl-L", t("换模式")),
                ("Esc", t("编辑器")),
            ],
            Focus::Preview => vec![
                // R80: the navigation + lookup keys stay first (the footer
                // shows only the leading hints that fit, so crowding them out
                // with rare toggles would be a regression). The R51–R79 result
                // additions follow, ordered by how often they are reached.
                ("↑↓", t("行")),
                ("←→", t("列")),
                ("Enter", t("整行")),
                ("v", t("单元格")),
                ("e", t("编辑")),
                ("i", t("插入")),
                ("Del", t("删行")),
                ("y", t("复制INSERT")),
                ("f", t("过滤")),
                ("/", t("搜索")),
                ("\\", t("查找")),
                ("gv", t("定位值")),
                ("|", t("跳列")),
                (":", t("跳行")),
                // R91: pin the focused column / the focused row (placed early so
                // the mini cheat-sheet reaches them on a small screen).
                ("gf", t("冻结列")),
                ("gs", t("钉行")),
                // R80 additions: the R51–R79 keys that were missing here.
                // `v` already leads this group; the epoch preview it shows is
                // passive (no key), so it stays documented in the full help.
                ("F8/Alt-E", t("错误定位")),
                ("gc", t("值分布")),
                ("J", t("JSON 美化")),
                ("#", t("数字格式")),
                ("%", t("斑马纹")),
                ("0", t("复位列宽")),
                ("Alt-0", t("清列宽")),
                ("gd/gt", t("结构/数据")),
                ("gb", t("切换表")),
                // R85: content auto-fit column widths.
                ("gw", t("适配列宽")),
                ("gW", t("全列适配")),
                ("[ ]", t("切标签")),
                ("Alt-W", t("关标签")),
                ("Alt-O", t("语句耗时")),
            ],
        },
    };
    // The help key is the one hint that is never dropped; in the two text
    // entry contexts `?` is a literal character, so the pinned hint names F1
    // there (matching the R70 invocation key).
    v.push((footer_help_key(ctx.focus), t("帮助")));
    v
}

/// Build the footer hint list for the current app state.
pub(crate) fn footer_hints(app: &App) -> Vec<Hint> {
    footer_hints_ctx(footer_ctx(app))
}

pub(crate) fn hint_width(h: &Hint) -> usize {
    disp_width(h.0) + 1 + disp_width(h.1)
}

/// Information-density tier for the footer, chosen from the terminal width.
/// Small screens get only the highest-frequency keys so the line never has to
/// cut a hint in half (and never silently loses its escape hatch).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FooterTier {
    Mini,    // < 60 cols
    Compact, // 60..99
    Full,    // >= 100
}

pub(crate) fn footer_tier(width: usize) -> FooterTier {
    if width < 60 {
        FooterTier::Mini
    } else if width < 100 {
        FooterTier::Compact
    } else {
        FooterTier::Full
    }
}

/// Maximum number of leading hints a tier shows, or `None` for "show them all".
/// R80 widened the two small tiers (Mini 4 → 6, Compact 6 → 8): a modern
/// terminal is at least 80 columns wide, where the six-to-eight highest-priority
/// hints plus the pinned help hint still fit on one line, so the count cap was
/// dropping keys the width could have shown.
pub(crate) fn footer_tier_cap(tier: FooterTier) -> Option<usize> {
    match tier {
        FooterTier::Mini => Some(6),
        FooterTier::Compact => Some(8),
        FooterTier::Full => None,
    }
}

/// The key that opens help in a given focus. `?` is a literal character inside
/// the editor and the command line (R70 moved invocation to F1), so the pinned
/// hint must name `F1` there and `?` everywhere else.
pub(crate) fn footer_help_key(focus: Focus) -> &'static str {
    match focus {
        Focus::Editor | Focus::CmdInput => "F1",
        _ => "?",
    }
}

/// The pinned final hint. When the tier hid some keys the label invites a second
/// look (`? 更多`) rather than merely naming the help overlay.
pub(crate) fn footer_help_hint(more: bool, key: &'static str) -> Hint {
    if more {
        (key, t("更多"))
    } else {
        (key, t("帮助"))
    }
}

#[cfg(test)]
/// Choose which leading hints to show in `width`. The width tier caps the count
/// (4 / 6 / all); within the cap hints are appended while they fit, so a single
/// hint is never split. Returns the chosen hints and whether any were hidden
/// (the pinned help hint then reads `? 更多`).
pub(crate) fn footer_select<'a>(hints: &'a [Hint], width: usize) -> (Vec<&'a Hint>, bool) {
    let (help, lead) = hints.split_last().expect("footer always has a hint");
    let cap = footer_tier_cap(footer_tier(width));
    // Reserve the pinned help hint plus the separators on either side of it.
    let mut budget = width.saturating_sub(hint_width(help) + 5);
    let mut chosen: Vec<&'a Hint> = Vec::new();
    let mut hidden = false;
    for (i, h) in lead.iter().enumerate() {
        if cap.is_some_and(|c| i >= c) {
            hidden = true;
            break;
        }
        let w = hint_width(h);
        if w + 3 <= budget {
            budget -= w + 3;
            chosen.push(h);
        } else {
            hidden = true;
            break;
        }
    }
    (chosen, hidden)
}

/// R86: the global keys most relevant to the surface that owns the keyboard.
/// They are appended to the footer's hint list in the Full tier only, so a
/// narrow line keeps its context keys. Ordered most-relevant first.
pub(crate) fn footer_global_hints(ctx: FooterCtx) -> Vec<Hint> {
    // The global keys are routed after the modal overlays, so they are only
    // live on the pane-browsing surfaces (`key` checks `confirm` / the edit
    // dialog before `Ctrl-L`, and `browse_key` checks the popups / pickers
    // before `Ctrl-O` / the pane toggles). A modal surface therefore gets no
    // global segment — the footer must never name a key it will not receive.
    if !matches!(
        ctx.view,
        FooterView::Browse | FooterView::RedisKeys | FooterView::RedisValue | FooterView::MongoDocs
    ) {
        return Vec::new();
    }
    match ctx.focus {
        // The editor already leads with `Ctrl-J` (run); the globals it cannot
        // see at a glance are the snippet / EXPLAIN / batch keys.
        Focus::Editor => vec![
            ("Ctrl-O", t("片段")),
            ("Ctrl-P", t("EXPLAIN")),
            ("Ctrl-S", t("提交批量")),
        ],
        Focus::CmdInput => vec![("Ctrl-J", t("执行")), ("Ctrl-L", t("换模式"))],
        // The results / sidebar panes share the layout globals; `Ctrl-A` is
        // both the auto-collapse switch and (in row-select mode) select-all.
        Focus::Preview => vec![
            ("Ctrl-L", t("切模式")),
            ("Ctrl-A", t("折叠栏")),
            ("Tab", t("切区")),
        ],
        Focus::Sidebar => vec![
            ("Ctrl-L", t("切模式")),
            ("Ctrl-A", t("折叠栏")),
            ("Tab", t("切区")),
        ],
    }
}

/// R86: choose the footer hints for a width, splitting off the global segment.
/// The global hints are reserved first (so they survive when `show_globals` is
/// set), then the context hints fill the remaining budget in priority order; the
/// pinned help hint always closes the line. Display order is context → globals →
/// help, matching [`render_footer`].
pub(crate) fn footer_select_split<'a>(
    hints: &'a [Hint],
    globals: &'a [Hint],
    show_globals: bool,
    width: usize,
) -> (Vec<&'a Hint>, Vec<&'a Hint>, bool) {
    let (help, lead) = hints.split_last().expect("footer always has a hint");
    let cap = footer_tier_cap(footer_tier(width));
    let mut budget = width.saturating_sub(hint_width(help) + 5);
    let mut hidden = false;
    let mut gchosen: Vec<&'a Hint> = Vec::new();
    if show_globals {
        for g in globals {
            let w = hint_width(g);
            if w + 3 <= budget {
                budget -= w + 3;
                gchosen.push(g);
            } else {
                hidden = true;
                break;
            }
        }
    }
    let mut chosen: Vec<&'a Hint> = Vec::new();
    for (i, h) in lead.iter().enumerate() {
        if cap.is_some_and(|c| i >= c) {
            hidden = true;
            break;
        }
        let w = hint_width(h);
        if w + 3 <= budget {
            budget -= w + 3;
            chosen.push(h);
        } else {
            hidden = true;
            break;
        }
    }
    (chosen, gchosen, hidden)
}

#[cfg(test)]
/// Display width of the rendered footer line for a chosen set, used by tests.
/// Mirrors exactly what [`render_footer`] draws: the chosen hints, the pinned
/// help hint (whose label depends on `more`), and one `" · "` per gap.
pub(crate) fn footer_line_width(chosen: &[&Hint], more: bool, key: &'static str) -> usize {
    let help = footer_help_hint(more, key);
    let w: usize = chosen.iter().map(|h| hint_width(h)).sum::<usize>() + hint_width(&help);
    w + chosen.len() * 3
}

#[cfg(test)]
/// Display width of the split footer line (context hints then the global
/// segment then the pinned help hint), used by the R86 tests.
pub(crate) fn footer_line_width_split(
    chosen: &[&Hint],
    globals: &[&Hint],
    more: bool,
    key: &'static str,
) -> usize {
    let help = footer_help_hint(more, key);
    let w: usize = chosen.iter().map(|h| hint_width(h)).sum::<usize>()
        + globals.iter().map(|h| hint_width(h)).sum::<usize>()
        + hint_width(&help);
    w + (chosen.len() + globals.len()) * 3
}

pub(crate) fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let hints = footer_hints(app);
    let globals = footer_global_hints(footer_ctx(app));
    let (chosen, gchosen, more) = footer_select_split(
        &hints,
        &globals,
        footer_tier(area.width as usize) == FooterTier::Full,
        area.width as usize,
    );
    let help = footer_help_hint(more, footer_help_key(app.focus));
    let mut spans: Vec<Span> = Vec::new();
    let sep = Span::styled(" · ", Style::default().fg(Color::DarkGray));
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::DarkGray);
    for h in chosen.iter().chain(gchosen.iter()) {
        if !spans.is_empty() {
            spans.push(sep.clone());
        }
        spans.push(Span::styled(h.0, key_style));
        spans.push(Span::styled(format!(" {}", h.1), desc_style));
    }
    if !spans.is_empty() {
        spans.push(sep);
    }
    spans.push(Span::styled(help.0, key_style));
    spans.push(Span::styled(format!(" {}", help.1), desc_style));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn render_browse(f: &mut Frame, area: Rect, app: &mut App) {
    let mode = app.layout_mode;
    let has_cmd = app.backend_kind != Backend::Sql;
    let sidebar_collapsed = pane_eff_collapsed(app, PANE_SIDEBAR);
    let editor_collapsed = pane_eff_collapsed(app, PANE_EDITOR);
    let results_collapsed = pane_eff_collapsed(app, PANE_RESULTS);

    // Stacked layout when the terminal is narrow or the sidebar is collapsed:
    // the sidebar becomes a one-line strip above the editor / results column.
    if mode == LayoutMode::Narrow || sidebar_collapsed {
        let sidebar_h = if sidebar_collapsed { 1 } else { 7 };
        let v = Layout::vertical([Constraint::Length(sidebar_h), Constraint::Min(4)]).split(area);
        app.rects.sidebar = v[0];
        if sidebar_collapsed {
            render_sidebar_strip(f, v[0], app);
        } else {
            render_sidebar(f, v[0], app);
        }
        render_main_area(f, v[1], app, editor_collapsed, results_collapsed, has_cmd);
    } else {
        let sidebar_w = sidebar_width(app.term_w, mode, sidebar_tree_longest_name(app));
        let hz =
            Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(20)]).split(area);
        app.rects.sidebar = hz[0];
        render_sidebar(f, hz[0], app);
        render_main_area(f, hz[1], app, editor_collapsed, results_collapsed, has_cmd);
    }
}

pub(crate) fn render_main_area(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    editor_collapsed: bool,
    results_collapsed: bool,
    has_cmd: bool,
) {
    let mode = app.layout_mode;
    let cmd_h = if has_cmd { 3 } else { 0 };
    let base_editor_h = if mode == LayoutMode::Narrow { 3 } else { 5 };
    // Narrow + Editor 焦点：同帧把编辑器 3→6 行（小屏 height<14 或放不下时不扩）
    let expand_editor = mode == LayoutMode::Narrow
        && app.focus == Focus::Editor
        && app.term_h >= 14
        && area.height as usize >= 6 + cmd_h as usize + 5;
    let editor_h = if editor_collapsed {
        1
    } else if expand_editor {
        6
    } else {
        base_editor_h
    };
    let results_c = if results_collapsed {
        Constraint::Length(1)
    } else {
        Constraint::Min(5)
    };
    let main_chunks = Layout::vertical([
        Constraint::Length(editor_h),
        Constraint::Length(cmd_h),
        results_c,
    ])
    .split(area);
    app.rects.editor = main_chunks[0];
    app.rects.cmd = if has_cmd {
        main_chunks[1]
    } else {
        Rect::default()
    };

    if editor_collapsed {
        app.editor_gutter = 0;
        render_editor_strip(f, main_chunks[0], app);
    } else {
        let focused = app.focus == Focus::Editor;
        // R54: a read-only connection says so right on the editor title, so the
        // write policy is visible next to the SQL being typed (not just on the
        // tree root).
        let ro = app.selected.as_ref().is_some_and(|c| c.read_only);
        // R88: reserve a left gutter for the optional statement ordinals. The
        // padding shifts tui-textarea's text right; every paint layer and the
        // click mapping read `app.editor_gutter` so they stay aligned, and the
        // buffer text itself is never touched (render-only).
        let gutter = statement_gutter_cols(app);
        app.editor_gutter = gutter;
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(if ro { " SQL 🔒 " } else { " SQL " })
            .border_set(border::ROUNDED)
            .border_style(border_style(focused));
        if gutter > 0 {
            block = block.padding(Padding::left(gutter));
        }
        app.editor.set_block(block);
        // Keep the click-to-place-caret mirror in step with the widget: the
        // bordered inner area is the viewport, and the widget re-derives its own
        // scroll origin from the previous one plus the cursor on every render.
        app.editor_vp.resize(
            main_chunks[0]
                .width
                .saturating_sub(2)
                .saturating_sub(gutter),
            main_chunks[0].height.saturating_sub(2),
        );
        app.editor_vp.follow(app.editor.cursor());
        f.render_widget(&app.editor, main_chunks[0]);
        // R56: dim every statement except the caret's own, then mark the bracket
        // pair on top (so the accent survives the dim). R61: the find matches go
        // on last so their background wins over both.
        paint_statement_dim(f, main_chunks[0], app);
        paint_bracket_pair(f, main_chunks[0], app);
        // R71: `{{…}}` placeholders sit on top of the dim / bracket marks but
        // under the find highlight.
        paint_editor_placeholders(f, main_chunks[0], app);
        paint_editor_find(f, main_chunks[0], app);
        // R77: the located execution-error statements sit on top of everything,
        // so a failure is never hidden by a find highlight or placeholder mark.
        paint_editor_errors(f, main_chunks[0], app);
        // R88: the statement ordinals go on last, in the reserved gutter, so the
        // dim / error backgrounds never cover them.
        paint_stmt_gutter(f, main_chunks[0], app);
    }

    if has_cmd {
        let title = match app.backend_kind {
            Backend::Redis => format!(" redis[{}] ", app.redis_db),
            Backend::Mongo => format!(" mongo({}) ", app.current_db()),
            Backend::Sql => " cmd ".into(),
        };
        let cfocused = app.focus == Focus::CmdInput;
        let b = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Magenta)))
            .border_set(border::ROUNDED)
            .border_style(border_style(cfocused));
        app.cmd_input.set_block(b);
        f.render_widget(&app.cmd_input, main_chunks[1]);
    }

    let res_area = main_chunks[2];
    app.rects.results = res_area;
    if results_collapsed {
        render_results_strip(f, res_area, app);
    } else {
        render_results_pane(f, res_area, app);
    }
}

/// R56: dim the editor lines that belong to a statement other than the one the
/// caret is in, so a multi-statement buffer reads as one bright block amid the
/// rest. Purely presentational — no key, no state, no query — and it only
/// touches the foreground colour of a cell, so the textarea's own caret / line
/// highlight (background + modifiers) survives. A single-statement buffer is
/// left untouched; the span is resolved by [`active_statement_rows`].
pub(crate) fn paint_statement_dim(f: &mut Frame, area: Rect, app: &App) {
    let (row, col) = app.editor.cursor();
    let Some((active_lo, active_hi)) = active_statement_rows(app.editor.lines(), row, col) else {
        return;
    };
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the glyph area is the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top_row = app.editor_vp.row as usize;
    for i in 0..inner.height as usize {
        let r = top_row + i;
        if r >= active_lo && r <= active_hi {
            continue;
        }
        let y = inner.y + i as u16;
        for x in inner.x..inner.x + inner.width {
            let cell = &mut f.buffer_mut()[(x, y)];
            cell.fg = Color::DarkGray;
        }
    }
}

/// R53: mark the editor caret's bracket and its match, straight in the frame
/// buffer after the textarea drew. Purely presentational — no key, no state, no
/// query — so it can never affect execution; the pair is resolved from the
/// caret's ±[`BRACKET_SCAN_LINES`] lines only. The glyph stays exactly where the
/// widget painted it; only the colour and modifiers are added. Underline alone
/// would not stand out (tui-textarea already underlines the caret's whole line),
/// so the pair gets the accent colour plus bold as well.
pub(crate) fn paint_bracket_pair(f: &mut Frame, area: Rect, app: &App) {
    let Some((a, b)) = editor_bracket_pair(app) else {
        return;
    };
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the glyph area is the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // The widget's own scroll origin for this frame, mirrored in `editor_vp`.
    let top_row = app.editor_vp.row as usize;
    let top_col = app.editor_vp.col as usize;
    // R88: text starts after the reserved statement gutter (0 when off).
    let gutter = app.editor_gutter as usize;
    let text_w = (inner.width as usize).saturating_sub(gutter);
    let lines = app.editor.lines();
    for (row, col) in [a, b] {
        let Some(line) = lines.get(row) else {
            continue;
        };
        let dcol = editor_display_col(line, col);
        if row < top_row || row - top_row >= inner.height as usize {
            continue;
        }
        if dcol < top_col || dcol - top_col >= text_w {
            continue;
        }
        let x = inner.x + gutter as u16 + (dcol - top_col) as u16;
        let y = inner.y + (row - top_row) as u16;
        let cell = &mut f.buffer_mut()[(x, y)];
        cell.fg = Color::LightCyan;
        cell.modifier.insert(Modifier::UNDERLINED | Modifier::BOLD);
    }
}

/// R61: paint the editor find matches straight into the frame buffer, after the
/// textarea drew (and after the statement dim / bracket pair), so the highlight
/// sits on top. Purely presentational: no key, no state change, no query. Every
/// match shares one background; the match the caret jumped to (the one the `3/7`
/// count names) gets the accent colour, so the count and the screen agree.
/// Char columns are mapped through [`editor_display_col`], so tabs and wide CJK
/// characters highlight their real screen cells.
pub(crate) fn paint_editor_find(f: &mut Frame, area: Rect, app: &App) {
    if app.editor_find_needle.is_empty() {
        return;
    }
    let hits = editor_find_hits(app.editor.lines(), &app.editor_find_needle);
    if hits.is_empty() {
        return;
    }
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the glyph area is the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top_row = app.editor_vp.row as usize;
    let top_col = app.editor_vp.col as usize;
    // R88: text starts after the reserved statement gutter (0 when off).
    let gutter = app.editor_gutter as usize;
    let text_w = (inner.width as usize).saturating_sub(gutter);
    let lines = app.editor.lines();
    for (hi, hit) in hits.iter().enumerate() {
        let Some(line) = lines.get(hit.row) else {
            continue;
        };
        if hit.row < top_row || hit.row - top_row >= inner.height as usize {
            continue;
        }
        let current = app.editor_find_idx == Some(hi);
        let y = inner.y + (hit.row - top_row) as u16;
        for k in 0..hit.len {
            let dcol = editor_display_col(line, hit.col + k);
            if dcol < top_col || dcol - top_col >= text_w {
                continue;
            }
            // The next char's display column minus this one is the exact cell
            // span (a tab is 1..4 cells, a wide CJK glyph 2, a combining mark 0).
            let dnext = editor_display_col(line, hit.col + k + 1);
            let span = dnext.saturating_sub(dcol).max(1);
            let x0 = inner.x + gutter as u16 + (dcol - top_col) as u16;
            for dx in 0..span as u16 {
                if x0 + dx >= inner.x + inner.width {
                    break;
                }
                let cell = &mut f.buffer_mut()[(x0 + dx, y)];
                cell.fg = Color::Black;
                cell.bg = if current {
                    Color::LightGreen
                } else {
                    Color::Yellow
                };
                if current {
                    cell.modifier.insert(Modifier::BOLD);
                }
            }
        }
    }
}

/// R77: paint the statements that failed in the last editor run straight into
/// the frame buffer, after every other editor layer, so a failure is never
/// hidden. Every failing statement gets a red block; the one `Alt-E` / `F8`
/// landed on gets the brighter accent. Purely presentational — no key, no state
/// change, no query — and char columns map through [`editor_display_col`], so
/// tabs and wide CJK glyphs highlight their real cells.
pub(crate) fn paint_editor_errors(f: &mut Frame, area: Rect, app: &App) {
    if app.editor_error_spans.is_empty() {
        return;
    }
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the glyph area is the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top_row = app.editor_vp.row as usize;
    let top_col = app.editor_vp.col as usize;
    // R88: text starts after the reserved statement gutter (0 when off).
    let gutter = app.editor_gutter as usize;
    let text_w = (inner.width as usize).saturating_sub(gutter);
    let lines = app.editor.lines();
    for (i, span) in app.editor_error_spans.iter().enumerate() {
        let current = i == app.editor_error_idx;
        for (row, c0, c1) in char_range_rows(lines, span.start, span.end) {
            if row < top_row || row - top_row >= inner.height as usize {
                continue;
            }
            let Some(line) = lines.get(row) else {
                continue;
            };
            let y = inner.y + (row - top_row) as u16;
            for col in c0..c1 {
                let dcol = editor_display_col(line, col);
                if dcol < top_col || dcol - top_col >= text_w {
                    continue;
                }
                let dnext = editor_display_col(line, col + 1);
                let span_w = dnext.saturating_sub(dcol).max(1);
                let x0 = inner.x + gutter as u16 + (dcol - top_col) as u16;
                for dx in 0..span_w as u16 {
                    if x0 + dx >= inner.x + inner.width {
                        break;
                    }
                    let cell = &mut f.buffer_mut()[(x0 + dx, y)];
                    cell.fg = Color::White;
                    cell.bg = if current { Color::LightRed } else { Color::Red };
                    cell.modifier.insert(Modifier::BOLD);
                }
            }
        }
    }
}

/// R88: paint the optional statement ordinals in the editor's left gutter.
/// Pure render layer, on top of every other editor mark, and only active while
/// `stmt_gutter` is on; the buffer text itself is never touched (the gutter
/// columns are reserved with a left padding, not by inserting characters).
/// Ordinals are right-aligned so the marker column stays tidy as the count
/// grows.
pub(crate) fn paint_stmt_gutter(f: &mut Frame, area: Rect, app: &App) {
    let gutter = app.editor_gutter as usize;
    if gutter == 0 {
        return;
    }
    let text = app.editor_sql();
    let rows = statement_start_rows(&text, STMT_DIM_MAX_BYTES);
    if rows.is_empty() {
        return;
    }
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the gutter lives in the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top_row = app.editor_vp.row as usize;
    let marker_w = gutter.saturating_sub(1).max(1);
    for (row, ord) in rows {
        if row < top_row || row - top_row >= inner.height as usize {
            continue;
        }
        let y = inner.y + (row - top_row) as u16;
        let padded = format!("{:>marker_w$} ", format!("{ord}."));
        for (i, ch) in padded.chars().enumerate() {
            if i >= gutter {
                break;
            }
            let cell = &mut f.buffer_mut()[(inner.x + i as u16, y)];
            cell.set_char(ch);
            cell.fg = Color::DarkGray;
        }
    }
}

/// R71: paint the `{{…}}` placeholders a just-inserted template left in the
/// buffer, straight into the frame after the textarea (and the dim / bracket
/// marks) drew, so they read as "fill me". Purely presentational: no key, no
/// state change, no query. Only active while `template_active` is on, so a
/// hand-typed `{{x}}` is never highlighted. Char columns map through
/// [`editor_display_col`], so tabs and wide CJK glyphs highlight real cells.
pub(crate) fn paint_editor_placeholders(f: &mut Frame, area: Rect, app: &App) {
    if !app.template_active {
        return;
    }
    let phs = editor_placeholders(app.editor.lines());
    if phs.is_empty() {
        return;
    }
    // tui-textarea draws the text inside the `Borders::ALL` block dbxt sets on
    // it, so the glyph area is the block's inner rect.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top_row = app.editor_vp.row as usize;
    let top_col = app.editor_vp.col as usize;
    // R88: text starts after the reserved statement gutter (0 when off).
    let gutter = app.editor_gutter as usize;
    let text_w = (inner.width as usize).saturating_sub(gutter);
    let lines = app.editor.lines();
    for p in &phs {
        let Some(line) = lines.get(p.row) else {
            continue;
        };
        if p.row < top_row || p.row - top_row >= inner.height as usize {
            continue;
        }
        let y = inner.y + (p.row - top_row) as u16;
        for k in 0..p.len {
            let dcol = editor_display_col(line, p.col + k);
            if dcol < top_col || dcol - top_col >= text_w {
                continue;
            }
            // The next char's display column minus this one is the exact cell
            // span (a tab is 1..4 cells, a wide CJK glyph 2, a combining mark 0).
            let dnext = editor_display_col(line, p.col + k + 1);
            let span = dnext.saturating_sub(dcol).max(1);
            let x0 = inner.x + gutter as u16 + (dcol - top_col) as u16;
            for dx in 0..span as u16 {
                if x0 + dx >= inner.x + inner.width {
                    break;
                }
                let cell = &mut f.buffer_mut()[(x0 + dx, y)];
                cell.fg = Color::Black;
                cell.bg = Color::LightMagenta;
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    }
}

/// One-line summary shown in place of the sidebar when it is collapsed.
pub(crate) fn render_sidebar_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Sidebar;
    let mut text = String::new();
    if let Some(c) = &app.selected {
        if app.backend_kind == Backend::Redis {
            text.push_str(&tf(
                "▸ {} · {} keys",
                &[&(truncate_disp(&c.name, 16)), &(app.redis_scan.keys.len())],
            ));
            text.push_str(&format!(" · db{}", app.redis_db));
            if let Some(v) = &app.redis_value {
                text.push_str(&format!(" · {}", fix_double_encoding(&v.key_display)));
            }
        } else {
            text.push_str(&tf(
                "▸ {} · {} 表",
                &[&(truncate_disp(&c.name, 16)), &(app.tables.len())],
            ));
            let db = app.current_db();
            if !db.is_empty() {
                let schema = if app.schema.is_empty() {
                    String::new()
                } else {
                    format!(".{}", fix_double_encoding(&app.schema))
                };
                text.push_str(&format!(" · {db}{schema}", db = fix_double_encoding(&db)));
            }
            if let Some(t) = app.selected_table() {
                text.push_str(&format!(" · {}", fix_double_encoding(&t.name)));
            }
        }
    } else {
        text.push_str(t("▸ 未连接"));
    }
    text.push_str(t(" · 点击/Ctrl-W 展开"));
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize)).style(style),
        area,
    );
}

/// One-line summary shown in place of the SQL editor when it is collapsed.
pub(crate) fn render_editor_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Editor;
    let sql = app.editor_sql();
    let first = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let text = if first.is_empty() {
        t("SQL ▸ 空 · 点击/Tab 展开").to_string()
    } else {
        tf("SQL ▸ {} · 点击/Tab 展开", &[&(one_line(first))])
    };
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize)).style(style),
        area,
    );
}

/// One-line summary shown in place of the results pane when it is collapsed.
pub(crate) fn render_results_strip(f: &mut Frame, area: Rect, app: &mut App) {
    let n = result_row_count(app);
    let text = tf("结果 ▸ {} 行 · 点击/Ctrl-W 展开", &[&(n)]);
    f.render_widget(
        Paragraph::new(truncate_disp(&text, area.width as usize))
            .style(Style::default().fg(Color::Gray)),
        area,
    );
}

/// R73: the short label for one result tab — its 1-based number plus the first
/// 12 display columns of the statement that produced it. Pure, so the strip and
/// its tests agree on the exact text.
pub(crate) fn result_tab_label(idx: usize, title: &str) -> String {
    format!("{}:{}", idx + 1, truncate_disp(title, 12))
}

/// R73: pure tab-strip layout. Given `n` tab label widths and `avail` columns,
/// return the inclusive index window to draw around `current`, plus whether a
/// `…` fold stands in for hidden tabs on the left / right. The current tab is
/// always inside the window; a hidden middle is folded to `…` on each side.
pub(crate) fn tab_strip_window(
    n: usize,
    current: usize,
    widths: &[usize],
    avail: usize,
) -> (usize, usize, bool, bool) {
    if n == 0 {
        return (0, 0, false, false);
    }
    let current = current.min(n - 1);
    // A tab token is ` label ` (2 padding columns); a fold token is `…`; tokens
    // are joined by `│`. Cost includes the separators.
    let cost = |lo: usize, hi: usize| -> usize {
        let mut sum = 0usize;
        let mut count = 0usize;
        if lo > 0 {
            sum += 1;
            count += 1;
        }
        for i in lo..=hi {
            sum += widths.get(i).copied().unwrap_or(0) + 2;
            count += 1;
        }
        if hi < n - 1 {
            sum += 1;
            count += 1;
        }
        sum + count.saturating_sub(1)
    };
    let (mut lo, mut hi) = (current, current);
    loop {
        if hi + 1 < n && cost(lo, hi + 1) <= avail {
            hi += 1;
            continue;
        }
        if lo > 0 && cost(lo - 1, hi) <= avail {
            lo -= 1;
            continue;
        }
        break;
    }
    // When even the folded single tab does not fit, drop the folds so the
    // caller knows to clip the label instead of drawing an overflowing row.
    if cost(lo, hi) > avail {
        return (current, current, false, false);
    }
    (lo, hi, lo > 0, hi < n - 1)
}

/// R73: draw the one-line result-tab strip. The active tab is bold, the rest
/// dim, and a too-wide bar folds its middle to `…` while the current tab stays
/// on screen. Purely presentational — the strip is drawn from the cached tabs,
/// so it never runs a query.
pub(crate) fn render_result_tabs(f: &mut Frame, area: Rect, app: &App) {
    if app.result_tabs.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let labels: Vec<String> = app
        .result_tabs
        .iter()
        .enumerate()
        .map(|(i, t)| result_tab_label(i, &t.title))
        .collect();
    let widths: Vec<usize> = labels.iter().map(|l| disp_width(l)).collect();
    let avail = area.width as usize;
    let (lo, hi, lfold, rfold) = tab_strip_window(labels.len(), app.result_tab, &widths, avail);
    let dim = Style::default().fg(Color::DarkGray);
    let current_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Gray)
        .add_modifier(Modifier::BOLD);
    // When even a single folded tab does not fit, fall back to a clipped label
    // so the active tab is still named rather than the row going blank.
    let single_overflow = lo == hi && !lfold && !rfold && (widths[lo] + 2) > avail;
    let mut spans: Vec<Span> = Vec::new();
    if single_overflow {
        spans.push(Span::styled(
            truncate_disp(&labels[lo], avail),
            current_style,
        ));
    } else {
        let push_token = |spans: &mut Vec<Span>, text: String, style: Style| {
            if !spans.is_empty() {
                spans.push(Span::styled("│", dim));
            }
            spans.push(Span::styled(text, style));
        };
        if lfold {
            push_token(&mut spans, "…".to_string(), dim);
        }
        for (i, label) in labels.iter().enumerate().take(hi + 1).skip(lo) {
            let cur = i == app.result_tab;
            push_token(
                &mut spans,
                format!(" {} ", label),
                if cur { current_style } else { dim },
            );
        }
        if rfold {
            push_token(&mut spans, "…".to_string(), dim);
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// R73: carve the one-line result-tab strip off the top of the results pane
/// when several query tabs are open, and hand back the area left for the grid.
/// Below the minimum height the strip is skipped so a tiny terminal keeps its
/// rows for data.
pub(crate) fn result_tab_strip_area(f: &mut Frame, area: Rect, app: &App) -> Rect {
    if app.result_tabs.len() > 1 && app.grid_kind == GridKind::Query && area.height >= 5 {
        let strip = Rect { height: 1, ..area };
        render_result_tabs(f, strip, app);
        Rect {
            y: area.y + 1,
            height: area.height - 1,
            ..area
        }
    } else {
        area
    }
}

pub(crate) fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

pub(crate) fn render_results_pane(f: &mut Frame, area: Rect, app: &mut App) {
    // R73: a query result with several tabs gets a one-line strip on top; the
    // grid (and the mouse mapping) then uses the area left below it.
    let area = result_tab_strip_area(f, area, app);
    app.rects.results = area;
    if let Some(s) = app.script.clone() {
        if let Some(i) = s.drilled {
            let o = &s.outcomes[i];
            let title = tf(
                " {}语句 {} 结果 · {} · Esc 返回脚本 ",
                &[
                    &(search_marker(app)),
                    &(i + 1),
                    &(if o.grid.note.is_empty() {
                        "".to_string()
                    } else {
                        o.grid.note.clone()
                    }),
                ],
            );
            if let Some(grid) = active_grid(app) {
                render_grid(f, area, app, &grid, GridKind::Query, &title, false);
            }
        } else if app.show_stmt_timing {
            // R42 console feel: statement separators + a `12.3ms` prefix per
            // statement (Alt-O).
            render_script_stream(f, area, app, &s);
        } else {
            render_script_list(f, area, app, &s);
        }
        return;
    }
    if app.struct_view == StructView::Ddl {
        if let Some(ddl) = app.ddl.clone() {
            render_ddl(f, area, app, &ddl);
            return;
        }
    }
    // R48: a pinned grid keeps an up-and-down comparison visible above the live
    // pane. Only on SQL data grids, and only when there is room for both.
    let area = if app.backend_kind == Backend::Sql {
        match app.pinned_result.clone() {
            Some(pin) if area.height >= 8 && area.width >= 12 => {
                let ph = (area.height / 3)
                    .clamp(4, 7)
                    .min(area.height.saturating_sub(4));
                let top = Rect { height: ph, ..area };
                let bottom = Rect {
                    y: area.y + ph,
                    height: area.height - ph,
                    ..area
                };
                let title = format!(
                    " 📌 {} ",
                    truncate_disp(&pin.title, area.width.saturating_sub(6) as usize)
                );
                // The pinned render writes app's scroll/grid caches for its own
                // widths; restore the column offset so the live pane below keeps
                // its place (the live render recomputes everything else).
                let saved_offset = app.col_offset;
                let saved_focus = app.focus;
                app.focus = Focus::Sidebar;
                render_grid(f, top, app, &pin.grid, pin.kind, &title, false);
                app.col_offset = saved_offset;
                app.focus = saved_focus;
                bottom
            }
            _ => area,
        }
    } else {
        area
    };
    if app.grid.is_some() {
        let title = grid_title(app);
        // Move the grid out instead of deep-cloning it every frame: a 20k-row
        // result is ~12 ms of allocation per frame otherwise. `render_grid` only
        // borrows it, so it is put back right after.
        let grid = app.grid.take().expect("grid present");
        let kind = app.grid_kind;
        render_grid(f, area, app, &grid, kind, &title, true);
        app.grid = Some(grid);
        return;
    }
    if app.backend_kind != Backend::Sql && !app.cmd_output.is_empty() {
        render_console(f, area, app);
        return;
    }
    let hint = if app.selected.is_none() {
        if app.picker_open {
            ""
        } else {
            t("q 显示连接列表")
        }
    } else if app.backend_kind == Backend::Redis {
        if app.redis_scan.keys.is_empty() {
            t("无 key · Tab 到命令台 · / 匹配模式 · r 重扫")
        } else {
            t("↑↓ 选 key · Enter 查看值 · n 更多 · / 匹配模式\nTab 到命令台 · Ctrl-L 切换模式")
        }
    } else if app.tables.is_empty() {
        t("无表 · Tab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行")
    } else {
        t("↑↓ 选表 · Enter 浏览数据 · r 表结构\nTab 到 SQL 编辑器 · Ctrl-L 切 redis/mongo 命令行")
    };
    f.render_widget(
        Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t(" 结果 "))
                    .border_set(border::ROUNDED)
                    .border_style(border_style(app.focus == Focus::Preview)),
            ),
        area,
    );
}

/// Leading marker for an active result search, so the indicator stays visible
/// even when a narrow pane clips the rest of the title.
pub(crate) fn search_marker(app: &App) -> String {
    let mut out = String::new();
    if !app.result_needle.trim().is_empty() {
        out.push_str(&tf(
            "🔍「{}」{} 命中 · ",
            &[&(app.result_needle), &(result_row_count(app))],
        ));
    }
    // R52 column filter: name the column and the live hit count so a filtered
    // grid is never mistaken for the whole result.
    if let Some((name, needle)) = app.col_filter_spec().filter(|(_, n)| !n.trim().is_empty()) {
        out.push_str(&tf(
            "▤{}「{}」{} 行 · ",
            &[
                &(fix_double_encoding(name)),
                &(needle),
                &(result_row_count(app)),
            ],
        ));
    }
    out
}

/// Compressed table-browser title for a narrow terminal: table name, page
/// number and the filter / search marks survive; everything else is dropped.
pub(crate) fn narrow_table_title(app: &App, ps: &PageState) -> String {
    let table = fix_double_encoding(&qualified_display(&ps.schema, &ps.table));
    let mut marks = String::new();
    if !app.result_needle.trim().is_empty() {
        marks.push('🔍');
    }
    if app
        .col_filter_spec()
        .is_some_and(|(_, n)| !n.trim().is_empty())
    {
        marks.push('▤');
    }
    if !ps.filter.trim().is_empty() {
        marks.push('⚑');
    }
    let marks = if marks.is_empty() {
        String::new()
    } else {
        format!(" {marks}")
    };
    tf(
        " {}.{}{} · p{} ",
        &[
            &fix_double_encoding(&app.current_db()),
            &table,
            &marks,
            &(ps.page + 1),
        ],
    )
}

pub(crate) fn grid_title(app: &App) -> String {
    // On a narrow terminal the title is clipped by the pane, so keep only the
    // three essentials — table name, page number, filter/search marks — and drop
    // the row range, note and next-page hint. `term_w == 0` (tests) is treated as
    // wide so the full title is exercised.
    let narrow = app.term_w > 0 && app.term_w < 56;
    match app.grid_kind {
        GridKind::TableData => {
            let Some(ps) = &app.page_state else {
                return t(" 结果 ").into();
            };
            if narrow {
                return narrow_table_title(app, ps);
            }
            let rows = app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0);
            let offset = ps.page * ps.page_size;
            let total = total_label(ps);
            let more = if ps.has_next {
                t(" · n 下一页")
            } else {
                ""
            };
            let table_label = fix_double_encoding(&qualified_display(&ps.schema, &ps.table));
            tf(
                " {}{}.{} · 第 {} 页 · {}–{} / {} · {}{}{} ",
                &[
                    &(search_marker(app)),
                    &(fix_double_encoding(&app.current_db())),
                    &(table_label),
                    &(ps.page + 1),
                    &(if rows == 0 { 0 } else { offset + 1 }),
                    &(offset + rows),
                    &(total),
                    &(app
                        .grid
                        .as_ref()
                        .map(|g| g.note.clone())
                        .unwrap_or_default()),
                    &(more),
                    &(page_state_extra(ps)),
                ],
            )
        }
        GridKind::Columns => {
            let table = app
                .selected_table()
                .map(|t| fix_double_encoding(&qualified_display(&app.schema, &t.name)))
                .unwrap_or_default();
            tf(
                " 表结构 · {} · {} · t 查看 DDL ",
                &[
                    &(table),
                    &(app
                        .grid
                        .as_ref()
                        .map(|g| g.note.clone())
                        .unwrap_or_default()),
                ],
            )
        }
        GridKind::Query => {
            let note = app
                .grid
                .as_ref()
                .map(|g| g.note.clone())
                .unwrap_or_default();
            if app.result_tabs.len() > 1 {
                let title = app
                    .result_tabs
                    .get(app.result_tab)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                tf(
                    " {}结果 {}/{} · {} · {} · [ ] 切换 ",
                    &[
                        &(search_marker(app)),
                        &(app.result_tab + 1),
                        &(app.result_tabs.len()),
                        &(note),
                        &(truncate_disp(&title, 20)),
                    ],
                )
            } else {
                tf(" {}结果 · {} ", &[&(search_marker(app)), &(note)])
            }
        }
        GridKind::RedisValue => {
            let note = app
                .grid
                .as_ref()
                .map(|g| g.note.clone())
                .unwrap_or_default();
            match &app.redis_value {
                Some(v) => tf(
                    " {}Redis · {} · {} · TTL {} · {} · e 编辑 x TTL m 重命名 Del 删除 ",
                    &[
                        &(search_marker(app)),
                        &(fix_double_encoding(&v.key_display)),
                        &(v.redis_type),
                        &(redis_ttl_label(v.ttl)),
                        &(note),
                    ],
                ),
                None => tf(" {}Redis value · {} ", &[&(search_marker(app)), &(note)]),
            }
        }
        GridKind::MongoDocs => {
            let Some(ps) = &app.page_state else {
                return t(" 文档 ").into();
            };
            if narrow {
                return narrow_table_title(app, ps);
            }
            let rows = app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0);
            let offset = ps.page * ps.page_size;
            let total = ps
                .total
                .map(|t| tf("共 {} 个", &[&(t)]))
                .unwrap_or_else(|| t("总数未知").into());
            let more = if ps.has_next {
                t(" · n 下一页")
            } else {
                ""
            };
            let filt = if ps.filter.trim().is_empty() {
                String::new()
            } else {
                tf(" · 过滤 {}", &[&(truncate_disp(&ps.filter, 24))])
            };
            // R82: echo the active client-side size ordering so the header never
            // hides that the rows are not in arrival order.
            let sort = if app.mongo_size_sort == MongoSizeSort::Natural {
                String::new()
            } else {
                tf(" · {}", &[&(app.mongo_size_sort.label())])
            };
            tf(
                " {}{}.{} · 第 {} 页 · {}–{} / {} · {}{}{}{} ",
                &[
                    &(search_marker(app)),
                    &(fix_double_encoding(&app.current_db())),
                    &(fix_double_encoding(&qualified_display(&ps.schema, &ps.table))),
                    &(ps.page + 1),
                    &(if rows == 0 { 0 } else { offset + 1 }),
                    &(offset + rows),
                    &(total),
                    &(app
                        .grid
                        .as_ref()
                        .map(|g| g.note.clone())
                        .unwrap_or_default()),
                    &(more),
                    &(filt),
                    &(sort),
                ],
            )
        }
    }
}

pub(crate) fn render_grid(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    grid: &Grid,
    kind: GridKind,
    title: &str,
    cache: bool,
) {
    let focused = app.focus == Focus::Preview;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_set(border::ROUNDED)
        .border_style(border_style(focused));

    if grid.columns.is_empty() {
        // R82: an empty list always names its next step instead of a bare `OK`.
        let base = if grid.note.is_empty() {
            String::new()
        } else {
            grid.note.clone()
        };
        let hint = match kind {
            GridKind::MongoDocs => t("无文档 · f JSON 过滤 · i 插入文档"),
            GridKind::TableData | GridKind::Query => t("无结果 · 修改 SQL 后 Ctrl-J 重新执行"),
            GridKind::RedisValue => t("无数据"),
            GridKind::Columns => "",
        };
        let body = match (base.is_empty(), hint.is_empty()) {
            (true, true) => "OK".to_string(),
            (false, true) => base,
            (true, false) => hint.to_string(),
            (false, false) => format!("{base}\n{hint}"),
        };
        f.render_widget(
            Paragraph::new(body)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }

    let total_rows = grid.rows.len();
    let gutter = ((total_rows + 1).to_string().len()).max(2) as u16;

    // Fixed layout for the structure field list; windowed layout for data grids.
    if kind == GridKind::Columns {
        render_columns_grid(f, area, app, grid, block, gutter);
        return;
    }

    let inner_w = area.width.saturating_sub(2) as usize;
    let ncols = grid.columns.len();
    // Highlight cells that match the active result search (`/` in the results).
    let needle_lc = app.result_needle.trim().to_lowercase();
    let needle = if needle_lc.is_empty() {
        None
    } else {
        Some(needle_lc.as_str())
    };
    // R64: the active cell-find needle (`\`) highlights its matches too, and the
    // match the cursor last landed on gets the accent colour — the grid twin of
    // the R61 editor find.
    let find_lc = app.cell_find_needle.trim().to_lowercase();
    let find = if find_lc.is_empty() {
        None
    } else {
        Some(find_lc.as_str())
    };
    let find_current = app.cell_find_hits.get(app.cell_find_idx).copied();
    // Compact (mobile) mode shares the pane among all columns so a wide table can
    // fit without horizontal scrolling; otherwise each column keeps its natural
    // content width, capped per layout.
    let max_cell = grid_max_cell(app, ncols, inner_w, gutter);
    app.grid_max_cell = max_cell;
    // R76: capture the big-number mode before the mutable width call below.
    let num_fmt = app.num_fmt;
    // Column widths are content-sized, so building them scans every cell. Cache
    // them per displayed grid (and width cap) so scrolling 20k rows is a lookup,
    // not a rescan. The drilled-script grid is rebuilt per frame, so it skips
    // the cache.
    let mut widths: Vec<usize> = if cache {
        app.column_widths(grid, max_cell, num_fmt)
    } else {
        natural_widths_fmt(grid, max_cell, num_fmt)
    };
    // R55: a session column-width override wins over the natural width, so a
    // manual `<` / `>` adjustment survives page turns and re-queries.
    apply_col_width_overrides(app, grid, &mut widths);
    // R91: pin the focused column(s) at the left edge. The `z` first-column
    // toggle and the `g f` set are merged, capped at `MAX_FROZEN_COLS` and
    // dropped when the grid could no longer scroll.
    let pinned = effective_frozen_cols(
        app.freeze_first,
        &app.frozen_cols,
        ncols,
        &widths,
        gutter as usize,
        inner_w,
    );
    let pinned_w: usize = pinned.iter().map(|&c| widths[c]).sum();
    let left_w: usize = gutter as usize + pinned.len() + pinned_w;
    const GAP: usize = 1;
    let avail = inner_w.saturating_sub(left_w + GAP).max(MIN_CELL_WIDTH);
    app.grid_avail = avail;
    let (off, visible) =
        window_for_cursor_pinned(&widths, app.col_cursor, app.col_offset, avail, &pinned);
    app.col_offset = off;
    app.vis_cols = visible;
    app.grid_gutter = gutter;
    app.grid_frozen = pinned.len();
    app.grid_frozen_cols = pinned.clone();
    app.grid_widths = widths.clone();
    // R91: the scrollable columns actually drawn, skipping the pinned ones.
    let win = scroll_window_cols(ncols, off, visible, &pinned);

    // Sort / filter marks only make sense for a browsed table.
    let (sort_keys, filter_text) = if kind == GridKind::TableData {
        match &app.page_state {
            Some(ps) => (parse_order_by(ps.order_by.as_deref()), ps.filter.clone()),
            None => (Vec::new(), String::new()),
        }
    } else {
        (Vec::new(), String::new())
    };
    let sort_of = |name: &str| {
        sort_keys
            .iter()
            .position(|(c, _)| c == name)
            .map(|i| (sort_keys[i].1, i + 1))
    };
    let filt_of = |name: &str| !filter_text.is_empty() && filter_mentions(&filter_text, name);

    let h = (area.height as usize).saturating_sub(3).max(1);
    let nrows = grid.rows.len();
    let start = app
        .sel
        .saturating_sub(h / 2)
        .min(nrows.saturating_sub(h.min(nrows)));
    let sel = app.sel;
    let cc = app.col_cursor;
    // R76: alternate-row banding, captured once for the row loops below.
    let stripe = app.stripe;
    // R57: the row-select block, if any, drawn as full-row reverse video.
    let sel_range = app.row_sel_anchor.map(|a| (a.min(sel), a.max(sel)));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // ── pinned block: row-number gutter + optionally the first data column ──
    let mut left_widths: Vec<usize> = vec![gutter as usize];
    left_widths.extend(pinned.iter().map(|&c| widths[c]));
    let mut lheader: Vec<Cell> = vec![gutter_header_cell()];
    for &ci in &pinned {
        let name = &grid.columns[ci];
        lheader.push(col_header_cell(
            &fix_double_encoding(name),
            widths[ci],
            ci == cc,
            sort_of(name),
            filt_of(name),
        ));
    }
    let mut lrows: Vec<Row> = Vec::new();
    for (i, row) in grid.rows.iter().enumerate().skip(start).take(h) {
        let mut cells: Vec<Cell> = vec![gutter_cell(i, i == sel)];
        for &ci in &pinned {
            cells.push(match row.get(ci) {
                Some(v) => cell_widget_hl(
                    v,
                    widths[ci],
                    i == sel && ci == cc,
                    needle,
                    find,
                    find_current == Some((i, ci)),
                    grid.col_type(ci),
                    num_fmt,
                ),
                None => Cell::from(""),
            });
        }
        let mut r = Row::new(cells);
        if sel_range.is_some_and(|(lo, hi)| i >= lo && i <= hi) {
            r = r.style(row_select_style());
        } else if i == sel {
            r = r.style(highlight_style());
        } else if stripe && i % 2 == 1 {
            r = r.style(stripe_style());
        }
        lrows.push(r);
    }
    let left_area = Rect {
        x: inner.x,
        y: inner.y,
        width: (left_w.min(inner.width as usize)) as u16,
        height: inner.height,
    };
    let ltable = Table::new(
        lrows,
        left_widths
            .iter()
            .map(|w| Constraint::Length(*w as u16))
            .collect::<Vec<_>>(),
    )
    .header(Row::new(lheader))
    .column_spacing(1);
    f.render_widget(ltable, left_area);

    // ── scrollable window ──
    if visible > 0 && (inner.width as usize) > left_w + GAP {
        let right_x = inner.x + (left_w + GAP) as u16;
        let right_w = (inner.x + inner.width).saturating_sub(right_x);
        if right_w > 0 {
            let right_area = Rect {
                x: right_x,
                y: inner.y,
                width: right_w,
                height: inner.height,
            };
            let mut rheader: Vec<Cell> = Vec::new();
            for &ci in &win {
                let name = &grid.columns[ci];
                rheader.push(col_header_cell(
                    &fix_double_encoding(name),
                    widths[ci],
                    ci == cc,
                    sort_of(name),
                    filt_of(name),
                ));
            }
            let mut rrows: Vec<Row> = Vec::new();
            for (i, row) in grid.rows.iter().enumerate().skip(start).take(h) {
                let mut cells: Vec<Cell> = Vec::new();
                for &ci in &win {
                    cells.push(match row.get(ci) {
                        Some(v) => cell_widget_hl(
                            v,
                            widths[ci],
                            i == sel && ci == cc,
                            needle,
                            find,
                            find_current == Some((i, ci)),
                            grid.col_type(ci),
                            num_fmt,
                        ),
                        None => Cell::from(""),
                    });
                }
                let mut r = Row::new(cells);
                if sel_range.is_some_and(|(lo, hi)| i >= lo && i <= hi) {
                    r = r.style(row_select_style());
                } else if i == sel {
                    r = r.style(highlight_style());
                } else if stripe && i % 2 == 1 {
                    r = r.style(stripe_style());
                }
                rrows.push(r);
            }
            let rtable = Table::new(
                rrows,
                win.iter()
                    .map(|&ci| Constraint::Length(widths[ci] as u16))
                    .collect::<Vec<_>>(),
            )
            .header(Row::new(rheader))
            .column_spacing(1);
            f.render_widget(rtable, right_area);
        }
    }

    // ── reference-row marker (R91) ──
    // The pinned row gets a light `❮` at the right edge of the results pane. It
    // is an overlay, so it never perturbs the column layout, and it is only
    // drawn when the row is inside the loaded window (paging past it shows
    // nothing instead of issuing a query). The pinned-result strip above the
    // live grid has focus forced to the sidebar, so it never inherits the
    // live grid's reference marker.
    if focused {
        if let Some(rd) = ref_display_row_in(app, nrows) {
            if rd >= start && rd < start + h && inner.width > 0 {
                let y = inner.y + 1 + (rd - start) as u16;
                let x = inner.x + inner.width - 1;
                if y < inner.y + inner.height {
                    f.buffer_mut().set_stringn(
                        x,
                        y,
                        "❮",
                        1,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }
    }

    // ── empty-result guidance (R82) ──
    // A grid with columns but no rows still shows its header; one gray line
    // names the next action so a zero-row result is never a dead end.
    if nrows == 0 && inner.height >= 2 {
        let hint = match kind {
            GridKind::MongoDocs => t("无匹配文档 · f 改过滤 · i 插入文档"),
            GridKind::TableData => t("0 行 · f 改过滤 / Ctrl-R 清除"),
            GridKind::RedisValue => t("无数据"),
            _ => t("0 行 · 修改 SQL 后 Ctrl-J 重新执行"),
        };
        let hint_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            hint_area,
        );
    }

    // ── horizontal scroll progress bar (drawn on the bottom border) ──
    app.rects.hbar_visible = false;
    app.rects.hbar_prev = Rect::default();
    app.rects.hbar_next = Rect::default();
    let scrollable_total = ncols.saturating_sub(pinned.len());
    // R47b: auto-hide. The bar only appears for a moment after a horizontal
    // scroll (`poke_hbar`), so the bottom border is not a permanent thick band.
    // The `列 k/N` readout in the status line still names the window at rest.
    let hbar_shown = hbar_should_show(app.hbar_until, Instant::now());
    // The bar sits on the bottom border, so a zero-height results pane (a tiny
    // terminal squeezes the pane to nothing) has no row to draw it on and
    // `area.height - 1` would underflow.
    if hbar_shown && visible > 0 && scrollable_total > visible && inner_w >= 16 && area.height > 0 {
        // The window's position within the *scrollable* sequence (the pinned
        // block is not part of it).
        let win_start = off.saturating_sub(pinned.iter().filter(|&&c| c < off).count());
        let (first_num, last_num) = match (win.first(), win.last()) {
            (Some(f), Some(l)) => (f + 1, l + 1),
            _ => (off + 1, off + visible),
        };
        let pin = frozen_label(&pinned);
        let label = tf(
            "列 {}{}-{}/{}",
            &[&(pin), &(first_num), &(last_num), &(ncols)],
        );
        // One cell at each end is a tap target for panning a whole window — the
        // touch-friendly control for phones whose terminal sends no h-wheel.
        let track = inner_w - 2;
        let label_w = disp_width(&label);
        let bar_len = if track > label_w + 6 {
            track - label_w - 1
        } else {
            track
        };
        let (ts, tl) = scrollbar_geom(scrollable_total, win_start, visible, bar_len);
        let tl = tl.max(1).min(bar_len);
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled("◀", Style::default().fg(Color::LightGreen)));
        // R47b: a dashed mid-line track plus a heavy mid-line thumb. Both sit on
        // the same thin line, so the bar reads as one hairline with a bright
        // segment instead of the old `▁`/`▄` band that stood half a cell tall.
        if ts > 0 {
            spans.push(Span::styled(
                "┄".repeat(ts),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            "━".repeat(tl),
            Style::default().fg(Color::LightGreen),
        ));
        let after = bar_len.saturating_sub(ts + tl);
        if after > 0 {
            spans.push(Span::styled(
                "┄".repeat(after),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if bar_len < track {
            let pad = track - bar_len - label_w;
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            spans.push(Span::styled(label, Style::default().fg(Color::Gray)));
        }
        spans.push(Span::styled("▶", Style::default().fg(Color::LightGreen)));
        let bar_area = Rect {
            x: inner.x,
            y: area.y + area.height - 1,
            width: inner_w as u16,
            height: 1,
        };
        f.render_widget(Paragraph::new(Line::from(spans)), bar_area);
        app.rects.hbar_prev = Rect {
            x: inner.x,
            y: bar_area.y,
            width: 1,
            height: 1,
        };
        app.rects.hbar_next = Rect {
            x: inner.x + inner_w as u16 - 1,
            y: bar_area.y,
            width: 1,
            height: 1,
        };
        app.rects.hbar = Rect {
            x: inner.x + 1,
            y: bar_area.y,
            width: bar_len as u16,
            height: 1,
        };
        app.rects.hbar_visible = bar_len > 0;
    }

    // ── vertical position indicator (drawn on the right border) ──
    let track_h = inner.height as usize;
    if track_h >= 3 {
        let win = h.min(nrows.saturating_sub(start)).max(1);
        let (v_total, v_start) = match (&app.page_state, kind) {
            (Some(ps), GridKind::TableData) => match ps.total {
                Some(t) if !ps.total_lower_bound && (t as usize) > win => {
                    (t as usize, ps.page * ps.page_size + start)
                }
                _ => (nrows, start),
            },
            _ => (nrows, start),
        };
        if v_total > win {
            let lines = vbar_lines(v_total, v_start, win, track_h);
            let v_area = Rect {
                x: area.x + area.width - 1,
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            f.render_widget(Paragraph::new(lines), v_area);
        }
    }
}

/// Thumb geometry for a scrollbar track: `(thumb_start, thumb_len)` within
/// `track` cells for a window of `win_len` at `win_start` out of `total` items.
pub(crate) fn scrollbar_geom(
    total: usize,
    win_start: usize,
    win_len: usize,
    track: usize,
) -> (usize, usize) {
    if total == 0 || track == 0 {
        return (0, 0);
    }
    let win_len = win_len.clamp(1, total);
    if win_len >= total {
        return (0, track);
    }
    let thumb_len = ((win_len * track) / total).max(1).min(track);
    let max_start = total - win_len;
    let travel = track - thumb_len;
    let start = (win_start.min(max_start) * travel)
        .checked_div(max_start)
        .unwrap_or(0);
    (start.min(travel), thumb_len)
}

pub(crate) fn vbar_lines(
    total: usize,
    start: usize,
    win: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let (ts, tl) = scrollbar_geom(total, start, win, height);
    let tl = tl.max(1).min(height);
    (0..height)
        .map(|i| {
            if i >= ts && i < ts + tl {
                // Half-width block: a thin vertical thumb on the right border.
                Line::from(Span::styled("▐", Style::default().fg(Color::LightGreen)))
            } else {
                Line::from(Span::styled("│", Style::default().fg(Color::DarkGray)))
            }
        })
        .collect()
}

/// True when `col` appears as a whole identifier inside the filter expression
/// (quote characters are ignored).
pub(crate) fn filter_mentions(filter: &str, col: &str) -> bool {
    if filter.trim().is_empty() || col.is_empty() {
        return false;
    }
    let cleaned: String = filter
        .chars()
        .filter(|c| !matches!(c, '`' | '"' | '[' | ']'))
        .collect();
    let hay = cleaned.to_ascii_lowercase();
    let needle = col.to_ascii_lowercase();
    let (hb, nb) = (hay.as_bytes(), needle.as_bytes());
    let (n, m) = (hb.len(), nb.len());
    if m == 0 || m > n {
        return false;
    }
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + m <= n {
        if &hb[i..i + m] == nb {
            let before_ok = i == 0 || !ident(hb[i - 1]);
            let after_ok = i + m == n || !ident(hb[i + m]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Table-structure field list: fixed percentage widths, but still shows the cell
/// cursor and focused-cell highlight for consistency with data grids.
pub(crate) fn render_columns_grid(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    grid: &Grid,
    block: Block,
    gutter: u16,
) {
    let widths = [
        Constraint::Length(gutter),
        Constraint::Percentage(22),
        Constraint::Percentage(20),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Percentage(22),
        Constraint::Percentage(26),
    ];
    let cc = app.col_cursor;
    let num_fmt = app.num_fmt;
    let stripe = app.stripe;
    let mut header = vec![gutter_header_cell()];
    header.extend(grid.columns.iter().enumerate().map(|(ci, c)| {
        let shown = fix_double_encoding(c);
        col_header_cell(&shown, disp_width(&shown), ci == cc, None, false)
    }));
    let rows: Vec<Row> = grid
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut cells = vec![gutter_cell(i, i == app.sel)];
            cells.extend(row.iter().enumerate().map(|(ci, v)| {
                cell_widget_hl(
                    v,
                    40,
                    i == app.sel && ci == cc,
                    None,
                    None,
                    false,
                    grid.col_type(ci),
                    num_fmt,
                )
            }));
            let mut r = Row::new(cells);
            if i == app.sel {
                r = r.style(highlight_style());
            } else if stripe && i % 2 == 1 {
                r = r.style(stripe_style());
            }
            r
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(Row::new(header))
        .column_spacing(1)
        .block(block);
    f.render_widget(table, area);
}

pub(crate) fn gutter_header_cell() -> Cell<'static> {
    Cell::from(Span::styled("#", Style::default().fg(Color::DarkGray)))
}

pub(crate) fn gutter_cell(i: usize, selected: bool) -> Cell<'static> {
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Cell::from(Span::styled(format!("{}", i + 1), style))
}

pub(crate) fn col_header_cell(
    name: &str,
    w: usize,
    current: bool,
    sort: Option<(bool, usize)>,
    filtered: bool,
) -> Cell<'static> {
    let mut suffix = String::new();
    if let Some((desc, rank)) = sort {
        suffix.push(' ');
        suffix.push(if desc { '▼' } else { '▲' });
        if rank > 1 {
            suffix.push_str(&rank.to_string());
        }
    }
    if filtered {
        suffix.push_str(" ⚑");
    }
    let sw = disp_width(&suffix);
    let text = if w > sw {
        format!("{}{}", truncate_disp(name, w - sw), suffix)
    } else {
        truncate_disp(suffix.trim_start(), w)
    };
    let style = if current {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if sort.is_some() {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if filtered {
        Style::default()
            .fg(Color::LightMagenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    Cell::from(Span::styled(text, style))
}

pub(crate) fn highlight_style() -> Style {
    Style::default()
        .bg(Color::Rgb(38, 48, 38))
        .add_modifier(Modifier::BOLD)
}

/// R76: alternate-row banding in the result grid. Uses the terminal's own dim
/// attribute rather than a fixed background, so the band reads as a subtle
/// difference on a light *and* a dark theme (a hard-coded grey would be too
/// strong on one and invisible on the other). The cursor row / row-select block
/// style replaces it, so the active row is never dimmed.
pub(crate) fn stripe_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// R57: a row inside the rows-select block — full-row reverse video so a
/// multi-row selection reads as one solid band, clearly distinct from the
/// single cursor row's dark highlight above.
pub(crate) fn row_select_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

pub(crate) fn focused_cell_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::LightCyan)
        .add_modifier(Modifier::BOLD)
}

/// Italic is a nicety, not a requirement: some terminals ignore it, some render
/// it as reverse video. The grey foreground alone still separates a real NULL
/// from ordinary text, so an unsupported italic degrades to grey-only. Set
/// `DBXT_NO_ITALIC=1` to force that fallback (or when the font's italic is hard
/// to read on a light theme).
pub(crate) fn italic_supported() -> bool {
    match std::env::var("DBXT_NO_ITALIC") {
        Ok(v) => {
            let v = v.trim();
            !(v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        }
        Err(_) => true,
    }
}

/// The style that marks a real SQL NULL: grey, italic when the terminal can.
pub(crate) fn null_style() -> Style {
    let mut style = Style::default().fg(Color::DarkGray);
    if italic_supported() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// The style for an empty string — grey, never italic, and always drawn as
/// `''` so it can never be mistaken for NULL.
pub(crate) fn empty_string_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Text and style a value should be drawn with, shared by the grid, the cell /
/// row modals and the edit dialog so every surface tells the same story:
/// `NULL` = grey italic, `''` = grey, anything else = plain. A literal string
/// `"NULL"` stays plain, which is exactly how it stays distinct from the real
/// thing.
pub(crate) fn value_display(v: &Val) -> (String, Style) {
    match v {
        Val::Null => ("NULL".to_string(), null_style()),
        Val::Text(s) if s.is_empty() => ("''".to_string(), empty_string_style()),
        Val::Text(s) => (s.clone(), Style::default()),
    }
}

/// Style for a cell whose value matches the active result search.
pub(crate) fn search_hit_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

/// R64: the cell the in-result find last jumped to. Same channel as
/// [`search_hit_style`], one shade louder, so the `3/7` count and the accent
/// cell always agree (the grid twin of the R61 editor find).
pub(crate) fn find_current_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::LightGreen)
        .add_modifier(Modifier::BOLD)
}

/// R58: longest value shown inline in a grid cell. A longer value collapses to
/// its first 38 display columns plus `…`, signalling that `v` (the R53 cell
/// popup) holds the full text. Kept as a constant so the width pass and the
/// render pass agree.
pub(crate) const CELL_TEXT_MAX: usize = 40;

/// R58: inline representation of a cell value. Values wider than
/// [`CELL_TEXT_MAX`] collapse to `first 38…`; everything else is returned as-is
/// (the caller still clamps to the column width).
pub(crate) fn abbreviate_cell_text(text: &str) -> String {
    if disp_width(text) > CELL_TEXT_MAX {
        truncate_disp(text, CELL_TEXT_MAX - 1)
    } else {
        text.to_string()
    }
}

/// R58: a cell value's contribution to its column width, capped at the inline
/// abbreviation so a long value no longer stretches its column past what is
/// actually drawn. R76: the display-formatted variant lives in `numfmt`; this
/// unchanged-value form is what the tests pin.
#[cfg(test)]
pub(crate) fn cell_text_width(v: &Val) -> usize {
    let w = disp_width(v.text());
    if w > CELL_TEXT_MAX {
        CELL_TEXT_MAX - 1
    } else {
        w
    }
}

/// Render one cell, optionally marking it as the focused cell or highlighting a
/// search hit. `needle` is the `/` row-search substring; `find` is the R64
/// cell-find substring and `find_current` marks the match the cursor landed on
/// (painted with the accent colour even while focused, so `n`/`N` have a clear
/// anchor). `col_type` / `mode` drive the R76 big-number display layer, which
/// only changes the drawn text — never the underlying value. The signature grew
/// for that pair, so the argument-count lint is waived here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cell_widget_hl(
    v: &Val,
    w: usize,
    focused: bool,
    needle: Option<&str>,
    find: Option<&str>,
    find_current: bool,
    col_type: Option<&str>,
    mode: NumFmt,
) -> Cell<'static> {
    let is_hit = |n: Option<&str>| {
        n.is_some_and(|n| {
            let s = match v {
                Val::Null => "null",
                Val::Text(s) => s.as_str(),
            };
            !n.is_empty() && s.to_lowercase().contains(n)
        })
    };
    let (text, base_style) = display_value(v, col_type, mode);
    let shown = truncate_disp(&abbreviate_cell_text(&text), w);
    if find_current {
        return Cell::from(Span::styled(shown, find_current_style()));
    }
    if focused {
        return Cell::from(Span::styled(shown, focused_cell_style()));
    }
    if is_hit(needle) || is_hit(find) {
        return Cell::from(Span::styled(shown, search_hit_style()));
    }
    Cell::from(Span::styled(shown, base_style))
}

pub(crate) fn render_script_list(f: &mut Frame, area: Rect, app: &mut App, script: &ScriptView) {
    let focused = app.focus == Focus::Preview;
    let errors = script.outcomes.iter().filter(|o| o.error.is_some()).count();
    let affected: u64 = script.outcomes.iter().map(|o| o.affected).sum();
    let title = tf(
        " 脚本 · {} 条语句 · 影响 {} 行 · {} 错误 · Enter 查看结果 ",
        &[&(script.outcomes.len()), &(affected), &(errors)],
    );
    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(18),
        Constraint::Length(9),
    ];
    let rows: Vec<Row> = script
        .outcomes
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let status = match &o.error {
                Some(e) => format!("✗ {}", truncate_disp(&one_line(e), 16)),
                None if !o.grid.columns.is_empty() => tf("{} 行", &[&(o.grid.rows.len())]),
                None => tf("影响 {} 行", &[&(o.affected)]),
            };
            let style = if o.error.is_some() {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let mut r = Row::new(vec![
                Cell::from(Span::styled(
                    format!("{}", i + 1),
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::raw(truncate_disp(&one_line(&o.sql), 80))),
                Cell::from(Span::styled(status, style)),
                Cell::from(Span::styled(
                    format!("{}ms", o.ms),
                    Style::default().fg(Color::DarkGray),
                )),
            ]);
            if i == script.sel {
                r = r.style(highlight_style());
            }
            r
        })
        .collect();
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["#", t("语句"), t("结果"), t("耗时")]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .column_spacing(1)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        );
    f.render_widget(table, area);
}

/// Compact elapsed-time label for the script console: `12ms`, `1.23s`, `2.0m`.
pub(crate) fn format_elapsed_ms(ms: u128) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}m", ms as f64 / 60_000.0)
    }
}

/// R42 console-style script output: a separator line per statement carrying the
/// statement number and elapsed time, then the statement and its result. Shown
/// only when `show_stmt_timing` is on (`Alt-O`); the plain table is the default.
pub(crate) fn render_script_stream(f: &mut Frame, area: Rect, app: &mut App, script: &ScriptView) {
    let focused = app.focus == Focus::Preview;
    let errors = script.outcomes.iter().filter(|o| o.error.is_some()).count();
    let affected: u64 = script.outcomes.iter().map(|o| o.affected).sum();
    let title = tf(
        " 脚本输出 · {} 条 · 影响 {} 行 · {} 错误 · Alt-O 关分隔 ",
        &[&(script.outcomes.len()), &(affected), &(errors)],
    );
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2).max(1) as usize;
    // Three lines per statement: separator, statement, result.
    let mut lines: Vec<Line> = Vec::with_capacity(script.outcomes.len() * 3);
    for (i, o) in script.outcomes.iter().enumerate() {
        let head = format!("── #{} · {} ", i + 1, format_elapsed_ms(o.ms));
        let pad = inner_w.saturating_sub(disp_width(&head));
        let sep_style = Style::default().fg(Color::DarkGray);
        lines.push(Line::from(Span::styled(
            format!("{head}{}", "─".repeat(pad)),
            if i == script.sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                sep_style
            },
        )));
        let sql_style = if i == script.sel {
            highlight_style()
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            truncate_disp(&one_line(&o.sql), inner_w),
            sql_style,
        )));
        let (status, status_style) = match &o.error {
            Some(e) => (
                format!(
                    "  ✗ {}",
                    truncate_disp(&one_line(e), inner_w.saturating_sub(4))
                ),
                Style::default().fg(Color::Red),
            ),
            None if !o.grid.columns.is_empty() => (
                format!("  ✓ {}", tf("{} 行", &[&o.grid.rows.len()])),
                Style::default().fg(Color::Green),
            ),
            None => (
                format!("  ✓ {}", tf("影响 {} 行", &[&o.affected])),
                Style::default().fg(Color::Green),
            ),
        };
        lines.push(Line::from(Span::styled(status, status_style)));
    }
    // Keep the selected statement visible: 3 lines per entry.
    let sel_line = script.sel * 3;
    let scroll = sel_line.saturating_sub(inner_h.saturating_sub(3)) as u16;
    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        ),
        area,
    );
}

pub(crate) fn render_ddl(f: &mut Frame, area: Rect, app: &mut App, ddl: &str) {
    let focused = app.focus == Focus::Preview;
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<String> = wrap_text(ddl, inner_w);
    let total = lines.len();
    let max_scroll = total.saturating_sub(inner_h) as u16;
    if app.ddl_scroll > max_scroll {
        app.ddl_scroll = max_scroll;
    }
    let table = app
        .selected_table()
        .map(|t| t.name.clone())
        .unwrap_or_default();
    let body: Vec<Line> = lines
        .iter()
        .map(|l| {
            let trimmed = l.trim_start();
            let indent = l.len() - trimmed.len();
            let style = if trimmed.starts_with("--") {
                Style::default().fg(Color::DarkGray)
            } else if is_ddl_keyword_line(trimmed) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(trimmed.to_string(), style),
            ])
        })
        .collect();
    let title = tf(
        " 表结构 (DDL) · {} · {}/{} 行 · t 返回字段 ",
        &[
            &(fix_double_encoding(&table)),
            &((app.ddl_scroll as usize + inner_h).min(total)),
            &(total),
        ],
    );
    f.render_widget(
        Paragraph::new(body).scroll((app.ddl_scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        ),
        area,
    );
}

pub(crate) fn is_ddl_keyword_line(s: &str) -> bool {
    let up = s.to_ascii_uppercase();
    up.starts_with("CREATE ")
        || up.starts_with("PRIMARY KEY")
        || up.starts_with("UNIQUE KEY")
        || up.starts_with("KEY ")
        || up.starts_with("CONSTRAINT")
        || up.starts_with("FOREIGN KEY")
        || up.starts_with(")")
        || up.starts_with("ENGINE")
        || up.starts_with("DEFAULT CHARSET")
}

pub(crate) fn render_console(f: &mut Frame, area: Rect, app: &App) {
    // Console log: keep the newest output visible by scrolling to the bottom.
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .cmd_output
        .iter()
        .flat_map(|entry| entry.lines().map(Line::raw))
        .collect();
    let rows: usize = lines
        .iter()
        .map(|l| {
            let w = l.width();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_w)
            }
        })
        .sum();
    let scroll_y = rows.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(t(" 输出 "))
                    .border_set(border::ROUNDED)
                    .border_style(border_style(app.focus == Focus::Preview)),
            ),
        area,
    );
}

pub(crate) fn render_sidebar(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Sidebar;

    let Some(c) = app.selected.clone() else {
        // no connection selected: sidebar is just a placeholder (picker overlay does the job)
        let hint = if app.picker_open {
            ""
        } else {
            t("q 显示\n连接列表")
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(t(" 连接 "))
                        .border_set(border::ROUNDED)
                        .border_style(border_style(focused)),
                ),
            area,
        );
        return;
    };

    // Redis connections browse keys, not databases/tables: keep the flat key
    // browser (the tree only makes sense for SQL / Mongo).
    if app.backend_kind == Backend::Redis {
        let conn_color = connection_color(&c);
        let mut lines: Vec<Line> = Vec::new();
        // R47b: the same status dot the SQL/Mongo tree draws — Redis shares the
        // LocalBackend pool, so `remove_connection_pools` disconnects it too.
        let status = conn_status_for(app, &c.id);
        let dot_style = match status {
            ConnStatus::Active => Style::default().fg(conn_color),
            ConnStatus::Connecting => Style::default().fg(conn_color).add_modifier(Modifier::BOLD),
            ConnStatus::Idle => Style::default().fg(conn_color).add_modifier(Modifier::DIM),
        };
        let name_style = match status {
            ConnStatus::Active => Style::default().fg(conn_color).add_modifier(Modifier::BOLD),
            ConnStatus::Connecting => Style::default().fg(conn_color),
            ConnStatus::Idle => Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", status.shape()), dot_style),
            Span::styled(c.name.clone(), name_style),
        ]));
        if sidebar_db_row(app) {
            let label = sidebar_db_label(app);
            let w = (area.width as usize).saturating_sub(4).max(6);
            lines.push(Line::from(vec![
                Span::styled("▤ ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    truncate_disp(&label, w),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        render_redis_sidebar(f, area, app, &mut lines);
        return;
    }

    // SQL / MongoDB: the connection → database → table tree (R43).
    rebuild_side_rows(app);
    let mut lines: Vec<Line> = Vec::new();

    // filter row: the active `/` filter, the `f` quick search, or the hints.
    let searching = app.tree_search_prompt.is_some();
    let filter_rows = if app.tables_all.is_empty() && !searching {
        0
    } else {
        1
    };
    if filter_rows > 0 {
        let w = (area.width as usize).saturating_sub(4).max(6);
        let (mark, text, style) = if searching {
            let (mark, text) = if app.tree_search.is_empty() {
                ("f ", t("f 搜索连接 / 库 / 表").to_string())
            } else {
                (
                    "▸ ",
                    format!("f{} · {}", app.tree_search, tree_search_hits(app)),
                )
            };
            (mark, text, Style::default().fg(Color::Magenta))
        } else if app.table_filter.is_empty() {
            (
                "/ ",
                t("/ 过滤表名").to_string(),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (
                "▸ ",
                format!(
                    "/{} · {}/{}",
                    app.table_filter,
                    app.tables.len(),
                    app.tables_all.len()
                ),
                Style::default().fg(Color::Yellow),
            )
        };
        lines.push(Line::from(vec![
            Span::styled(mark, Style::default().fg(Color::Yellow)),
            Span::styled(truncate_disp(&text, w), style),
        ]));
    }

    let cap = (area.height as usize)
        .saturating_sub(2 + filter_rows)
        .max(1);
    let n = app.side_rows.len();
    let sel = if n == 0 { 0 } else { app.side_sel.min(n - 1) };
    let start = if n <= cap {
        0
    } else {
        sel.saturating_sub(cap / 2).min(n - cap)
    };
    let needle = if searching {
        app.tree_search.trim().to_ascii_lowercase()
    } else {
        app.table_filter.trim().to_ascii_lowercase()
    };
    for i in start..(start + cap).min(n) {
        let row = app.side_rows[i].clone();
        lines.push(side_row_line(app, &row, i == sel, &needle, area.width));
    }

    let title = if app.table_filter.is_empty() {
        format!(" {} ({}) ", c.name, app.tables_all.len())
    } else {
        tf(
            " {} 表 {}/{} ",
            &[&(c.name), &(app.tables.len()), &(app.tables_all.len())],
        )
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        ),
        area,
    );
}

/// Render one row of the sidebar connection tree: an expand triangle, a
/// connection / database symbol and the name, indented two cells per level. The
/// selected row is filled so the single tree cursor is unmistakable.
pub(crate) fn side_row_line(
    app: &App,
    row: &SideRow,
    selected: bool,
    needle: &str,
    area_w: u16,
) -> Line<'static> {
    let depth = side_row_depth(row);
    let indent = "  ".repeat(depth);
    let mk = |s: Style| -> Style {
        if selected {
            s.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            s
        }
    };
    let inner = (area_w as usize).saturating_sub(2 + indent.len()).max(4);
    let mut spans: Vec<Span> = vec![Span::styled(indent, mk(Style::default()))];
    match row {
        SideRow::Group {
            id,
            name,
            depth: _,
            count,
            open,
        } => {
            spans.push(Span::styled(
                if *open { "▾ " } else { "▸ " }.to_string(),
                mk(Style::default().fg(Color::DarkGray)),
            ));
            spans.push(Span::styled(
                "📁 ".to_string(),
                mk(Style::default().fg(Color::Yellow)),
            ));
            let badge = format!(" [{count}]");
            let name_w = inner.saturating_sub(disp_width(&badge));
            // R55: while this group is being renamed, draw the live edit buffer
            // (with a caret) in place of the name.
            let editing = app
                .rename_edit
                .as_ref()
                .filter(|e| matches!(&e.target, RenameTarget::Group { id: gid } if gid == id));
            match editing {
                Some(e) => spans.push(Span::styled(
                    format!("{}▏", e.text),
                    mk(Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)),
                )),
                None => spans.push(Span::styled(
                    truncate_disp(name, name_w),
                    mk(Style::default().add_modifier(Modifier::BOLD)),
                )),
            }
            spans.push(Span::styled(
                badge,
                mk(Style::default().fg(Color::DarkGray)),
            ));
            // The group's fold memory is keyed by id, so a filter that hides
            // then reveals it keeps the user's choice.
            let _ = id;
        }
        SideRow::Conn { idx, .. } => {
            let (cid, name, color, open, ro) = match side_root_cfg(app, *idx) {
                Some(c) => (
                    c.id.clone(),
                    c.name.clone(),
                    connection_color(c),
                    side_conn_open(app, *idx),
                    c.read_only,
                ),
                None => (String::new(), String::new(), Color::Gray, false, false),
            };
            let status = side_conn_status(app, *idx);
            spans.push(Span::styled(
                if open { "▾ " } else { "▸ " }.to_string(),
                mk(Style::default().fg(Color::DarkGray)),
            ));
            // R47b: shape carries the state (● active / ○ idle / ◐ connecting),
            // the connection colour carries identity. Idle dots are dimmed so a
            // disconnected root reads as muted without losing its colour.
            let dot_style = match status {
                ConnStatus::Active => Style::default().fg(color),
                ConnStatus::Connecting => Style::default().fg(color).add_modifier(Modifier::BOLD),
                ConnStatus::Idle => Style::default().fg(color).add_modifier(Modifier::DIM),
            };
            spans.push(Span::styled(format!("{} ", status.shape()), mk(dot_style)));
            if ro {
                spans.push(Span::styled(
                    "🔒 ".to_string(),
                    mk(Style::default().fg(color)),
                ));
            }
            // R55: while this root is being renamed, draw the live edit buffer
            // (with a caret) instead of the name.
            let editing = app
                .rename_edit
                .as_ref()
                .filter(|e| matches!(&e.target, RenameTarget::Conn { id } if *id == cid));
            if let Some(e) = editing {
                spans.push(Span::styled(
                    format!("{}▏", e.text),
                    mk(Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)),
                ));
                return Line::from(spans);
            }
            let name_w = inner.saturating_sub(if ro { 2 } else { 0 });
            // A disconnected root's name is muted grey so it cannot be mistaken
            // for a live connection at a glance.
            let name_style = match status {
                ConnStatus::Active => Style::default().fg(color).add_modifier(Modifier::BOLD),
                ConnStatus::Connecting => Style::default().fg(color),
                ConnStatus::Idle => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            };
            spans.push(Span::styled(truncate_disp(&name, name_w), mk(name_style)));
        }
        SideRow::ConnLoading { .. } => {
            spans.push(Span::styled(
                truncate_disp(t("… 加载库列表"), inner + 2),
                mk(Style::default().fg(Color::DarkGray)),
            ));
        }
        SideRow::ConnError { msg, .. } => {
            spans.push(Span::styled(
                format!("✗ {}", truncate_disp(msg, inner)),
                mk(Style::default().fg(Color::Red)),
            ));
        }
        SideRow::Db { idx, db, .. } => {
            let is_cur = side_is_active(app, *idx);
            let is_cur_db = is_cur && *db == app.current_db();
            let open = is_cur_db
                && side_root_cfg(app, *idx)
                    .is_some_and(|c| !app.tree_db_closed.contains(&db_node_key(&c.id, db)));
            let tri = if is_cur_db && open { "▾ " } else { "▸ " };
            spans.push(Span::styled(
                tri.to_string(),
                mk(Style::default().fg(Color::DarkGray)),
            ));
            spans.push(Span::styled(
                "▤ ".to_string(),
                mk(Style::default().fg(Color::Cyan)),
            ));
            let disp = fix_double_encoding(db);
            // R47b: a database under a disconnected connection is a cached,
            // muted row — still visible so the tree keeps its shape, but dimmed
            // so it reads as stale. Clicking it reconnects (switch_to_db).
            let live = side_root_cfg(app, *idx).is_some_and(|c| conn_is_live(app, &c.id));
            let mut style = Style::default()
                .fg(if is_cur { Color::Cyan } else { Color::Gray })
                .add_modifier(Modifier::BOLD);
            if !live {
                style = style.add_modifier(Modifier::DIM);
            }
            let style = mk(style);
            let hit = style.add_modifier(Modifier::UNDERLINED);
            spans.extend(highlight_match_spans(
                &truncate_disp(&disp, inner),
                needle,
                style,
                hit,
            ));
        }
        SideRow::Table { table, .. } => {
            let view = app
                .tables
                .get(*table)
                .is_some_and(|t| t.table_type.eq_ignore_ascii_case("VIEW"));
            let name = app
                .tables
                .get(*table)
                .map(|t| fix_double_encoding(&qualified_display(&app.schema, &t.name)))
                .unwrap_or_default();
            let w = inner.saturating_sub(usize::from(view));
            let disp = truncate_table_name(&name, w);
            let style = mk(if view {
                Style::default().fg(Color::Blue)
            } else {
                Style::default()
            });
            let hit = style.add_modifier(Modifier::UNDERLINED);
            spans.extend(highlight_match_spans(&disp, needle, style, hit));
            if view {
                spans.push(Span::styled(
                    "~".to_string(),
                    mk(Style::default().fg(Color::Blue)),
                ));
            }
        }
    }
    // R45 size column: right-aligned + muted. It only appears when the terminal
    // is wide enough (<56 hides it so the table name keeps the width).
    if let Some(size) = side_row_size(app, row) {
        let full_w = (area_w as usize).saturating_sub(2);
        let used: usize = spans.iter().map(|s| disp_width(&s.content)).sum();
        let size_w = disp_width(&size);
        let pad = full_w.saturating_sub(used + size_w);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), mk(Style::default())));
        }
        spans.push(Span::styled(size, mk(Style::default().fg(Color::DarkGray))));
    }
    // R50: a selected row fills the whole inner width so the highlight reads as
    // one solid band instead of stopping at the last glyph. The size column
    // already pads its own row, so this tops up whatever is still missing (and
    // pads the rows that have no size cell at all) — never a double pad. The
    // width is measured with `disp_width`, so wide emoji (`📁` / `▤` / `🔒`)
    // count as the two cells they actually occupy.
    if selected {
        let full_w = (area_w as usize).saturating_sub(2);
        let used: usize = spans.iter().map(|s| disp_width(&s.content)).sum();
        let pad = full_w.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), mk(Style::default())));
        }
    }
    Line::from(spans)
}

/// The right-aligned cell for a database (`2.1 GB`) or table (`1.2k` row
/// estimate) row, or `None` when there is nothing (or no room) to show (R45).
/// R73: a connection root instead shows its connect-time latency (R63 cache),
/// or `-` when this session never probed it.
pub(crate) fn side_row_size(app: &App, row: &SideRow) -> Option<String> {
    // Width first: a narrow screen gives every cell to the name.
    if app.term_w < 56 {
        return None;
    }
    match row {
        SideRow::Conn { idx, .. } => {
            let id = side_root_cfg(app, *idx).map(|c| c.id.clone())?;
            Some(
                app.server_rtts
                    .get(&id)
                    .map(|d| format_rtt(*d))
                    .unwrap_or_else(|| "-".to_string()),
            )
        }
        SideRow::Db { db, .. } => match app.db_size_state.get(db) {
            Some(TreeDbState::Loading) => Some("…".to_string()),
            Some(TreeDbState::Error(_)) => Some("✗".to_string()),
            None => app
                .db_sizes
                .get(db)
                .and_then(|i| i.total_bytes)
                .map(human_bytes),
        },
        SideRow::Table { table, .. } => {
            let name = app.tables.get(*table)?.name.to_lowercase();
            app.db_sizes
                .get(&app.current_db())
                .and_then(|i| i.rows.get(&name))
                .map(|n| human_count(*n))
        }
        _ => None,
    }
}

/// Single-letter type badge shown next to a key in the sidebar.
pub(crate) fn redis_type_badge(t: &str) -> (&'static str, Color) {
    match t.to_ascii_lowercase().as_str() {
        "string" => ("S", Color::Green),
        "list" => ("L", Color::Yellow),
        "set" => ("E", Color::Cyan),
        "zset" => ("Z", Color::Magenta),
        "hash" => ("H", Color::Blue),
        "stream" => ("X", Color::LightRed),
        "rejson-rl" | "json" => ("J", Color::LightYellow),
        _ => ("?", Color::DarkGray),
    }
}

/// The type badge shown for one key. On a narrow screen the TTL fuses into the
/// badge (`S·12s`) so the key name keeps its width and the row never wraps
/// (R42). R81 renders the compact TTL (`5m` / `2h` / `-1`).
pub(crate) fn redis_badge_token(narrow: bool, badge: &str, ttl: Option<i64>) -> String {
    match (narrow, ttl) {
        (true, Some(t)) => format!("{badge}·{}", redis_ttl_short(t)),
        _ => badge.to_string(),
    }
}

/// Sidebar body for a Redis connection: a `/` pattern row followed by the SCAN
/// key list with type + TTL badges.
pub(crate) fn render_redis_sidebar(f: &mut Frame, area: Rect, app: &App, lines: &mut Vec<Line>) {
    let focused = app.focus == Focus::Sidebar;
    let w = (area.width as usize).saturating_sub(4).max(6);
    let needle = app.redis_filter.trim().to_lowercase();
    // pattern row (server-side SCAN MATCH) + client-side filter row (R42).
    let (mark, mut text, style) = if app.redis_scan.pattern == "*" {
        (
            "/ ",
            t("/ 匹配模式（SCAN MATCH）").to_string(),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (
            "▸ ",
            format!(
                "/{} · {} keys",
                app.redis_scan.pattern,
                app.redis_scan.keys.len()
            ),
            Style::default().fg(Color::Yellow),
        )
    };
    // R81: surface the active type filter / TTL ordering on the same header row
    // so the layout maths below stay untouched.
    if let Some(ty) = &app.redis_type_filter {
        text.push_str(&format!(" · t:{ty}"));
    }
    if app.redis_sort != RedisSort::Scan {
        text.push_str(&format!(" · {}", app.redis_sort.label()));
    }
    lines.push(Line::from(vec![
        Span::styled(mark, Style::default().fg(Color::Yellow)),
        Span::styled(truncate_disp(&text, w), style),
    ]));
    if !app.redis_filter.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("f ", Style::default().fg(Color::Yellow)),
            Span::styled(
                truncate_disp(
                    &tf(
                        "{} · {} 命中",
                        &[&app.redis_filter, &(app.redis_scan.keys.len())],
                    ),
                    w,
                ),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    // Header rows: connection + db row + pattern row (+ filter row).
    let extra = if app.redis_filter.is_empty() { 0 } else { 1 };
    let cap = (area.height as usize).saturating_sub(5 + extra).max(1);
    let n = app.redis_scan.keys.len();
    let sel = app.redis_list.selected();
    let start = sel
        .unwrap_or(0)
        .saturating_sub(cap / 2)
        .min(n.saturating_sub(cap.min(n)));
    // On a narrow screen the type badge and TTL fuse into one `S·12s` token so a
    // key row always stays on a single line (R42). 42-column terminals (the
    // small-screen acceptance size) give the sidebar ~28 columns, so the
    // threshold sits just above that.
    let narrow = area.width < 30;
    for (i, key) in app.redis_scan.keys.iter().enumerate().skip(start).take(cap) {
        let (badge, color) = redis_type_badge(&key.key_type);
        // R81: the TTL is always shown (compact), including `-1` permanent.
        let ttl_num = Some(key.ttl);
        let badge_token = redis_badge_token(narrow, badge, ttl_num);
        let ttl = if narrow {
            String::new()
        } else {
            ttl_num
                .map(|t| format!(" {}", redis_ttl_short(t)))
                .unwrap_or_default()
        };
        let picked = app.redis_selected.contains(&key.key_raw);
        let marker = if sel == Some(i) { "▸" } else { " " };
        let check = if picked { "[x]" } else { "[ ]" };
        let check_style = if picked {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let prefix_w = 2 + badge_token.chars().count() + 1 + ttl.len();
        let name_w = w.saturating_sub(prefix_w).max(4);
        let name = truncate_disp(&fix_double_encoding(&key.key_display), name_w);
        let row_style = if sel == Some(i) {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(marker, row_style),
            Span::styled(check, check_style),
            Span::styled(
                badge_token,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", row_style),
        ];
        spans.extend(highlight_match_spans(
            &name,
            &needle,
            row_style,
            search_hit_style(),
        ));
        if !ttl.is_empty() {
            spans.push(Span::styled(ttl, Style::default().fg(Color::DarkGray)));
        }
        lines.push(Line::from(spans));
    }
    if !app.redis_scan.exhausted {
        lines.push(Line::from(Span::styled(
            t("  n 加载更多…"),
            Style::default().fg(Color::DarkGray),
        )));
    } else if n == 0 {
        lines.push(Line::from(Span::styled(
            if app.redis_filter.is_empty() {
                t("  （无匹配 key）").to_string()
            } else {
                tf("  （无「{}」命中）", &[&app.redis_filter])
            },
            Style::default().fg(Color::DarkGray),
        )));
    }

    let conn = app
        .selected
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let title = if app.redis_selected.is_empty() {
        format!(" {} · {} keys ", conn, n)
    } else {
        tf(
            " {} · {} keys · 已选 {} ",
            &[&(conn), &(n), &(app.redis_selected.len())],
        )
    };
    f.render_widget(
        Paragraph::new(lines.clone()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(border_style(focused)),
        ),
        area,
    );
}

/// Rendered value of one form row (passwords masked).
pub(crate) fn form_row_value(f: &ConnForm, row: FormRow) -> String {
    match row {
        FormRow::Name => f.name.clone(),
        FormRow::DbType => f.db_type.clone(),
        FormRow::Host => f.host.clone(),
        FormRow::Port => f.port.clone(),
        FormRow::Username => f.username.clone(),
        FormRow::Password => "*".repeat(f.password.chars().count()),
        FormRow::Database => f.database.clone(),
        FormRow::QueryTimeout => {
            let v = f.query_timeout.trim();
            if v.is_empty() {
                t("默认（60s）").to_string()
            } else if v == "0" {
                t("不限").to_string()
            } else {
                format!("{} s", v)
            }
        }
        FormRow::Ssl => if f.ssl { "y" } else { "n" }.to_string(),
        FormRow::ReadOnly => {
            if f.read_only {
                format!("y  {}", t("拒绝写语句"))
            } else {
                "n".to_string()
            }
        }
        FormRow::Color => {
            if f.color.trim().is_empty() {
                t("无（按类型）").to_string()
            } else {
                f.color.clone()
            }
        }
        FormRow::SshEnabled => if f.ssh_enabled { "y" } else { "n" }.to_string(),
        FormRow::SshHost => f.ssh_host.clone(),
        FormRow::SshPort => f.ssh_port.clone(),
        FormRow::SshUser => f.ssh_user.clone(),
        FormRow::SshAuth => f.ssh_auth.as_str().to_string(),
        FormRow::SshPassword => "*".repeat(f.ssh_password.chars().count()),
        FormRow::SshKeyPath => f.ssh_key_path.clone(),
        FormRow::SshKeyPassphrase => "*".repeat(f.ssh_key_passphrase.chars().count()),
        FormRow::SshAgentSock => f.ssh_agent_sock.clone(),
        FormRow::Save => {
            if f.edit_id.is_some() {
                t("↵ 更新连接").to_string()
            } else {
                t("↵ 保存连接").to_string()
            }
        }
    }
}

pub(crate) fn render_form(f: &mut Frame, area: Rect, app: &mut App) {
    let form = app.form.clone();
    let rows = form_rows(&form);
    let box_w = if app.layout_mode == LayoutMode::Narrow {
        area.width.saturating_sub(2)
    } else {
        52.min(area.width.saturating_sub(4))
    };
    // Two lines are reserved below the fields for the error / type hint.
    let reserved: u16 = 2;
    let want_h = (rows.len() as u16).saturating_add(reserved + 2);
    let box_h = want_h
        .min(area.height.saturating_sub(2))
        .max(3.min(area.height));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let box_area = Rect {
        x,
        y,
        width: box_w,
        height: box_h,
    };

    let inner_h = box_h.saturating_sub(2);
    let visible = inner_h.saturating_sub(reserved).max(1) as usize;
    let active = form.field.min(rows.len().saturating_sub(1));
    let mut scroll = form.scroll.min(rows.len().saturating_sub(visible));
    if active < scroll {
        scroll = active;
    }
    if active >= scroll + visible {
        scroll = active + 1 - visible;
    }
    app.form.scroll = scroll;

    let mut lines: Vec<Line> = Vec::new();
    // R41: on a very narrow terminal the label column shrinks and the SSH
    // section's labels are abbreviated so the value keeps a readable width. The
    // rows themselves already expand / collapse with the tunnel toggle.
    let narrow = app.layout_mode == LayoutMode::Narrow || box_w < 44;
    let label_w = if narrow { 9usize } else { 16 };
    let end = (scroll + visible).min(rows.len());
    for (i, (row, label)) in rows.iter().enumerate().take(end).skip(scroll) {
        let (row, label) = (*row, *label);
        let label = if narrow {
            form_label_short(label)
        } else {
            label
        };
        let is_active = i == active;
        // R58: while the query-timeout row is being edited, show its raw buffer
        // (not the derived `30 s` / `默认（60s）` display) so the caret sits where
        // the next digit will land.
        let mut value = if form.editing && is_active && row == FormRow::QueryTimeout {
            form.query_timeout.clone()
        } else {
            form_row_value(&form, row)
        };
        if form.editing && is_active {
            value.push('▏');
        }
        let marker = if is_active { "▸ " } else { "  " };
        let style = if is_active {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from({
            let mut spans = vec![Span::styled(
                format!("{marker}{label:<label_w$} {value}"),
                style,
            )];
            // Read-only colour preview swatch: the connection colour when set,
            // else the database-family default the sidebar will use.
            if row == FormRow::Color {
                let swatch =
                    parse_hex_color(&form.color).unwrap_or_else(|| db_type_color(&form.db_type));
                spans.push(Span::styled("  ███", Style::default().fg(swatch)));
            }
            spans
        }));
        // The kernel forwards the tunnel to the connection's own host:port —
        // there is no separate remote-target field in TransportLayerConfig, so
        // it is shown (and overridden by editing host/port above).
        if row == FormRow::SshAuth && form.ssh_enabled {
            let target_port = if form.port.trim().is_empty() {
                "?".to_string()
            } else {
                form.port.trim().to_string()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "  {:<label_w$} {}:{}",
                    if narrow { "remote" } else { t("远端目标") },
                    form.host.trim(),
                    target_port
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    if !form.err.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("✗ {}", form.err),
            Style::default().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "types: mysql postgres sqlite redis mongodb clickhouse sqlserver …",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let title = if form.edit_id.is_some() {
        t(" 编辑连接 ")
    } else {
        t(" 新建连接 ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Clear, box_area);
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}
