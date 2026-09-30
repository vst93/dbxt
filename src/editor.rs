use crate::prelude::*;
use crate::*;

/// R68: apply a confirmed read-only toggle. The in-memory config (and the
/// active `selected` copy the write guards read) is updated first so the 🔒 and
/// the interception are immediate; the store write confirms it, and a failure
/// surfaces as a red status rather than a silent no-op.
pub(crate) fn apply_conn_readonly(app: &mut App, tx: &Tx, cc: &ConnConfirm, read_only: bool) {
    let Some(mut cfg) = app.connections.iter().find(|c| c.id == cc.id).cloned() else {
        app.status = t("✗ 连接已不存在").into();
        return;
    };
    cfg.read_only = read_only;
    if let Some(slot) = app.connections.iter_mut().find(|c| c.id == cfg.id) {
        *slot = cfg.clone();
    }
    if app.selected.as_ref().is_some_and(|c| c.id == cfg.id) {
        app.selected = Some(cfg.clone());
    }
    rebuild_side_rows(app);
    app.status = if read_only {
        tf("保存连接 {} 只读设置…", &[&cfg.name])
    } else {
        tf("保存连接 {} 可写设置…", &[&cfg.name])
    };
    app.spawn(tx, Op::SetConnReadOnly(Box::new(cfg)));
}

/// `s` on a database row: lazily fetch that database's aggregate size and
/// per-table row estimates, cached for the session (R45). This is the *only*
/// trigger — nothing scans sizes on startup or automatically.
pub(crate) fn request_db_size(app: &mut App, tx: &Tx) {
    let Some(SideRow::Db { idx, db, .. }) = app.side_rows.get(app.side_sel).cloned() else {
        app.status = t("把光标移到库行上再按 s 查尺寸").into();
        return;
    };
    if !side_is_active(app, idx) {
        app.status = t("先切换到该连接再查尺寸").into();
        return;
    }
    if matches!(app.db_size_state.get(&db), Some(TreeDbState::Loading)) {
        app.status = tf("正在查询 {} 尺寸…", &[&db]);
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let gen = {
        let g = app.db_size_gen.entry(db.clone()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    };
    app.db_size_state.insert(db.clone(), TreeDbState::Loading);
    rebuild_side_rows(app);
    app.status = tf("查询 {} 尺寸…（只读元数据，不扫表）", &[&db]);
    app.spawn(
        tx,
        Op::DbSize {
            cfg: Box::new(cfg),
            db,
            schema: app.schema.clone(),
            gen,
        },
    );
}

/// Switch to the connection at `idx` and land on `db` once its database list
/// arrives (used when drilling into a non-active connection's database).
pub(crate) fn switch_to_db(app: &mut App, tx: &Tx, idx: usize, db: &str) {
    let target_id = side_root_cfg(app, idx).map(|c| c.id.clone());
    if side_is_active(app, idx) {
        // Already active: just move to the database.
        if let Some(pos) = app.databases.iter().position(|d| d == db) {
            app.db_index = pos;
            reload_tables(app, tx);
        }
        return;
    }
    switch_connection(app, tx, idx);
    if app.selected.as_ref().map(|c| c.id.clone()) == target_id {
        match app.pending_restore.as_mut() {
            Some(p) => p.db = db.to_string(),
            None => {
                app.pending_restore = Some(ConnPointer {
                    db: db.to_string(),
                    ..Default::default()
                })
            }
        }
    }
}

/// `Enter` / `Space` on the tree: activate the row under the cursor.
pub(crate) fn side_activate(app: &mut App, tx: &Tx) {
    let Some(row) = app.side_rows.get(app.side_sel).cloned() else {
        return;
    };
    match row {
        SideRow::Group { id, .. } => {
            if app.group_closed.contains(&id) {
                app.group_closed.remove(&id);
            } else {
                app.group_closed.insert(id);
            }
            rebuild_side_rows(app);
        }
        SideRow::Conn { idx, .. } => {
            let id = side_root_cfg(app, idx)
                .map(|c| c.id.clone())
                .unwrap_or_default();
            if side_is_active(app, idx) {
                if side_conn_open(app, idx) {
                    app.tree_conn_closed.insert(id);
                } else {
                    app.tree_conn_closed.remove(&id);
                }
                rebuild_side_rows(app);
            } else {
                if idx < app.connections.len() {
                    switch_connection(app, tx, idx);
                }
                app.tree_conn_open.insert(id);
            }
        }
        SideRow::Db { idx, db, .. } => {
            if side_is_active(app, idx) {
                let id = side_root_cfg(app, idx)
                    .map(|c| c.id.clone())
                    .unwrap_or_default();
                app.tree_db_closed.remove(&db_node_key(&id, &db));
                if let Some(pos) = app.databases.iter().position(|d| d == &db) {
                    if app.db_index != pos {
                        app.db_index = pos;
                        reload_tables(app, tx);
                    } else {
                        rebuild_side_rows(app);
                    }
                }
            } else {
                switch_to_db(app, tx, idx, &db);
            }
        }
        SideRow::Table { .. } => open_table_data(app, tx),
        SideRow::ConnError { idx, .. } => {
            // Retry the lazy fetch.
            if let Some(id) = side_root_cfg(app, idx).map(|c| c.id.clone()) {
                app.tree_db_state.remove(&id);
            }
            expand_conn(app, tx, idx);
        }
        SideRow::ConnLoading { .. } => {}
    }
}

// ── editor / cmd input / preview ──

/// `Alt-F`: format the editor's SQL, or compress an already-formatted statement
/// back to one line. The previous text is snapshotted so a single `Ctrl-U` can
/// undo the whole reformat.
pub(crate) fn toggle_format_editor(app: &mut App) {
    let text = app.editor_sql();
    if text.trim().is_empty() {
        app.status = t("编辑器为空，无需格式化").into();
        return;
    }
    let next = if is_sql_formatted(&text) {
        compress_sql(&text)
    } else {
        format_sql(&text)
    };
    if next == text {
        app.status = t("已是最简形式").into();
        return;
    }
    let formatted = is_sql_formatted(&next);
    let lines = next.lines().count();
    app.editor_undo = Some(text);
    // Replace the whole buffer in one gesture; tui-textarea records it on its
    // own undo stack as well.
    app.editor.select_all();
    app.editor.insert_str(&next);
    app.status = if formatted {
        tf("已格式化 · {} 行 · Ctrl-U 撤销", &[&lines])
    } else {
        t("已压缩为单行 · Ctrl-U 撤销").into()
    };
}

/// True for the bracket characters `%` can jump between.
pub(crate) fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// `(row, col)` (char index in the line) → offset in the flattened text. Returns
/// `None` when the position does not exist (a stale cursor after an edit).
pub(crate) fn text_offset(text: &str, row: usize, col: usize) -> Option<usize> {
    let mut off = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i == row {
            return (col <= line.chars().count()).then_some(off + col);
        }
        off += line.chars().count() + 1;
    }
    None
}

/// Offset in the flattened text → `(row, col)`.
pub(crate) fn offset_to_cursor(text: &str, off: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    for c in text.chars().take(off) {
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
}

/// Classify every character as *code* (`true`) or as part of a string literal,
/// quoted identifier (`"…"`, `` `…` ``) or comment (`false`). The lexer is
/// deliberately small but honours SQL's escaping rules (doubled quotes,
/// backslash escapes) so a `;` or bracket inside a literal is never mistaken for
/// code. Shared by the `%` bracket matcher and the R41 statement splitter.
pub(crate) fn code_mask(chars: &[char]) -> Vec<bool> {
    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Normal,
        Sq,
        Dq,
        Bt,
        Line,
        Block,
    }
    let mut mask = vec![false; chars.len()];
    let mut state = St::Normal;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        mask[i] = state == St::Normal;
        match state {
            St::Normal => {
                if c == '\'' {
                    state = St::Sq;
                } else if c == '"' {
                    state = St::Dq;
                } else if c == '`' {
                    state = St::Bt;
                } else if c == '-' && chars.get(i + 1) == Some(&'-') {
                    state = St::Line;
                    i += 1;
                } else if c == '/' && chars.get(i + 1) == Some(&'*') {
                    state = St::Block;
                    i += 1;
                }
            }
            St::Sq => {
                if c == '\\' {
                    i += 1;
                } else if c == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 1;
                    } else {
                        state = St::Normal;
                    }
                }
            }
            St::Dq => {
                if c == '\\' {
                    i += 1;
                } else if c == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        i += 1;
                    } else {
                        state = St::Normal;
                    }
                }
            }
            St::Bt => {
                if c == '`' {
                    if chars.get(i + 1) == Some(&'`') {
                        i += 1;
                    } else {
                        state = St::Normal;
                    }
                }
            }
            St::Line => {
                if c == '\n' {
                    state = St::Normal;
                }
            }
            St::Block => {
                if c == '*' && chars.get(i + 1) == Some(&'/') {
                    state = St::Normal;
                    i += 1;
                }
            }
        }
        i += 1;
    }
    mask
}

