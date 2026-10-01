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
            // R87: toggle the fold state and persist it (via `set_group_open`).
            let open = app.group_closed.contains(&id);
            set_group_open(app, &id, open);
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
        // R109: Enter on a column outline row drops the column name into the
        // SQL editor at the caret (the tree has no table-name insert pipeline of
        // its own, so this is the light implementation the round asked for). The
        // sidebar keeps the keyboard so several columns can be picked in a row.
        SideRow::Column { table, col, .. } => {
            let Some(name) = outline_column(app, table, col).map(|c| c.name.clone()) else {
                return;
            };
            app.editor.insert_str(&name);
            app.status = tf("已插入列 {}", &[&(fix_double_encoding(&name))]);
        }
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

/// Split a whole-buffer char range into per-row `(row, col_start, col_end)` spans
/// (char columns) so a paint pass can colour every screen cell a multi-line span
/// covers. Rows outside `start..end` are skipped; pure and allocation-light.
pub(crate) fn char_range_rows(
    lines: &[String],
    start: usize,
    end: usize,
) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for (row, line) in lines.iter().enumerate() {
        let len = line.chars().count();
        let ls = off;
        let le = off + len;
        if end > ls && start < le {
            let c0 = start.saturating_sub(ls).min(len);
            let c1 = end.saturating_sub(ls).min(len);
            if c1 > c0 {
                out.push((row, c0, c1));
            }
        }
        off = le + 1;
        if off > end {
            break;
        }
    }
    out
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

/// The 0-based line at which each of `statements` starts inside the script they
/// were split from (a statement's own `\n` count advances the next start).
/// Used by the R77 line fallback, where the driver reports a line *within its
/// statement* and the statement's own offset in the buffer is unknown.
pub(crate) fn statement_start_lines(statements: &[String]) -> Vec<usize> {
    let mut out = Vec::with_capacity(statements.len());
    let mut line = 0usize;
    for st in statements {
        out.push(line);
        line += st.matches('\n').count() + 1;
    }
    out
}

/// R77 fallback: the statement `index` of `statements` when the driver reported
/// `err_line` (1-based, relative to that statement). Returns the editor span
/// containing that absolute line, or `None` when it is out of range. Pure.
pub(crate) fn locate_statement_at_line(
    editor_text: &str,
    statements: &[String],
    index: usize,
    err_line: usize,
) -> Option<(usize, usize)> {
    let starts = statement_start_lines(statements);
    let base = *starts.get(index)?;
    let line = base + err_line.saturating_sub(1);
    let chars: Vec<char> = editor_text.chars().collect();
    let mut off = 0usize;
    let mut cur = 0usize;
    while cur < line {
        if off >= chars.len() {
            return None;
        }
        if chars[off] == '\n' {
            cur += 1;
        }
        off += 1;
    }
    let ranges = statement_ranges(editor_text);
    statement_range_at(&ranges, off).or_else(|| ranges.last().copied())
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

// ── R79: editor input assist (auto-indent on Enter / bracket auto-pair) ──────

/// Line endings that open a block: the next line stays one level deeper.
const INDENT_OPENERS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "JOIN", "ON", "SET", "VALUES",
];

