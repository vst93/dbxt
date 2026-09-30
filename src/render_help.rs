use crate::prelude::*;
use crate::*;

/// A bottom-anchored one-line input overlay shared by the result search, value
/// locate and column jump. A long title would be clipped mid-word on a narrow
/// terminal, so an unclipped short title is substituted instead (R39: titles are
/// never truncated).
pub(crate) fn render_prompt_input(
    f: &mut Frame,
    area: Rect,
    ta: Option<&mut TextArea<'static>>,
    title: &str,
    short_title: &str,
) {
    if area.width == 0 || area.height == 0 {
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
    let label = if disp_width(title) > box_area.width.saturating_sub(2) as usize {
        short_title
    } else {
        title
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(label.to_string())
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = ta {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// SQL prefix-completion popup, anchored just under the editor.
pub(crate) fn render_completion(f: &mut Frame, app: &App) {
    let Some(c) = app.completion.clone() else {
        return;
    };
    let screen = f.area();
    let ed = app.rects.editor;
    let w = 40.min(screen.width.saturating_sub(2)).max(12);
    let h = (c.items.len() as u16 + 2).min(screen.height.saturating_sub(1));
    let x = (ed.x + 2).min(screen.x + screen.width.saturating_sub(w));
    let mut y = ed.y + ed.height;
    if y + h > screen.y + screen.height {
        y = screen.y + screen.height - h;
    }
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let lines: Vec<Line> = c
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == c.sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let tag = item.kind.to_string();
            let room = (w as usize).saturating_sub(6);
            // Identifiers are shown decoded (a name stored through a latin1
            // connection is CP1252 mojibake); `item.text` stays raw so the
            // accepted fragment is still the name the server actually knows.
            let shown = fix_double_encoding(&item.text);
            Line::from(vec![
                Span::styled(format!("{:<room$}", truncate_disp(&shown, room)), style),
                Span::styled(format!("[{tag}]"), style.fg(Color::DarkGray)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(fit_title(
                    t(" 补全 · T表 C列 K关键字 · Tab 上屏 · ↑↓ · Esc "),
                    t(" 补全 · Tab 上屏 · Esc "),
                    box_area.width,
                ))
                .border_set(border::ROUNDED)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

/// Name prompt for saving the editor's SQL into DBX's favourites.
pub(crate) fn render_snippet_name(f: &mut Frame, area: Rect, app: &mut App) {
    let w = area.width.saturating_sub(4).max(24).min(area.width);
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
            t(" 收藏为 SQL 片段（DBX saved_sql_files）· Enter 保存 · Esc 取消 "),
            t(" 收藏片段 · Enter 保存 "),
            box_area.width,
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.snippet_name.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

/// Shared scrollable text popup used for both cell values and row details.
///
/// Returns the drawn box, its inner (scrollable) area and the largest scroll
/// offset, so a caller can record click geometry (the error box pages on a tap;
/// the cell popup closes on one).
pub(crate) fn render_text_popup(
    f: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[PopupLine],
    scroll: u16,
    cache: &mut Option<PopupCache>,
) -> (Rect, Rect, u16) {
    let inner_w = popup_inner_width(area.width);
    // Wrap each logical line on its own so the style that marks NULL / ''
    // survives across physical rows. The result is memoised: re-wrapping a
    // 100 KB value on every scroll frame would stall the TUI (R42).
    if cache.as_ref().is_none_or(|c| c.width != inner_w) {
        *cache = Some(PopupCache {
            width: inner_w,
            lines: popup_lines_plain(lines, inner_w),
        });
    }
    let body = &cache.as_ref().expect("popup cache filled above").lines;
    render_popup_body(f, area, title, body, scroll)
}

/// R74: the cell popup, which adds the pretty-JSON view. The body is rebuilt
/// from the raw or pretty source depending on the `J` toggle; the memoised wrap
/// is keyed on width only, and the toggle clears `cache` before redrawing.
pub(crate) fn render_cell_popup(
    f: &mut Frame,
    area: Rect,
    popup: &CellPopup,
    cache: &mut Option<PopupCache>,
) -> (Rect, Rect, u16) {
    let inner_w = popup_inner_width(area.width);
    if cache.as_ref().is_none_or(|c| c.width != inner_w) {
        let body = match (&popup.pretty, popup.show_pretty) {
            (Some(pretty), true) => popup_lines_rich(pretty, inner_w),
            _ => popup_lines_plain(&popup.lines, inner_w),
        };
        *cache = Some(PopupCache {
            width: inner_w,
            lines: body,
        });
    }
    let title = if popup.pretty.is_some() {
        if popup.show_pretty {
            tf("{} · JSON 美化", &[&popup.title])
        } else {
            tf("{} · JSON 原值", &[&popup.title])
        }
    } else {
        popup.title.clone()
    };
    let body = &cache.as_ref().expect("popup cache filled above").lines;
    render_popup_body(f, area, &title, body, popup.scroll)
}

/// The text-popup width (`overlay_width` minus the two borders and padding),
/// shared by the plain and rich body builders so the wrap and the draw agree.
pub(crate) fn popup_inner_width(area_w: u16) -> usize {
    overlay_width(area_w, 88, 24).saturating_sub(4).max(1) as usize
}

/// Plain popup body: wrap every logical line on its own, carrying its style.
pub(crate) fn popup_lines_plain(lines: &[PopupLine], inner_w: usize) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|pl| {
            let style = pl.style;
            wrap_text(&pl.text, inner_w)
                .into_iter()
                .map(move |t| Line::from(Span::styled(t, style)))
        })
        .collect()
}

/// Rich popup body (the pretty-JSON cell view): wrap every line's styled token
/// runs, keeping each token's colour across the wrap boundary.
pub(crate) fn popup_lines_rich(lines: &[Vec<PopupSpan>], inner_w: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for spans in lines {
        out.extend(wrap_spans(spans, inner_w));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

/// Wrap one line's styled spans to `width` display columns, one `Line` per
/// physical row. A span may carry an explicit `\n` (unlikely in pretty JSON,
/// but the scanner is generic); it starts a new row like a width overflow.
fn wrap_spans(spans: &[PopupSpan], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for sp in spans {
        let style = sp.style;
        let mut seg = String::new();
        for c in sp.text.chars() {
            if c == '\n' {
                if !seg.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut seg), style));
                }
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
                continue;
            }
            let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
            if cur_w + cw > width && cur_w > 0 {
                if !seg.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut seg), style));
                }
                out.push(Line::from(std::mem::take(&mut cur)));
                cur_w = 0;
            }
            seg.push(c);
            cur_w += cw;
        }
        if !seg.is_empty() {
            cur.push(Span::styled(seg, style));
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(Line::from(cur));
    }
    out
}

/// Draw a pre-built body in the shared scrollable popup frame and report its
/// geometry (box, inner area, largest scroll offset).
pub(crate) fn render_popup_body(
    f: &mut Frame,
    area: Rect,
    title: &str,
    body: &[Line<'static>],
    scroll: u16,
) -> (Rect, Rect, u16) {
    let w = overlay_width(area.width, 88, 24);
    let total = body.len();
    let max_h = area.height.saturating_sub(4).max(3);
    let h = ((total as u16) + 2).min(max_h);
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
    f.render_widget(Clear, box_area);
    let max_scroll = total.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    let scroll = scroll.min(max_scroll);
    let title = tf(
        " {} · {}/{} · Esc 关闭 ",
        &[
            &(title),
            &((scroll as usize + inner_h).min(total)),
            &(total),
        ],
    );
    f.render_widget(
        Paragraph::new(body.to_vec()).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
    (
        box_area,
        Rect {
            x: box_area.x + 1,
            y: box_area.y + 1,
            width: box_area.width.saturating_sub(2),
            height: box_area.height.saturating_sub(2),
        },
        max_scroll,
    )
}

/// Below this inner width the row popup stacks a field's value on its own line
/// under the name (`字段:` then the indented value) instead of running
/// `字段 = 值` together, so a long name can never squeeze the value off a phone.
pub(crate) const ROW_POPUP_STACK_W: usize = 56;

/// Build the (filtered) body of the row popup plus the physical-line → entry
/// mapping and the physical line the cursor's entry starts on. Wide popups keep
/// one `name = value` line per field; a narrow popup (`inner_w <
/// ROW_POPUP_STACK_W`) stacks the value under the name, one field per column.
pub(crate) fn row_popup_body(
    popup: &RowPopup,
    inner_w: usize,
) -> (Vec<Line<'static>>, Vec<usize>, usize) {
    let visible = row_popup_visible(popup);
    let total = visible.len();
    let cursor = if total == 0 {
        0
    } else {
        popup.cursor.min(total - 1)
    };
    let mut body: Vec<Line<'static>> = Vec::new();
    let mut hit: Vec<usize> = Vec::new();
    let mut sel_line = 0usize;
    if total == 0 {
        body.push(Line::from(Span::styled(
            t("（无匹配字段）").to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        return (body, hit, sel_line);
    }
    let stacked = inner_w < ROW_POPUP_STACK_W;
    for (pos, &ei) in visible.iter().enumerate() {
        let pl = &popup.lines[ei];
        let selected = pos == cursor;
        if selected {
            sel_line = body.len();
        }
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            pl.style.add_modifier(Modifier::BOLD)
        } else {
            pl.style
        };
        let push = |line: String, hit: &mut Vec<usize>, body: &mut Vec<Line<'static>>| {
            for t in wrap_text(&line, inner_w) {
                body.push(Line::from(Span::styled(t, style)));
                hit.push(pos);
            }
        };
        if stacked {
            let name = popup.cols.get(ei).map(String::as_str).unwrap_or("");
            // `shown` is the display value ("NULL" / "''" / the text); fall back
            // to the whole line for a fixture that only filled `lines`.
            let shown = popup
                .shown
                .get(ei)
                .filter(|s| !s.is_empty())
                .map(String::as_str)
                .unwrap_or_else(|| pl.text.as_str());
            push(format!("{marker}{name}:"), &mut hit, &mut body);
            push(format!("  {shown}"), &mut hit, &mut body);
        } else {
            push(format!("{marker}{}", pl.text), &mut hit, &mut body);
        }
    }
    (body, hit, sel_line)
}

/// R42b: the row-detail popup. A scrollable `column = value` list with a
/// cursor (highlighted with `▶` and bold, keeping the NULL / '' style), a
/// column-name / value filter and a vim count prefix. Drawn under a drilled
/// cell popup; a narrow terminal stacks each value under its name.
pub(crate) fn render_row_popup(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 88, 24);
    let inner_w = w.saturating_sub(4).max(1) as usize;
    // Build the (filtered) body first, tracking where the cursor's entry starts.
    // `hit` maps each physical (wrapped) line back to its entry position, so a
    // click can select the value under the pointer even when it wrapped.
    let (base_title, body, hit, sel_line, filtering, filter, cur, total) = {
        let Some(popup) = app.row_popup.as_ref() else {
            return;
        };
        let total = row_popup_visible(popup).len();
        let cursor = if total == 0 {
            0
        } else {
            popup.cursor.min(total - 1)
        };
        let (body, hit, sel_line) = row_popup_body(popup, inner_w);
        (
            popup.title.clone(),
            body,
            hit,
            sel_line,
            popup.filtering,
            popup.filter.clone(),
            if total == 0 { 0 } else { cursor + 1 },
            total,
        )
    };
    app.row_popup_hit = hit;
    let total_lines = body.len();
    let max_h = area.height.saturating_sub(4).max(3);
    let h = ((total_lines as u16) + 2).min(max_h);
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    // Auto-scroll so the cursor's entry stays on screen.
    let mut scroll = app
        .row_popup
        .as_ref()
        .map(|p| p.scroll)
        .unwrap_or(0)
        .min(max_scroll);
    if sel_line < scroll as usize {
        scroll = sel_line as u16;
    } else if inner_h > 0 && sel_line >= scroll as usize + inner_h {
        scroll = (sel_line + 1 - inner_h) as u16;
    }
    let scroll = scroll.min(max_scroll);
    if let Some(p) = app.row_popup.as_mut() {
        p.scroll = scroll;
    }
    // Click geometry: the inner area is the wrapped-line viewport and `scroll`
    // its origin, so a press maps to `row_popup_hit[scroll + rel_y]`.
    app.rects.row_popup_inner = Rect {
        x: box_area.x + 1,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(2),
        height: box_area.height.saturating_sub(2),
    };
    app.rects.row_popup_scroll = scroll;
    app.rects.row_popup_visible = true;
    // Title: base locator · filter · position · hints. The full hint line is
    // swapped for a short one when it would not fit, so a title is never cut.
    let mut base = format!(" {} ", base_title);
    if filtering {
        base.push_str(&format!("· /{}_ ", filter));
    } else if !filter.is_empty() {
        base.push_str(&format!("· /{} ", filter));
    }
    base.push_str(&format!("· {}/{} ", cur, total));
    let full = format!(
        "{base}· ↑↓/n p {} · Enter/v {} · y/Y {} · / {} · Esc {} ",
        t("移动"),
        t("看值"),
        t("复制值"),
        t("过滤名/值"),
        t("关闭")
    );
    let short = format!("{}· Esc {} ", base, t("关闭"));
    // The footer already carries the keys, so a narrow box can drop the hint
    // tail entirely rather than clip it.
    let minimal = base.trim_end().to_string();
    let title = if disp_width(&full) <= box_area.width.saturating_sub(2) as usize {
        full
    } else if disp_width(&short) <= box_area.width.saturating_sub(2) as usize {
        short
    } else {
        minimal
    };
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new(body).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

/// R41: the compact execution-error box. On a small screen it shows only the
/// first line plus the line count; `Enter` widens it to the full scrollable
/// text. `Enter` again (or `Esc`) closes it.
pub(crate) fn render_error_popup(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(popup) = app.error_popup.clone() else {
        return;
    };
    let total = popup.lines.len().max(1);
    if popup.expanded {
        let lines: Vec<PopupLine> = popup
            .lines
            .iter()
            .map(|l| PopupLine {
                text: l.clone(),
                style: Style::default().fg(Color::Red),
            })
            .collect();
        let cache = &mut app.popup_cache;
        let (box_area, inner, max_scroll) =
            render_text_popup(f, area, t("执行错误"), &lines, popup.scroll, cache);
        app.rects.error_box = box_area;
        app.rects.error_inner = inner;
        app.rects.error_max_scroll = max_scroll;
        return;
    }
    let w = overlay_width(area.width, 72, 24);
    let inner_w = w.saturating_sub(4).max(1) as usize;
    let box_area = centered_overlay(area, w, 5);
    app.rects.error_box = box_area;
    app.rects.error_inner = Rect {
        x: box_area.x + 1,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(2),
        height: box_area.height.saturating_sub(2),
    };
    app.rects.error_max_scroll = 0;
    f.render_widget(Clear, box_area);
    let first = truncate_disp(
        popup.lines.first().map(String::as_str).unwrap_or(""),
        inner_w,
    );
    let hint = if total > 1 {
        tf(
            "✗ {} · 共 {} 行 · Enter 看全量 · Esc 关 ",
            &[&first, &total],
        )
    } else {
        tf("✗ {} · Enter 看全量 · Esc 关 ", &[&first])
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::Red),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    t(" 执行错误 "),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ))
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Red)),
        ),
        box_area,
    );
}