/// Every bracket that sits in *code* — outside string literals, quoted
/// identifiers (`"…"`, `` `…` ``) and comments — paired with its char offset.
pub(crate) fn code_brackets(text: &str) -> Vec<(usize, char)> {
    let chars: Vec<char> = text.chars().collect();
    let mask = code_mask(&chars);
    chars
        .iter()
        .enumerate()
        .filter(|(i, c)| mask[*i] && is_bracket(**c))
        .map(|(i, c)| (i, *c))
        .collect()
}

/// Split `text` into statement spans (char-offset ranges, whitespace-trimmed)
/// at every semicolon that sits in code. A `;` inside a string literal or
/// comment never splits, so `SELECT ';'` stays one statement.
pub(crate) fn statement_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mask = code_mask(&chars);
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i <= chars.len() {
        let split = i == chars.len() || (mask[i] && chars[i] == ';');
        if split {
            if let Some(r) = trim_char_range(&chars, start, i) {
                ranges.push(r);
            }
            start = i + 1;
        }
        i += 1;
    }
    ranges
}

/// Trim ASCII/Unicode whitespace off both ends of a char range, dropping it when
/// nothing but whitespace remains.
pub(crate) fn trim_char_range(chars: &[char], start: usize, end: usize) -> Option<(usize, usize)> {
    let mut a = start;
    let mut b = end.min(chars.len());
    while a < b && chars[a].is_whitespace() {
        a += 1;
    }
    while b > a && chars[b - 1].is_whitespace() {
        b -= 1;
    }
    (a < b).then_some((a, b))
}

