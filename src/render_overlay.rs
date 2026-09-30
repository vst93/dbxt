use crate::prelude::*;
use crate::*;

/// Height and top offset for a list / picker overlay so it always fits inside
/// `area`. `Clear` (and every ratatui widget) panics when asked to draw outside
/// the buffer, and a tiny terminal makes the naive `area.height - 2` shrink
/// below the three-row border minimum, so the box is clamped to the area here.
/// Center a `w × h` overlay box inside `area`, clamping both dimensions to the
/// area first. ratatui's widgets (and `Clear` in particular) panic when asked to
/// draw outside the buffer, and a tiny terminal can make a fixed overlay taller
/// than the screen, so every centered overlay goes through here.
/// Shared overlay width. On a narrow terminal the box takes the whole width so
/// it stays readable; otherwise it keeps a two-column margin on each side and is
/// capped at `max`. This one helper keeps every overlay's width policy uniform
/// (and is the single place the small-screen fallback is tuned).
pub(crate) fn overlay_width(area_width: u16, max: u16, min_avail: u16) -> u16 {
    let avail = area_width.saturating_sub(4);
    if avail < min_avail {
        area_width
    } else {
        avail.min(max)
    }
}

/// Build an overlay title from a `prefix`, a list of key hints and a `suffix`,
/// dropping whole hints when they do not fit so a title is never cut mid-key.
/// This is the overlay-title counterpart of [`footer_select`]: key hints survive
/// longest, and the border is never clipped through the middle of a hint.
pub(crate) fn overlay_hint_title(width: u16, prefix: &str, hints: &[Hint], suffix: &str) -> String {
    let budget = width.saturating_sub(2) as usize;
    let mut out = prefix.to_string();
    let mut used = disp_width(&out) + disp_width(suffix);
    for (key, desc) in hints {
        let piece = format!(" · {key} {desc}");
        let pw = disp_width(&piece);
        if used + pw <= budget {
            out.push_str(&piece);
            used += pw;
        } else {
            break;
        }
    }
    out.push_str(suffix);
    out
}

pub(crate) fn centered_overlay(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Pick a title that fits `box_width` (the overlay's outer width). ratatui clips
/// an over-long title mid-word, so a short variant is substituted when the full
/// one would not fit — the R39 "titles are never truncated" rule. Two columns
/// are reserved for the border corners.
pub(crate) fn fit_title(full: &str, short: &str, box_width: u16) -> String {
    if disp_width(full) <= box_width.saturating_sub(2) as usize {
        full.to_string()
    } else {
        short.to_string()
    }
}

pub(crate) fn overlay_list_box(rows: usize, area: Rect) -> (u16, u16) {
    if area.height == 0 {
        return (area.y, 0);
    }
    let want = (rows as u16).saturating_add(2);
    let h = want.min(area.height).max(3.min(area.height));
    let y = area.y + area.height.saturating_sub(h) / 2;
    (y, h)
}

pub(crate) fn render_conn_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        56
    });
    let (y, h) = overlay_list_box(app.connections.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    app.rects.picker = box_area;
    app.rects.picker_visible = true;

    f.render_widget(Clear, box_area);
    let inner_w = (box_area.width as usize).saturating_sub(2);
    let items: Vec<ListItem> = app
        .connections
        .iter()
        .map(|c| {
            let color = connection_color(c);
            // R73: the connect-time latency (R63 cache) sits right-aligned on
            // every row; a connection never probed in this session shows `-`.
            // Read from the session cache only, so the list never opens a
            // socket or issues a probe.
            let lat = app
                .server_rtts
                .get(&c.id)
                .map(|d| format_rtt(*d))
                .unwrap_or_else(|| "-".to_string());
            let lat_field = format!("{:>6}", lat);
            let name_w = inner_w.saturating_sub(11 + 1 + 1 + disp_width(&lat_field));
            let name = truncate_disp(&c.name, name_w.max(4));
            let used = 11 + 1 + disp_width(&name);
            let pad = inner_w.saturating_sub(used + disp_width(&lat_field));
            let mut spans = vec![
                Span::styled(
                    format!("{:11}", truncate_disp(c.db_type.as_str(), 11)),
                    Style::default().fg(color),
                ),
                Span::raw(" "),
                Span::styled(
                    name,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ];
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            spans.push(Span::styled(
                lat_field,
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                // Key hints are added whole, so a narrow picker drops the least
                // important ones instead of clipping `s 排序` into `s…`.
                .title(overlay_hint_title(
                    box_area.width,
                    &format!(" {} ", t("连接")),
                    &[
                        ("↑↓", t("选择连接")),
                        ("Enter", t("连接")),
                        ("c", t("新建")),
                        ("p", t("复制")),
                        ("L", t("SQLite")),
                        ("s", t("排序")),
                        ("x", t("删除")),
                        ("q", t("显隐")),
                    ],
                    " ",
                ))
                .border_set(border::ROUNDED),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.conn_list);
    // R82: an empty picker names its next step instead of showing a blank box.
    if app.connections.is_empty() && box_area.height > 2 {
        let hint_area = Rect {
            x: box_area.x + 1,
            y: box_area.y + box_area.height / 2,
            width: box_area.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(t("还没有连接 · c 新建")).style(Style::default().fg(Color::DarkGray)),
            hint_area,
        );
    }
}

pub(crate) fn render_db_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let entries = db_entries(app);
    let cur = db_current_index(app);
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        46
    });
    let (y, h) = overlay_list_box(entries.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    app.rects.db_picker = box_area;
    app.rects.db_picker_visible = true;
    f.render_widget(Clear, box_area);

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let style = if i == cur {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mark = if i == cur { "● " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(
                    truncate_disp(
                        &fix_double_encoding(name),
                        (box_area.width as usize).saturating_sub(6),
                    ),
                    style,
                ),
            ]))
        })
        .collect();
    let title = match app.backend_kind {
        Backend::Redis => t(" Redis db · ↑↓ Enter · Esc 关 "),
        _ if !app.schemas.is_empty() => t(" 模式 / 数据库 · ↑↓ Enter · Esc 关 "),
        _ => t(" 数据库 · ↑↓ Enter · Esc 关 "),
    };
    // The `d` overlay header is tinted by the connection colour (with the name
    // shown) so the switcher belongs visibly to the active connection.
    let accent = app
        .selected
        .as_ref()
        .map(connection_color)
        .unwrap_or(Color::Cyan);
    let title = match app.selected.as_ref() {
        Some(c) => Span::styled(
            format!(" ● {} ·{title}", truncate_disp(&c.name, 16)),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        None => Span::styled(title, Style::default().fg(accent)),
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(accent)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.db_list);
}

/// Saved-SQL snippet overlay (DBX's `saved_sql_files`). Enter inserts into the
/// editor; `r` reloads from the shared DBX store.
pub(crate) fn render_snippets(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        60
    });
    let total = app.snippets.len();
    let shown = app.snippet_view.len();
    let filter_h = if app.snippet_filter.is_some() { 1 } else { 0 };
    let (y, h) = overlay_list_box(shown.max(1) + filter_h, area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let title = if app.snippet_needle.trim().is_empty() {
        fit_title(
            &tf(
                " SQL 收藏 · {} 个 · Enter 插入 · / 过滤 · d 删除 · r 刷新 · Esc 关 ",
                &[&total],
            ),
            t(" SQL 收藏 · Enter 插入 · Esc "),
            box_area.width,
        )
    } else {
        fit_title(
            &tf(
                " SQL 收藏 · 过滤「{}」 {}/{} · Esc 关 ",
                &[&(app.snippet_needle), &shown, &total],
            ),
            t(" SQL 收藏（已过滤）· Esc "),
            box_area.width,
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(Color::Cyan)))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }
    let (list_area, filter_area) = if app.snippet_filter.is_some() {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };
    let items: Vec<ListItem> = if shown == 0 {
        let hint = if total == 0 {
            t("暂无收藏 · 编辑器内 Ctrl-O 后按 s 或 Alt-S 添加")
        } else {
            t("（没有匹配的收藏）")
        };
        vec![ListItem::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.snippet_view
            .iter()
            .filter_map(|&i| app.snippets.get(i))
            .map(|s| {
                let head = s
                    .sql
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:20}", truncate_disp(&s.label, 20)),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        truncate_disp(head, (box_area.width as usize).saturating_sub(24)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect()
    };
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, list_area, &mut app.snippet_list);
    if let Some(fa) = filter_area {
        if let Some(ta) = app.snippet_filter.as_mut() {
            ta.set_block(Block::default());
            f.render_widget(&*ta, fa);
        }
    }
    if app.snippet_confirm.is_some() {
        render_snippet_confirm(f, area);
    }
}