/// R79: the leading whitespace (spaces / tabs, verbatim) of `line`.
pub(crate) fn leading_ws(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// R79: true when the line's own brackets are unbalanced — more `(` / `[` / `{`
/// than closers, counting *code* only (a `(` inside a string or comment never
/// counts; the shared [`code_mask`] lexer draws the boundary).
pub(crate) fn line_has_unclosed_bracket(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mask = code_mask(&chars);
    let mut depth = 0i32;
    for (i, c) in chars.iter().enumerate() {
        if !mask[i] {
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth > 0
}

/// R79: true when a new line under `line` should be indented one level deeper —
/// the line ends with a block keyword, `(` / `[` / `{` / `,`, or has an unclosed
/// bracket of its own. Only *code* counts, so a trailing keyword or bracket
/// inside a string / comment never opens a block. Pure, so the whole
/// inheritance matrix is unit-testable.
pub(crate) fn indent_one_deeper(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mask = code_mask(&chars);
    // The last non-whitespace character that is real code.
    let last_code = (0..chars.len())
        .rev()
        .find(|&i| mask[i] && !chars[i].is_whitespace());
    if let Some(i) = last_code {
        if matches!(chars[i], '(' | '[' | '{' | ',') {
            return true;
        }
        // The trailing identifier: a line that ends with `WHERE` opens a block,
        // while `WHERE x = 1` does not.
        if chars[i].is_alphanumeric() || chars[i] == '_' {
            let start = (0..=i)
                .rev()
                .take_while(|&j| mask[j] && (chars[j].is_alphanumeric() || chars[j] == '_'))
                .last()
                .unwrap_or(i);
            let token: String = chars[start..=i].iter().collect();
            if INDENT_OPENERS.contains(&token.to_ascii_uppercase().as_str()) {
                return true;
            }
        }
    }
    line_has_unclosed_bracket(line)
}

/// R79: the whitespace a new line should start with when `Enter` is pressed on
/// `lines[row]`. A blank (whitespace-only) previous line inherits nothing; a
/// block-opening one inherits its own indent plus two spaces; every other line
/// just passes its indent down.
pub(crate) fn auto_indent(lines: &[String], row: usize) -> String {
    let Some(line) = lines.get(row) else {
        return String::new();
    };
    if line.trim().is_empty() {
        return String::new();
    }
    let base = leading_ws(line);
    if indent_one_deeper(line) {
        format!("{base}  ")
    } else {
        base
    }
}

/// R79: is the char offset `cursor` in *code* (not inside a string literal,
/// quoted identifier or comment)? A cursor at the very end of the buffer is
/// treated as the state the text leaves behind, so typing into an unterminated
/// literal still counts as being inside it. Shared with [`pair_action`].
pub(crate) fn char_in_code(text: &str, cursor: usize) -> bool {
    let chars: Vec<char> = text.chars().chain(std::iter::once(' ')).collect();
    let mask = code_mask(&chars);
    mask.get(cursor).copied().unwrap_or(true)
}

/// R79: what the auto-pair layer should do with a typed bracket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PairAction {
    /// Type `ch` and its closer, leaving the caret between them.
    Pair(char),
    /// The caret already sits on the matching closer — step over it.
    Skip,
    /// Not a bracket, or inside a literal / comment: type it normally.
    Pass,
}

/// R79: decide how a typed `ch` at char offset `cursor` is handled. Only `(`
/// and `[` auto-close (a `{` is left alone so it never fights the template
/// `{{…}}` placeholders), and both the opener and the closer-skip respect the
/// lexer, so a `(` typed inside `'…'` or a `-- comment` stays literal.
pub(crate) fn pair_action(text: &str, cursor: usize, ch: char) -> PairAction {
    let chars: Vec<char> = text.chars().collect();
    let next = chars.get(cursor).copied();
    match ch {
        '(' | '[' => {
            if char_in_code(text, cursor) {
                PairAction::Pair(if ch == '(' { ')' } else { ']' })
            } else {
                PairAction::Pass
            }
        }
        ')' | ']' => {
            if next == Some(ch) && char_in_code(text, cursor) {
                PairAction::Skip
            } else {
                PairAction::Pass
            }
        }
        _ => PairAction::Pass,
    }
}

// ── R93: comment toggle (`Ctrl-/` / `Alt-C`) ──────────────────────────────────

/// What a comment toggle did, so the caller can name it in the status bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CommentAction {
    Commented,
    Uncommented,
}

/// True when `line`'s first non-whitespace characters are a *real* `--` line
/// comment. `line_start` is the line's char offset inside the whole-buffer
/// lexer and `mask` its [`code_mask`] flags, so a `--` that continues a
/// multi-line string literal (or any other non-code context) is never mistaken
/// for a comment.
///
/// `code_mask` records the first `-` of a real line comment as *code* (it only
/// flips to the comment state after recording), so the mask bit is exactly the
/// discriminator: `true` = a real opener, `false` = inside a literal / comment.
pub(crate) fn line_is_commented(line: &str, line_start: usize, mask: &[bool]) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let Some(i) = chars.iter().position(|c| !c.is_whitespace()) else {
        return false;
    };
    chars.get(i) == Some(&'-')
        && chars.get(i + 1) == Some(&'-')
        && mask.get(line_start + i).copied().unwrap_or(false)
}