pub(crate) fn render_edit_dialog(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(d) = app.edit_dialog.clone() else {
        return;
    };
    let w = overlay_width(area.width, 78, 34);
    let inner_w = w.saturating_sub(4).max(1) as usize;

    match d.kind {
        EditKind::Update => {
            let (old, old_style) = match &d.old {
                Val::Null => ("NULL".to_string(), null_style()),
                Val::Text(s) if s.is_empty() => ("''".to_string(), empty_string_style()),
                Val::Text(s) => (s.clone(), Style::default().fg(Color::Red)),
            };
            let mut header_lines: Vec<Line> = Vec::new();
            header_lines.push(Line::from(vec![
                Span::styled(t("列   "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    d.column.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", d.data_type.clone().unwrap_or_else(|| "?".into())),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            header_lines.push(Line::from(vec![
                Span::styled(t("旧值 "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_disp(&one_line(&old), inner_w.saturating_sub(6)),
                    old_style,
                ),
            ]));
            header_lines.push(Line::from(vec![
                Span::styled("WHERE ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate_disp(&one_line(&d.where_clause), inner_w.saturating_sub(6)),
                    Style::default().fg(Color::Gray),
                ),
            ]));
            if d.no_pk {
                header_lines.push(Line::from(Span::styled(
                    t("⚠ 未检测到主键：WHERE 用全部列匹配，请确认条件唯一"),
                    Style::default().fg(Color::Yellow),
                )));
            } else {
                header_lines.push(Line::from(vec![
                    Span::styled(t("主键 "), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        truncate_disp(
                            &d.keys
                                .iter()
                                .map(|k| fix_double_encoding(k))
                                .collect::<Vec<_>>()
                                .join(", "),
                            inner_w.saturating_sub(6),
                        ),
                        Style::default().fg(Color::Cyan),
                    ),
                ]));
            }
            // The full statement is always shown: nothing is executed from a
            // summary alone.
            let sql_lines = wrap_sql_lines(&d.sql(), inner_w.saturating_sub(2));
            header_lines.push(Line::from(""));
            header_lines.push(Line::from(Span::styled(
                t("生成的 SQL（Enter 执行）"),
                Style::default().fg(Color::DarkGray),
            )));
            for l in &sql_lines {
                header_lines.push(Line::from(Span::styled(
                    l.clone(),
                    Style::default().fg(Color::White),
                )));
            }
            let header_h = header_lines.len() as u16;
            let h = (header_h + 3 + 1 + 2).min(area.height);
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(
                        " ✎ 编辑 {}.{} ",
                        &[
                            &(fix_double_encoding(&d.db)),
                            &(fix_double_encoding(&qualified_display(&d.schema, &d.table))),
                        ],
                    ),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Yellow));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            // Keep room for the value input + hint even on a short terminal.
            let max_header = inner.height.saturating_sub(3 + 1) as usize;
            let shown: Vec<Line> = header_lines.iter().take(max_header).cloned().collect();
            let shown_h = shown.len() as u16;
            let hdr_area = Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: shown_h.min(inner.height),
            };
            f.render_widget(Paragraph::new(shown), hdr_area);
            let ta_y = inner.y + shown_h;
            let ta_h = 3.min((inner.y + inner.height).saturating_sub(ta_y));
            if ta_h > 0 {
                let ta_area = Rect {
                    x: inner.x,
                    y: ta_y,
                    width: inner.width,
                    height: ta_h,
                };
                if let Some(dd) = app.edit_dialog.as_mut() {
                    let b = Block::default()
                        .borders(Borders::ALL)
                        .title(t(" 新值 · Enter 执行 "))
                        .border_set(border::ROUNDED)
                        .border_style(Style::default().fg(Color::Green));
                    dd.new_input.set_block(b);
                    f.render_widget(&dd.new_input, ta_area);
                }
            }
            let hint_y = ta_y + ta_h;
            if hint_y < inner.y + inner.height {
                let hint_area = Rect {
                    x: inner.x,
                    y: hint_y,
                    width: inner.width,
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new(truncate_disp(
                        t("Enter 执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量"),
                        inner_w,
                    ))
                    .style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }
        EditKind::Insert => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(Span::styled(
                tf(
                    "新增一行到 {}.{}",
                    &[
                        &(fix_double_encoding(&d.db)),
                        &(fix_double_encoding(&qualified_display(&d.schema, &d.table))),
                    ],
                ),
                Style::default().fg(Color::Cyan),
            )));
            for (col, val) in d.insert_preview.iter().take(10) {
                lines.push(Line::from(vec![
                    Span::styled(
                        truncate_disp(col, inner_w.saturating_sub(14)),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::raw(" = "),
                    Span::styled(
                        truncate_disp(val, 12),
                        if val == "NULL" {
                            null_style()
                        } else if val == "''" {
                            empty_string_style()
                        } else {
                            Style::default().fg(Color::Green)
                        },
                    ),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t("生成的 SQL（Enter 执行）"),
                Style::default().fg(Color::DarkGray),
            )));
            for l in wrap_sql_lines(&d.sql(), inner_w.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    l,
                    Style::default().fg(Color::White),
                )));
            }
            lines.push(Line::from(Span::styled(
                t("Enter 执行 · Esc 取消 · Ctrl-V 转编辑器 · Ctrl-T 加入批量"),
                Style::default().fg(Color::DarkGray),
            )));
            let h = (lines.len() as u16 + 2).min(area.height);
            let box_area = centered_overlay(area, w, h);
            f.render_widget(Clear, box_area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    tf(
                        " ➕ 插入 {}.{} ",
                        &[
                            &(fix_double_encoding(&d.db)),
                            &(fix_double_encoding(&qualified_display(&d.schema, &d.table))),
                        ],
                    ),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Green));
            f.render_widget(Paragraph::new(lines).block(block), box_area);
        }
    }
}