/// R71: built-in SQL template panel (`Alt-T` in the editor). Read-only: Enter
/// inserts the highlighted template at the editor caret, `/` filters. A header
/// line spells out the division from the user-owned `Ctrl-O` favourites so the
/// two panels are never confused.
pub(crate) fn render_templates(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        64
    });
    let total = TEMPLATES.len();
    let shown = app.template_view.len();
    let filter_h = if app.template_filter.is_some() { 1 } else { 0 };
    // +1 for the "built-in vs favourites" header line inside the box.
    let (y, h) = overlay_list_box(shown.max(1) + filter_h + 1, area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let title = if app.template_needle.trim().is_empty() {
        fit_title(
            &tf(
                " SQL 模板 · 内置只读 · {} 个 · Enter 插入 · / 过滤 · Esc 关 ",
                &[&total],
            ),
            t(" SQL 模板 · Enter 插入 · Esc "),
            box_area.width,
        )
    } else {
        fit_title(
            &tf(
                " SQL 模板 · 过滤「{}」 {}/{} · Esc 关 ",
                &[&(app.template_needle), &shown, &total],
            ),
            t(" SQL 模板（已过滤）· Esc "),
            box_area.width,
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(Color::Cyan)))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }
    // Header, then the list, then (while typing) the `/` filter line.
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(filter_h as u16),
    ])
    .split(inner);
    let header = Paragraph::new(Line::from(Span::styled(
        truncate_disp(
            t("内置只读模板 · 自存片段见 Ctrl-O（可编辑）"),
            inner.width as usize,
        ),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(header, chunks[0]);
    let items: Vec<ListItem> = if shown == 0 {
        let hint = if total == 0 {
            t("暂无内置模板")
        } else {
            t("（没有匹配的模板）")
        };
        vec![ListItem::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.template_view
            .iter()
            .filter_map(|&i| TEMPLATES.get(i))
            .map(|s| {
                let head = s.sql.lines().next().unwrap_or("").trim();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:22}", truncate_disp(t(s.label), 22)),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        truncate_disp(head, (box_area.width as usize).saturating_sub(26)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect()
    };
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, chunks[1], &mut app.template_list);
    if app.template_filter.is_some() {
        if let Some(ta) = app.template_filter.as_mut() {
            ta.set_block(Block::default());
            f.render_widget(&*ta, chunks[2]);
        }
    }
}

/// The `d` delete confirmation for a saved SQL favourite (keyboard-only, like
/// the history delete layer; the overlay swallows mouse input anyway).
pub(crate) fn render_snippet_confirm(f: &mut Frame, area: Rect) {
    let w = area.width.saturating_sub(4).clamp(24, 64);
    let inner_w = w.saturating_sub(2) as usize;
    let msg = t("将删除这条 SQL 收藏（只删本地配置，不影响数据库）");
    let mut lines: Vec<Line> = wrap_text(msg, inner_w.max(1))
        .into_iter()
        .map(|l| {
            Line::from(Span::styled(
                l,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t("Enter/y 执行   Esc/n 取消"),
        Style::default().fg(Color::DarkGray),
    )));
    let h = (lines.len() as u16 + 2).min(area.height.max(3));
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            t(" ⚠ 删除收藏确认 "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}

/// Ctrl-Shift-H column-visibility overlay: space toggles the highlighted column,
/// `a` shows all, `x` keeps only the first. Changes apply live behind the popup.
/// R48 `gc`: the column-structure mini popup. A compact, scrollable list of the
/// open table's columns (name / type / PK / NOT NULL / comment) drawn over the
/// results pane, so the columns can be checked while writing SQL without
/// switching to the full `gd` structure view. On a narrow screen it uses the
/// full width; otherwise it caps at 72 columns.
pub(crate) fn render_cols_popup(f: &mut Frame, area: Rect, app: &mut App) {
    let all_rows = cols_popup_rows(app);
    if all_rows.is_empty() {
        app.cols_popup_open = false;
        return;
    }
    let rows: Vec<&ColPopupRow> = all_rows
        .iter()
        .filter(|r| cols_popup_matches(r, &app.cols_popup_needle))
        .collect();
    // R56: the popup carries two extra columns now, so it opens wider than the
    // R48 72-column cap; a narrow terminal still uses the full width. R72: with
    // the distribution sparkline it may grow a little more, so the column list
    // keeps its width while the stats pane fits the counts + 12-cell sparkline.
    let show_spark = area.width >= COL_SPARK_MIN_W;
    let cap = if show_spark { 112 } else { 96 };
    let w = if area.width < 48 {
        area.width
    } else {
        area.width.min(cap)
    };
    let inner_w = w.saturating_sub(2) as usize;
    // R66: the value stats follow the highlighted row. The lookup is client-side
    // over the loaded page only; on a wide popup the stats sit in a right pane,
    // on a phone they stack under the list.
    let sel = app.cols_popup_sel.min(rows.len().saturating_sub(1));
    let sel_name = rows.get(sel).map(|r| r.name.clone());
    let stats = sel_name.as_deref().and_then(|n| cols_popup_stats(app, n));
    let side_by_side = inner_w >= COL_STATS_SIDE_MIN && !rows.is_empty();
    let stats_text_w = if side_by_side {
        let base = (inner_w / 3).clamp(18, 32);
        if show_spark {
            // The counts line plus a 12-cell sparkline needs ~46 cells; grow the
            // pane but never starve the column list below ~24 cells.
            base.max(46).min(inner_w.saturating_sub(24)).max(18)
        } else {
            base
        }
    } else {
        inner_w.saturating_sub(2).max(8)
    };
    let stats_lines: Vec<Line> = if rows.is_empty() {
        Vec::new()
    } else {
        col_stats_lines(
            sel_name.as_deref().unwrap_or(""),
            stats.as_ref(),
            stats_text_w,
            show_spark,
        )
    };
    let total_lines = if stats_lines.is_empty() {
        rows.len().max(1)
    } else if side_by_side {
        rows.len().max(1).max(stats_lines.len())
    } else {
        rows.len().max(1) + stats_lines.len() + 1
    };
    let (y, h) = overlay_list_box(total_lines, area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let table = app
        .table_meta
        .as_ref()
        .map(|m| fix_double_encoding(&m.table))
        .unwrap_or_default();
    let needle = app.cols_popup_needle.trim();
    let full = if needle.is_empty() {
        tf(
            " 列结构 · {} · {} 列 · / 过滤 · j/k 选 · Enter 跳列 · Esc 关 ",
            &[&table, &rows.len()],
        )
    } else {
        tf(
            " 列结构 · {} · {}/{} 列 · 过滤「{}」· Enter 跳列 · Esc 关 ",
            &[&table, &rows.len(), &all_rows.len(), &needle],
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(fit_title(&full, t(" 列结构 · j/k · Esc "), box_area.width))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    // Split the interior. Side by side keeps the list and the stats visible at
    // once; stacked reserves the bottom for the stats and lets the list scroll.
    let (list_area, stats_area) = if stats_lines.is_empty() {
        (inner, None)
    } else if side_by_side {
        let stats_area_w = stats_text_w + 2;
        let list_w = inner_w.saturating_sub(stats_area_w + 1);
        let list_area = Rect {
            x: inner.x,
            y: inner.y,
            width: list_w as u16,
            height: inner.height,
        };
        let sep_x = inner.x + list_w as u16;
        f.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray)),
            Rect {
                x: sep_x,
                y: inner.y,
                width: 1,
                height: inner.height,
            },
        );
        let stats_area = Rect {
            x: sep_x + 2,
            y: inner.y,
            width: stats_text_w as u16,
            height: inner.height,
        };
        (list_area, Some(stats_area))
    } else {
        let stats_h = stats_lines.len() as u16;
        let list_h = inner.height.saturating_sub(stats_h + 1).max(1);
        let list_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_h,
        };
        let sep_y = inner.y + list_h;
        f.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
            Rect {
                x: inner.x,
                y: sep_y,
                width: inner.width,
                height: 1,
            },
        );
        let stats_area = Rect {
            x: inner.x,
            y: sep_y + 1,
            width: inner.width,
            height: inner.height.saturating_sub(list_h + 1),
        };
        (list_area, Some(stats_area))
    };
    // R65: the highlighted row is a cursor now, so the scroll window follows it
    // (and both stay clamped to the filtered list).
    let list_h = list_area.height as usize;
    let max_scroll = rows.len().saturating_sub(list_h.max(1)) as u16;
    let mut scroll = app.cols_popup_scroll.min(max_scroll);
    if sel < scroll as usize {
        scroll = sel as u16;
    } else if list_h > 0 && sel >= scroll as usize + list_h {
        scroll = (sel + 1 - list_h) as u16;
    }
    app.cols_popup_scroll = scroll.min(max_scroll);
    let list_w = list_area.width as usize;
    let items: Vec<Line> = if rows.is_empty() {
        vec![Line::from(Span::styled(
            t("（没有匹配的列）").to_string(),
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let layout = cols_popup_layout(&rows, list_w);
        rows.iter()
            .enumerate()
            .map(|(i, r)| {
                // R65: the cursor row is highlighted like the other list
                // overlays, so Enter's target is never ambiguous.
                let style = if i == sel {
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(cols_popup_line(r, &layout, list_w), style))
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(items).scroll((app.cols_popup_scroll, 0)),
        list_area,
    );
    if let Some(sa) = stats_area {
        f.render_widget(Paragraph::new(stats_lines), sa);
    }
}

/// R75: the sidebar table-node info card (`i`). Every value is read from
/// session-cached metadata (see [`table_info_lines`]); the card issues no query
/// and closes itself if the cursor is no longer on a table.
pub(crate) fn render_table_info(f: &mut Frame, area: Rect, app: &mut App) {
    let lines = table_info_lines(app);
    if lines.is_empty() {
        app.table_info_open = false;
        return;
    }
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        64
    });
    let (y, h) = overlay_list_box(lines.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let name = cursor_table(app)
        .map(|t| fix_double_encoding(&t.name))
        .unwrap_or_default();
    let full = tf(" 表信息 · {} · ↑↓ 滚动 · Esc 关 ", &[&name]);
    let short = t(" 表信息 · Esc ");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(fit_title(&full, short, box_area.width))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let inner_w = inner.width as usize;
    // Align the values on the widest label, measured in display columns (the
    // labels are CJK, so char count would be wrong).
    let label_w = lines
        .iter()
        .map(|l| disp_width(&l.label))
        .max()
        .unwrap_or(0)
        .min(12);
    let items: Vec<Line> = lines
        .iter()
        .map(|l| {
            let value_style = if l.hint {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            if l.label.is_empty() {
                return Line::from(Span::styled(truncate_disp(&l.value, inner_w), value_style));
            }
            let pad = " ".repeat(label_w.saturating_sub(disp_width(&l.label)));
            let value_w = inner_w.saturating_sub(label_w + 2);
            Line::from(vec![
                Span::styled(
                    format!("{}{}  ", pad, l.label),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(truncate_disp(&l.value, value_w), value_style),
            ])
        })
        .collect();
    let max_scroll = (lines.len().saturating_sub(inner.height as usize)) as u16;
    let scroll = app.table_info_scroll.min(max_scroll);
    app.table_info_scroll = scroll;
    f.render_widget(Paragraph::new(items).scroll((scroll, 0)), inner);
}

pub(crate) fn render_col_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(grid) = app.grid_full.clone() else {
        return;
    };
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        52
    });
    let (y, h) = overlay_list_box(grid.columns.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = grid
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let shown = !app.col_hidden.contains(name.as_str());
            let mark = if shown { "[x] " } else { "[ ] " };
            let style = if shown {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(
                    truncate_disp(
                        &fix_double_encoding(name),
                        (box_area.width as usize).saturating_sub(6),
                    ),
                    style,
                ),
                Span::styled(format!("  {}", i + 1), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let visible = grid
        .columns
        .iter()
        .filter(|c| !app.col_hidden.contains(c.as_str()))
        .count();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(fit_title(
                    &tf(
                        " 列显示 {}/{} · 空格勾选 · a 全选 · x 仅首列 · Esc 关 ",
                        &[&(visible), &(grid.columns.len())],
                    ),
                    t(" 列显示 · 空格/a/x · Esc "),
                    box_area.width,
                ))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.col_picker_list);
}

/// Ctrl-Shift-R recent-table overlay: Enter jumps straight to the table.
pub(crate) fn render_recent_tables(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        54
    });
    let (y, h) = overlay_list_box(app.recent_tables.len(), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let cur_db = app.current_db();
    // R63: render the panel in the active order (`s` toggles), which is just an
    // index permutation over the canonical recency list.
    let order = recent_order(app);
    let items: Vec<ListItem> = order
        .iter()
        .map(|&i| {
            let (db, schema, table) = &app.recent_tables[i];
            let here = *db == cur_db;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if here { "● " } else { "○ " },
                    Style::default().fg(if here { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(
                    fix_double_encoding(&qualified_display(schema, table)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", fix_double_encoding(db)),
                    Style::default().fg(Color::Cyan),
                ),
            ]))
        })
        .collect();
    let title = tf(
        " 最近表 · {} · s 排序 · ↑↓ Enter 直达 · Esc 关 ",
        &[&t(recent_sort_label(app.recent_sort))],
    );
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.recent_list);
}

/// R87: the session's recent-connection list (`Alt-Shift-H`). Enter switches
/// straight back to the highlighted connection (its per-connection pointer is
/// restored too), so hopping between the last few databases is two keystrokes.
pub(crate) fn render_conn_recent(f: &mut Frame, area: Rect, app: &mut App) {
    let rows = conn_recent_rows(app);
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        54
    });
    let (y, h) = overlay_list_box(rows.len().max(1), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(_, name, sub)| {
            ListItem::new(Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::styled(
                    fix_double_encoding(name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {sub}"), Style::default().fg(Color::Cyan)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(t(" 最近连接 · ↑↓ Enter 直连 · Esc 关 "))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.conn_recent_list);
}

/// R65: the in-data-view table switcher (`g b`). A type-to-filter list of the
/// current database's tables, drawn like the recent-table overlay: the needle
/// rides the title and the highlighted row is the one Enter opens.
pub(crate) fn render_table_jump(f: &mut Frame, area: Rect, app: &mut App) {
    let rows = table_jump_rows(app);
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        54
    });
    let (y, h) = overlay_list_box(rows.len().max(1), area);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(name, kind)| {
            let view = kind.eq_ignore_ascii_case("VIEW");
            let mut spans = vec![Span::styled(
                fix_double_encoding(name),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if view {
                spans.push(Span::styled(
                    "~".to_string(),
                    Style::default().fg(Color::Blue),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let needle = app.table_jump_needle.trim();
    let title = if needle.is_empty() {
        tf(
            " 切换表 · {} · {} 张 · 输入即过滤 · Esc 关 ",
            &[&fix_double_encoding(&app.current_db()), &rows.len()],
        )
    } else {
        tf(
            " 切换表 · {}/{} 张 · 过滤「{}」· Esc 关 ",
            &[&rows.len(), &app.tables_all.len(), &needle],
        )
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, box_area, &mut app.table_jump_list);
}

/// One non-selectable section header inside the history panel (R45).
pub(crate) fn history_section_header(text: &str, color: Color) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )))
}

/// One history list row: time · ★ · summary · duration · origin · source. On a
/// narrow panel the time shrinks to `HH:MM` and the duration / origin / source
/// columns drop out entirely so the statement summary keeps a usable width
/// (R41 / R45).
pub(crate) fn history_list_item(app: &App, ri: usize, list_w: usize) -> ListItem<'static> {
    let Some(r) = app.history_rows.get(ri) else {
        return ListItem::new(Line::from(""));
    };
    let fav = app.history_favorites.contains(&r.sql);
    let time = if list_w >= 44 {
        history_time_label(&r.executed_at)
    } else {
        history_time_label_short(&r.executed_at)
    };
    let duration = if list_w >= 40 && r.duration_ms > 0 {
        history_duration_label(r.duration_ms)
    } else {
        String::new()
    };
    let origin = if list_w >= 52 {
        history_origin_badge(&r.origin).unwrap_or("").to_string()
    } else {
        String::new()
    };
    let src = if list_w >= 56 {
        truncate_disp(&r.connection_name, 18)
    } else {
        String::new()
    };
    let count = if r.count > 1 {
        format!("×{}", r.count)
    } else {
        String::new()
    };
    let reserved = disp_width(&time)
        + 4
        + disp_width(&src)
        + 2
        + disp_width(&duration)
        + disp_width(&origin)
        + disp_width(&count)
        + if r.session { 2 } else { 0 };
    let summary = truncate_disp(
        &history_summary(&r.sql),
        list_w.saturating_sub(reserved).max(8),
    );
    let sum_style = if r.success {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    let mut spans = vec![
        Span::styled(time, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(
            if fav { "★" } else { " " },
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" "),
    ];
    // R84: a cyan dot marks a run from the in-memory session log, so the user
    // can tell "what I just ran" from DBX's persisted history at a glance.
    if r.session {
        spans.push(Span::styled("● ", Style::default().fg(Color::Cyan)));
    }
    spans.push(Span::styled(summary, sum_style));
    if !count.is_empty() {
        spans.push(Span::styled(
            format!(" {count}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !duration.is_empty() {
        spans.push(Span::styled(
            format!("  {duration}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !origin.is_empty() {
        spans.push(Span::styled(
            format!(" {origin}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    if !src.is_empty() {
        spans.push(Span::styled(
            format!("  {src}"),
            Style::default().fg(Color::Cyan),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// The query-history overlay (`Alt-H`): a list of recent statements on top and a
/// wrapped preview of the focused statement below. The `/` filter input takes
/// the bottom slot while it is being typed.
pub(crate) fn render_history_panel(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width < 10 || area.height < 5 {
        return;
    }
    let w = if area.width > 108 {
        104
    } else {
        area.width.saturating_sub(2).max(8)
    };
    let h = area.height.saturating_sub(1).max(3);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);

    let total = app.history_rows.len();
    let shown = app.history_view.len();
    let title = if app.history_needle.trim().is_empty() {
        fit_title(
            &tf(
                " 查询历史 · {} 条 · Enter 回填 · Ctrl-↵ 直跑 · f 收藏 · Del 删除 · y/Y 复制 · / 搜索 · Esc 关 ",
                &[&total],
            ),
            t(" 查询历史 · Enter 回填 · Ctrl-↵ 直跑 · Esc "),
            box_area.width,
        )
    } else {
        fit_title(
            &tf(
                " 查询历史 · 过滤「{}」 {}/{} · Esc 关 ",
                &[&(app.history_needle), &shown, &total],
            ),
            t(" 查询历史（已过滤）· Esc "),
            box_area.width,
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(title, Style::default().fg(Color::Cyan)));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    let list_w = inner.width as usize;
    let fav_count = history_fav_count(app);
    let mut items: Vec<ListItem> = Vec::new();
    // View index → display index (the section headers occupy slots too).
    let mut display_of: Vec<usize> = Vec::with_capacity(app.history_view.len());
    if app.history_view.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            t("（没有匹配的历史记录）"),
            Style::default().fg(Color::DarkGray),
        ))));
    } else {
        // Favourites get their own section pinned above the chronological list
        // (R45); `f` moves an entry between the two.
        if fav_count > 0 {
            items.push(history_section_header(
                &tf("★ 收藏 · {}", &[&fav_count]),
                Color::Yellow,
            ));
        }
        for (vi, &ri) in app.history_view.iter().enumerate() {
            if vi == fav_count && fav_count < app.history_view.len() {
                items.push(history_section_header(
                    &tf("─ 时间序 · {}", &[&(app.history_view.len() - fav_count)]),
                    Color::DarkGray,
                ));
            }
            display_of.push(items.len());
            items.push(history_list_item(app, ri, list_w));
        }
    }
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    // Section headers are not selectable: map the cursor onto its display slot
    // and let the list scroll the selection into view (a fresh state each frame
    // is fine; `List` re-derives its offset from the selection).
    let mut list_state = ListState::default();
    if let Some(sel) = app.history_list.selected() {
        if let Some(&d) = display_of.get(sel) {
            list_state.select(Some(d));
        }
    }

    // Too short to split: show the list alone rather than squeezing three panes.
    if inner.height < 6 {
        f.render_stateful_widget(list, inner, &mut list_state);
        return;
    }

    let filter_h = if app.history_filter.is_some() { 3 } else { 0 };
    let region = inner.height.saturating_sub(1 + filter_h);
    let list_h = ((region as u32 * 45 / 100) as u16).clamp(1, region.max(1));
    let preview_h = region.saturating_sub(list_h).max(1);
    let chunks = Layout::vertical([
        Constraint::Length(list_h),
        Constraint::Length(preview_h),
        Constraint::Length(1),
        Constraint::Length(filter_h),
    ])
    .split(inner);
    f.render_stateful_widget(list, chunks[0], &mut list_state);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(chunks[2].width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );

    if let Some(ta) = app.history_filter.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, chunks[3]);
        return;
    }

    let sel_sql = history_selected_row(app)
        .map(|r| r.sql.clone())
        .unwrap_or_default();
    let lines = wrap_text(&sel_sql, chunks[1].width.max(1) as usize);
    let truncated = lines.len() > 40;
    let mut shown: Vec<Line> = lines
        .iter()
        .take(40)
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(Color::White))))
        .collect();
    if truncated {
        shown.push(Line::from(Span::styled(
            tf("…（预览截断，共 {} 行）", &[&(lines.len())]),
            Style::default().fg(Color::DarkGray),
        )));
    }
    f.render_widget(Paragraph::new(shown), chunks[1]);
}

/// The red confirmation layer for deleting one query-history entry.
/// Returns the `[ 执行 ]` / `[ 取消 ]` hit rectangles (empty when the button row
/// is clipped away).
pub(crate) fn render_history_confirm(
    f: &mut Frame,
    area: Rect,
    hc: &HistoryConfirm,
) -> (Rect, Rect) {
    let w = if area.width < 30 {
        area.width
    } else {
        area.width.saturating_sub(4).min(72)
    };
    let inner_w = w.saturating_sub(4) as usize;
    let sql_lines = wrap_sql_lines(&hc.sql, inner_w.max(1));
    let max_h = area.height.saturating_sub(2) as usize;
    let h = (sql_lines.len() + 5).min(max_h).max(3) as u16;
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            t("⚠ 将删除这条查询历史（不可撤销；不影响数据库数据）"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    let room = (box_area.height as usize).saturating_sub(4);
    for l in sql_lines.iter().take(room) {
        lines.push(Line::from(Span::styled(
            l.clone(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    let inner = Rect {
        x: box_area.x + 1,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(2),
        height: box_area.height.saturating_sub(2),
    };
    let (buttons, ok, cancel) = confirm_buttons(
        inner,
        inner.y + lines.len() as u16,
        t("Enter/y 执行"),
        t("Esc/n 取消"),
    );
    lines.push(buttons);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            t(" ⚠ 删除历史确认 "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
    (ok, cancel)
}

/// The `/` table-name filter prompt, drawn as a one-line box at the bottom.
/// Highlight every case-insensitive occurrence of `needle` inside `text`. Used
/// by the global-search list so a hit is visible at a glance.
pub(crate) fn search_highlight(
    text: &str,
    needle: &str,
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    let nchars = needle.chars().count();
    if nchars == 0 {
        return vec![Span::styled(text.to_string(), base)];
    }
    let nlow = needle.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if i + nchars <= chars.len() && rest.to_lowercase().starts_with(&nlow) {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            let matched: String = chars[i..i + nchars].iter().collect();
            spans.push(Span::styled(matched, hit));
            i += nchars;
        } else {
            plain.push(chars[i]);
            i += 1;
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

/// The global-search overlay (`Alt-G`): one row per hit with the table / column
/// and the matched value (needle highlighted). A running scan shows `done/total`.
pub(crate) fn render_search_panel(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width < 10 || area.height < 5 {
        return;
    }
    let w = if area.width > 108 {
        104
    } else {
        area.width.saturating_sub(2).max(8)
    };
    let h = area.height.saturating_sub(1).max(3);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);

    let done = app.search_hits.len();
    let progress = app
        .search_progress
        .map(|(d, total)| tf(" · {}/{} 表", &[&d, &total]))
        .unwrap_or_default();
    let cap = if app.search_truncated {
        tf(" · 上限 {}", &[&SEARCH_MAX_HITS])
    } else {
        String::new()
    };
    let skip = if app.search_skipped.is_empty() {
        String::new()
    } else {
        tf(" · 跳过 {}", &[&(app.search_skipped.len())])
    };
    let title = tf(
        " 全库搜索「{}」· {} 命中{}{}{} · Enter 定位 · y 复制 · r 重搜 · Esc 关 ",
        &[&(app.search_query), &done, &cap, &skip, &progress],
    );
    let title = fit_title(
        &title,
        &tf(" 全库搜索「{}」· {} 命中 ", &[&(app.search_query), &done]),
        box_area.width,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(title, Style::default().fg(Color::Magenta)));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }

    if app.search_hits.is_empty() {
        let msg = if app.search_running {
            tf("扫描中…{}", &[&progress])
        } else {
            t("（没有命中）").to_string()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    }

    let list_w = inner.width as usize;
    let items: Vec<ListItem> = app
        .search_hits
        .iter()
        .map(|hit| {
            let loc = format!(
                "{}.{}",
                qualified_display(&hit.schema, &hit.table),
                hit.column
            );
            let loc = truncate_disp(&loc, (list_w / 3).clamp(8, 40));
            let matched = truncate_disp(
                &one_line(&hit.matched),
                list_w.saturating_sub(loc.chars().count() + 3).max(8),
            );
            let mut spans = vec![
                Span::styled(
                    loc,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            spans.extend(search_highlight(
                &matched,
                &app.search_query,
                Style::default().fg(Color::White),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut app.search_list);
}

/// The global-search term prompt, drawn as a one-line box at the bottom.
pub(crate) fn render_search_input(f: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 3 || area.width < 12 {
        return;
    }
    let w = area.width.saturating_sub(4).max(20).min(area.width);
    let h = 3.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 1);
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(fit_title(
            &tf(
                " 全库搜索 · {} · Enter 开始 · Esc 取消 ",
                &[&(app.selected_name())],
            ),
            t(" 全库搜索 · Enter 开始 "),
            box_area.width,
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.search_input.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// One row of the column diff list. Wide screens show `src → tgt`; a narrow
/// screen folds to the mark + the changed-attribute summary.
pub(crate) fn diff_col_line(row: &ColDiffRow, width: usize, narrow: bool) -> Line<'static> {
    let same = row.mark == DiffMark::Same;
    let mark = Span::styled(
        format!(" {} ", row.mark.sign()),
        Style::default()
            .fg(row.mark.color())
            .add_modifier(Modifier::BOLD),
    );
    let name_style = if same {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let name = Span::styled(
        truncate_disp(&fix_double_encoding(&row.name), 20),
        name_style,
    );
    let rest = if narrow {
        row.detail.clone()
    } else {
        match row.mark {
            DiffMark::Add => row.src.clone(),
            DiffMark::Drop => row.tgt.clone(),
            DiffMark::Modify => format!("{}  →  {}", row.src, row.tgt),
            DiffMark::Same => row.src.clone(),
        }
    };
    // An unmapped cross-dialect type is flagged with `?` (the detail line always
    // carries it; the wide layout gets it prepended).
    let rest = if !narrow && row.detail.contains("type ?") {
        format!("? {rest}")
    } else {
        rest
    };
    let room = width.saturating_sub(24).max(6);
    let rest = Span::styled(
        truncate_disp(&one_line(&rest), room),
        if same {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        },
    );
    Line::from(vec![mark, name, Span::raw("  "), rest])
}

/// One row of the index diff list.
pub(crate) fn diff_index_line(row: &IndexDiffRow, width: usize) -> Line<'static> {
    let same = row.mark == DiffMark::Same;
    let mark = Span::styled(
        format!(" {} ", row.mark.sign()),
        Style::default()
            .fg(row.mark.color())
            .add_modifier(Modifier::BOLD),
    );
    let text = if same {
        row.detail.clone()
    } else if row.mark == DiffMark::Drop {
        row.tgt.clone()
    } else {
        row.detail.clone()
    };
    Line::from(vec![
        mark,
        Span::styled(
            truncate_disp(&one_line(&text), width.saturating_sub(3).max(6)),
            if same {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
    ])
}

/// The `Alt-D` target picker overlay.
pub(crate) fn render_diff_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let Some((mode, stage, kind)) = app.diff_picker.as_ref().map(|p| (p.mode, p.stage, p.kind))
    else {
        return;
    };
    if area.width < 12 || area.height < 5 {
        return;
    }
    let src = match diff_source(app) {
        Some((_, schema, table)) => fix_double_encoding(&qualified_display(&schema, &table)),
        None => fix_double_encoding(&app.current_db()),
    };
    // Database mode compares whole databases, so its title names the source
    // database rather than the focused table.
    let src_db = fix_double_encoding(&app.current_db());
    let target_conn = app
        .diff_picker
        .as_ref()
        .and_then(|p| p.target_conn.as_ref())
        .map(|c| c.name.clone());
    let loading = app.diff_picker.as_ref().map(|p| p.loading).unwrap_or(false);
    let comparing = app
        .diff_picker
        .as_ref()
        .map(|p| p.comparing)
        .unwrap_or(false);
    let where_input = app
        .diff_picker
        .as_ref()
        .map(|p| p.where_input.clone())
        .unwrap_or_default();
    let entries = app
        .diff_picker
        .as_ref()
        .map(|p| p.entries.clone())
        .unwrap_or_default();
    let w = if area.width > 82 {
        78
    } else {
        area.width.saturating_sub(2).max(10)
    };
    let h = if entries.is_empty() {
        4
    } else {
        (entries.len() as u16 + 3).min(area.height.saturating_sub(2).max(4))
    };
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let data = kind == DiffKind::Data;
    let mode_key = if data {
        t("m 切结构")
    } else {
        t("d 表/库 · m 切数据")
    };
    let title = if comparing {
        let progress = app
            .data_progress
            .map(|(d, tt)| {
                if tt > 0 {
                    tf("{}/{}", &[&d, &tt])
                } else {
                    format!("{d}")
                }
            })
            .unwrap_or_else(|| "0".into());
        tf(
            " 数据对比中… · {} 块 · Esc 中止（保留已比结果） ",
            &[&progress],
        )
    } else if stage == DiffPickStage::Connections {
        if data {
            t(" 数据对比 · 选择目标连接 · Enter 进入 · Esc 返回 ").to_string()
        } else {
            t(" 结构对比 · 选择目标连接 · Enter 进入 · Esc 返回 ").to_string()
        }
    } else if let Some(tn) = target_conn {
        if data {
            tf(
                " 数据对比 · 源 {} → 连接 {} · Enter 对比 · c 换连接 · w WHERE · Esc 关 ",
                &[&src, &tn],
            )
        } else {
            tf(
                " 结构对比 · 源 {} → 连接 {} · Enter 对比 · c 换连接 · Esc 关 ",
                &[&src, &tn],
            )
        }
    } else if data {
        let mut s = tf(
            " 数据对比 · 源表 {} · 选择目标 · {} · c 换连接 · w WHERE · Enter 对比 · Esc 关 ",
            &[&src, &mode_key],
        );
        if !where_input.trim().is_empty() {
            s.push_str(&tf(" · WHERE: {} ", &[&truncate_disp(&where_input, 40)]));
        }
        s
    } else {
        match mode {
            DiffPickMode::Table => tf(
                " 结构对比 · 源表 {} · 选择目标 · {} · c 换连接 · Enter 对比 · Esc 关 ",
                &[&src, &mode_key],
            ),
            DiffPickMode::Database => tf(
                " 结构对比 · 源库 {} · 选择目标 · {} · Enter 对比 · Esc 关 ",
                &[&src_db, &mode_key],
            ),
        }
    };
    // A long diff title (source table + target + every key hint) clips on a
    // narrow terminal; fall back to the essentials (R39 titles-never-truncated).
    let title = fit_title(
        &title,
        t(" 结构/数据对比 · Enter 对比 · Esc "),
        box_area.width,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(title, Style::default().fg(Color::Magenta)));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }
    if entries.is_empty() {
        let msg = if loading {
            t("加载表…").to_string()
        } else {
            t("（没有可选项）").to_string()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    }
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            ListItem::new(Line::from(truncate_disp(
                &fix_double_encoding(e),
                inner.width as usize,
            )))
        })
        .collect();
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    if let Some(p) = app.diff_picker.as_mut() {
        f.render_stateful_widget(list, inner, &mut p.list);
    }
}

/// The two-table diff overlay (columns / indexes / generated ALTER).
pub(crate) fn render_diff_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(state) = app.diff.as_mut() else {
        return;
    };
    if area.width < 10 || area.height < 5 {
        return;
    }
    let narrow = area.width < 64;
    let badge = if state.diff.equal() {
        t("结构一致").to_string()
    } else {
        tf("{} 处差异", &[&state.diff.changed()])
    };
    let tab_label = t(state.tab.label());
    let cross = state.diff.cross;
    let alter = state.alter.clone();
    let mut title = tf(
        " 结构对比 {} → {} · {} · {} · Tab 切换 · y 摘要 · g ALTER · Esc 关 ",
        &[
            &state.diff.src.label(),
            &state.diff.tgt.label(),
            &badge,
            &tab_label,
        ],
    );
    if cross {
        title.push_str(&format!(" · {} ", t("⚠ 跨方言")));
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(title, Style::default().fg(Color::Magenta)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }
    // Legend line so the `+` / `-` / `~` markers read without guessing.
    let legend = if narrow {
        t("+ 新增  - 多余  ~ 差异").to_string()
    } else {
        t("+ 目标缺少（新增）   - 目标多余（删除）   ~ 属性不同").to_string()
    };
    let legend_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_disp(&legend, inner.width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        legend_area,
    );
    let body = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if body.height == 0 {
        return;
    }
    let width = body.width as usize;
    match state.tab {
        DiffTab::Columns => {
            let items: Vec<ListItem> = state
                .diff
                .cols
                .iter()
                .map(|row| ListItem::new(diff_col_line(row, width, narrow)))
                .collect();
            let list = List::new(items).highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, body, &mut state.list);
        }
        DiffTab::Indexes => {
            if state.diff.idx.is_empty() {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        t("（没有索引信息）"),
                        Style::default().fg(Color::DarkGray),
                    ))),
                    body,
                );
                return;
            }
            let items: Vec<ListItem> = state
                .diff
                .idx
                .iter()
                .map(|row| ListItem::new(diff_index_line(row, width)))
                .collect();
            let list = List::new(items).highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, body, &mut state.list);
        }
        DiffTab::Alter => {
            let lines: Vec<Line> = alter
                .lines()
                .map(|l| {
                    let color = if l.trim_start().starts_with("--") {
                        Color::DarkGray
                    } else {
                        Color::White
                    };
                    Line::from(Span::styled(l.to_string(), Style::default().fg(color)))
                })
                .collect();
            f.render_widget(Paragraph::new(lines).scroll((state.scroll, 0)), body);
        }
    }
}

/// The two-database table-list diff overlay.
pub(crate) fn render_db_diff(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(state) = app.db_diff.as_mut() else {
        return;
    };
    if area.width < 10 || area.height < 5 {
        return;
    }
    let only_src = state.diff.count(DbTableMark::OnlySrc);
    let only_tgt = state.diff.count(DbTableMark::OnlyTgt);
    let both = state.diff.count(DbTableMark::Both);
    let title = tf(
        " 库结构对比 {} → {} · 仅源 {} · 仅目标 {} · 共有 {} · Enter 对比同有表 · Esc 关 ",
        &[
            &state.diff.src_label,
            &state.diff.tgt_label,
            &only_src,
            &only_tgt,
            &both,
        ],
    );
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(title, Style::default().fg(Color::Magenta)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }
    let width = inner.width as usize;
    let items: Vec<ListItem> = state
        .diff
        .entries
        .iter()
        .map(|e| {
            let (sign, color) = match e.mark {
                DbTableMark::OnlySrc => ("+", Color::Green),
                DbTableMark::OnlyTgt => ("-", Color::Red),
                DbTableMark::Both => ("=", Color::Gray),
            };
            Line::from(vec![
                Span::styled(
                    format!(" {sign} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_disp(
                        &fix_double_encoding(&e.table),
                        width.saturating_sub(3).max(4),
                    ),
                    Style::default().fg(if e.mark == DbTableMark::Both {
                        Color::White
                    } else {
                        color
                    }),
                ),
            ])
            .into()
        })
        .collect();
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state.list);
}

/// One list line for an only-source / only-target data row: the marker, the
/// primary key, then as many other column values as fit.
pub(crate) fn data_only_line(row: &DataDiffRow, align: &DataAlign, width: usize) -> Line<'static> {
    let color = row.mark.color();
    let mut spans = vec![
        Span::styled(
            format!(" {} ", row.mark.sign()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            row.key.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut used = row.key.chars().count() + 3;
    for (i, col) in align.cols.iter().enumerate().skip(align.pk_len) {
        let Some(v) = row.vals.get(i) else {
            continue;
        };
        let (text, _) = value_display(v);
        let piece = format!(" · {}={}", col.name, text);
        if used + piece.chars().count() > width.saturating_sub(2) {
            break;
        }
        used += piece.chars().count();
        spans.push(Span::styled(piece, Style::default().fg(Color::Gray)));
    }
    Line::from(spans)
}

/// One list line for a `≠` row: the key and the names of the differing columns.
pub(crate) fn data_diff_row_line(row: &DataDiffRow, width: usize) -> Line<'static> {
    let head = format!(" ≠ {} ", row.key);
    let mut rest = row
        .cells
        .iter()
        .map(|c| c.col.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if row.cells.iter().any(|c| c.unknown_type) {
        rest.push_str(" ?");
    }
    let avail = width.saturating_sub(head.chars().count() + 2).max(4);
    Line::from(vec![
        Span::styled(
            head,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_disp(&rest, avail),
            Style::default().fg(Color::Gray),
        ),
    ])
}

/// The two-table data-diff overlay (summary / only-src / only-tgt / diff / sync).
pub(crate) fn render_data_diff(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(state) = app.data_diff.as_mut() else {
        return;
    };
    if area.width < 10 || area.height < 5 {
        return;
    }
    let narrow = area.width < 64;
    let cmp = &state.result;
    let badge = if cmp.equal() {
        t("数据一致").to_string()
    } else {
        tf("{} 处差异", &[&cmp.changed()])
    };
    let tab_label = t(state.tab.label());
    let mut title = tf(
        " 数据对比 {} → {} · {} · {} · Tab 切换 · y 摘要 · g 同步 SQL · Esc 关 ",
        &[&cmp.src_label, &cmp.tgt_label, &badge, &tab_label],
    );
    if cmp.cross() {
        title.push_str(&format!(" · {} ", t("⚠ 跨方言")));
    }
    if cmp.truncated {
        title.push_str(&format!(" · {} ", t("⚠ 已截断")));
    }
    if cmp.cancelled {
        title.push_str(&format!(" · {} ", t("已中止")));
    }
    if cmp.positional {
        title.push_str(&format!(" · {} ", t("按行序对齐")));
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Magenta))
        .title(Span::styled(title, Style::default().fg(Color::Magenta)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }
    let count = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let summary = format!(
        "{} {} · {} {} · {} {} · {} {} · {} {} · {} {}",
        t("源"),
        count(cmp.src_count),
        t("目标"),
        count(cmp.tgt_count),
        t("仅源"),
        cmp.only_src,
        t("仅目标"),
        cmp.only_tgt,
        t("差异"),
        cmp.differing,
        t("已比"),
        cmp.compared,
    );
    // Header line: counts + filter.
    let header = if cmp.filter.trim().is_empty() {
        summary
    } else {
        format!(
            "{} · {}: {}",
            summary,
            t("过滤"),
            truncate_disp(&one_line(&cmp.filter), 40)
        )
    };
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_disp(&header, inner.width as usize),
            Style::default().fg(Color::Gray),
        ))),
        header_area,
    );
    let body = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if body.height == 0 {
        return;
    }
    let width = body.width as usize;
    match state.tab {
        DataTab::Summary => {
            let pk = cmp
                .align
                .pk()
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let cols = cmp
                .align
                .cols
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let mut lines = vec![
                Line::from(Span::styled(
                    if cmp.positional {
                        t("按行序对齐（两侧都无主键，各取前 500 行）：逐行比对，不按键值配对。")
                    } else {
                        t("按主键归一对齐，分块流式拉取（每块 1000 行）。")
                    },
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::raw(format!(
                    "{}: {}",
                    t("主键"),
                    if cmp.positional {
                        t("（无，按行序）").to_string()
                    } else {
                        pk
                    }
                ))),
                Line::from(Span::raw(format!(
                    "{}: {}",
                    t("对比列"),
                    truncate_disp(&cols, width.saturating_sub(8))
                ))),
            ];
            if cmp.equal() {
                lines.push(Line::from(Span::styled(
                    t("数据一致，无差异。"),
                    Style::default().fg(Color::Green),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    t("Tab 切换到 仅源 / 仅目标 / 差异 查看明细；差异行按 Enter 展开列级对照。"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), body);
        }
        DataTab::OnlySrc | DataTab::OnlyTgt | DataTab::Diff => {
            let legend = if narrow {
                t("< 仅源  > 仅目标  ≠ 差异").to_string()
            } else {
                t("< 仅源有   > 仅目标有   ≠ 两边都有但内容不同（Enter 展开列级对照）").to_string()
            };
            let legend_area = Rect {
                x: body.x,
                y: body.y,
                width: body.width,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate_disp(&legend, width),
                    Style::default().fg(Color::DarkGray),
                ))),
                legend_area,
            );
            let list_area = Rect {
                x: body.x,
                y: body.y + 1,
                width: body.width,
                height: body.height.saturating_sub(1),
            };
            if list_area.height == 0 {
                return;
            }
            let items: Vec<ListItem> = match state.tab {
                DataTab::Diff => cmp
                    .rows
                    .iter()
                    .filter(|r| r.mark == RowMark::Diff)
                    .map(|r| ListItem::new(data_diff_row_line(r, width)))
                    .collect(),
                _ => {
                    let mark = if state.tab == DataTab::OnlySrc {
                        RowMark::OnlySrc
                    } else {
                        RowMark::OnlyTgt
                    };
                    cmp.rows
                        .iter()
                        .filter(|r| r.mark == mark)
                        .map(|r| ListItem::new(data_only_line(r, &cmp.align, width)))
                        .collect()
                }
            };
            if items.is_empty() {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        t("（本类没有行）"),
                        Style::default().fg(Color::DarkGray),
                    ))),
                    list_area,
                );
                return;
            }
            let list = List::new(items).highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );
            f.render_stateful_widget(list, list_area, &mut state.list);
        }
        DataTab::Sync => {
            let sql = state.sync_sql.clone();
            let lines: Vec<Line> = sql
                .lines()
                .map(|l| {
                    let color = if l.trim_start().starts_with("--") {
                        Color::DarkGray
                    } else {
                        Color::White
                    };
                    Line::from(Span::styled(l.to_string(), Style::default().fg(color)))
                })
                .collect();
            f.render_widget(Paragraph::new(lines).scroll((state.scroll, 0)), body);
        }
    }
}

/// The optional data-compare `WHERE` input.
pub(crate) fn render_data_where(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 74, 24);
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(fit_title(
            t(" 数据对比 WHERE（两边同时生效）· Enter 开始 · Esc 取消 "),
            t(" 数据对比 WHERE · Enter 开始 "),
            box_area.width,
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let hint_h = 2u16.min(inner.height.saturating_sub(1));
    let ta_h = inner.height.saturating_sub(hint_h).max(1);
    let ta_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: ta_h,
    };
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + ta_h,
        width: inner.width,
        height: hint_h,
    };
    if let Some(ta) = app.data_where.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, ta_area);
    }
    if hint_h > 0 {
        let hints = vec![
            Line::from(Span::styled(
                t("例: created_at > '2026-01-01' AND status = 'active'"),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                t("两边使用相同条件；列名按各自方言书写 · 留空 = 无过滤"),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hints), hint_area);
    }
}

/// The `Alt-T` transfer wizard overlay (three steps + the red overwrite layer).
pub(crate) fn render_transfer_wizard(f: &mut Frame, area: Rect, app: &mut App) {
    if app.transfer.as_ref().is_some_and(|w| w.submitted) {
        render_transfer_running(f, area, app);
        return;
    }
    let step = match app.transfer.as_ref().map(|w| w.step) {
        Some(s) => s,
        None => return,
    };
    if area.width < 12 || area.height < 5 {
        return;
    }
    let w = if area.width > 88 {
        84
    } else {
        area.width.saturating_sub(2).max(10)
    };
    match step {
        TransferStep::Connection => {
            let (src, entries) = {
                let w = app.transfer.as_ref().unwrap();
                (
                    fix_double_encoding(&qualified_display(&w.src_schema, &w.src_table)),
                    w.conns
                        .iter()
                        .map(|c| format!("{}  ({})", c.name, c.db_type.as_str()))
                        .collect::<Vec<_>>(),
                )
            };
            let h = (entries.len() as u16 + 3).min(area.height.saturating_sub(2).max(4));
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan))
                .title(fit_title(
                    &tf(
                        " 数据搬运 ① 目标连接 · 源 {} · Enter 下一步 · Esc 取消 ",
                        &[&src],
                    ),
                    t(" 数据搬运 ① · Enter 下一步 "),
                    box_area.width,
                ));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            let items: Vec<ListItem> = entries.iter().map(|e| ListItem::new(e.clone())).collect();
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            let list_state = &mut app.transfer.as_mut().unwrap().conn_list;
            f.render_stateful_widget(list, inner, list_state);
        }
        TransferStep::Name => {
            let (tgt, lines, focus_label, live) = {
                let w = app.transfer.as_ref().unwrap();
                let tgt = format!(
                    "{} ({})",
                    w.target_conn.name,
                    w.target_conn.db_type.as_str()
                );
                let labels = [t("数据库"), t("模式/Schema"), t("目标表名")];
                let live = w.name_input.lines().join("\n");
                let mut lines = Vec::new();
                for (i, label) in labels.iter().enumerate() {
                    let val = if i == w.name_focus.index() {
                        live.clone()
                    } else {
                        w.name_values[i].clone()
                    };
                    let mark = if i == w.name_focus.index() {
                        "▸"
                    } else {
                        " "
                    };
                    lines.push(format!("{mark} {label:<10} {val}"));
                }
                (tgt, lines, labels[w.name_focus.index()], live)
            };
            let _ = live;
            let h = 9.min(area.height.saturating_sub(2).max(5));
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan))
                .title(fit_title(
                    &tf(
                        " 数据搬运 ② 目标库/表 · {} · Tab 切换 · Enter 下一步 · Esc 返回 ",
                        &[&tgt],
                    ),
                    t(" 数据搬运 ② · Tab/Enter "),
                    box_area.width,
                ));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            let err = app.transfer.as_ref().and_then(|w| w.error.clone());
            let mut text_lines: Vec<Line> = lines
                .iter()
                .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(Color::Gray))))
                .collect();
            text_lines.push(Line::from(""));
            text_lines.push(Line::from(Span::styled(
                tf("编辑字段: {}（直接输入，Tab 切换）", &[&focus_label]),
                Style::default().fg(Color::DarkGray),
            )));
            if let Some(e) = err {
                text_lines.push(Line::from(Span::styled(e, Style::default().fg(Color::Red))));
            }
            let list_h = text_lines.len() as u16;
            let para = Paragraph::new(text_lines);
            f.render_widget(
                para,
                Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: list_h.min(inner.height),
                },
            );
            // The live one-line editor for the focused field.
            let input_y = inner.y + list_h.min(inner.height);
            if input_y < inner.y + inner.height {
                let input_area = Rect {
                    x: inner.x,
                    y: input_y,
                    width: inner.width,
                    height: 1,
                };
                if let Some(w) = app.transfer.as_mut() {
                    w.name_input.set_block(Block::default());
                    f.render_widget(&w.name_input, input_area);
                }
            }
        }
        TransferStep::Options => {
            let (rows, cursor, mode, where_v, large) = {
                let w = app.transfer.as_ref().unwrap();
                (
                    transfer_option_rows(w),
                    w.opt_list.selected().unwrap_or(0),
                    w.mode,
                    w.where_input.clone(),
                    w.large_warn,
                )
            };
            let _ = mode;
            let _ = where_v;
            let h = (rows.len() as u16 + 4).min(area.height.saturating_sub(2).max(5));
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan))
                .title(fit_title(
                    t(" 数据搬运 ③ 模式与选项 · ↑↓ 选择 · Enter 切换 · Enter 开搬 · Esc 取消 "),
                    t(" 数据搬运 ③ · Enter 开搬 "),
                    box_area.width,
                ));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            let items: Vec<ListItem> = rows
                .iter()
                .enumerate()
                .map(|(i, (label, value))| {
                    if i == rows.len() - 1 {
                        ListItem::new(Line::from(Span::styled(
                            format!("▶ {label}"),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        )))
                    } else {
                        ListItem::new(format!("{label:<12} {value}"))
                    }
                })
                .collect();
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            let mut state = ListState::default();
            state.select(Some(cursor));
            let list_h = (rows.len() as u16).min(inner.height.saturating_sub(2));
            f.render_stateful_widget(
                list,
                Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: list_h,
                },
                &mut state,
            );
            let note_y = inner.y + list_h;
            if note_y < inner.y + inner.height {
                let note = if let Some(n) = large {
                    Line::from(Span::styled(
                        tf("⚠ 预估 {} 行，再按 Enter 确认开始", &[&n]),
                        Style::default().fg(Color::Yellow),
                    ))
                } else {
                    Line::from(Span::styled(
                        t("m 模式 · o 覆盖 · s 出错处理 · w/l WHERE/LIMIT · i 索引 · a 自增"),
                        Style::default().fg(Color::DarkGray),
                    ))
                };
                f.render_widget(
                    Paragraph::new(note),
                    Rect {
                        x: inner.x,
                        y: note_y,
                        width: inner.width,
                        height: 1,
                    },
                );
            }
        }
        TransferStep::Confirm => {
            let tgt = {
                let w = app.transfer.as_ref().unwrap();
                format!(
                    "{} · {} · {}",
                    w.target_conn.name,
                    w.name_values[0],
                    qualified_display(&w.name_values[1], &w.name_values[2])
                )
            };
            let h = 8.min(area.height.saturating_sub(2).max(5));
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .title(t(" ⚠ 覆盖确认 · 将先 DROP 目标表 "));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            let lines = vec![
                Line::from(Span::styled(
                    tf("目标表: {}", &[&tgt]),
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    t("DROP TABLE 会永久删除目标表的全部数据，且无法恢复。"),
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    t("Enter 确认覆盖并开始搬运 · Esc 返回（保持报错停下）"),
                    Style::default().fg(Color::Yellow),
                )),
            ];
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

/// The read-only progress overlay shown while a transfer runs.
pub(crate) fn render_transfer_running(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(w) = app.transfer.as_ref() else {
        return;
    };
    if area.width < 12 || area.height < 5 {
        return;
    }
    let src = fix_double_encoding(&qualified_display(&w.src_schema, &w.src_table));
    let table = w.name_values[2].clone();
    let box_w = if area.width > 76 {
        72
    } else {
        area.width.saturating_sub(2).max(10)
    };
    let h = 7.min(area.height.saturating_sub(2).max(4));
    let box_area = centered_overlay(area, box_w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan))
        .title(fit_title(
            t(" 数据搬运中… · Esc 中止（已提交批次保留） "),
            t(" 数据搬运中… · Esc 中止 "),
            box_area.width,
        ));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let (rows, chunks, elapsed, total) = app.transfer_progress.unwrap_or((0, 0, 0, None));
    let rate = (rows as u128 * 1000)
        .checked_div(elapsed)
        .unwrap_or(rows as u128) as u64;
    let mut lines = vec![
        Line::from(Span::styled(
            tf("{} → {}", &[&src, &fix_double_encoding(&table)]),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            tf(
                "已搬 {} 行 / 已完成 {} 块 ({} 行/秒)",
                &[&rows, &chunks, &rate],
            ),
            Style::default().fg(Color::Green),
        )),
    ];
    if let Some(t) = total.filter(|t| *t > 0) {
        lines.push(Line::from(Span::styled(
            tf("源预估 {} 行", &[&t]),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t("Esc 中止并保留已提交批次；完成前请勿关闭终端"),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The `WHERE` / `LIMIT` input for the transfer wizard's options step.
pub(crate) fn render_transfer_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(w) = app.transfer.as_ref() else {
        return;
    };
    let Some(p) = w.prompt.as_ref() else {
        return;
    };
    let field = p.field;
    let label = match field {
        TransferField::Where => t("数据搬运 WHERE（只搬子集）"),
        TransferField::Limit => t("数据搬运 LIMIT 上限"),
    };
    let box_w = if area.width.saturating_sub(4) < 24 {
        area.width
    } else {
        (area.width - 4).min(74)
    };
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, box_w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow))
        .title(tf(" {} · Enter 确定 · Esc 取消 ", &[&label]));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let hint_h = 2u16.min(inner.height.saturating_sub(1));
    let ta_h = inner.height.saturating_sub(hint_h).max(1);
    let ta_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: ta_h,
    };
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + ta_h,
        width: inner.width,
        height: hint_h,
    };
    if let Some(w) = app.transfer.as_mut() {
        if let Some(p) = w.prompt.as_mut() {
            p.input.set_block(Block::default());
            f.render_widget(&p.input, ta_area);
        }
    }
    if hint_h > 0 {
        let hint = match field {
            TransferField::Where => t("例: id > 100 AND status = 'ok'（留空 = 全表）"),
            TransferField::Limit => t("例: 5000（留空 = 不限；顶层上限）"),
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            hint_area,
        );
    }
}

/// The transfer completion summary (`Alt-T`).
pub(crate) fn render_transfer_report(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(rep) = app.transfer_report.as_ref() else {
        return;
    };
    if area.width < 12 || area.height < 5 {
        return;
    }
    let box_w = if area.width > 88 {
        84
    } else {
        area.width.saturating_sub(2).max(10)
    };
    let h = 16.min(area.height.saturating_sub(2).max(5));
    let box_area = centered_overlay(area, box_w, h);
    f.render_widget(Clear, box_area);
    let color = if rep.ok() {
        Color::Green
    } else if rep.cancelled {
        Color::Yellow
    } else {
        Color::Red
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(color))
        .title(fit_title(
            t(" 数据搬运汇总 · g 复制摘要 · b 浏览目标表 · Esc 关闭 "),
            t(" 搬运汇总 · g 摘要 · b 浏览 "),
            box_area.width,
        ));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k}: "), Style::default().fg(Color::DarkGray)),
            Span::raw(v),
        ])
    };
    lines.push(kv(
        t("源"),
        format!("{} ({})", rep.src_label, rep.src_db_type.as_str()),
    ));
    let conflict = if rep.mode == TransferMode::Append {
        String::new()
    } else {
        format!(" · {}", t(rep.conflict.label()))
    };
    lines.push(kv(
        t("目标"),
        format!(
            "{} ({}) · {}{} · {}",
            rep.tgt_label,
            rep.tgt_db_type.as_str(),
            t(rep.mode.label()),
            conflict,
            t(rep.on_error.label())
        ),
    ));
    let src_rows = rep.src_rows;
    lines.push(kv(
        t("已搬运"),
        tf(
            "{} 行 · 跳过 {} 行 · 源读取 {} 行 · {} 块",
            &[&rep.moved, &rep.skipped.len(), &src_rows, &rep.chunks_done],
        ),
    ));
    lines.push(kv(
        t("耗时/速率"),
        format!("{}ms · {} {}", rep.elapsed_ms, rep.rate(), t("行/秒")),
    ));
    lines.push(kv(
        t("源预估"),
        rep.estimated
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string()),
    ));
    if rep.created {
        lines.push(kv(t("建表"), t("已在目标创建").to_string()));
    }
    if let Some(bp) = &rep.breakpoint {
        lines.push(kv(t("断点主键"), bp.clone()));
    }
    if rep.cancelled {
        lines.push(Line::from(Span::styled(
            t("⚠ 已中止（已提交批次保留，可按断点续搬）"),
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some((row, err)) = &rep.aborted {
        lines.push(Line::from(Span::styled(
            tf("✗ 中止于源行 {}: {}", &[&row, &err]),
            Style::default().fg(Color::Red),
        )));
    }
    for (row, err) in rep.skipped.iter().take(3) {
        lines.push(Line::from(Span::styled(
            tf("跳过源行 {}: {}", &[&row, &err]),
            Style::default().fg(Color::Yellow),
        )));
    }
    for warn in rep.warnings.iter().take(2) {
        lines.push(Line::from(Span::styled(
            tf("⚠ {}", &[&warn]),
            Style::default().fg(Color::Yellow),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The `Alt-L` file-path input, drawn as a one-line box at the bottom.
pub(crate) fn render_file_load_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    if area.height < 3 || area.width < 12 {
        return;
    }
    let w = area.width.saturating_sub(4).max(20).min(area.width);
    let h = 3.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 1);
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(fit_title(
            &tf(
                " 加载 SQL 文件 · {} · Enter 预览 · Esc 取消 ",
                &[&(app.selected_name())],
            ),
            t(" 加载 SQL 文件 · Enter 预览 "),
            box_area.width,
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.file_load_prompt.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// The `Alt-L` confirmation layer: file size, statement count, target connection
/// and a wrapped preview of the script, with any destructive statements called
/// out in red before Enter runs it.
pub(crate) fn render_file_load_plan(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(plan) = app.file_load_plan.as_ref() else {
        return;
    };
    if area.width < 14 || area.height < 5 {
        return;
    }
    let w = if area.width < 32 {
        area.width
    } else {
        area.width.saturating_sub(4).min(84)
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;
    let sql_lines = wrap_sql_lines(&plan.sql, inner_w);
    let max_h = area.height.saturating_sub(2) as usize;
    let h = (sql_lines.len() + 8).min(max_h).max(5) as u16;
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);

    let target = if plan.db.trim().is_empty() {
        plan.connection.clone()
    } else {
        format!("{}.{}", plan.connection, fix_double_encoding(&plan.db))
    };
    let label = Style::default().fg(Color::DarkGray);
    let value = Style::default().fg(Color::White);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(t("文件 "), label),
        Span::styled(
            plan.path.display().to_string(),
            value.add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(t("大小 "), label),
        Span::styled(human_size(plan.bytes), value),
        Span::styled(t(" · 语句 "), label),
        Span::styled(plan.statements.to_string(), value),
        Span::styled(t(" · 目标 "), label),
        Span::styled(target, value),
    ]));
    if let Some(w) = &plan.warning {
        lines.push(Line::from(Span::styled(
            format!("⚠ {w}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    for d in &plan.danger {
        lines.push(Line::from(Span::styled(
            format!("⚠ {d}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    let room = (box_area.height as usize).saturating_sub(lines.len() + 2);
    for l in sql_lines.iter().take(room) {
        lines.push(Line::from(Span::styled(
            l.clone(),
            Style::default().fg(Color::White),
        )));
    }
    if sql_lines.len() > room {
        lines.push(Line::from(Span::styled(
            tf("…（预览截断，共 {} 行）", &[&(sql_lines.len())]),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(Span::styled(
        t("Enter 执行 · e 转编辑器 · Esc 取消"),
        Style::default().fg(Color::Yellow),
    )));
    let border = if plan.danger.is_empty() {
        Color::Cyan
    } else {
        Color::Red
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            t(" 执行 SQL 文件 "),
            Style::default().fg(border).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}

pub(crate) fn render_table_filter(f: &mut Frame, area: Rect, app: &mut App) {
    render_prompt_input(
        f,
        area,
        app.table_prompt.as_mut(),
        &tf(
            " 过滤表名 {}/{} · Enter 首个匹配 · Esc 清除 ",
            &[&(app.tables.len()), &(app.tables_all.len())],
        ),
        t(" 过滤表名 · Enter/Esc "),
    );
}

/// Bottom prompt for the `f` tree quick search (R54). Mirrors the table filter
/// prompt, but titles itself as a search and reports the direct-hit count.
pub(crate) fn render_tree_search(f: &mut Frame, area: Rect, app: &mut App) {
    let hits = tree_search_hits(app);
    render_prompt_input(
        f,
        area,
        app.tree_search_prompt.as_mut(),
        &tf(" 搜索连接树 {} 个命中 · Enter 跳首个 · Esc 清除 ", &[&hits]),
        t(" 搜索连接树 · Enter/Esc "),
    );
}

/// Result-row search prompt (`/` in the results pane), styled like the table
/// filter so both filter-as-you-type flows feel identical.
pub(crate) fn render_result_filter(f: &mut Frame, area: Rect, app: &mut App) {
    let hits = result_row_count(app);
    render_prompt_input(
        f,
        area,
        app.result_filter.as_mut(),
        &tf(" 搜索结果 {} 行命中 · Enter 保留 · Esc 清除 ", &[&(hits)]),
        t(" 搜索 · Enter 保留 "),
    );
}