/// R93: add / remove a `-- ` line comment across rows `r0..=r1` (inclusive).
///
/// The decision is uniform for the whole range: when every non-blank line is
/// already a real `--` comment the toggle removes one `--` (plus one following
/// space) from each; otherwise it inserts `-- ` after each line's indentation.
/// Blank lines are left untouched and never affect the decision. [`code_mask`]
/// (the shared SQL lexer) draws the boundary, so a `--` continuing a multi-line
/// string literal is invisible to the uncomment branch. `/* … */` wrapping is
/// deliberately *not* used: nested / partial block comments are easy to get
/// wrong, while the line-prefix form is exactly revertible.
///
/// Returns the rewritten lines plus the action taken, or `None` when the range
/// holds nothing but blank lines. Pure, so the whole matrix is unit-testable.
pub(crate) fn toggle_comment_lines(
    lines: &[String],
    r0: usize,
    r1: usize,
) -> Option<(Vec<String>, CommentAction)> {
    if lines.is_empty() || r0 > r1 || r1 >= lines.len() {
        return None;
    }
    let text = lines.join("\n");
    let chars: Vec<char> = text.chars().collect();
    let mask = code_mask(&chars);
    let mut starts = Vec::with_capacity(lines.len());
    let mut off = 0usize;
    for l in lines {
        starts.push(off);
        off += l.chars().count() + 1;
    }
    let blank = |row: usize| lines[row].trim().is_empty();
    if (r0..=r1).all(blank) {
        return None;
    }
    let all_commented =
        (r0..=r1).all(|r| blank(r) || line_is_commented(&lines[r], starts[r], &mask));
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (row, line) in lines.iter().enumerate() {
        if row < r0 || row > r1 || blank(row) {
            out.push(line.clone());
            continue;
        }
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let (head, body) = line.split_at(indent);
        if all_commented {
            let body_chars: Vec<char> = body.chars().collect();
            let drop = if body_chars.get(2) == Some(&' ') {
                3
            } else {
                2
            };
            out.push(format!(
                "{head}{}",
                body_chars[drop..].iter().collect::<String>()
            ));
        } else {
            out.push(format!("{head}-- {body}"));
        }
    }
    Some((
        out,
        if all_commented {
            CommentAction::Uncommented
        } else {
            CommentAction::Commented
        },
    ))
}

/// R93 `Ctrl-/` (and the in-editor `Alt-C` fallback): toggle `-- ` line comments
/// over the selection, or just the caret's line when nothing is selected. A
/// multi-line selection is commented one line at a time (never `/* */`), keeping
/// each line's indentation; a block that stays selected keeps the gesture
/// repeatable, so a second press uncomments it. Render-state only — the buffer
/// is rewritten in one gesture and the SQL semantics are never touched beyond
/// the comment markers.
pub(crate) fn toggle_comment(app: &mut App) {
    let lines = app.editor.lines().to_vec();
    let had_selection = app.editor.is_selecting();
    let cursor = app.editor.cursor();
    let (r0, r1) = match app.editor.selection_range() {
        Some(((sr, _), (er, ec))) => {
            // A selection that ends at column 0 does not include that line.
            let end = if ec == 0 && er > sr { er - 1 } else { er };
            (sr, end)
        }
        None => (cursor.0, cursor.0),
    };
    let Some((next, action)) = toggle_comment_lines(&lines, r0, r1) else {
        app.status = t("没有可注释的行").into();
        return;
    };
    let n = (r0..=r1).filter(|&r| !lines[r].trim().is_empty()).count();
    let text = next.join("\n");
    app.editor.select_all();
    app.editor.insert_str(&text);
    app.editor_clip_idx = None;
    if had_selection {
        // Keep the same block selected so a repeat flips the comment back.
        let end_col = next.get(r1).map(|l| l.chars().count()).unwrap_or(0);
        app.editor.cancel_selection();
        app.editor.move_cursor(CursorMove::Jump(r0 as u16, 0));
        app.editor.start_selection();
        app.editor
            .move_cursor(CursorMove::Jump(r1 as u16, end_col as u16));
    } else {
        // A single-line toggle keeps the caret on the same text (shifted by the
        // marker) instead of leaving the whole line selected — otherwise the
        // next typed character would replace it.
        let indent = lines[r0].len() - lines[r0].trim_start_matches([' ', '\t']).len();
        let delta: isize = match action {
            CommentAction::Commented => 3,
            CommentAction::Uncommented => {
                if lines[r0][indent..].chars().nth(2) == Some(' ') {
                    -3
                } else {
                    -2
                }
            }
        };
        let new_len = next.get(r0).map(|l| l.chars().count()).unwrap_or(0);
        let col = if cursor.1 <= indent {
            cursor.1
        } else {
            (cursor.1 as isize + delta).clamp(indent as isize, new_len as isize) as usize
        };
        app.editor.cancel_selection();
        app.editor
            .move_cursor(CursorMove::Jump(r0 as u16, col as u16));
    }
    app.status = match action {
        CommentAction::Commented => tf("已注释 {} 行", &[&n]),
        CommentAction::Uncommented => tf("已取消注释 {} 行", &[&n]),
    };
}