pub(crate) fn render_redis_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 74, 24);
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = app
        .redis_prompt
        .as_ref()
        .map(|p| p.title.clone())
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(
                " {} · {} ",
                truncate_disp(&title, 40),
                t("Enter 确认 · Esc 取消")
            ),
            Style::default().fg(Color::Yellow),
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let ta_h = inner.height.saturating_sub(2).max(1);
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
        height: inner.height.saturating_sub(ta_h),
    };
    if let Some(p) = app.redis_prompt.as_mut() {
        p.input.set_block(Block::default());
        f.render_widget(&p.input, ta_area);
    }
    if hint_area.height > 0 {
        let hint = match app.redis_prompt.as_ref().map(|p| p.kind) {
            Some(RedisPromptKind::Pattern) => t("SCAN MATCH 模式，例 app:* · 支持 * ? []"),
            Some(RedisPromptKind::Ttl) => t("秒数；-1 = 持久化，0 = 立即删除"),
            Some(RedisPromptKind::TtlKey) => {
                t("TTL：300 / 30m / 2h / 500ms（-1 = 持久化，0 = 立即删除）")
            }
            Some(RedisPromptKind::Rename) => t("新 key 名（已存在的 key 会被覆盖）"),
            Some(RedisPromptKind::StringValue) => t("新的 string 内容（支持多行）"),
            Some(RedisPromptKind::HashField) => t("新的 hash 字段值"),
            Some(RedisPromptKind::BatchTtl) => t("秒数；-1 = 持久化，0 = 立即删除"),
            Some(RedisPromptKind::BatchRenamePrefix) => t("旧前缀=新前缀，例 app: = new:"),
            Some(RedisPromptKind::BatchConfirm) => t("输入 key 数或 YES 以确认删除"),
            None => "",
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

/// The MongoDB document JSON editor: a full-height text area plus a validation
/// / hint line at the bottom.
pub(crate) fn render_mongo_dialog(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(d) = app.mongo_dialog.clone() else {
        return;
    };
    let w = overlay_width(area.width, 86, 34);
    let h = area.height.saturating_sub(2).max(5);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = match d.mode {
        MongoDocMode::Edit => tf(
            " ✎ 编辑文档 {}.{} ",
            &[
                &fix_double_encoding(&d.db),
                &fix_double_encoding(&d.collection),
            ],
        ),
        MongoDocMode::Insert => tf(
            " ➕ 插入文档 {}.{} ",
            &[
                &fix_double_encoding(&d.db),
                &fix_double_encoding(&d.collection),
            ],
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let bottom_h = 2u16.min(inner.height.saturating_sub(1));
    let ta_h = inner.height.saturating_sub(bottom_h).max(1);
    let ta_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: ta_h,
    };
    if let Some(dd) = app.mongo_dialog.as_mut() {
        dd.editor.set_block(Block::default());
        f.render_widget(&dd.editor, ta_area);
    }
    let mut lines: Vec<Line> = Vec::new();
    if let Some(err) = &d.error {
        lines.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(Color::Red),
        )));
    } else if d.mode == MongoDocMode::Edit {
        lines.push(Line::from(Span::styled(
            t("_id 不可修改 · Ctrl-S 预览 diff 后确认"),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            t("输入 JSON 对象，可省略 _id"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(Span::styled(
        t("Ctrl-S 校验并保存 · Esc 取消 · 方向键移动"),
        Style::default().fg(Color::DarkGray),
    )));
    let hint_area = Rect {
        x: inner.x,
        y: inner.y + ta_h,
        width: inner.width,
        height: inner.height.saturating_sub(ta_h),
    };
    f.render_widget(Paragraph::new(lines), hint_area);
}

pub(crate) fn render_filter_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 74, 24);
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = if app.grid_kind == GridKind::MongoDocs {
        t(" MongoDB 过滤 (JSON) · Enter 应用 · Esc 取消 · 留空清除 ")
    } else {
        t(" WHERE 过滤 · Enter 应用 · Esc 取消 · 留空清除 ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    // Leave two lines at the bottom for the syntax quick-reference.
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
    if let Some(ta) = app.filter_prompt.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, ta_area);
    }
    if hint_h > 0 {
        let hints = if app.grid_kind == GridKind::MongoDocs {
            vec![
                Line::from(Span::styled(
                    t("JSON: {\"age\": {\"$gt\": 30}} · {\"name\": \"Ada\"}"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    t("运算符: $eq $gt $lt $in $regex $exists · 留空 = 全部"),
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    t("语法: = != <> > < >= <= LIKE IN BETWEEN IS NULL · AND/OR · 字符串单引号"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    t("MySQL 反引号 `col` · PG 双引号 \"col\"（区分大小写）"),
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        };
        f.render_widget(Paragraph::new(hints), hint_area);
    }
}

/// The `?` shortcut cheat-sheet, generated from the same list the README table
/// mirrors.
pub(crate) const HELP_ROWS: &[(&str, &str)] = &[
    ("— 全局 —", ""),
    (
        "q / Ctrl-C",
        "退出（编辑器有未执行语句时两段确认：再按一次退出，Esc 留下）",
    ),
    ("Ctrl-L", "切换命令模式 SQL → Redis → MongoDB"),
    ("F5 / Ctrl-J", "执行 SQL（有选区只跑选区，否则整段）"),
    (
        "Alt-Enter",
        "只执行光标处语句（有选区则执行选区；分号分隔，字面量/注释里的分号不算）",
    ),
    ("Tab / Shift-Tab", "循环切换区域（侧栏 → 编辑器 → 结果）"),
    (
        "Alt-1..9",
        "直切第 N 个连接（侧栏连接顺序；智能恢复上次库/表）",
    ),
    ("Alt-Tab / Alt-`", "当前连接与上一个连接对切"),
    (
        "Alt-Shift-1/2/3",
        "直接聚焦 侧栏 / 编辑器 / 结果（终端可能报成 Alt-! @ #）",
    ),
    ("Ctrl-A", "自动折叠 开 / 关（开=非焦点栏收起）"),
    ("Ctrl-W", "收起 / 展开当前焦点区域"),
    ("Ctrl-G", "横滚模式（触屏兜底：滚轮/上下滑 = 横滚列）"),
    ("Alt-C / w", "紧凑列宽：窄屏自动共享列宽，宽表尽量一屏放下"),
    (
        "Alt-V / c",
        "列显隐：空格勾选显示的列（按 库.表 记住，跨会话）",
    ),
    ("Alt-R / t", "最近浏览的 5 张表，Enter 直达 · s 排序（侧栏 t）"),
    (
        "Alt-← →",
        "最近表 / 集合 / Redis key 后退 / 前进（浏览器语义，最多 50 个，跨库可用）",
    ),
    (
        "Alt-O",
        "脚本输出：语句分隔线 + 每条耗时前缀（默认关；Alt-T 已被数据搬运占用）",
    ),
    ("Shift+← →", "列窗口横滚一列（任意区域，按住连滚）"),
    ("Ctrl-O", "SQL 片段收藏（DBX saved_sql_files）"),
    ("Ctrl-P", "EXPLAIN 当前 SQL（SQL 后端）"),
    ("?", "本帮助（面板内 / 过滤键位或功能名；上下文键位排前）"),
    (
        "F1",
        "打开本帮助（编辑器 / 命令输入里 ? 是字面字符，改用 F1）",
    ),
    (
        "DBXT_MOUSE_DEBUG=1",
        "启动时显示鼠标事件浮层（滑动无效时排查终端编码）",
    ),
    ("— 显示约定 —", ""),
    (
        "NULL",
        "真正的 SQL NULL：灰色斜体（终端不支持斜体时仅灰色）",
    ),
    ("''", "空字符串：灰色，带引号的空串，不会与 NULL 混淆"),
    ("DBXT_NO_ITALIC=1", "强制 NULL 仅用灰色，不依赖终端斜体"),
    ("— 连接选择 —", ""),
    (
        "Alt-1..9",
        "直切第 N 个连接（按当前排序；同库表存在则直达，否则落首屏）",
    ),
    ("Alt-Tab / Alt-`", "与上一个连接对切（双缓冲，来回横跳）"),
    ("↑ ↓ / Enter", "选择 / 连接"),
    ("c", "新建连接"),
    ("e", "编辑选中连接（含 SSH 隧道，预填表单）"),
    ("p", "复制连接（预填表单）"),
    ("s", "循环排序：名称 / 类型 / 颜色（同色连接排在一起）"),
    (
        "x / Del",
        "删除选中连接（红色确认；只删配置，不删数据库数据）",
    ),
    (
        "d",
        "断开选中连接（关闭连接池，未提交手动事务回滚；配置保留，可重连）",
    ),
    ("q", "折叠 / 展开连接列表"),
    (
        "L",
        "打开 SQLite 文件（.db / .sqlite / .sqlite3）：当前目录起步，输入路径 / ↑↓ 选择 / Tab 补全，Enter 打开；顶部列出最近 5 个（Del 移除）。临时连接不写入配置，重启不残留",
    ),
    ("— 连接表单 —", ""),
    ("↑ ↓ / Tab", "切换字段：db_type → name → host → port → user → password → database → query_timeout（开启 ssh_tunnel 后自动展开 SSH 段）"),
    ("Enter", "编辑字段 / 切换开关 / 保存连接"),
    ("query_timeout", "查询超时秒数：留空=默认 60s，0=不限；PostgreSQL 同时以 statement_timeout 连接选项生效（连接级，不逐条查询）"),
    ("默认端口", "选定 db_type 即带出 MySQL 3306 / PG 5432 / Redis 6379 / Mongo 27017；手动改过 port 则不覆盖"),
    ("name 留空", "保存时按 host-db_type 自动生成连接名（如 localhost-postgres）"),
    ("Space", "切换 ssh_tunnel / ssl / read_only / 登录方式"),
    (
        "color",
        "Space 循环预设颜色（无色→10 色→自定义），Enter 输入 #RRGGBB；色块为只读预览",
    ),
    (
        "ssh_tunnel",
        "开启 SSH 跳板隧道（ssh_host / ssh_port / ssh_user / 登录方式）",
    ),
    (
        "read_only",
        "只读连接：拒绝 INSERT/UPDATE/DELETE/DDL（SELECT/SHOW/EXPLAIN 照常；树中显 🔒）",
    ),
    (
        "登录方式",
        "password / key（密钥路径 + 口令）/ agent（SSH_AUTH_SOCK）",
    ),
    (
        "远端目标",
        "隧道转发目标 = 连接的 host:port（改 host / port 即改目标）",
    ),
    (
        "~/.ssh/config",
        "ssh_host 可填别名；ProxyJump 自动展开为多跳",
    ),
    (
        "SSH 主机密钥",
        "首次连接弹出指纹确认（y 接受并记住 / s 仅本次 / n 拒绝）",
    ),
    ("— 连接导入 / 导出（Alt-E / Alt-I）—", ""),
    (
        "Alt-E",
        "导出全部连接为 JSON 包（默认 ~/dbxt-connections.json）",
    ),
    (
        "Enter / y / p",
        "Enter 写文件 · y 复制 JSON 到剪贴板 · p 切换含密码导出（红色确认）",
    ),
    (
        "Alt-I / i",
        "导入连接：自动识别 dbxt JSON / DBeaver data-sources.json / Navicat .ncx",
    ),
    (
        "预览 s/r/b",
        "同名策略：s 跳过 · r 覆盖（红色确认，按 name 匹配） · b 都存（名加 -imported）",
    ),
    (
        "预览 Space / d",
        "Space 勾选/取消该条 · d 逐条循环 跳过/覆盖/都存 · Enter 导入",
    ),
    (
        "密码",
        "导出默认不含密码（p 显式开启）；DBeaver / Navicat 密码加密，不解析，导入后标「需补密码」",
    ),
    ("— 侧栏（连接树）—", ""),
    (
        "↑ ↓ / j k",
        "在 连接 → 库 → 表 树上移动（可计数：3 j 下移 3 项）",
    ),
    (
        "h l / ← →",
        "折叠 / 展开当前节点：展开的节点收起、再按回到父层；l 打开收起的节点并进入首个子项（连接列库 / 库列表，分组同规则）",
    ),
    ("Enter", "打开：连接=切换并展开 · 库=切到该库 · 表=浏览数据"),
    ("1-9", "直跳第 N 个连接 / 表（树光标跟随）"),
    (
        "3 j / 3 k",
        "计数前缀：下 / 上移动 3 项（侧栏 / 结果 / 历史通用）",
    ),
    (
        "a-z / /",
        "过滤：命中表名 / 库名，父节点保留（Enter 打开首个命中，Esc 清除）",
    ),
    (
        "f",
        "快速搜索连接 / 库 / 表名（跨组搜，命中组自动展开；纯客户端不回库，Enter 跳到首个命中并清输入，Esc 清除）",
    ),
    ("Ctrl-U / Alt-⌫", "清除表过滤 / 树搜索（提示框内）"),
    ("s", "表排序：名称 / 类型（TABLE / VIEW）"),
    (
        "s（库行）",
        "惰性查询该库聚合大小 + 各表行数估计（information_schema，不扫表；会话缓存）",
    ),
    (
        "Y",
        "复制连接（新名字 xxx-copy，含密码 / SSH 隧道，树中新根）",
    ),
    (
        "状态点（连接根）",
        "● 活跃（可查）/ ○ 已断开 / ◐ 连接中；沿用连接色，形状区分（色盲友好）",
    ),
    (
        "x（连接根）",
        "断开连接：关闭连接池（未提交手动事务回滚）；树保留灰根，展开可重连",
    ),
    (
        "!（连接树 / Redis 键列表）",
        "切换当前连接只读开关（红色确认）：只读下写语句 / 删行 / Redis 写 / 导入全部拦截；连接根显 🔒",
    ),
    ("尺寸列", "库大小 / 表行数估计右对齐；终端 <56 列自动隐藏"),
    (
        "分组节点",
        "DBX 桌面的连接分组（▾ 组名 [n]）；h l / ← → 折叠展开，会话内记忆；无分组则平铺",
    ),
    (
        "x（分组节点）",
        "分组行无连接池：x 无动作（不会误进表过滤）",
    ),
    (
        "Alt+a-z · ; ,",
        "首字母跳：跳到以该字母开头的下一张表；; , 前后循环（与过滤互斥）",
    ),
    ("t", "最近表浮层（Enter 直达 · s 排序）"),
    (
        "i（表节点）",
        "表信息卡：类型 / 引擎 / 注释 / 列数 / 索引 / 行数估算 / 数据大小 / 创建时间——全部读已缓存元数据（打开过的表 + 库行按 s 的估计），绝不发查询；从未打开的表提示「打开表后可用」；Esc 关闭",
    ),
    ("r", "表结构（字段 + DDL）"),
    (
        "r（连接根 / 分组行）",
        "就地重命名：改好后 Enter 保存、Esc 取消、Ctrl-U 清空（空 / 超长会提示）",
    ),
    (
        "Shift+↑/↓",
        "在同一层内上 / 下移动连接或分组（改桌面分组顺序并写回 sidebar_layout；顶层未分组连接按名称排序）",
    ),
    ("I", "导入 CSV 到当前表（预览 + 追加/覆盖确认）"),
    (
        "d",
        "数据库 / 模式列表（PG 等支持 schema 的连接；浮层内 r 刷新）",
    ),
    ("[ ]", "切换数据库（快捷）"),
    ("o", "返回连接选择"),
    ("c", "新建连接"),
    ("p", "复制连接（预填表单）"),
    ("— 结果（表格浏览）—", ""),
    ("↑ ↓ / j k", "行光标（到边自动翻页）"),
    ("PgUp / PgDn", "整屏滚动，跨页衔接"),
    (
        "Ctrl-U",
        "半屏向上滚动（Ctrl-D 让位给「删除当前行」；编辑器内为撤销）",
    ),
    ("n / p", "下一页 / 上一页（可计数：5 n = 翻 5 页）"),
    ("Ctrl-F / Ctrl-B", "下一页 / 上一页"),
    (
        "大表翻页",
        "有主键时按主键续读（keyset），翻页耗时与页深无关",
    ),
    ("行数上限", "50 万行以上的表显示 >50万，不再每页 COUNT"),
    ("← → / h l", "单元格光标（列窗口跟随）"),
    (
        "Home / End · g g / G",
        "首行 / 末行（列光标同时回第一列；脚本语句列表同样适用）",
    ),
    (
        "y（脚本列表）",
        "复制当前语句结果为 CSV（与 Ctrl-Y 导出的首选格式一致）",
    ),
    ("Ctrl-E", "聚焦 SQL 编辑器"),
    (
        "Shift/Alt/Ctrl+滚轮 · 横滑",
        "横向滚动列（触屏左右滑动 / 拖动）；PC 终端若 Shift+滚轮 无效，用 Ctrl+滚轮 或 Ctrl-G",
    ),
    ("Shift+← →", "横滚列一列（任意区域，按住连滚）"),
    ("Ctrl-G", "横滚模式：纵向滚轮/上下滑改为横滚列"),
    (
        "◀ ▶（底部）",
        "点击向左/右翻一屏列（触屏可用；滚动条横滚后短暂显示，静止自动隐藏）",
    ),
    (
        "底部进度条",
        "当前列窗口位置 · 横滚后 2.5s 内显示 · 点击可跳转",
    ),
    ("[ ]", "切换本次会话的结果标签"),
    (
        "Alt-W",
        "关闭当前结果标签（最后一个不可关；有未确认编辑时先处理；纯客户端，不查询）",
    ),
    (
        "Ctrl-Y",
        "导出当前结果（CSV / JSON / NDJSON / Markdown / INSERT）",
    ),
    ("y", "复制当前行为 INSERT 语句（OSC52 + 文件兜底）"),
    ("Y", "复制当前单元格值（状态栏显示列名与字符数）"),
    (
        "V",
        "行选模式：↑↓ 移动 · Shift+↑↓ / v 扩展 · Y 复制 TSV（含列头）· d 生成 DELETE · c 生成 UPDATE 模板 · Esc 退出；d/c 只把语句送进编辑器，绝不执行",
    ),
    (
        "/",
        "搜索结果行（隐藏不匹配行，输入即筛，Enter 保留，Esc 清除）",
    ),
    (
        "\\",
        "在结果集里查找词：命中单元格标亮（Esc 清除；/ 是隐藏不匹配行，\\ 是标亮定位）",
    ),
    (
        "*",
        "按当前列过滤：输入值只留该列含值的行（预填当前单元格，Esc 清除）",
    ),
    (
        "g v",
        "定位值：在排序列 / 主键列内搜值并跳转，不隐藏行（n/N 循环命中）",
    ),
    ("|", "跳列：输入列号或列名前缀直达该列（宽表横滚）"),
    (
        "(:)",
        "跳行：输入行号直达该行（超出范围钳到末行）；分页表视图按整数行号跳到目标页，:$ 跳末行（结果 / 表 / Redis / Mongo 均可）",
    ),
    (
        "{ }",
        "跳到上 / 下一个非空单元格所在行（跳过 NULL / 空串，状态栏显示行号；n / p 仍为翻页）",
    ),
    (
        "n / Shift-N",
        "结果搜索 / 定位 / 单元格查找命中时：下 / 上一个命中（否则 n 翻页）",
    ),
    ("Ctrl-N", "结果被截断时加载更多行"),
    (
        "Enter",
        "整行详情（纵向，含隐藏列；看某一行从这里进；结果区双击行同效）",
    ),
    ("v", "完整单元格（任意模式，不进整行弹层；JSON 对象/数组自动美化缩进）"),
    ("o", "整行详情（与 Enter 等价）"),
    ("e", "编辑单元格 → diff 确认后执行"),
    ("i", "快速插入 → diff 确认后执行"),
    ("Delete / Ctrl-D", "删除当前行 → 确认后执行"),
    ("f", "WHERE 过滤（预填当前列）"),
    ("Ctrl-R", "清除过滤"),
    ("s", "按当前列升 / 降序"),
    ("Ctrl-K", "附加排序键（多列排序）"),
    ("z", "钉住 / 取消首列"),
    (
        "w / Alt-C",
        "紧凑列宽 开 / 关（窄屏默认自动开，按 库.表 记住）",
    ),
    (
        "#",
        "大数字显示：原样 → 千分位 → 缩写 三档循环（1,234,567 / 1.2M；仅数字列，复制 / 编辑仍用原值）· 默认关闭，# 手动开启，跨会话记住",
    ),
    (
        "%",
        "斑马纹：结果集奇偶行底色微差 开 / 关 · 默认关闭，% 手动开启，跨会话记住",
    ),
    (
        "< / > / 0",
        "收窄 / 加宽 / 复位当前列（按 库.表+列名 记忆并跨会话持久化；0 复位当前列）",
    ),
    (
        "Alt-0",
        "清除当前表全部列宽记忆（会话 + tui.json）",
    ),
    (
        "c / Alt-V",
        "列显隐浮层（空格勾选 / a 全选 / x 仅首列，按 库.表 记住）",
    ),
    ("Alt-R", "最近表直达浮层"),
    ("t", "字段 ↔ DDL（表结构）"),
    ("g d / g t", "跳表结构视图 / 回表数据"),
    (
        "g c",
        "列结构弹层：列名 / 类型 / 键(PRI/UNI/MUL) / 默认值 / 可空 / 注释；右侧就地显示选中列的值分布（非空/空/去重，去重旁附 12 格分布 sparkline，数值列 min/max/avg；缓存元数据+已加载数据，不额外查库；窄屏 < 56 列隐藏 sparkline；/ 过滤列名；Enter 跳到该列）",
    ),
    (
        "g b",
        "切换同库其他表：输入即过滤的浮层（复用最近表样式，↑↓/j/k 选），Enter 打开该表数据",
    ),
    (
        "Alt-F",
        "钉住 / 解除当前结果区（钉住后切换表/库仍显示，上下对照）",
    ),
    ("g v", "定位值（排序列 / 主键列，不隐藏行）"),
    ("Esc", "收起结果 / 关闭浮层（状态栏 1.5 秒提示「关闭 X / 已清除 Y」）"),
    ("— 行详情浮层（Enter / o）—", ""),
    ("↑ ↓ / j k / n p · 5j", "移动选中列（计数前缀：5j 跳 5 列；n/p 与 j/k 同义）"),
    (
        "Enter / v",
        "下钻完整单元格（Esc 返回行弹层，再 Esc 回表格）",
    ),
    ("y / Y", "复制选中列值（y / Y 均可；状态栏带列名，与结果区 Y 同一路径）"),
    ("/", "按列名或值过滤（输入即筛；宽表 40+ 列找列）"),
    ("< 56 cols", "窄屏：每行「字段:」+ 缩进值单列自适应"),
    ("标题", "主键定位：第 12 行 · id=4821"),
    ("— 单元格弹层（v）—", ""),
    ("↑ ↓ / j k · PgUp/PgDn", "滚动长值（换行结果缓存，100 KB 单元格也不卡）"),
    (
        "J",
        "JSON 对象/数组：美化 ↔ 原值切换（键/字符串/数字用主题色区分；非 JSON 时提示）",
    ),
    (
        "y / Y",
        "复制原值（美化视图下仍复制原始 JSON，不复制缩进格式）",
    ),
    (
        "时间戳",
        "整型值落在 epoch 秒范围（1e9~4e10）时，弹层底部灰显本地时间 + 相对时间（如 2024-06-01 12:34:56 · 3 天前）；仅预览，不改数据、不猜测时区语义",
    ),
    ("Esc / Enter / q", "关闭（从行弹层下钻时先回行弹层）"),
    ("— 编辑确认层 —", ""),
    ("Enter", "执行（UPDATE / INSERT，SQL 全文可见）"),
    ("Esc", "取消编辑"),
    ("Ctrl-V", "将生成的 SQL 转入编辑器微调"),
    ("Ctrl-T", "加入批量队列（Ctrl-S 打包事务提交）"),
    (
        "插入层 v / b",
        "转编辑器 / 加入批量（等价 Ctrl-V / Ctrl-T）",
    ),
    ("Ctrl-S / Ctrl-X", "提交 / 清空批量队列"),
    ("— 编辑器 / 命令 —", ""),
    (
        "Alt-H",
        "查询历史面板（最近 300 条：时间 / 摘要 / 来源连接；顶部 ● 为本次会话内存记录）",
    ),
    (
        "Alt-F",
        "格式化当前 SQL（关键字大写 / 子句换行）；再按压缩为单行",
    ),
    (
        "Ctrl-Z / Ctrl-U",
        "撤销（Alt-F 格式化或编辑历史；状态栏提示已撤销 / 没有可撤销的）",
    ),
    (
        "Ctrl-D",
        "半屏向下滚动（Ctrl-U 让位给撤销；Del 键仍删后一个字符）",
    ),
    (
        "Ctrl-Y / Ctrl-R",
        "重做上一次撤销（状态栏提示已重做 / 没有可重做的；编辑器内 Ctrl-Y 原为内部 yank，改到 Alt-Y）",
    ),
    ("Alt-Y", "粘贴内部 yank 缓冲区（Ctrl-K 删掉的内容）"),
    (
        "Alt-S",
        "收藏当前 SQL 为片段（编辑器内一步；等价 Ctrl-O 面板内 s）",
    ),
    (
        "Alt-↓ / Alt-↑",
        "跳到下 / 上一条 SQL 语句开头（分号边界，注释/空语句跳过；状态栏显示 语句 i/n；当前语句高亮、其余淡化）",
    ),
    (
        "F8 / Shift+F8 · Alt-E",
        "跳到本次执行出错的语句：整段红色高亮 + 光标移到语句首，状态栏保留错误并追加 第 N 条语句；多条语句失败时循环切换（F8 下一个 / Shift+F8 上一个）；纯文本定位，不发任何查询。Alt-E 只在已定位到错误时生效，否则仍是连接导出",
    ),
    (
        "Ctrl-F",
        "编辑器内查找：底栏输入，Enter/F3/Alt-N 下一个、Alt-B 上一个（Shift-Enter / Shift-F3 在支持的终端也可用）；命中高亮 + 状态栏 3/7 计数；大小写不敏感、纯客户端不发查询；Esc 退出保留高亮，下次编辑自动清除",
    ),
    (
        "F3 / Shift-F3 · Alt-N / Alt-B",
        "查找命中循环：F3/Alt-N 下一个、Alt-B 上一个（Esc 退出查找后仍可用；无查找词时按下即打开查找框）",
    ),
    (
        "Alt-/",
        "SQL 前缀补全（表名 T / 列名 C / 关键字 K，Tab 上屏）",
    ),
    ("Alt-P", "片段收藏：选中即插到光标处（一步）"),
    (
        "Alt-T（编辑器）",
        "常用 SQL 模板面板（内置只读；Enter 插入光标处并选中第一个 {{}} 占位符，Tab 跳下一个；片段 Ctrl-O 为用户自存）",
    ),
    ("%", "跳到配对括号（光标在 ()[]{} 上或旁；否则照常输入 %；停在括号上时配对项自动高亮）"),
    (
        "Enter",
        "回车自动缩进：上一行以 SELECT/FROM/WHERE/AND/OR/JOIN/ON/SET/VALUES 或 ( [ { , 结尾，或含未闭合括号时，新行继承缩进 + 2 空格；纯空白行不继承（默认开；tui.json 的 editor_indent 可关）",
    ),
    (
        "( [",
        "括号自动补对：输入 ( / [ 自动补上 ) / ] 并把光标停在中间；光标处已是相同的右括号时按 ) / ] 直接跳过；字符串 / 注释内照常输入（默认开；tui.json 的 editor_pairs 可关）",
    ),
    ("Ctrl-A / Ctrl-E", "行首 / 行尾（Home / End 同）"),
    ("Ctrl-K / Ctrl-⇧K", "删至行尾（kill line）"),
    ("Ctrl-W", "删前一个词"),
    ("— 全库搜索（Alt-G）—", ""),
    (
        "Alt-G",
        "全库搜索：扫描当前连接所有表的文本列（每表 LIMIT，大表跳过）",
    ),
    ("↑ ↓ / Enter", "选择命中 / 跳到该表并定位到命中行"),
    ("y / r", "复制命中值 / 以同一关键词重搜"),
    ("Esc", "关闭；扫描中按一下中止（保留已扫描结果）"),
    (
        "Alt-L",
        "加载并执行 .sql 文件（预览语句数/大小/目标库，危险语句先确认）",
    ),
    ("— 结构对比（Alt-D）—", ""),
    (
        "Alt-D",
        "结构对比：源 = 当前表，选择目标表（对比列 / 主键 / 索引 / 字符集）",
    ),
    (
        "c（对比浮层内）",
        "选择其他连接作为目标（跨库 / 跨方言对比）",
    ),
    (
        "Shift+Alt-D",
        "库对库对比：两库的表清单（仅源 / 仅目标 / 共有）",
    ),
    ("d（对比浮层内）", "切换 表 / 库 两种对比模式"),
    ("Tab", "切换 列 / 索引 / ALTER 三个视图"),
    ("y", "复制差异摘要（纯文本，可贴进工单）"),
    ("g", "生成 ALTER 同步语句（方向：源 → 目标，只生成不执行）"),
    ("Enter", "对比：库清单里两库都有的表进入单表对比"),
    ("— 数据对比（Alt-K）—", ""),
    (
        "Alt-K",
        "数据对比：按主键对齐两张表的数据（源 = 当前表；选择目标，可跨连接）",
    ),
    ("m（对比浮层内）", "切换 结构对比 / 数据对比"),
    ("w（数据对比内）", "输入 WHERE 过滤（两边同时生效，可留空）"),
    ("Tab", "切换 汇总 / 仅源 / 仅目标 / 差异 四个视图"),
    ("Enter", "展开差异行的列级对照（两侧值对照）"),
    ("y", "复制差异摘要（纯文本）"),
    (
        "g",
        "生成同步 INSERT/UPDATE/DELETE（方向：源 → 目标，只生成不执行）",
    ),
    ("Esc", "关闭；对比进行中按一下中止（保留已比结果）"),
    ("— 数据搬运（Alt-T）—", ""),
    (
        "Alt-T",
        "跨库搬数据：源 = 当前表，目标可同连接或跨连接/跨方言（MySQL↔PG 双向）；编辑器内 Alt-T 为内置模板面板",
    ),
    ("① 选目标连接", "Enter 下一步；默认当前连接（同方言）"),
    (
        "② 目标库/表",
        "Tab 切换 库/Schema/表名；改名 = 表复制；默认同名",
    ),
    (
        "③ 模式",
        "建表+搬数据（默认）/ 仅建表 / 插入已有表（append）",
    ),
    (
        "表已存在（o）",
        "报错停下（默认）或 覆盖 = 先 DROP（红色确认层）",
    ),
    (
        "选项",
        "w WHERE 子集 · l LIMIT 上限 · i 带索引 · a 自增值 · s 停止/跳过",
    ),
    (
        "搬运引擎",
        "源 keyset 分块（1000 行）→ 目标事务批量 INSERT（500/批）",
    ),
    (
        "进度 / 中止",
        "状态栏显示 行数/块数/速率；Esc 中止（已提交批次保留）",
    ),
    (
        "大表防护",
        "源预估 ≥100 万行需再按 Enter 确认；单批失败重试 1 次",
    ),
    (
        "完成汇总",
        "g 复制摘要 · b 浏览目标表（同连接/库时）· Esc 关闭",
    ),
    (
        "补全上下文",
        "表名. 后只补该表列名；FROM/JOIN 后只补表名；WHERE/ON 后只补列名",
    ),
    ("↑ ↓", "历史（首行 / 末行）"),
    ("Esc", "回到侧栏"),
    ("[ ]", "Redis 逻辑库"),
    ("use <db>", "MongoDB 切库"),
    ("— Redis key 浏览器 —", ""),
    ("↑ ↓ / Enter", "选择 key / 查看 value"),
    (
        "a-z / f",
        "按已加载 key 子串过滤（一步直达，命中高亮；Enter 查看首位，Esc 清除）",
    ),
    (
        "Alt+a-z · ; ,",
        "首字母跳：跳到以该字母开头的下一个 key；; , 前后循环",
    ),
    ("1-9", "直跳第 N 个已加载 key"),
    ("Space / Shift+↑↓", "勾选 key / 范围选（a 全选已加载）"),
    ("/", "编辑 SCAN MATCH 模式（服务端，留空 = 全部）"),
    ("n / End", "加载下一 SCAN 页"),
    ("r", "以当前模式重扫"),
    ("[ ]", "切换逻辑 db"),
    (
        "Del / x / m",
        "删除 / 设 TTL / 前缀重命名选中 key（均确认）；删单个 key 就地移除，SCAN 游标不动",
    ),
    ("T", "设置焦点 key 的 TTL（红确认层；秒，可加 s/ms/m/h/d 后缀；只读连接拦截）"),
    ("t", "按类型循环过滤 string/hash/list/set/zset/stream（客户端，零查询）"),
    ("Ctrl-T", "按 TTL 排序已加载 key：扫描顺序 → 升序 → 降序 循环（客户端）"),
    (
        "行尾 TTL",
        "紧凑显示 `45s` / `5m` / `2h` / `3d`，`-1` 永久、`-2` 不存在；每秒本地倒计时",
    ),
    ("y", "复制选中的 key 名（每行一个）"),
    (
        "value: e x m Del",
        "编辑 string·hash 字段 / TTL / 重命名 / 删除 key（均确认）",
    ),
    ("value: y / Esc", "复制值（string）/ 返回 key 列表"),
    ("value: n", "大集合继续加载 200 项"),
    ("窄屏徽章", "类型与 TTL 融合为单行 `S·12s`，key 名不换行；TTL 秒数本地倒计时刷新"),
    ("— MongoDB 文档浏览器 —", ""),
    ("Enter", "浏览 collection 文档（JSON 网格）"),
    ("n / p", "文档翻页"),
    ("f", "JSON 过滤（如 {\"age\": {\"$gt\": 30}}，留空清除）"),
    (
        "g f",
        "跳到首个含该字段的文档（已加载页内客户端跳转；点路径支持嵌套，不发查询）",
    ),
    (
        "Ctrl-S",
        "按文档字节数排序：原始顺序 → 大小降序 → 大小升序（纯本地，行尾 size(B) 列）",
    ),
    (
        "c",
        "按点路径提取子值并复制（如 a.b.0.name；路径不存在则状态栏报错）",
    ),
    (
        "e / i / Del",
        "编辑 / 插入 / 删除文档（均确认，_id 不可改）",
    ),
    ("y / Esc", "复制当前文档 JSON / 返回集合列表"),
    (
        "a-z / Alt+a-z / 1-9",
        "集合列表：子串过滤 / 首字母跳 / 直跳（同表列表）",
    ),
    ("r", "查看 collection 索引"),
    ("— 危险操作 / 删除确认 —", ""),
    ("Enter / y", "执行（SQL 全文可见）"),
    ("Esc / n", "取消"),
    ("— SQL 片段（Ctrl-O）—", ""),
    ("↑ ↓ / Enter", "选择 / 插入到编辑器"),
    (
        "s",
        "把编辑器里的 SQL 收藏为片段（写入 DBX saved_sql_files）",
    ),
    (
        "/",
        "过滤收藏：匹配名称 / SQL 文本（大小写不敏感子串）",
    ),
    (
        "d / Del",
        "删除选中收藏（红色确认；只删本地配置，不动数据库）",
    ),
    ("上限 100", "收藏上限 100 条，满时先删再存"),
    ("r / Esc", "刷新 / 关闭"),
    ("— 查询历史（Alt-H）—", ""),
    ("↑ ↓ / PgUp PgDn", "移动光标（列表即过滤视图）"),
    ("Enter", "回填到编辑器（关面板，光标到末尾）"),
    ("Ctrl-↵ / p", "直跑选中语句（不经编辑器，结果直接进结果区）"),
    ("f", "收藏 / 取消收藏该条（同一 DBX saved_sql_files 存储）"),
    ("y / Y", "复制整条语句"),
    ("●", "青色圆点 = 本次会话内 dbxt 自己发到服务端的语句（内存 LRU 20，重复语句去重上浮；只读内存不落盘、不发查询，退出即清空）"),
    ("Del", "删除单条历史（红色确认，不影响数据库数据）"),
    (
        "/",
        "过滤历史：匹配语句文本 / 来源连接 / 来源标（大小写不敏感子串）",
    ),
    ("×n", "同一语句多次执行合并为一行并计数（Ctrl-↵ / p 仍直跑该条）"),
    ("— 鼠标 / 触屏 —", ""),
    (
        "点击（结果区）",
        "选中该行；同一位置 400ms 内再点一次 = 双击，打开整行详情（等价 Enter）",
    ),
    (
        "双击（行弹层）",
        "下钻该值到完整单元格弹层（等价 Enter）；单击 = 光标移到该值",
    ),
    (
        "点击（▶ / ▼ 图标）",
        "折叠 / 展开该连接或库（不必先选中该行）；行其余部分仍是两击选中 + 激活",
    ),
    (
        "点击（确认弹层按钮）",
        "点 [ 执行 ] / [ 取消 ] = Enter / Esc 两条分支",
    ),
    (
        "点击（错误弹层）",
        "紧凑态点开全量；长错误逐页下翻，翻到底再点关闭",
    ),
    ("点击（单元格弹层）", "关闭，回到下面的行弹层"),
    (
        "点击（编辑器）",
        "聚焦并把光标放到点击处（含横滚偏移；点在文本下方 = 跳文末）",
    ),
    (
        "滚轮 / 横滑",
        "纵向滚行；Shift/Alt/Ctrl+滚轮 或左右滑动 = 横滚列",
    ),
];

/// Width of the `?` help overlay. The cheat-sheet has grown a lot (R15–R33),
/// so on a wide terminal it takes almost the whole screen — capped at 96
/// columns instead of the old 64. Narrow terminals keep the previous fallback:
/// all but 4 columns, or the whole area when even that is too small.
pub(crate) fn help_overlay_width(area_width: u16) -> u16 {
    overlay_width(area_width, 96, 30)
}

/// Width of the help key column. Wide layouts get 24 columns so long chords and
/// descriptive labels stay on one line; narrow layouts keep the old 16 (and
/// shrink further rather than overflow the box).
pub(crate) fn help_key_width(w: u16) -> usize {
    let base = if w >= 72 { 24 } else { 16 };
    base.min(w.saturating_sub(6) as usize)
}

/// One display row of the `?` cheat-sheet. Section headers and blank spacers
/// span the full width; items carry the keycap + description pair and whether
/// they belong to the surface underneath the overlay (so they can float first).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum HelpRow {
    Section(&'static str),
    Blank,
    Item {
        key: &'static str,
        desc: &'static str,
        relevant: bool,
    },
}

/// Split a keycap into comparable tokens: lowercase words plus the individual
/// arrow glyphs, so `↑ ↓ / j k` and `↑↓` compare meaningfully.
pub(crate) fn help_key_tokens(key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        let s = s.to_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    for raw in key.split(['/', ' ', '+', ',', '·']) {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        add(t);
        for ch in t.chars() {
            if matches!(ch, '↑' | '↓' | '←' | '→') {
                add(&ch.to_string());
            }
        }
    }
    out
}

/// R60: the keycaps that matter on the surface underneath the help overlay,
/// derived from the same footer hints the status bar shows (plus a few results
/// keys the compact footer drops for width). Used to float those rows first.
pub(crate) fn help_context_tokens(app: &App) -> Vec<String> {
    let ctx = footer_ctx_inner(app, false);
    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, k: &str| {
        for t in help_key_tokens(k) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    };
    // The results surface: the spec's `v / Y / *` trio leads, so the quick-access
    // block opens on exactly the keys the user is looking at.
    if ctx.view == FooterView::Browse && app.focus == Focus::Preview {
        for k in ["*", "Y", "v", "y", "<", ">", "gg", "G", "#", "%"] {
            push(&mut out, k);
        }
    }
    for (k, _) in footer_hints_ctx(ctx) {
        push(&mut out, k);
    }
    out
}

/// Case-insensitive match of the `/` needle against a row's keycap or
/// description, in both languages (the cheat-sheet is data, so the English
/// translation is looked up directly).
pub(crate) fn help_needle_matches(needle: &str, key: &'static str, desc: &'static str) -> bool {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return true;
    }
    let hit = |s: &str| s.to_lowercase().contains(&n);
    hit(key)
        || hit(desc)
        || hit(ui_text::t_lang(key, ui_text::Lang::En))
        || hit(ui_text::t_lang(desc, ui_text::Lang::En))
}

/// The unfiltered, grouped reference: sections in table order, a blank spacer
/// before every section after the first. `tokens` marks context-relevant rows.
pub(crate) fn help_grouped_rows(tokens: &[String]) -> Vec<HelpRow> {
    let mut rows: Vec<HelpRow> = Vec::with_capacity(HELP_ROWS.len() + 16);
    let mut first_group = true;
    for (k, d) in HELP_ROWS {
        if d.is_empty() {
            if !first_group {
                rows.push(HelpRow::Blank);
            }
            first_group = false;
            rows.push(HelpRow::Section(k));
        } else {
            let relevant = help_key_tokens(k).iter().any(|t| tokens.contains(t));
            rows.push(HelpRow::Item {
                key: k,
                desc: d,
                relevant,
            });
        }
    }
    rows
}

/// Build the cheat-sheet's display rows for the current filter + context. With
/// no filter, a `— 当前上下文 —` quick-access section (context-relevant rows)
/// leads the grouped reference; with a filter, a flat list of matches with the
/// context-relevant ones first.
pub(crate) fn help_rows(app: &App) -> Vec<HelpRow> {
    let tokens = help_context_tokens(app);
    let needle = app.help_needle.trim().to_string();
    if !needle.is_empty() {
        let mut rows: Vec<HelpRow> = Vec::new();
        // Which sections the needle names directly (so “结果” lists that group).
        let mut section_hit = false;
        for (k, d) in HELP_ROWS {
            if d.is_empty() {
                section_hit = help_needle_matches(&needle, k, "");
            } else if section_hit || help_needle_matches(&needle, k, d) {
                let relevant = help_key_tokens(k).iter().any(|t| tokens.contains(t));
                rows.push(HelpRow::Item {
                    key: k,
                    desc: d,
                    relevant,
                });
            }
        }
        // Context-relevant matches float first (stable within each bucket).
        rows.sort_by_key(|r| match r {
            HelpRow::Item { relevant: true, .. } => 0,
            _ => 1,
        });
        return rows;
    }
    let mut rows: Vec<HelpRow> = Vec::new();
    // Quick-access section: the context's own rows, in the order the context
    // lists its keys (so the surface's headline keys lead), deduped and capped
    // so the first screen is the surface the user is actually on.
    let mut quick: Vec<(&'static str, &'static str)> = Vec::new();
    // First pass: at most two rows per context token, so a broad token like `y`
    // cannot crowd out the surface's other headline keys.
    'outer: for tok in &tokens {
        let mut taken = 0;
        for (k, d) in HELP_ROWS {
            if d.is_empty() {
                continue;
            }
            if help_key_tokens(k).contains(tok) && !quick.iter().any(|(qk, _)| qk == k) {
                quick.push((k, d));
                taken += 1;
                if taken >= 2 || quick.len() >= 10 {
                    break;
                }
            }
        }
        if quick.len() >= 10 {
            break 'outer;
        }
    }
    // Second pass: top the block up from any remaining relevant rows.
    if quick.len() < 10 {
        for (k, d) in HELP_ROWS {
            if d.is_empty() {
                continue;
            }
            if !quick.iter().any(|(qk, _)| qk == k)
                && help_key_tokens(k).iter().any(|t| tokens.contains(t))
            {
                quick.push((k, d));
                if quick.len() >= 10 {
                    break;
                }
            }
        }
    }
    if !quick.is_empty() {
        rows.push(HelpRow::Section("— 当前上下文 —"));
        for (k, d) in &quick {
            rows.push(HelpRow::Item {
                key: k,
                desc: d,
                relevant: true,
            });
        }
        rows.push(HelpRow::Blank);
    }
    rows.extend(help_grouped_rows(&tokens));
    rows
}

/// One keycap/description cell. `desc_w = None` leaves the description whole
/// (single column); `Some(w)` truncates it to `w` columns and pads the cell to
/// a fixed width so two-column packing lines up.
pub(crate) fn help_item_spans(
    key: &'static str,
    desc: &'static str,
    key_w: usize,
    desc_w: Option<usize>,
    relevant: bool,
) -> Vec<Span<'static>> {
    let key_style = if relevant {
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    };
    let keytext = format!("{:<key_w$}", t(key));
    let mut spans = vec![Span::styled(keytext, key_style), Span::raw(" ")];
    match desc_w {
        None => spans.push(Span::raw(t(desc))),
        Some(w) => {
            let dtext = truncate_disp(t(desc), w);
            let used = key_w + 1 + disp_width(&dtext);
            spans.push(Span::raw(dtext));
            let cell_w = key_w + 1 + w;
            if used < cell_w {
                spans.push(Span::raw(" ".repeat(cell_w - used)));
            }
        }
    }
    spans
}

/// Single-column render: one key/description pair per line.
pub(crate) fn help_lines_single(rows: &[HelpRow], key_w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len());
    for row in rows {
        match row {
            HelpRow::Blank => lines.push(Line::from("")),
            HelpRow::Section(s) => lines.push(Line::from(Span::styled(
                t(s),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))),
            HelpRow::Item {
                key,
                desc,
                relevant,
            } => lines.push(Line::from(help_item_spans(
                key, desc, key_w, None, *relevant,
            ))),
        }
    }
    lines
}

/// Two-column render (wide terminals): items pack two per line, headers and
/// spacers span the full width so the sections stay legible.
pub(crate) fn help_lines_two_col(
    rows: &[HelpRow],
    key_w: usize,
    col_w: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len() / 2 + 4);
    let mut pending: Option<Vec<Span<'static>>> = None;
    let flush = |pending: &mut Option<Vec<Span<'static>>>, lines: &mut Vec<Line<'static>>| {
        if let Some(left) = pending.take() {
            lines.push(Line::from(left));
        }
    };
    for row in rows {
        match row {
            HelpRow::Item {
                key,
                desc,
                relevant,
            } => {
                let cell = help_item_spans(
                    key,
                    desc,
                    key_w,
                    Some(col_w.saturating_sub(key_w + 1)),
                    *relevant,
                );
                match pending.take() {
                    None => pending = Some(cell),
                    Some(mut left) => {
                        left.push(Span::raw("  "));
                        left.extend(cell);
                        lines.push(Line::from(left));
                    }
                }
            }
            HelpRow::Section(s) => {
                flush(&mut pending, &mut lines);
                lines.push(Line::from(Span::styled(
                    t(s),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            HelpRow::Blank => {
                flush(&mut pending, &mut lines);
                lines.push(Line::from(""));
            }
        }
    }
    flush(&mut pending, &mut lines);
    lines
}

/// The unfiltered single-column lines, kept as the reference layout for tests
/// and narrow terminals.
#[cfg(test)]
pub(crate) fn help_overlay_lines(key_w: usize) -> Vec<Line<'static>> {
    help_lines_single(&help_grouped_rows(&[]), key_w)
}

/// Choose the two-column key width so a wide column keeps room for the
/// description; falls back to a couple of characters on a very narrow one.
pub(crate) fn help_two_col_key_width(base: usize, col_w: usize) -> usize {
    base.min(col_w.saturating_sub(8)).clamp(2, 20)
}

/// R60: wide overlays pack two key/description pairs per line; narrow ones
/// (<56 columns) fall back to a single column so long descriptions stay
/// readable.
pub(crate) fn help_two_columns(w: u16) -> bool {
    w >= 56
}

pub(crate) fn render_help(f: &mut Frame, area: Rect, app: &mut App) {
    let w = help_overlay_width(area.width);
    let inner_w = w.saturating_sub(2) as usize;
    let two_col = help_two_columns(w);
    let rows = help_rows(app);
    let key_w = help_key_width(w);
    let mut lines = if two_col {
        let gutter = 2usize;
        let col_w = (inner_w.saturating_sub(gutter)) / 2;
        let kw = help_two_col_key_width(key_w, col_w);
        help_lines_two_col(&rows, kw, col_w)
    } else {
        help_lines_single(&rows, key_w)
    };
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            t("（没有匹配的快捷键）"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let filter_h = u16::from(app.help_filter.is_some());
    let total = lines.len();
    let max_h = area.height.saturating_sub(2).max(3);
    let h = (total as u16 + 2 + filter_h).min(max_h);
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
    let list_h = inner_h.saturating_sub(filter_h as usize);
    f.render_widget(Clear, box_area);
    let max_scroll = total.saturating_sub(list_h) as u16;
    let scroll = app.help_scroll.min(max_scroll);
    let filtered = !app.help_needle.trim().is_empty();
    let title = if filtered {
        fit_title(
            &tf(
                " 快捷键 · 过滤「{}」 {} 项 · Enter 保留 · Esc 清除 ",
                &[&(app.help_needle), &(total)],
            ),
            t(" 快捷键（已过滤）· Esc "),
            box_area.width,
        )
    } else if box_area.width < 56 {
        // Narrow: the footer already carries the scroll/close hints, so the
        // title only names the sheet and its position.
        tf(
            " 快捷键 · {}/{} ",
            &[&((scroll as usize + list_h).min(total)), &(total)],
        )
    } else {
        tf(
            " 快捷键 · {}/{} · / 过滤 · ↑↓ 滚动 · Esc 关闭 ",
            &[&((scroll as usize + list_h).min(total)), &(total)],
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (list_area, filter_area) = if app.help_filter.is_some() {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), list_area);
    if let Some(fa) = filter_area {
        if let Some(ta) = app.help_filter.as_mut() {
            ta.set_block(Block::default());
            f.render_widget(&*ta, fa);
        }
    }
}

/// R80: minimum number of keys a normal terminal's mini cheat-sheet lists. The
/// old `take(10)` silently dropped the newest keys on every context; a screen
/// tall enough for twelve rows always shows at least this many.
pub(crate) const MINI_HELP_MIN_ROWS: usize = 12;

/// How many context keys the mini cheat-sheet shows on a screen `area_h` rows
/// tall. The card is `rows + 2` tall (top / bottom border) and the overlay
/// keeps two rows of breathing room inside the screen, so `h` rows fit `h - 4`
/// hints; never fewer than [`MINI_HELP_MIN_ROWS`], and the card still never
/// scrolls (the box height is clamped to the screen).
pub(crate) fn mini_help_rows(area_h: u16) -> usize {
    (area_h.saturating_sub(4) as usize).max(MINI_HELP_MIN_ROWS)
}

/// The context mini cheat-sheet: as many keys for the surface that owns the
/// keyboard right now as the screen can hold (at least [`MINI_HELP_MIN_ROWS`]),
/// sized to fit one screen so it never scrolls. `?` again widens it to
/// [`render_help`].
pub(crate) fn render_help_mini(f: &mut Frame, area: Rect, app: &mut App) {
    // Reuse the footer's context-aware group for the surface *under* the mini
    // sheet; drop the pinned `?` hint and size the list to the screen height.
    let hints: Vec<Hint> = footer_hints_ctx(footer_ctx_inner(app, false))
        .into_iter()
        .filter(|h| h.0 != "?" && h.0 != "F1")
        .take(mini_help_rows(area.height))
        .collect();
    let key_w = hints
        .iter()
        .map(|h| disp_width(h.0))
        .max()
        .unwrap_or(4)
        .min(12);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(hints.len());
    for (key, desc) in &hints {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<key_w$}  ", key),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(*desc),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(t("当前上下文没有快捷操作")));
    }
    let max_h = area.height.saturating_sub(2).max(3);
    let h = (lines.len() as u16 + 2).min(max_h);
    let w = overlay_width(area.width, 64, 30);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let title = if box_area.width < 60 {
        tf(" 快捷键 · {} 项 ", &[&(hints.len())])
    } else {
        tf(
            " 快捷键 · 当前上下文 · {} 项 · ? 全部 · Esc 关闭 ",
            &[&(hints.len())],
        )
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        box_area,
    );
}

/// The two button rectangles of a confirmation layer, plus the line to draw.
///
/// `inner` is the box's content area and `y` the row the buttons sit on; when the
/// row is clipped by a very short terminal (or the box is too narrow) the
/// corresponding rectangle is empty, so a click there can never fire a branch the
/// user cannot see.
pub(crate) fn confirm_buttons(
    inner: Rect,
    y: u16,
    ok_label: &str,
    cancel_label: &str,
) -> (Line<'static>, Rect, Rect) {
    let ok = format!("[ {ok_label} ]");
    let cancel = format!("[ {cancel_label} ]");
    let gap = 2u16;
    let empty = Rect {
        x: inner.x,
        y,
        width: 0,
        height: 0,
    };
    let line = Line::from(vec![
        Span::styled(
            ok.clone(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            cancel.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    // A row outside the content area is clipped by the widget: report no target.
    if inner.height == 0 || y >= inner.y + inner.height {
        return (line, empty, empty);
    }
    let ok_w = (disp_width(&ok) as u16).min(inner.width);
    let cx = inner.x.saturating_add(ok_w).saturating_add(gap);
    let cancel_w = (disp_width(&cancel) as u16).min((inner.x + inner.width).saturating_sub(cx));
    (
        line,
        Rect {
            x: inner.x,
            y,
            width: ok_w,
            height: 1,
        },
        Rect {
            x: cx,
            y,
            width: cancel_w,
            height: 1,
        },
    )
}

/// The red layer for deleting a saved connection. Deliberately explicit that
/// only the connection config is removed, never the database's data. Returns the
/// `[ 删除 ]` / `[ 取消 ]` hit rectangles.
pub(crate) fn render_conn_confirm(f: &mut Frame, area: Rect, cc: &ConnConfirm) -> (Rect, Rect) {
    let w = overlay_width(area.width, 72, 30);
    // R47b: the disconnect variant shares this red layer but spells out what a
    // disconnect does (pools close, uncommitted manual transactions roll back)
    // and that the tree keeps its shape. R68 adds the read-only toggle variant.
    let (title, body, ok_label) = if let Some(new_ro) = cc.readonly {
        let (head, effect, ok) = if new_ro {
            (
                tf("将连接 {} ({}) 设为只读？", &[&cc.name, &cc.db_type]),
                t("开启后写语句 / 删行 / Redis 写 / 导入全部拦截（SELECT/SHOW 照常）"),
                t("Enter/y 设为只读"),
            )
        } else {
            (
                tf("将连接 {} ({}) 恢复为可写？", &[&cc.name, &cc.db_type]),
                t("关闭后该连接可再次执行写操作（重新允许 INSERT/UPDATE/DELETE/DDL）"),
                t("Enter/y 恢复可写"),
            )
        };
        (
            t(" ⚠ 切换只读开关 "),
            vec![
                Line::from(Span::styled(
                    head,
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(effect, Style::default().fg(Color::Yellow))),
                Line::from(Span::styled(
                    t("只改这条连接配置，不改数据库里的任何数据；树上随即显示/隐藏 🔒"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
            ],
            ok,
        )
    } else if cc.disconnect {
        (
            t(" ⚠ 断开连接 "),
            vec![
                Line::from(Span::styled(
                    tf("断开连接 {} ({})？", &[&cc.name, &cc.db_type]),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    t("未提交的手动事务将回滚；下次展开该连接时重新连接"),
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    t("侧栏保留该连接根（灰点），已缓存的库/表仍可见"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
            ],
            t("Enter/y 断开"),
        )
    } else {
        (
            t(" ⚠ 删除连接 "),
            vec![
                Line::from(Span::styled(
                    tf("将删除连接 {} ({})", &[&cc.name, &cc.db_type]),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    t("只删除这条连接配置，不会删除数据库里的任何数据"),
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
            ],
            t("Enter/y 删除"),
        )
    };
    let mut lines = body;
    let h = (lines.len() as u16 + 1 + 2)
        .min(area.height)
        .max(3.min(area.height));
    let box_area = centered_overlay(area, w, h);
    let inner = Rect {
        x: box_area.x + 1,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(2),
        height: box_area.height.saturating_sub(2),
    };
    let (buttons, ok, cancel) = confirm_buttons(
        inner,
        inner.y + lines.len() as u16,
        ok_label,
        t("Esc/n 取消"),
    );
    lines.push(buttons);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
    (ok, cancel)
}

/// Returns the `[ 执行 ]` / `[ 取消 ]` hit rectangles of the confirmation layer
/// (empty when the button row is clipped away).
pub(crate) fn render_confirm(f: &mut Frame, area: Rect, confirm: &Confirm) -> (Rect, Rect) {
    let empty = Rect::default();
    if let Some(cc) = &confirm.conn {
        return render_conn_confirm(f, area, cc);
    }
    let w = overlay_width(area.width, 72, 30);
    let inner_w = w.saturating_sub(4) as usize;
    // Keep the statement's own line structure so a multi-line UPDATE / DELETE /
    // BEGIN … COMMIT stays readable; nothing is run from a summary alone.
    let sql_lines = wrap_sql_lines(&confirm.sql, inner_w);
    // Impact estimate: the WHERE predicate of every UPDATE / DELETE in the
    // statement(s), echoed back verbatim (truncated) with no extra query.
    let impact = where_predicates(&confirm.sql);
    let max_h = area.height.saturating_sub(2) as usize;
    // reasons + impact + blank + SQL + blank + hint, plus the two border rows
    let needed = confirm.reasons.len() + impact.len() + sql_lines.len() + 5;
    let h = needed.min(max_h).max(3) as u16;
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);

    let mut lines: Vec<Line> = Vec::new();
    for r in &confirm.reasons {
        lines.push(Line::from(Span::styled(
            format!("⚠ {r}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    for p in &impact {
        lines.push(Line::from(Span::styled(
            tf("WHERE 谓词：{}", &[&p]),
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    let sql_room =
        (box_area.height as usize).saturating_sub(confirm.reasons.len() + impact.len() + 5);
    let truncated = sql_lines.len() > sql_room;
    let shown_sql = if truncated {
        sql_room.saturating_sub(1)
    } else {
        sql_room
    };
    for l in sql_lines.iter().take(shown_sql) {
        lines.push(Line::from(Span::styled(
            l.clone(),
            Style::default().fg(Color::White),
        )));
    }
    if truncated {
        lines.push(Line::from(Span::styled(
            t("…（语句过长，已截断显示）"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    let inner = Rect {
        x: box_area.x + 1,
        y: box_area.y + 1,
        width: box_area.width.saturating_sub(2),
        height: box_area.height.saturating_sub(2),
    };
    let hint_y = inner.y + lines.len() as u16;
    let (buttons, ok, cancel) = confirm_buttons(inner, hint_y, t("Enter/y 执行"), t("Esc/n 取消"));
    lines.push(buttons);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            t(" ⚠ 危险操作确认 "),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(Color::Red));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
    if ok.width == 0 && cancel.width == 0 {
        return (empty, empty);
    }
    (ok, cancel)
}

/// The blocking SSH prompt dialog (host-key TOFU / keyboard-interactive).
pub(crate) fn render_ssh_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(state) = app.ssh_prompt.as_ref() else {
        return;
    };
    let w = overlay_width(area.width, 76, 30);
    let req = &state.request;
    let mut lines: Vec<Line> = Vec::new();
    let (title, color) = match req.kind {
        SshPromptKind::HostKeyVerify => (t(" SSH 主机密钥确认 "), Color::Cyan),
        SshPromptKind::HostKeyChanged => (t(" ⚠ SSH 主机密钥已变化 "), Color::Red),
        SshPromptKind::SecretInput => (t(" SSH 需要验证 "), Color::Cyan),
        SshPromptKind::WorkerUploadConsent => (t(" SSH 请求确认 "), Color::Cyan),
        SshPromptKind::UserInput => (t(" 需要输入 "), Color::Cyan),
    };
    lines.push(Line::from(Span::styled(
        tf("主机 {}:{}", &[&(req.host), &(req.port)]),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    match req.kind {
        SshPromptKind::HostKeyVerify | SshPromptKind::HostKeyChanged => {
            if let Some(kt) = req.key_type.as_deref() {
                lines.push(Line::from(tf("密钥类型 {}", &[&(kt)])));
            }
            if let Some(fp) = req.fingerprint.as_deref() {
                lines.push(Line::from(Span::styled(
                    tf("指纹 {}", &[&(fp)]),
                    Style::default().fg(Color::Yellow),
                )));
            }
            if let Some(prev) = req.previous_fingerprint.as_deref() {
                lines.push(Line::from(Span::styled(
                    tf("原指纹 {}", &[&(prev)]),
                    Style::default().fg(Color::Red),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t("核对该指纹后再继续；仅在你确认这是目标主机时才接受"),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                t("y/Enter 接受并记住 · s 仅本次会话 · n/Esc 拒绝"),
                Style::default().fg(Color::Yellow),
            )));
        }
        SshPromptKind::SecretInput => {
            if let Some(prompt) = req.prompt.as_deref() {
                for l in prompt.lines() {
                    lines.push(Line::from(l.to_string()));
                }
            }
            let shown = if req.echo {
                state.input.clone()
            } else {
                "*".repeat(state.input.chars().count())
            };
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("> {shown}▏"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                t("Enter 提交 · Esc 取消"),
                Style::default().fg(Color::Yellow),
            )));
        }
        SshPromptKind::WorkerUploadConsent => {
            lines.push(Line::from(req.prompt.clone().unwrap_or_default()));
            lines.push(Line::from(Span::styled(
                t("Enter/y 允许 · Esc/n 取消"),
                Style::default().fg(Color::Yellow),
            )));
        }
        SshPromptKind::UserInput => {
            if let Some(source) = req.source.as_deref() {
                lines.push(Line::from(Span::styled(
                    tf("来自 {}", &[&(source)]),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if let Some(title) = req.title.as_deref() {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            if let Some(prompt) = req.prompt.as_deref() {
                for l in prompt.lines() {
                    lines.push(Line::from(l.to_string()));
                }
            }
            for (i, option) in req.options.iter().enumerate() {
                lines.push(Line::from(format!("{}. {}", i + 1, option.label)));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("> {}▏", state.input),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                t("Enter 提交 · Esc 取消"),
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    let h = (lines.len() as u16 + 2)
        .min(area.height)
        .max(3.min(area.height));
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .border_set(border::THICK)
        .border_style(Style::default().fg(color));
    f.render_widget(Paragraph::new(lines).block(block), box_area);
}

// ── CSV import overlays ──────────────────────────────────────────────────────

pub(crate) fn render_import_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 78, 24);
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" 导入 CSV · 输入文件路径 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
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
    let error = app.import_prompt.as_ref().and_then(|p| p.error.clone());
    let target = app
        .import_prompt
        .as_ref()
        .map(|p| format!("{}.{}", p.db, qualified_display(&p.schema, &p.table)))
        .unwrap_or_default();
    if let Some(p) = app.import_prompt.as_mut() {
        p.input.set_block(Block::default());
        f.render_widget(&p.input, ta_area);
    }
    if hint_h > 0 {
        let first = match error {
            Some(e) => Line::from(Span::styled(
                format!("✗ {e}"),
                Style::default().fg(Color::Red),
            )),
            None => Line::from(Span::styled(
                t("~ 展开为 $HOME · UTF-8/GBK 自动探测 · 首行视为表头"),
                Style::default().fg(Color::DarkGray),
            )),
        };
        let second = Line::from(Span::styled(
            tf("目标表: {}", &[&target]),
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(Paragraph::new(vec![first, second]), hint_area);
    }
}

/// Build the preview lines for an import plan.
pub(crate) fn import_plan_lines(plan: &ImportPlan) -> Vec<PopupLine> {
    let plain = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Yellow);
    let bad = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let mut lines: Vec<PopupLine> = Vec::new();
    lines.push(PopupLine {
        text: tf(
            "目标表: {}.{}",
            &[&plan.db, &qualified_display(&plan.schema, &plan.table)],
        ),
        style: head,
    });
    lines.push(PopupLine {
        text: tf(
            "文件: {} ({})",
            &[&plan.path.display(), &human_size(plan.file_size)],
        ),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "编码 {} · 分隔符 {} · 数据行 {}",
            &[
                &plan.encoding,
                &delim_label(plan.delimiter),
                &plan.rows.len(),
            ],
        ),
        style: plain,
    });
    let mode_label = if plan.mode == ImportMode::Overwrite {
        t("覆盖（先清空表）")
    } else {
        t("追加")
    };
    let err_label = if plan.on_error == ImportOnError::Skip {
        t("跳过继续")
    } else {
        t("遇错停止")
    };
    lines.push(PopupLine {
        text: tf(
            "模式 {}（m 切换） · 错误行 {}（s 切换）",
            &[&mode_label, &err_label],
        ),
        style: if plan.mode == ImportMode::Overwrite {
            bad
        } else {
            warn
        },
    });
    if let Some(err) = &plan.error {
        lines.push(PopupLine {
            text: format!("✗ {err}"),
            style: bad,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "列映射（{} 列 · {} 缺失）",
            &[&plan.columns.len(), &plan.missing.len()],
        ),
        style: head,
    });
    for c in &plan.columns {
        let src = match c.src {
            Some(i) => plan.headers.get(i).cloned().unwrap_or_default(),
            None => "—".to_string(),
        };
        let suffix = if c.src.is_none() {
            t("  ⚠ 缺失→默认")
        } else {
            ""
        };
        lines.push(PopupLine {
            text: format!("  {} ← {} · {}{}", c.name, src, c.ty.label(), suffix),
            style: if c.src.is_none() { warn } else { plain },
        });
    }
    if !plan.extra.is_empty() {
        lines.push(PopupLine {
            text: tf("  ⚠ 多余列: {}", &[&plan.extra.join(", ")]),
            style: bad,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "预览（前 {} 行）",
            &[&plan.rows.len().min(IMPORT_SAMPLE_ROWS)],
        ),
        style: head,
    });
    for row in plan.rows.iter().take(IMPORT_SAMPLE_ROWS) {
        let cells: Vec<String> = plan
            .columns
            .iter()
            .map(|c| match c.src {
                Some(i) => row.get(i).cloned().unwrap_or_else(|| "NULL".into()),
                None => "NULL".into(),
            })
            .collect();
        lines.push(PopupLine {
            text: format!("  {}", cells.join(" | ")),
            style: dim,
        });
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: if plan.error.is_some() {
            t("Esc 取消（存在错误，无法导入）").to_string()
        } else {
            t("Enter 开始导入 · m 追加/覆盖 · s 遇错停止/跳过 · ↑↓ 滚动 · Esc 取消").to_string()
        },
        style: warn,
    });
    lines
}

pub(crate) fn render_import_plan(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(plan) = app.import_plan.clone() else {
        return;
    };
    let lines = import_plan_lines(&plan);
    let w = {
        let avail = area.width.saturating_sub(2);
        if avail < 24 {
            area.width
        } else {
            avail.min(96)
        }
    };
    let inner_w = w.saturating_sub(4).max(1) as usize;
    let body: Vec<Line> = lines
        .iter()
        .flat_map(|pl| {
            let style = pl.style;
            wrap_text(&pl.text, inner_w)
                .into_iter()
                .map(move |t| Line::from(Span::styled(t, style)))
        })
        .collect();
    let total = body.len();
    let max_h = area.height.saturating_sub(2).max(3);
    let h = ((total as u16) + 2).min(max_h);
    let box_area = centered_overlay(area, w, h);
    let inner_h = box_area.height.saturating_sub(2) as usize;
    f.render_widget(Clear, box_area);
    let max_scroll = total.saturating_sub(inner_h).min(u16::MAX as usize) as u16;
    let scroll = app.import_scroll.min(max_scroll);
    app.import_scroll = scroll;
    let (style, title) = if plan.error.is_some() {
        (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            t(" ⚠ CSV 导入 · 无法导入 "),
        )
    } else if plan.mode == ImportMode::Overwrite {
        (
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            t(" ⚠ CSV 导入 · 覆盖确认 "),
        )
    } else {
        (Style::default().fg(Color::Cyan), t(" CSV 导入预览 "))
    };
    f.render_widget(
        Paragraph::new(body).scroll((scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(style),
        ),
        box_area,
    );
}

pub(crate) fn render_import_report(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(rep) = app.import_report.clone() else {
        return;
    };
    let plain = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let head = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Yellow);
    let bad = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let mut lines: Vec<PopupLine> = Vec::new();
    let mode_label = if rep.mode == ImportMode::Overwrite {
        t("覆盖")
    } else {
        t("追加")
    };
    lines.push(PopupLine {
        text: tf("目标表: {}", &[&qualified_display(&rep.schema, &rep.table)]),
        style: head,
    });
    lines.push(PopupLine {
        text: tf("模式: {}", &[&mode_label]),
        style: plain,
    });
    lines.push(PopupLine {
        text: tf(
            "总行数 {} · 成功 {} · 跳过 {} · 耗时 {}ms",
            &[
                &rep.total,
                &rep.inserted,
                &rep.skipped.len(),
                &rep.elapsed_ms,
            ],
        ),
        style: if rep.ok() { plain } else { warn },
    });
    if let Some((row, err)) = &rep.aborted {
        lines.push(PopupLine {
            text: tf("✗ 中止于第 {} 行: {}", &[&row, &err]),
            style: bad,
        });
    }
    if !rep.skipped.is_empty() {
        lines.push(PopupLine {
            text: String::new(),
            style: plain,
        });
        lines.push(PopupLine {
            text: tf("跳过的行（{}）", &[&rep.skipped.len()]),
            style: head,
        });
        for (row, err) in rep.skipped.iter().take(50) {
            lines.push(PopupLine {
                text: tf("  第 {} 行: {}", &[&row, &err]),
                style: dim,
            });
        }
        if rep.skipped.len() > 50 {
            lines.push(PopupLine {
                text: tf("  … 其余 {} 行", &[&(rep.skipped.len() - 50)]),
                style: dim,
            });
        }
    }
    lines.push(PopupLine {
        text: String::new(),
        style: plain,
    });
    lines.push(PopupLine {
        text: t("Enter/Esc 关闭").to_string(),
        style: warn,
    });

    let w = overlay_width(area.width, 84, 24);
    let inner_w = w.saturating_sub(4).max(1) as usize;
    let body: Vec<Line> = lines
        .iter()
        .flat_map(|pl| {
            let style = pl.style;
            wrap_text(&pl.text, inner_w)
                .into_iter()
                .map(move |t| Line::from(Span::styled(t, style)))
        })
        .collect();
    let total = body.len();
    let max_h = area.height.saturating_sub(4).max(3);
    let h = ((total as u16) + 2).min(max_h);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let style = if rep.ok() {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };
    let title = if rep.ok() {
        t(" CSV 导入完成 ")
    } else {
        t(" ⚠ CSV 导入未完成 ")
    };
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_set(border::THICK)
                .border_style(style),
        ),
        box_area,
    );
}

// ── export overlays ──────────────────────────────────────────────────────────

pub(crate) fn render_export(f: &mut Frame, area: Rect, app: &mut App) {
    let w = overlay_width(area.width, 76, 30);
    let h = (EXPORT_FORMATS.len() as u16 + 3).min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let rows = active_grid(app).map(|g| g.rows.len()).unwrap_or(0);
    let title = if rows > EXPORT_SLOW_ROWS {
        tf(" 导出结果 · {} 行（生成可能耗时）· Esc 取消 ", &[&rows])
    } else {
        tf(" 导出结果 · {} 行 · ↑↓ Enter · Esc ", &[&rows])
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let items: Vec<ListItem> = EXPORT_FORMATS
        .iter()
        .enumerate()
        .map(|(i, fmt)| {
            let text = format!("{}. {} — {}", i + 1, fmt.label(), fmt.description());
            ListItem::new(Line::from(Span::styled(text, Style::default())))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut st = app.export_list.clone();
    f.render_stateful_widget(list, inner, &mut st);
    app.export_list = st;
}

pub(crate) fn render_export_path(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(pending) = app.export_pending.as_ref() else {
        return;
    };
    let fmt = pending.format;
    let w = overlay_width(area.width, 78, 24);
    let h = 7.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(tf(" 导出 {} · 目标 ", &[&fmt.label()]))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
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
    if let Some(ta) = app.export_path.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, ta_area);
    }
    if hint_h > 0 {
        let hints = vec![
            Line::from(Span::styled(
                t("留空 = 复制到剪贴板（OSC52）· 输入路径 = 写入文件（支持 ~）"),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                t("Enter 确认 · Esc 取消"),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hints), hint_area);
    }
}

/// One preview row: `[x] name  type  host:port  SSH  colour  needs-password  dup`.
pub(crate) fn conn_import_row_text(row: &ConnImportRow) -> String {
    let mut s = String::new();
    s.push_str(if row.selected { "[x] " } else { "[ ] " });
    if row.conn.name.trim().is_empty() {
        s.push_str(&row.conn.host);
    } else {
        s.push_str(&row.conn.name);
    }
    s.push_str("  ");
    match &row.conn.db_type {
        Some(dt) => s.push_str(dt),
        None => s.push_str(&row.conn.driver),
    }
    if !row.conn.host.is_empty() {
        match row.conn.port {
            Some(p) => s.push_str(&format!("  {}:{}", row.conn.host, p)),
            None => s.push_str(&format!("  {}", row.conn.host)),
        }
    }
    if row.conn.ssh.is_some() {
        s.push_str("  SSH");
    }
    if let Some(c) = &row.conn.color {
        s.push_str(&format!("  {c}"));
    }
    if row.conn.needs_password {
        s.push_str("  ");
        s.push_str(t("需补密码"));
    }
    if row.dup {
        s.push_str("  [");
        s.push_str(row.policy.marker());
        s.push(']');
    }
    s
}

pub(crate) fn render_conn_export(f: &mut Frame, area: Rect, app: &mut App) {
    let count = app.connections.len();
    let Some(ex) = app.conn_export.as_mut() else {
        return;
    };
    if area.width < 12 || area.height < 4 {
        return;
    }
    let w = overlay_width(area.width, 82, 30);
    let h = 11.min(area.height).max(4);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let border = if ex.confirm_pw {
        Color::Red
    } else {
        Color::Cyan
    };
    let title = if ex.confirm_pw {
        t(" ⚠ 含密码导出确认 · Enter 确认 · Esc 取消 ")
    } else {
        // The translated title is templated, so build it outside the widget chain.
        ""
    };
    let block = if title.is_empty() {
        Block::default()
            .borders(Borders::ALL)
            .title(tf(
                " 导出连接 · {} 条 · Enter 导出 · y 复制 · p 密码 · Esc ",
                &[&count],
            ))
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(border))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(border))
    };
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if ex.confirm_pw {
        let lines = vec![
            Line::from(Span::styled(
                t("⚠ 开启后密码将以明文写入 JSON 文件。"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                t("请勿提交到版本库，导出后及时删除该文件。"),
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(Span::styled(
                t("Enter / y 确认开启 · Esc / n 取消"),
                Style::default().fg(Color::Yellow),
            )),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let hl = |i: usize| {
        if ex.field == i {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        }
    };
    let marker = |i: usize| if ex.field == i { "▶ " } else { "  " };

    // Path row: a fixed label plus either the live textarea or its value.
    let label = t("文件 ");
    let label_w = (disp_width(label) as u16).min(inner.width);
    let label_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: label_w,
        height: 1,
    };
    let value_rect = Rect {
        x: inner.x + label_w,
        y: inner.y,
        width: inner.width.saturating_sub(label_w),
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(label, dim))),
        label_rect,
    );
    if ex.editing {
        ex.path.set_block(Block::default());
        f.render_widget(&ex.path, value_rect);
    } else {
        let value = ex.path.lines().join("");
        let value = if value.trim().is_empty() {
            CONN_EXPORT_DEFAULT_PATH.to_string()
        } else {
            value
        };
        let shown = truncate_disp(&value, value_rect.width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(shown, hl(0)))),
            value_rect,
        );
    }

    if inner.height >= 2 {
        let rest = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height - 1,
        };
        let pw = if ex.include_passwords {
            t("是")
        } else {
            t("否")
        };
        let mut lines = vec![
            Line::from(Span::styled(
                format!("{}{} {}", marker(1), t("含密码"), pw),
                hl(1),
            )),
            Line::from(Span::styled(
                format!("{}{}", marker(2), t("Enter 导出到文件")),
                hl(2),
            )),
            Line::from(Span::styled(
                t("y 复制 JSON 到剪贴板 · p 切换密码 · i 导入连接 · Esc 取消"),
                dim,
            )),
        ];
        lines.truncate(rest.height as usize);
        f.render_widget(Paragraph::new(lines), rest);
    }
}

pub(crate) fn render_conn_import_prompt(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width < 16 || area.height < 3 {
        return;
    }
    let w = overlay_width(area.width, 86, 30);
    let h = 4.min(area.height);
    let box_area = centered_overlay(area, w, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(t(" 导入连接 · 输入文件路径 · Enter 预览 · Esc 取消 "))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if let Some(ta) = app.conn_import_path.as_mut() {
        ta.set_block(Block::default());
        f.render_widget(&*ta, inner);
    }
}

pub(crate) fn render_conn_import_plan(f: &mut Frame, area: Rect, app: &mut App) {
    let cursor_hint;
    let confirm;
    let source;
    let origin;
    let rows_n;
    let skipped_n;
    {
        let Some(plan) = app.conn_import_plan.as_ref() else {
            return;
        };
        confirm = plan.confirm;
        source = plan.source.label();
        origin = truncate_disp(&plan.origin, 40);
        rows_n = plan.rows.len();
        skipped_n = plan.skipped.len();
        cursor_hint = plan.cursor;
    }
    if area.width < 16 || area.height < 5 {
        return;
    }
    let w = overlay_width(area.width, 96, 40);
    let body_h = (rows_n as u16 + 5)
        .min(area.height.saturating_sub(2))
        .max(3);
    let box_area = centered_overlay(area, w, body_h);
    f.render_widget(Clear, box_area);
    let border = if confirm.is_some() {
        Color::Red
    } else {
        Color::Cyan
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(tf(
            " 导入连接 · {} · {} · {} 条 ",
            &[&source, &origin, &rows_n],
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(border));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if let Some(scope) = confirm {
        let who = match scope {
            ConnOverwriteScope::All => t("全部同名连接").to_string(),
            ConnOverwriteScope::Row(_) => t("该同名连接").to_string(),
        };
        let lines = vec![
            Line::from(Span::styled(
                tf("⚠ 覆盖 {}：将先删除原有配置再写入。", &[&who]),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                t("数据库数据不受影响，仅替换保存的连接配置。"),
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(Span::styled(
                t("Enter / y 确认覆盖 · Esc / n 取消"),
                Style::default().fg(Color::Yellow),
            )),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    // Reserve the last line for the footer hint; one more when drivers were skipped.
    let hint_h = 1u16;
    let skip_h = if skipped_n > 0 { 1u16 } else { 0 };
    let list_h = inner.height.saturating_sub(hint_h + skip_h);
    let list_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: list_h.max(1),
    };
    let items: Vec<ListItem> = app
        .conn_import_plan
        .as_ref()
        .map(|plan| {
            plan.rows
                .iter()
                .map(|row| {
                    let label = truncate_disp(&conn_import_row_text(row), list_area.width as usize);
                    let style = if !row.selected {
                        Style::default().fg(Color::DarkGray)
                    } else if row.dup && row.policy == DupPolicy::Overwrite {
                        Style::default().fg(Color::Red)
                    } else if row.dup {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(Span::styled(label, style)))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut st = ListState::default();
    if rows_n > 0 {
        st.select(Some(cursor_hint.min(rows_n - 1)));
    }
    f.render_stateful_widget(list, list_area, &mut st);

    let mut foot_y = inner.y + list_h.max(1);
    if skipped_n > 0 && foot_y < inner.y + inner.height {
        let skipped: Vec<String> = app
            .conn_import_plan
            .as_ref()
            .map(|p| p.skipped.iter().take(6).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut joined = skipped.join(", ");
        if skipped_n > 6 {
            joined.push_str(&format!(" +{}", skipped_n - 6));
        }
        let rect = Rect {
            x: inner.x,
            y: foot_y,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_disp(&tf("跳过未知驱动: {}", &[&joined]), inner.width as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            rect,
        );
        foot_y += 1;
    }
    if foot_y < inner.y + inner.height {
        let rect = Rect {
            x: inner.x,
            y: foot_y,
            width: inner.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_disp(
                    t("↑↓ 选择 · Space 勾选 · s/r/b 跳过/覆盖/都存 · d 逐条 · Enter 导入 · Esc 取消"),
                    inner.width as usize,
                ),
                Style::default().fg(Color::Yellow),
            ))),
            rect,
        );
    }
}