/// The statement span containing `cursor` (a char offset). A cursor parked on a
/// separator or the whitespace between statements resolves to the following
/// statement, or the last one when the cursor trails the whole buffer.
pub(crate) fn statement_range_at(
    ranges: &[(usize, usize)],
    cursor: usize,
) -> Option<(usize, usize)> {
    if ranges.is_empty() {
        return None;
    }
    if let Some(&r) = ranges.iter().find(|&&(s, e)| cursor >= s && cursor < e) {
        return Some(r);
    }
    ranges
        .iter()
        .copied()
        .find(|&(s, _)| cursor < s)
        .or_else(|| ranges.last().copied())
}

/// Index of the statement span `cursor` sits in (or, in the gap before the
/// first / after the last, the nearest span), so a jump can step relative to it.
/// `None` only when there is no statement at all.
pub(crate) fn statement_index_at(ranges: &[(usize, usize)], cursor: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    Some(ranges.iter().rposition(|&(s, _)| s <= cursor).unwrap_or(0))
}

/// R56: the inclusive `(first_row, last_row)` of the statement the caret at
/// `(row, col)` sits in, for dimming every other statement in the editor.
///
/// `None` when there is nothing to dim — no text, a single-statement buffer, or
/// a caret outside the lines. The `;` split reuses the R52 statement splitter
/// (literals / comments never split a statement). Under
/// [`STMT_DIM_MAX_BYTES`] the whole buffer is split; above it only
/// ±[`STMT_DIM_SCAN_LINES`] lines around the caret are read, so a huge SQL file
/// costs the same per frame as a small one.
pub(crate) fn active_statement_rows(
    lines: &[String],
    row: usize,
    col: usize,
) -> Option<(usize, usize)> {
    if lines.is_empty() || row >= lines.len() {
        return None;
    }
    // A single-statement buffer (the common case) has no `;` at all: skip the
    // lexer entirely.
    if !lines.iter().any(|l| l.contains(';')) {
        return None;
    }
    let bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
    let (lo, hi) = if bytes <= STMT_DIM_MAX_BYTES {
        (0, lines.len())
    } else {
        let lo = row.saturating_sub(STMT_DIM_SCAN_LINES);
        let hi = (row + STMT_DIM_SCAN_LINES + 1).min(lines.len());
        (lo, hi)
    };
    if hi <= lo {
        return None;
    }
    // Caret char offset inside the window text (one `\n` between joined lines).
    let mut off = 0usize;
    for l in &lines[lo..row] {
        off += l.chars().count() + 1;
    }
    off += col.min(lines[row].chars().count());

    let win = lines[lo..hi].join("\n");
    let ranges = statement_ranges(&win);
    if ranges.len() < 2 {
        return None;
    }
    // Resolve the caret's statement the same way `Alt-↓`/`Alt-↑` does (the last
    // statement starting at or before the caret), so a caret parked just after a
    // `;` still belongs to the statement it just ended rather than jumping to the
    // next one.
    let idx = statement_index_at(&ranges, off)?;
    let (start, end) = ranges[idx];
    let (sr, _) = offset_to_cursor(&win, start);
    let (er, _) = offset_to_cursor(&win, end.saturating_sub(1).max(start));
    Some((lo + sr, lo + er))
}

/// First char offset in `start..end` that is real code (not whitespace and not
/// inside a leading comment), so a statement jump lands on the statement's first
/// token rather than on the comment block above it. Falls back to `start` when
/// the span is comment-only.
pub(crate) fn statement_code_start(
    chars: &[char],
    mask: &[bool],
    start: usize,
    end: usize,
) -> usize {
    let end = end.min(chars.len());
    let mut i = start;
    while i < end {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // The lexer marks the first char of a comment opener as code (it only
        // flips state *after* recording the mask), so skip a leading comment
        // block explicitly here: a statement should reveal its first real token,
        // not the `--` / `/*` above it.
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < end && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < end && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(end);
            continue;
        }
        if mask[i] {
            return i;
        }
        i += 1;
    }
    start
}