// ── R93: editor clipboard ring (`Ctrl-Shift-V`) ──────────────────────────────

/// Push one editor yank payload (a copy / cut / `Ctrl-K` kill) onto the ring:
/// newest first, a repeat of an existing entry floats to the top instead of
/// duplicating, and the ring never grows past [`EDITOR_CLIP_MAX`]. Pure, so the
/// ordering / dedupe / cap are unit-testable.
pub(crate) fn clip_ring_push(ring: &mut Vec<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    if ring.first().map(String::as_str) == Some(text) {
        return;
    }
    ring.retain(|s| s != text);
    ring.insert(0, text.to_string());
    ring.truncate(EDITOR_CLIP_MAX);
}

/// R93: capture a fresh editor yank into the ring. Runs after every key, so any
/// path that fills tui-textarea's yank buffer (copy, cut, `Ctrl-K`) is
/// remembered; an unchanged buffer is a cheap no-op. Purely in memory — nothing
/// is persisted and the system clipboard is never read or written.
pub(crate) fn record_editor_yank(app: &mut App) {
    let yanked = app.editor.yank_text();
    if yanked.is_empty() {
        // A fresh TextArea (history recall, file load, reformat) has no yank;
        // forget the last payload so re-copying it later is still detected.
        app.editor_clip_last.clear();
        return;
    }
    if yanked == app.editor_clip_last {
        return;
    }
    app.editor_clip_last = yanked.clone();
    clip_ring_push(&mut app.editor_clip_ring, &yanked);
}

/// R93 `Ctrl-Shift-V`: paste the next ring entry, replacing the current
/// selection (or inserting at the caret) and leaving the pasted text *selected*
/// so a repeat replaces it and advances — a true ring rather than a growing
/// paste. The newest entry is used first; any other key resets the cycle.
pub(crate) fn editor_clip_paste_step(app: &mut App) {
    let n = app.editor_clip_ring.len();
    if n == 0 {
        app.status = t("剪贴板环为空（先复制或剪切）").into();
        return;
    }
    let next = match app.editor_clip_idx {
        Some(i) => (i + 1) % n,
        None => 0,
    };
    let text = app.editor_clip_ring[next].clone();
    // Where the paste starts: the selection head when there is one, else the
    // caret.
    let start = app
        .editor
        .selection_range()
        .map(|(s, _)| s)
        .unwrap_or_else(|| app.editor.cursor());
    app.editor.insert_str(&text);
    let end = app.editor.cursor();
    app.editor.cancel_selection();
    app.editor
        .move_cursor(CursorMove::Jump(start.0 as u16, start.1 as u16));
    app.editor.start_selection();
    app.editor
        .move_cursor(CursorMove::Jump(end.0 as u16, end.1 as u16));
    app.editor_clip_idx = Some(next);
    app.status = tf("剪贴板环 {}/{} · 再按替换", &[&(next + 1), &n]);
}

/// True when `k` is the clipboard-ring paste key: `Ctrl-Shift-V`, which the
/// terminal reports either as an uppercase `V` or as `v` with the SHIFT flag set
/// (kitty / extended keyboard protocol). Used to keep the cycle position across
/// repeats while any other key resets it.
pub(crate) fn is_clip_ring_key(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
        && (matches!(k.code, KeyCode::Char('V'))
            || (matches!(k.code, KeyCode::Char('v')) && k.modifiers.contains(KeyModifiers::SHIFT)))
}

// ── R77: execution-error statement location ──────────────────────────────────