/// Offset of the bracket matching the bracket at `pos`, or `None` when it is
/// unbalanced. Brackets inside strings / comments are ignored, so a `)` in a
/// literal never pairs with a `(` in code.
pub(crate) fn matching_bracket(text: &str, pos: usize) -> Option<usize> {
    let brackets = code_brackets(text);
    let idx = brackets.iter().position(|(p, _)| *p == pos)?;
    let c = brackets[idx].1;
    let (open, close) = match c {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        _ => return None,
    };
    if matches!(c, '(' | '[' | '{') {
        let mut depth = 0i32;
        for &(p, cc) in &brackets[idx..] {
            if cc == open {
                depth += 1;
            } else if cc == close {
                depth -= 1;
                if depth == 0 {
                    return Some(p);
                }
            }
        }
    } else {
        let mut depth = 0i32;
        for &(p, cc) in brackets[..=idx].iter().rev() {
            if cc == close {
                depth += 1;
            } else if cc == open {
                depth -= 1;
                if depth == 0 {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// `%` in the editor: when the cursor is on, or immediately after, a bracket,
/// move it to the matching bracket and report `true`. Reports `false` when there
/// is no bracket there, so the caller inserts a literal `%` instead.
pub(crate) fn jump_matching_bracket(app: &mut App) -> bool {
    let text = app.editor_sql();
    let (row, col) = app.editor.cursor();
    let Some(off) = text_offset(&text, row, col) else {
        return false;
    };
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let pos = if at(off).is_some_and(is_bracket) {
        off
    } else if off > 0 && at(off - 1).is_some_and(is_bracket) {
        off - 1
    } else {
        return false;
    };
    match matching_bracket(&text, pos) {
        Some(m) => {
            let (r, c) = offset_to_cursor(&text, m);
            app.editor.move_cursor(CursorMove::Jump(r as u16, c as u16));
            app.status = t("已跳到配对括号").into();
        }
        None => {
            app.status = t("未找到配对括号").into();
        }
    }
    true
}

/// R53: the bracket the caret sits on (or right after, the same rule `%` uses)
/// and its match, both as editor `(row, col)` char positions. Only the caret's
/// ±`span`-line window is read: the lines are joined into a small string and run
/// through the shared `code_mask` lexer, so a `)` inside a string literal or a
/// comment still cannot pair with a `(` in code, while a buffer of any size
/// costs the same. `None` when the caret is not beside a bracket, the pair is
/// unbalanced, or the match lies outside the window.
pub(crate) fn bracket_pair_near(
    lines: &[String],
    row: usize,
    col: usize,
    span: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let line = lines.get(row)?;
    let lchars: Vec<char> = line.chars().collect();
    // Cheap rejection first: the caret must be on a bracket or immediately after
    // one, so most frames never build the window string at all.
    let near = lchars.get(col).copied().is_some_and(is_bracket)
        || (col > 0 && lchars.get(col - 1).copied().is_some_and(is_bracket));
    if !near {
        return None;
    }

    let lo = row.saturating_sub(span);
    let hi = (row + span).min(lines.len().saturating_sub(1));
    // Bounded by lines *and* bytes, so a pathological buffer (very long lines)
    // still cannot make a frame expensive.
    if lines[lo..=hi].iter().map(|l| l.len() + 1).sum::<usize>() > BRACKET_SCAN_BYTES {
        return None;
    }
    // Caret offset inside the window text (one `\n` between the joined lines).
    let mut base = 0usize;
    for l in &lines[lo..row] {
        base += l.chars().count() + 1;
    }
    let pos = base + col;

    let win = lines[lo..=hi].join("\n");
    let wchars: Vec<char> = win.chars().collect();
    let at = |i: usize| wchars.get(i).copied();
    let pos = if at(pos).is_some_and(is_bracket) {
        pos
    } else if pos > 0 && at(pos - 1).is_some_and(is_bracket) {
        pos - 1
    } else {
        return None;
    };
    let m = matching_bracket(&win, pos)?;
    let (pr, pc) = offset_to_cursor(&win, pos);
    let (mr, mc) = offset_to_cursor(&win, m);
    Some(((lo + pr, pc), (lo + mr, mc)))
}

/// The pair [`bracket_pair_near`] should highlight for the editor's current
/// caret, read at the session window size.
pub(crate) fn editor_bracket_pair(app: &App) -> Option<((usize, usize), (usize, usize))> {
    let (row, col) = app.editor.cursor();
    bracket_pair_near(app.editor.lines(), row, col, BRACKET_SCAN_LINES)
}

/// Display (terminal) column of char column `col` in `line`, expanding tabs the
/// way tui-textarea does (tab stop 4, its default and the one dbxt never
/// changes). Used to place the bracket highlight on the exact screen cell,
/// including wide CJK characters.
pub(crate) fn editor_display_col(line: &str, col: usize) -> usize {
    let mut w = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i >= col {
            break;
        }
        if ch == '\t' {
            w += 4 - (w % 4);
        } else {
            w += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    w
}

/// One match of the editor find needle: a run of `len` chars starting at char
/// column `col` on line `row`. Columns are char indices (not bytes), matching
/// tui-textarea's cursor coordinates, so a hit maps straight onto the caret and
/// onto [`editor_display_col`] for painting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FindHit {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) len: usize,
}

/// Case-insensitive char equality. ASCII is the hot path; non-ASCII falls back
/// to a per-char `to_lowercase`, so `SELECT` matches `select` and `Ä` matches
/// `ä`. A multi-char lowercase expansion simply cannot match one needle char,
/// which is the correct substring behaviour.
pub(crate) fn chars_equal_ci(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a.to_lowercase().eq(b.to_lowercase())
}

/// R61: case-insensitive, client-side substring search over the editor buffer.
/// Pure so the find state machine is unit-testable without a backend. A match
/// never crosses a line boundary (each line is searched on its own) and the
/// returned columns are char indices.
pub(crate) fn editor_find_hits(lines: &[String], needle: &str) -> Vec<FindHit> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if chars.len() < needle.len() {
            continue;
        }
        for start in 0..=chars.len() - needle.len() {
            let matched = chars[start..start + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| chars_equal_ci(*a, *b));
            if matched {
                hits.push(FindHit {
                    row,
                    col: start,
                    len: needle.len(),
                });
            }
        }
    }
    hits
}

// ── R71: built-in template placeholders (`{{name}}`) ─────────────────────────

/// One `{{name}}` placeholder in the editor buffer: a run of `len` chars starting
/// at char column `col` on line `row` (columns are char indices, matching
/// tui-textarea and [`editor_display_col`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Placeholder {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) len: usize,
}

/// Scan the buffer for `{{name}}` tokens (the placeholders a template writes).
/// Pure and client-side: a token is `{{`, one or more chars that are neither
/// `{` nor `}`, then `}}` — all on one line, so a stray `{{` never swallows the
/// rest of the buffer. Used both to jump the caret and to paint the highlight.
pub(crate) fn editor_placeholders(lines: &[String]) -> Vec<Placeholder> {
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i + 1 < chars.len() {
            if chars[i] == '{' && chars[i + 1] == '{' {
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '}' && chars[j] != '{' {
                    j += 1;
                }
                if j + 1 < chars.len() && chars[j] == '}' && chars[j + 1] == '}' && j > i + 2 {
                    out.push(Placeholder {
                        row,
                        col: i,
                        len: j + 2 - i,
                    });
                    i = j + 2;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

/// Put the caret on placeholder `p` and select the whole token, so typing
/// replaces `{{name}}` in one gesture. Records the start so [`jump_next_placeholder`]
/// can find the following token even after the text changed.
pub(crate) fn select_placeholder(app: &mut App, p: Placeholder) {
    app.editor.cancel_selection();
    app.editor
        .move_cursor(CursorMove::Jump(p.row as u16, p.col as u16));
    app.editor.start_selection();
    app.editor
        .move_cursor(CursorMove::Jump(p.row as u16, (p.col + p.len) as u16));
    app.template_ph_start = Some((p.row, p.col));
}

/// R71 `Tab`: move to the next `{{…}}` placeholder, wrapping to the first. The
/// anchor is the placeholder the caret last selected (falling back to the
/// caret), so a just-typed replacement does not make it skip a token. Returns
/// false when the buffer has no placeholder left, so the caller can fall back
/// to the normal pane switch.
pub(crate) fn jump_next_placeholder(app: &mut App) -> bool {
    let lines = app.editor.lines().to_vec();
    let phs = editor_placeholders(&lines);
    if phs.is_empty() {
        app.template_active = false;
        app.template_ph_start = None;
        return false;
    }
    let anchor = app.template_ph_start;
    let next = phs
        .iter()
        .find(|p| Some((p.row, p.col)) > anchor)
        .or_else(|| phs.first())
        .copied();
    if let Some(p) = next {
        select_placeholder(app, p);
        let idx = phs.iter().position(|q| *q == p).unwrap_or(0);
        app.status = tf("占位符 {}/{} · 替换后执行", &[&(idx + 1), &(phs.len())]);
        true
    } else {
        false
    }
}

/// Drop the placeholder mode once the buffer holds no `{{…}}` token (every
/// placeholder has been filled), so `Tab` returns to pane switching. Runs after
/// every key, the same hook as the editor-find invalidation.
pub(crate) fn sync_editor_template(app: &mut App) {
    if !app.template_active {
        return;
    }
    if editor_placeholders(app.editor.lines()).is_empty() {
        app.template_active = false;
        app.template_ph_start = None;
        app.status = t("占位符已填完 · Tab 切栏").into();
    }
}

/// Outcome of an editor undo, so the caller can name what happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditorUndo {
    /// Undid a pending Alt-F reformat via the R57 snapshot.
    Reformatted,
    /// Undid one step through tui-textarea's own history.
    History,
    /// Nothing left to undo.
    None,
}

/// R58: undo one editor step. A pending Alt-F reformat (`editor_undo` snapshot)
/// is undone in one gesture; otherwise tui-textarea's own history is used.
/// tui-textarea leaves Ctrl-Z unbound, so R58 wires it (Ctrl-U stays for muscle
/// memory). The return value lets the caller flash a truthful status instead of
/// a silent no-op.
pub(crate) fn editor_undo_step(app: &mut App) -> EditorUndo {
    if let Some(prev) = app.editor_undo.take() {
        app.set_editor_text(&prev);
        EditorUndo::Reformatted
    } else if app.editor.undo() {
        EditorUndo::History
    } else {
        EditorUndo::None
    }
}

/// R58: redo one undone edit through tui-textarea's history.
pub(crate) fn editor_redo_step(app: &mut App) -> bool {
    app.editor.redo()
}

/// Status text for an undo outcome, kept beside the step so they never drift.
pub(crate) fn editor_undo_status(outcome: EditorUndo) -> &'static str {
    match outcome {
        EditorUndo::Reformatted => t("已撤销格式化"),
        EditorUndo::History => t("已撤销"),
        EditorUndo::None => t("没有可撤销的"),
    }
}

/// R61: open the `Ctrl-F` find prompt (bottom bar). Pre-filled with the last
/// needle so a repeated search is one keystroke away; the highlight is kept.
pub(crate) fn open_editor_find(app: &mut App) {
    let mut ta = TextArea::from([app.editor_find_needle.clone()]);
    ta.set_placeholder_text(t("查找…（大小写不敏感，纯客户端）"));
    ta.move_cursor(CursorMove::End);
    app.editor_find = Some(ta);
    app.editor_find_snapshot = app.editor.lines().to_vec();
    editor_find_recompute_idx(app);
    app.status = editor_find_status(app);
}

/// The current hit index for the caret: the first match at or after it, wrapping
/// to the first. `None` when the needle is empty or matches nothing.
pub(crate) fn editor_find_recompute_idx(app: &mut App) {
    if app.editor_find_needle.is_empty() {
        app.editor_find_idx = None;
        return;
    }
    let hits = editor_find_hits(app.editor.lines(), &app.editor_find_needle);
    app.editor_find_idx = if hits.is_empty() {
        None
    } else {
        let cur = app.editor.cursor();
        Some(hits.iter().position(|h| (h.row, h.col) >= cur).unwrap_or(0))
    };
}

/// The `3/7` status line for the active find. Kept beside the state machine so
/// the count and the highlight can never disagree.
pub(crate) fn editor_find_status(app: &App) -> String {
    if app.editor_find_needle.is_empty() {
        return t("查找：输入关键词 · Enter/F3 下一个 · Esc 退出").into();
    }
    let n = editor_find_hits(app.editor.lines(), &app.editor_find_needle).len();
    if n == 0 {
        return tf("查找「{}」· 无命中", &[&(app.editor_find_needle)]);
    }
    let i = app.editor_find_idx.map(|i| i + 1).unwrap_or(1).min(n);
    tf(
        "查找「{}」· {}/{} · Enter/F3/Alt-N 下一个 · Alt-B 上一个 · Esc 退出",
        &[&(app.editor_find_needle), &(i), &(n)],
    )
}

/// Move the caret to the next (`dir > 0`) / previous (`dir < 0`) match, wrapping
/// around. Anchors on the caret when it is not itself a hit, so the first step
/// lands on the nearest match rather than the first in the buffer. Used by
/// Enter / F3 / Alt-N and their reverse keys, both in the prompt and after it
/// closed (highlight kept). Returns whether a match was found.
pub(crate) fn editor_find_step(app: &mut App, dir: i32) -> bool {
    if app.editor_find_needle.is_empty() {
        // No needle yet: F3 / Alt-N just open the input.
        open_editor_find(app);
        return false;
    }
    let lines = app.editor.lines().to_vec();
    let hits = editor_find_hits(&lines, &app.editor_find_needle);
    if hits.is_empty() {
        app.editor_find_idx = None;
        app.status = tf("查找「{}」· 无命中", &[&(app.editor_find_needle)]);
        return false;
    }
    let n = hits.len();
    let cur = app.editor.cursor();
    let idx = match hits.iter().position(|h| (h.row, h.col) == cur) {
        Some(i) => (i as i32 + dir).rem_euclid(n as i32) as usize,
        None if dir > 0 => hits.iter().position(|h| (h.row, h.col) > cur).unwrap_or(0),
        None => hits
            .iter()
            .rposition(|h| (h.row, h.col) < cur)
            .unwrap_or(n - 1),
    };
    let hit = hits[idx];
    app.editor_find_idx = Some(idx);
    app.editor_find_snapshot = lines;
    app.editor
        .move_cursor(CursorMove::Jump(hit.row as u16, hit.col as u16));
    app.status = editor_find_status(app);
    true
}

/// Drop the whole find state (needle, prompt, index, snapshot). Called on an
/// edit so a stale highlight never lingers over text it no longer matches, and
/// by a backend switch.
pub(crate) fn clear_editor_find(app: &mut App) {
    app.editor_find = None;
    app.editor_find_needle.clear();
    app.editor_find_idx = None;
    app.editor_find_snapshot.clear();
}

/// R61: drop the highlight once the buffer changed under it (any edit). While
/// the buffer is untouched the needle survives, so `Esc` keeps the highlight
/// exactly until the next edit.
pub(crate) fn sync_editor_find(app: &mut App) {
    if app.editor_find_needle.is_empty() {
        return;
    }
    if app.editor.lines() != app.editor_find_snapshot.as_slice() {
        clear_editor_find(app);
    }
}

/// Prompt handler for `Ctrl-F`. Enter / F3 / Alt-N step forward, Shift-Enter /
/// Shift-F3 / Alt-B step back, Esc closes the input but keeps the highlight.
/// Anything else types into the needle and refreshes the live count.
pub(crate) fn editor_find_key(app: &mut App, k: KeyEvent) {
    match (k.modifiers, k.code) {
        (m, KeyCode::Enter) if m.contains(KeyModifiers::SHIFT) => {
            editor_find_step(app, -1);
        }
        (KeyModifiers::NONE, KeyCode::Enter) => {
            editor_find_step(app, 1);
        }
        (m, KeyCode::F(3)) if m.contains(KeyModifiers::SHIFT) => {
            editor_find_step(app, -1);
        }
        (_, KeyCode::F(3)) => {
            editor_find_step(app, 1);
        }
        (KeyModifiers::ALT, KeyCode::Char('n')) | (KeyModifiers::ALT, KeyCode::Char('N')) => {
            editor_find_step(app, 1);
        }
        (KeyModifiers::ALT, KeyCode::Char('b')) | (KeyModifiers::ALT, KeyCode::Char('B')) => {
            editor_find_step(app, -1);
        }
        (KeyModifiers::NONE, KeyCode::Esc) => {
            app.editor_find = None;
            let n = editor_find_hits(app.editor.lines(), &app.editor_find_needle).len();
            if app.editor_find_needle.is_empty() || n == 0 {
                app.status = t("已退出查找").into();
            } else {
                let i = app.editor_find_idx.map(|i| i + 1).unwrap_or(1).min(n);
                app.status = tf(
                    "查找「{}」· {}/{} · F3/Alt-N 下一个 · Alt-B 上一个 · 编辑后清除高亮",
                    &[&(app.editor_find_needle), &(i), &(n)],
                );
            }
        }
        _ => {
            if let Some(t) = &mut app.editor_find {
                t.input(k);
            }
            app.editor_find_needle = app
                .editor_find
                .as_ref()
                .map(|t| t.lines().join(" "))
                .unwrap_or_default();
            app.editor_find_snapshot = app.editor.lines().to_vec();
            editor_find_recompute_idx(app);
            app.status = editor_find_status(app);
        }
    }
}

pub(crate) fn editor_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    editor_key_inner(app, tx, k);
    // R61: any edit invalidates the find highlight (matches moved); `Esc` keeps
    // it until exactly this moment.
    sync_editor_find(app);
}

pub(crate) fn editor_key_inner(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The completion popup owns the keyboard while it is open: Tab / Enter
    // accept, Esc cancels, arrows move, anything else keeps typing (and refines
    // the candidate list).
    if app.completion.is_some() {
        completion_key(app, k);
        return;
    }
    // R62: half-page scroll in the editor. `Ctrl-D` is free in dbxt (its
    // forward-delete alias also lives on the `Del` key), so it scrolls down half
    // a screen and the widget pulls the cursor into the new viewport; `Ctrl-U`
    // keeps its R51 undo role and yields here — see `half_page_key`.
    if let Some(dir) = half_page_key(
        Focus::Editor,
        k.modifiers.contains(KeyModifiers::CONTROL),
        k.code,
    ) {
        let down = dir == HalfPage::Down;
        app.editor_vp.half_page(down);
        app.editor.scroll(if down {
            Scrolling::HalfPageDown
        } else {
            Scrolling::HalfPageUp
        });
        return;
    }
    match (k.modifiers, k.code) {
        (m, KeyCode::Char('j')) if m.contains(KeyModifiers::CONTROL) => run_current(app, tx),
        // R61 Ctrl-F: find inside the editor buffer (client-side, no query).
        // Free in the editor — the results pane owns Ctrl-F for page turn.
        (m, KeyCode::Char('f')) if m.contains(KeyModifiers::CONTROL) => open_editor_find(app),
        // R61: F3 / Shift-F3 and Alt-N / Alt-B cycle the matches. They work both
        // in the find prompt and after it closed (Esc keeps the highlight); with
        // no needle yet they just open the input. Alt-N is the non-Shift forward
        // key, Alt-B the non-Shift backward key (both free in the editor).
        (KeyModifiers::NONE, KeyCode::F(3)) => {
            editor_find_step(app, 1);
        }
        (m, KeyCode::F(3)) if m.contains(KeyModifiers::SHIFT) => {
            editor_find_step(app, -1);
        }
        (KeyModifiers::ALT, KeyCode::Char('n')) | (KeyModifiers::ALT, KeyCode::Char('N')) => {
            editor_find_step(app, 1);
        }
        (KeyModifiers::ALT, KeyCode::Char('b')) | (KeyModifiers::ALT, KeyCode::Char('B')) => {
            editor_find_step(app, -1);
        }
        // Alt-/ : table / column / keyword prefix completion at the cursor.
        // Chosen over Ctrl-Space because that combination is claimed by the
        // input-method switcher in fcitx5/ibus and on Windows.
        (KeyModifiers::ALT, KeyCode::Char('/')) => open_completion(app),
        // Ctrl-Space remains as an unadvertised compatibility alias; some
        // terminals deliver it as NUL, which is the legacy path below.
        (m, KeyCode::Char(' ')) if m.contains(KeyModifiers::CONTROL) => open_completion(app),
        (m, KeyCode::Null) if m.contains(KeyModifiers::CONTROL) || m.is_empty() => {
            open_completion(app)
        }
        // Alt-F: format the editor's SQL, or compress it back to one line when
        // it is already in canonical form (idempotent toggle).
        (KeyModifiers::ALT, KeyCode::Char('f')) | (KeyModifiers::ALT, KeyCode::Char('F')) => {
            toggle_format_editor(app)
        }
        // Alt-L: read a .sql file, preview it, then run it as a script.
        (KeyModifiers::ALT, KeyCode::Char('l')) | (KeyModifiers::ALT, KeyCode::Char('L')) => {
            open_file_load(app)
        }
        // Alt-S: one-step favourite of the current editor SQL (the same store as
        // Ctrl-O's list). Bare `f` cannot carry this — it is an ordinary SQL
        // character — and bare `F` would need Shift, so the modifier key wins.
        (KeyModifiers::ALT, KeyCode::Char('s')) | (KeyModifiers::ALT, KeyCode::Char('S')) => {
            open_snippet_name(app)
        }
        // R52: Alt-↓ / Alt-↑ step to the next / previous statement start in a
        // multi-statement script (semicolon-delimited, comments ignored), so a
        // long script can be inspected one statement at a time without arrowing
        // through the whole buffer. Free in the editor (Alt-← / Alt-→ stay the
        // local cursor motion, Alt-↑/↓ are unbound there).
        (KeyModifiers::ALT, KeyCode::Down) => {
            jump_statement(app, 1);
        }
        (KeyModifiers::ALT, KeyCode::Up) => {
            jump_statement(app, -1);
        }
        // Ctrl-Z / Ctrl-U: undo the last Alt-F reformat in one step (R57
        // snapshot); with no reformat to undo it falls back to the editor's own
        // undo history (tui-textarea, which has no default Ctrl-Z binding). R58
        // flashes the outcome so a no-op undo is no longer silent. Ctrl-U stays
        // as the R51 key for muscle memory.
        (m, KeyCode::Char('z')) if m.contains(KeyModifiers::CONTROL) => {
            app.status = editor_undo_status(editor_undo_step(app)).into();
        }
        (m, KeyCode::Char('u')) if m.contains(KeyModifiers::CONTROL) => {
            app.status = editor_undo_status(editor_undo_step(app)).into();
        }
        // Ctrl-Y / Ctrl-R: redo (R58). tui-textarea bound Ctrl-Y to its internal
        // yank buffer, so Alt-Y keeps that available; Ctrl-R was already
        // tui-textarea's redo. Both flash the outcome.
        (m, KeyCode::Char('y')) if m.contains(KeyModifiers::CONTROL) => {
            app.status = if editor_redo_step(app) {
                t("已重做")
            } else {
                t("没有可重做的")
            }
            .into();
        }
        (m, KeyCode::Char('r')) if m.contains(KeyModifiers::CONTROL) => {
            app.status = if editor_redo_step(app) {
                t("已重做")
            } else {
                t("没有可重做的")
            }
            .into();
        }
        // Alt-Y: the editor's internal yank/paste, freed from Ctrl-Y by R58.
        (KeyModifiers::ALT, KeyCode::Char('y')) | (KeyModifiers::ALT, KeyCode::Char('Y')) => {
            app.status = if app.editor.paste() {
                t("已粘贴缓冲区")
            } else {
                t("粘贴缓冲区为空")
            }
            .into();
        }
        // `%`: vim's bracket jump. It only fires when the cursor sits on, or
        // immediately after, a `()[]{}` bracket — anywhere else `%` is typed
        // literally, so `LIKE '%x%'` and `a % b` are never hijacked.
        (m, KeyCode::Char('%'))
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            if !jump_matching_bracket(app) {
                app.editor.input(k);
            }
        }
        // Ctrl-Shift-K kills to the end of the line. Terminals report the shifted
        // key as an uppercase `K`, which tui-textarea's lowercase Ctrl-K binding
        // does not match, so it is claimed here (lowercase Ctrl-K still reaches
        // the built-in binding below).
        (m, KeyCode::Char('K')) if m.contains(KeyModifiers::CONTROL) => {
            app.editor.delete_line_by_end();
        }
        // Readline-style line editing rides tui-textarea's built-in bindings,
        // which reach `_` below: Ctrl-A / Home = line head, Ctrl-E / End = line
        // tail, Ctrl-K (and Ctrl-Shift-K) = kill to end of line, Ctrl-W = delete
        // the previous word. `browse_key` deliberately does not claim those
        // combos while the editor is focused, so the muscle memory survives.
        (KeyModifiers::NONE, KeyCode::F(5)) => run_current(app, tx),
        (KeyModifiers::NONE, KeyCode::Tab) => {
            app.focus = if app.backend_kind == Backend::Sql {
                Focus::Preview
            } else {
                Focus::CmdInput
            }
        }
        (KeyModifiers::NONE, KeyCode::Esc) => app.focus = Focus::Sidebar,
        // shell-style history recall: ↑ on the first line walks back in time
        (KeyModifiers::NONE, KeyCode::Up) if app.editor.cursor().0 == 0 => {
            if !app.history_prev() {
                app.editor.input(k);
            }
        }
        (KeyModifiers::NONE, KeyCode::Down)
            if app.history_idx.is_some()
                && app.editor.cursor().0 + 1 == app.editor.lines().len() =>
        {
            if !app.history_next() {
                app.editor.input(k);
            }
        }
        _ => {
            // The page-scroll keys move tui-textarea's viewport without moving
            // the cursor out of it, which the render-time cursor-follow cannot
            // see; replay the same delta on the click-mapping mirror first.
            app.editor_vp.note_key(&k);
            app.editor.input(k);
        }
    }
}

pub(crate) fn cmd_input_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match (k.modifiers, k.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => run_cmd_line(app, tx),
        (KeyModifiers::NONE, KeyCode::Esc) => app.focus = Focus::Editor,
        (KeyModifiers::NONE, KeyCode::Tab) => app.focus = Focus::Preview,
        (KeyModifiers::NONE, KeyCode::Char('[')) => {
            app.redis_db = app.redis_db.saturating_sub(1);
            app.set_placeholder();
        }
        (KeyModifiers::NONE, KeyCode::Char(']')) => {
            app.redis_db = app.redis_db.saturating_add(1);
            app.set_placeholder();
        }
        _ => {
            app.cmd_input.input(k);
        }
    }
}