/// Parse the 1-based line number out of a driver error message. Recognises the
/// two shapes the bundled drivers actually emit — MySQL's `... at line 3` and
/// PostgreSQL's `LINE 3: ...` — and nothing else, so a `line` in an unrelated
/// message is never mistaken for a location. Pure text, no query.
pub(crate) fn extract_error_line(msg: &str) -> Option<usize> {
    let lower = msg.to_ascii_lowercase();
    scan_line_marker(&lower).or_else(|| scan_at_line(&lower))
}

/// PostgreSQL-style `LINE 3:` marker (a `line` token, spaces, digits, colon).
fn scan_line_marker(lower: &str) -> Option<usize> {
    let b = lower.as_bytes();
    let mut i = 0usize;
    while i + 4 <= b.len() {
        if &b[i..i + 4] == b"line" && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let mut j = i + 4;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            let s = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > s {
                let mut k = j;
                while k < b.len() && b[k] == b' ' {
                    k += 1;
                }
                if k < b.len() && b[k] == b':' {
                    if let Ok(n) = lower[s..j].parse::<usize>() {
                        if n > 0 {
                            return Some(n);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// MySQL-style `at line 3` marker (digits run to the end of the token).
fn scan_at_line(lower: &str) -> Option<usize> {
    let pat = "at line ";
    let b = lower.as_bytes();
    let mut from = 0usize;
    while from <= lower.len() {
        let Some(p) = lower[from..].find(pat) else {
            break;
        };
        let s = from + p + pat.len();
        let mut j = s;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > s {
            if let Ok(n) = lower[s..j].parse::<usize>() {
                if n > 0 {
                    return Some(n);
                }
            }
        }
        from += p + 1;
    }
    None
}

/// Map each statement the engine ran (in order) back to its char-offset span in
/// the editor buffer, or `None` when it cannot be matched. The counts usually
/// agree, in which case the index is trusted after a text check; when the engine
/// split the script differently (or stopped early) the span is found by text.
/// Pure and client-side, so the mapping is unit-testable without a backend.
pub(crate) fn locate_statement_spans(
    editor_text: &str,
    statements: &[String],
) -> Vec<Option<(usize, usize)>> {
    locate_statement_indices(editor_text, statements)
        .into_iter()
        .map(|o| o.map(|(_, span)| span))
        .collect()
}

/// R88: like [`locate_statement_spans`] but also returns each statement's
/// 0-based index into the buffer's statement list, so a scoped run can name the
/// editor ordinal (`第 N 条`) of the statements it executed. Pure.
pub(crate) fn locate_statement_indices(
    editor_text: &str,
    statements: &[String],
) -> Vec<Option<(usize, (usize, usize))>> {
    let ranges = statement_ranges(editor_text);
    let chars: Vec<char> = editor_text.chars().collect();
    let text_of = |(s, e): (usize, usize)| -> String {
        chars[s..e].iter().collect::<String>().trim().to_string()
    };
    let same_count = ranges.len() == statements.len();
    let mut used = vec![false; ranges.len()];
    let mut out = Vec::with_capacity(statements.len());
    for (i, st) in statements.iter().enumerate() {
        let needle = st.trim();
        let mut pick = None;
        if same_count && i < ranges.len() && !used[i] && text_of(ranges[i]) == needle {
            pick = Some(i);
        }
        if pick.is_none() {
            pick = (0..ranges.len()).find(|&r| !used[r] && text_of(ranges[r]) == needle);
        }
        match pick {
            Some(r) => {
                used[r] = true;
                out.push(Some((r, ranges[r])));
            }
            None => out.push(None),
        }
    }
    out
}

/// R88: the 0-based row each statement's first code token sits on, paired with
/// its 1-based ordinal. A line that carries two statements keeps the first (the
/// ordinal of the line's leading statement). Purely textual — used by the
/// optional editor statement gutter — and `None` (empty) above `max_bytes`, so a
/// huge script costs nothing per frame. Client-side, zero queries.
pub(crate) fn statement_start_rows(text: &str, max_bytes: usize) -> Vec<(usize, usize)> {
    if text.len() > max_bytes {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mask = code_mask(&chars);
    let ranges = statement_ranges(text);
    // Line start offsets (char indices) so an offset maps to a row in O(log n).
    let mut line_starts = vec![0usize];
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            line_starts.push(i + 1);
        }
    }
    let row_of =
        |off: usize| -> usize { line_starts.partition_point(|&s| s <= off).saturating_sub(1) };
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, (s, e)) in ranges.iter().enumerate() {
        let at = statement_code_start(&chars, &mask, *s, *e);
        let row = row_of(at);
        if out.last().is_some_and(|&(r, _)| r == row) {
            continue;
        }
        out.push((row, i + 1));
    }
    out
}

/// R88: the width of the editor's statement-ordinal gutter for the last frame.
/// Zero when the toggle is off, when the buffer has fewer than two statements,
/// or when it is above the size cap (a huge script must not pay a lex per
/// frame). The width is `digits + "." + one space`; purely presentational, so
/// the SQL text itself never changes.
pub(crate) fn statement_gutter_cols(app: &App) -> u16 {
    if !app.stmt_gutter {
        return 0;
    }
    let text = app.editor_sql();
    let rows = statement_start_rows(&text, STMT_DIM_MAX_BYTES);
    if rows.len() < 2 {
        return 0;
    }
    (rows.len().to_string().len() + 2) as u16
}

/// R88: `F2` — toggle the editor's statement-ordinal gutter. A pure render
/// layer (the buffer is never touched); the choice persists in `tui.json` and
/// is applied on the next launch. Default off (a visual preference).
pub(crate) fn toggle_stmt_gutter(app: &mut App) {
    app.stmt_gutter = !app.stmt_gutter;
    app.config.set_stmt_gutter(app.stmt_gutter);
    app.persist();
    app.flash(if app.stmt_gutter {
        t("语句序号 开（F2 关闭）").to_string()
    } else {
        t("语句序号 关（F2 开启）").to_string()
    });
}

/// Drop every located execution error (new run, edit, backend switch).
pub(crate) fn clear_editor_errors(app: &mut App) {
    app.editor_error_spans.clear();
    app.editor_error_idx = 0;
    app.editor_error_snapshot.clear();
    app.editor_error_base.clear();
    // R88: a located-error reset also drops any half-consumed run scope.
    app.pending_scope = None;
}

/// Drop the located errors once the buffer changed under them — the char offsets
/// would no longer address the same text. Called after every edit path, exactly
/// like [`sync_editor_find`].
pub(crate) fn sync_editor_errors(app: &mut App) {
    if app.editor_error_spans.is_empty() {
        return;
    }
    if app.editor.lines() != app.editor_error_snapshot.as_slice() {
        clear_editor_errors(app);
    }
}

/// Locate the failing statements of the last editor run, highlight them and park
/// the caret on the first. `statements` is what the engine actually ran (the
/// core's dialect-aware split); `errors` is `(statement index, message)` per
/// failure; `base` is the status line that carried the error, kept so a jump can
/// append the ordinal without losing the message. Returns whether anything was
/// located. Text-only, zero queries.
pub(crate) fn record_editor_errors(
    app: &mut App,
    statements: &[String],
    errors: &[(usize, String)],
    base: &str,
) -> bool {
    if errors.is_empty() {
        clear_editor_errors(app);
        return false;
    }
    let text = app.editor_sql();
    let Some(exec) = app.last_executed.clone() else {
        clear_editor_errors(app);
        return false;
    };
    // R88: a scoped run (selection / current statement) already knows the
    // buffer spans it sent; a whole-buffer run re-derives them from the text.
    // Either way the editor must still hold what ran (a buffer edited while the
    // query was in flight would make every stored offset stale).
    let scope = app.pending_scope.take();
    let mut located: Vec<Option<(usize, usize)>>;
    let ordinals: Vec<usize>;
    match &scope {
        Some(sr) => {
            located = sr.spans.clone();
            ordinals = sr.ordinals.clone();
            let chars: Vec<char> = text.chars().collect();
            for (i, slot) in located.iter_mut().enumerate() {
                if let Some((s, e)) = *slot {
                    if e > chars.len() {
                        *slot = None;
                        continue;
                    }
                    let cur: String = chars[s..e].iter().collect::<String>().trim().to_string();
                    if statements.get(i).map(|t| t.trim()) != Some(cur.as_str()) {
                        *slot = None;
                    }
                }
            }
        }
        None => {
            // Only locate when the editor still holds exactly what ran.
            if text.trim() != exec {
                clear_editor_errors(app);
                return false;
            }
            located = locate_statement_spans(&text, statements);
            ordinals = (1..=statements.len()).collect();
        }
    }
    let mut spans: Vec<EditorErrorSpan> = Vec::new();
    for (i, msg) in errors {
        let err_line = extract_error_line(msg);
        let primary = located.get(*i).copied().flatten();
        // Primary: match the statement the engine ran back to the buffer. For a
        // whole-buffer run, fall back to the line the driver named, offset by
        // where statement `i` begins in the script. A scoped run skips that
        // fallback: its statements are not the whole buffer, so a buffer-relative
        // line number would point at the wrong text.
        let range = if scope.is_some() {
            primary
        } else {
            primary.or_else(|| {
                err_line.and_then(|l| locate_statement_at_line(&text, statements, *i, l))
            })
        };
        if let Some((s, e)) = range {
            spans.push(EditorErrorSpan {
                start: s,
                end: e,
                ordinal: ordinals.get(*i).copied().unwrap_or(i + 1),
                err_line,
            });
        }
    }
    if spans.is_empty() {
        clear_editor_errors(app);
        return false;
    }
    spans.sort_by_key(|s| s.start);
    app.editor_error_spans = spans;
    app.editor_error_idx = 0;
    app.editor_error_snapshot = app.editor.lines().to_vec();
    app.editor_error_base = base.to_string();
    jump_editor_error(app);
    true
}

/// Park the caret on the first real token of the currently selected failing
/// statement (a leading comment block is stepped over).
pub(crate) fn jump_editor_error(app: &mut App) {
    let Some(span) = app.editor_error_spans.get(app.editor_error_idx).cloned() else {
        return;
    };
    let text = app.editor_sql();
    let chars: Vec<char> = text.chars().collect();
    let mask = code_mask(&chars);
    let at = statement_code_start(&chars, &mask, span.start, span.end);
    let (r, c) = offset_to_cursor(&text, at);
    app.editor.move_cursor(CursorMove::Jump(r as u16, c as u16));
}

/// Step to the next (`dir > 0`) / previous failing statement, wrapping, focus the
/// editor and refresh the status. `false` when nothing is located.
pub(crate) fn cycle_editor_error(app: &mut App, dir: i32) -> bool {
    let n = app.editor_error_spans.len();
    if n == 0 {
        return false;
    }
    if dir > 0 {
        app.editor_error_idx = (app.editor_error_idx + 1) % n;
    } else {
        app.editor_error_idx = (app.editor_error_idx + n - 1) % n;
    }
    app.focus = Focus::Editor;
    jump_editor_error(app);
    apply_editor_error_status(app);
    true
}

/// The status line for the located errors: the original error message followed
/// by the failing statement's ordinal (bilingual).
pub(crate) fn editor_error_status(app: &App) -> Option<String> {
    let span = app.editor_error_spans.get(app.editor_error_idx)?;
    let total = app.editor_error_spans.len();
    let loc = if total > 1 {
        tf(
            "第 {} 条语句（{}/{}）",
            &[&span.ordinal, &(app.editor_error_idx + 1), &(total)],
        )
    } else {
        tf("第 {} 条语句", &[&span.ordinal])
    };
    let base = app.editor_error_base.trim_end();
    Some(if base.is_empty() {
        loc
    } else {
        format!("{base} · {loc}")
    })
}

/// Overwrite the status line with [`editor_error_status`].
pub(crate) fn apply_editor_error_status(app: &mut App) {
    if let Some(s) = editor_error_status(app) {
        app.status = s;
    }
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
    // R93: any key other than the ring paste resets the cycle, so the next
    // Ctrl-Shift-V starts from the newest entry.
    if !is_clip_ring_key(&k) {
        app.editor_clip_idx = None;
    }
    editor_key_inner(app, tx, k);
    // R93: a copy / cut / kill fills the yank buffer — remember it in the ring.
    record_editor_yank(app);
    // R61: any edit invalidates the find highlight (matches moved); `Esc` keeps
    // it until exactly this moment.
    sync_editor_find(app);
    // R77: the located execution-error highlight goes with it.
    sync_editor_errors(app);
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
        // R93: Ctrl-/ toggles `-- ` line comments over the selection (or the
        // caret's line). Terminals disagree on what Ctrl-/ sends: the extended
        // keyboard protocol reports `Char('/')`, while a legacy terminal sends
        // 0x1F, which crossterm spells `Ctrl-_` or `Ctrl-7` — all three are
        // accepted so the muscle memory works everywhere. `Alt-C` is the
        // documented fallback (handled in the global layer, where it is
        // context-sensitive with the compact-columns toggle).
        (m, KeyCode::Char('/')) if m.contains(KeyModifiers::CONTROL) => toggle_comment(app),
        (m, KeyCode::Char('_')) if m.contains(KeyModifiers::CONTROL) => toggle_comment(app),
        (m, KeyCode::Char('7')) if m.contains(KeyModifiers::CONTROL) => toggle_comment(app),
        // R93: Ctrl-Shift-V pastes from the editor clipboard ring (the last few
        // copy / cut / kill payloads), cycling on repeat.
        (m, KeyCode::Char('V')) if m.contains(KeyModifiers::CONTROL) => editor_clip_paste_step(app),
        (m, KeyCode::Char('v'))
            if m.contains(KeyModifiers::CONTROL) && m.contains(KeyModifiers::SHIFT) =>
        {
            editor_clip_paste_step(app)
        }
        // R88: Ctrl-J runs the selection when there is one, otherwise the
        // statement under the cursor; F5 still runs the whole editor.
        (m, KeyCode::Char('j')) if m.contains(KeyModifiers::CONTROL) => {
            run_current_scoped(app, tx, RunScope::CurrentStatement)
        }
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
        // R79: `Enter` auto-indents the new line. The rule is the pure
        // `auto_indent`: a blank previous line inherits nothing, a block-opening
        // one (SELECT/FROM/WHERE/AND/OR/JOIN/ON/SET/VALUES / `(` `[` `{` `,` /
        // an unclosed bracket) adds two spaces, and every other line passes its
        // own indent down. A selection replaces normally, so this only fires on
        // a bare Enter. Off when `editor_indent` is disabled in `tui.json`.
        (KeyModifiers::NONE, KeyCode::Enter) if app.editor_indent && !app.editor.is_selecting() => {
            let lines = app.editor.lines().to_vec();
            let (row, _) = app.editor.cursor();
            let indent = auto_indent(&lines, row);
            app.editor.insert_newline();
            if !indent.is_empty() {
                app.editor.insert_str(indent);
            }
            app.editor_vp.note_key(&k);
        }
        // R79: `(` / `[` auto-close and leave the caret between the pair. The
        // lexer decides: inside a string literal, quoted identifier or comment
        // the bracket is typed literally. A selection replaces normally.
        (KeyModifiers::NONE, KeyCode::Char(c @ ('(' | '[')))
            if app.editor_pairs && !app.editor.is_selecting() =>
        {
            let text = app.editor_sql();
            let (row, col) = app.editor.cursor();
            let off = text_offset(&text, row, col).unwrap_or_else(|| text.chars().count());
            match pair_action(&text, off, c) {
                PairAction::Pair(close) => {
                    app.editor.insert_char(c);
                    app.editor.insert_char(close);
                    app.editor.move_cursor(CursorMove::Back);
                    app.editor_vp.note_key(&k);
                }
                _ => {
                    app.editor.input(k);
                }
            }
        }
        // R79: `)` / `]` step over an identical closer already sitting after
        // the caret instead of typing a second one (again, code only).
        (KeyModifiers::NONE, KeyCode::Char(c @ (')' | ']')))
            if app.editor_pairs && !app.editor.is_selecting() =>
        {
            let text = app.editor_sql();
            let (row, col) = app.editor.cursor();
            let off = text_offset(&text, row, col).unwrap_or_else(|| text.chars().count());
            match pair_action(&text, off, c) {
                PairAction::Skip => app.editor.move_cursor(CursorMove::Forward),
                _ => {
                    app.editor.input(k);
                }
            }
        }
        // Readline-style line editing rides tui-textarea's built-in bindings,
        // which reach `_` below: Ctrl-A / Home = line head, Ctrl-E / End = line
        // tail, Ctrl-K (and Ctrl-Shift-K) = kill to end of line, Ctrl-W = delete
        // the previous word. `browse_key` deliberately does not claim those
        // combos while the editor is focused, so the muscle memory survives.
        (KeyModifiers::NONE, KeyCode::F(5)) => run_current(app, tx),
        // R88: F2 toggles the optional statement-ordinal gutter (render-only).
        (KeyModifiers::NONE, KeyCode::F(2)) => toggle_stmt_gutter(app),
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
