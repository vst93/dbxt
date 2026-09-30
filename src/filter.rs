use crate::prelude::*;
use crate::*;

/// Recompute the visible table list from `tables_all` + `table_filter`, keeping
/// the previously selected table selected when it still matches.
pub(crate) fn apply_table_filter(app: &mut App) {
    let prev = app.selected_table().map(|t| t.name.clone());
    let needle = app.table_filter.trim().to_lowercase();
    // Match the qualified `schema.table` the sidebar draws, so `/inv` finds
    // every table in the `inv` schema.
    let schema = app.schema.clone();
    let mut list = app.tables_all.clone();
    sort_table_list(&mut list, app.table_sort);
    app.tables = if needle.is_empty() {
        list
    } else {
        list.into_iter()
            .filter(|t| {
                qualified_display(&schema, &t.name)
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect()
    };
    let n = app.tables.len();
    if n == 0 {
        app.table_list.select(None);
        rebuild_side_rows(app);
        return;
    }
    let sel = prev
        .and_then(|p| app.tables.iter().position(|t| t.name == p))
        .unwrap_or(0)
        .min(n - 1);
    app.table_list.select(Some(sel));
    rebuild_side_rows(app);
}

// ── Redis key list: client-side type-to-filter + first-letter jump (R42) ──

/// Recompute the visible Redis key list (`redis_scan.keys`) from the full loaded
/// window (`redis_scan.all`) and the active `redis_filter`, keeping the
/// previously selected key when it still matches. This is the KV twin of
/// [`apply_table_filter`].
pub(crate) fn apply_redis_filter(app: &mut App) {
    let prev = app
        .redis_list
        .selected()
        .and_then(|i| app.redis_scan.keys.get(i))
        .map(|k| k.key_raw.clone());
    let needle = app.redis_filter.trim().to_lowercase();
    let type_filter = app.redis_type_filter.clone();
    let mut list: Vec<RedisKeyInfo> = app
        .redis_scan
        .all
        .iter()
        .filter(|k| {
            type_filter
                .as_deref()
                .is_none_or(|ty| k.key_type.eq_ignore_ascii_case(ty))
        })
        .filter(|k| {
            needle.is_empty()
                || fix_double_encoding(&k.key_display)
                    .to_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect();
    redis_sort_keys(&mut list, app.redis_sort);
    app.redis_scan.keys = list;
    let n = app.redis_scan.keys.len();
    if n == 0 {
        app.redis_list.select(None);
        return;
    }
    let sel = prev
        .and_then(|p| app.redis_scan.keys.iter().position(|k| k.key_raw == p))
        .unwrap_or(0)
        .min(n - 1);
    app.redis_list.select(Some(sel));
}

/// R81: re-sort the loaded key window in place. `Scan` is a no-op (arrival
/// order); the TTL modes order by remaining seconds, with persistent (`-1`) and
/// missing (`-2`) keys ranked last (ascending) / first (descending). The sort is
/// stable, so equal TTLs keep their SCAN order.
pub(crate) fn redis_sort_keys(keys: &mut [RedisKeyInfo], sort: RedisSort) {
    fn ttl_rank(ttl: i64) -> i64 {
        if ttl < 0 {
            i64::MAX
        } else {
            ttl
        }
    }
    match sort {
        RedisSort::Scan => {}
        RedisSort::TtlAsc => keys.sort_by_key(|k| ttl_rank(k.ttl)),
        RedisSort::TtlDesc => keys.sort_by_key(|k| std::cmp::Reverse(ttl_rank(k.ttl))),
    }
}

/// R81: `Ctrl-T` in the key browser cycles the loaded-key ordering
/// `扫描顺序 → TTL 升序 → TTL 降序 → 扫描顺序`. Pure client-side re-sorting; the
/// SCAN cursor and the loaded window are untouched.
pub(crate) fn cycle_redis_sort(app: &mut App) {
    app.redis_sort = app.redis_sort.next();
    apply_redis_filter(app);
    app.status = tf(
        "key 排序：{} · {} 个 key · Ctrl-T 循环",
        &[&(app.redis_sort.label()), &(app.redis_scan.keys.len())],
    );
}

/// R81: `t` in the key browser cycles the client-side type filter
/// (`全部 → string → hash → list → set → zset → stream → 全部`) over the loaded
/// keys. Zero queries — the type came from the SCAN page itself.
pub(crate) fn cycle_redis_type_filter(app: &mut App) {
    let cur = app.redis_type_filter.as_deref().unwrap_or("");
    let i = REDIS_TYPE_FILTERS
        .iter()
        .position(|f| *f == cur)
        .unwrap_or(0);
    let next = REDIS_TYPE_FILTERS[(i + 1) % REDIS_TYPE_FILTERS.len()];
    app.redis_type_filter = (!next.is_empty()).then(|| next.to_string());
    apply_redis_filter(app);
    app.status = match &app.redis_type_filter {
        Some(ty) => tf(
            "类型过滤 {} · {} 个 key · t 循环",
            &[&(ty.as_str()), &(app.redis_scan.keys.len())],
        ),
        None => tf("类型过滤 全部 · {} 个 key", &[&(app.redis_scan.keys.len())]),
    };
}

/// Open the Redis key filter, optionally seeded with the character that started
/// it (R42 one-step type-to-filter).
pub(crate) fn open_redis_filter_with(app: &mut App, seed: Option<char>) {
    if app.redis_scan.all.is_empty() {
        app.status = t("还没有 key 可过滤").into();
        return;
    }
    let mut text = app.redis_filter.clone();
    if let Some(c) = seed {
        text.push(c);
    }
    let mut ta = TextArea::from([text.clone()]);
    ta.move_cursor(CursorMove::End);
    app.redis_filter_prompt = Some(ta);
    app.redis_filter = text;
    apply_redis_filter(app);
}

/// Open the Redis key filter with the current needle (the `f` binding).
pub(crate) fn open_redis_filter(app: &mut App) {
    if app.redis_scan.all.is_empty() {
        app.status = t("还没有 key 可过滤").into();
        return;
    }
    let mut ta = TextArea::from([app.redis_filter.clone()]);
    ta.move_cursor(CursorMove::End);
    app.redis_filter_prompt = Some(ta);
}

/// Clear the Redis key filter and its prompt in one gesture.
pub(crate) fn clear_redis_filter(app: &mut App) {
    app.redis_filter_prompt = None;
    app.redis_filter.clear();
    apply_redis_filter(app);
}

/// Keys while the Redis key filter prompt owns the keyboard: every keystroke
/// refilters live; Enter keeps the filter and opens the first hit; Esc clears.
pub(crate) fn redis_filter_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('u'))
        || (k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Backspace)
    {
        clear_redis_filter(app);
        app.status = tf(
            "已清除 key 过滤 · {} 个 key",
            &[&(app.redis_scan.all.len())],
        );
        return;
    }
    match k.code {
        KeyCode::Enter => {
            app.redis_filter = app
                .redis_filter_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.redis_filter_prompt = None;
            apply_redis_filter(app);
            let (n, total) = (app.redis_scan.keys.len(), app.redis_scan.all.len());
            if n == 0 {
                app.status = tf("过滤「{}」· 0 个 key 命中", &[&app.redis_filter]);
                return;
            }
            app.redis_list.select(Some(0));
            app.status = if app.redis_filter.is_empty() {
                tf("{} 个 key", &[&(total)])
            } else {
                tf(
                    "过滤「{}」· 查看第 1 个命中 · Esc 清除",
                    &[&(app.redis_filter)],
                )
            };
            open_redis_value(app, tx);
        }
        KeyCode::Esc => {
            clear_redis_filter(app);
            app.flash(tf(
                "已清除 key 过滤 · {} 个 key",
                &[&(app.redis_scan.all.len())],
            ));
        }
        _ => {
            if let Some(t) = app.redis_filter_prompt.as_mut() {
                t.input(k);
            }
            app.redis_filter = app
                .redis_filter_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            apply_redis_filter(app);
            app.status = tf(
                "过滤「{}」· {} 个命中",
                &[&(app.redis_filter), &(app.redis_scan.keys.len())],
            );
        }
    }
}

/// R42: cycle to the next loaded Redis key whose name starts with `letter`
/// (case-insensitive, wrapping). Mirrors [`table_jump_by_letter`].
pub(crate) fn redis_jump_by_letter(app: &mut App, letter: char, dir: i32) -> Option<usize> {
    if app.redis_scan.keys.is_empty() {
        return None;
    }
    let n = app.redis_scan.keys.len();
    let cur = app.redis_list.selected().unwrap_or(0);
    let lower = letter.to_ascii_lowercase();
    let matches = |i: usize| {
        app.redis_scan.keys[i]
            .key_display
            .chars()
            .next()
            .is_some_and(|c| c.to_ascii_lowercase() == lower)
    };
    let step = if dir >= 0 { 1 } else { n - 1 };
    for k in 1..=n {
        let i = (cur + k * step) % n;
        if matches(i) {
            app.redis_list.select(Some(i));
            app.redis_jump_letter = Some(lower);
            return Some(i);
        }
    }
    None
}

/// `;` / `,` in the Redis sidebar: repeat the last first-letter jump.
pub(crate) fn repeat_redis_jump(app: &mut App, dir: i32) {
    let Some(letter) = app.redis_jump_letter else {
        app.status = t("先用 Alt+字母 做首字母跳，再用 ; , 循环").into();
        return;
    };
    match redis_jump_by_letter(app, letter, dir) {
        Some(i) => {
            let name = fix_double_encoding(&app.redis_scan.keys[i].key_display);
            app.status = tf("首字母跳「{}」→ {}", &[&letter, &name]);
        }
        None => app.status = tf("没有以「{}」开头的 key", &[&letter]),
    }
}

/// R39: cycle to the next table whose name starts with `letter` (case-
/// insensitive), wrapping around. `dir` is +1 for the forward cycle (`g`-less
/// first-letter press / `;`) and -1 for backward (`,`). The unqualified name is
/// matched, so `inv.items` jumps on `i` not on the `inv` schema prefix. Returns
/// the new index, or `None` when no table starts with that letter.
pub(crate) fn table_jump_by_letter(app: &mut App, letter: char, dir: i32) -> Option<usize> {
    if app.tables.is_empty() {
        return None;
    }
    let n = app.tables.len();
    let cur = app.table_list.selected().unwrap_or(0);
    let lower = letter.to_ascii_lowercase();
    let matches = |i: usize| {
        app.tables[i]
            .name
            .chars()
            .next()
            .is_some_and(|c| c.to_ascii_lowercase() == lower)
    };
    let step = if dir >= 0 { 1 } else { n - 1 };
    for k in 1..=n {
        let i = (cur + k * step) % n;
        if matches(i) {
            app.table_list.select(Some(i));
            app.table_jump_letter = Some(lower);
            return Some(i);
        }
    }
    None
}

/// Enter the sidebar table filter directly with `seed` pre-typed (R39 one-step
/// type-to-filter). `/` still opens an empty filter for refinement.
pub(crate) fn open_table_filter_with(app: &mut App, seed: Option<char>) {
    if app.tables_all.is_empty() {
        app.status = t("还没有表可过滤").into();
        return;
    }
    let mut text = app.table_filter.clone();
    if let Some(c) = seed {
        text.push(c);
    }
    let mut ta = TextArea::from([text.clone()]);
    ta.move_cursor(CursorMove::End);
    app.table_prompt = Some(ta);
    app.table_filter = text;
    apply_table_filter(app);
}

/// Clear the sidebar table filter and the one-step prompt in one gesture.
pub(crate) fn clear_table_filter(app: &mut App) {
    app.table_prompt = None;
    app.table_filter.clear();
    apply_table_filter(app);
}

/// Split `text` into `(pre, match, post)` spans, underlining the first
/// case-insensitive occurrence of `needle` (R39 filter-hit highlight). Uses
/// ASCII-only case folding so byte offsets stay valid on UTF-8 table names; the
/// needle is already lower-cased by the caller.
pub(crate) fn highlight_match_spans(
    text: &str,
    needle_lower: &str,
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    if needle_lower.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let hay = text.to_ascii_lowercase();
    let Some(pos) = hay.find(needle_lower) else {
        return vec![Span::styled(text.to_string(), base)];
    };
    let end = pos + needle_lower.len();
    let mut out = Vec::with_capacity(3);
    if pos > 0 {
        out.push(Span::styled(text[..pos].to_string(), base));
    }
    out.push(Span::styled(text[pos..end].to_string(), hit));
    if end < text.len() {
        out.push(Span::styled(text[end..].to_string(), base));
    }
    out
}

/// Make `name` the selectable sidebar table and return its index in
/// `app.tables`. An active `/` name filter that hides the table is cleared
/// first: a global-search hit (`Alt-G`) or a recent-table jump is an explicit
/// request for that table, so a leftover filter must not turn it into a bogus
/// "table not found".
pub(crate) fn focus_table_in_sidebar(app: &mut App, name: &str) -> Option<usize> {
    if !app.tables.iter().any(|t| t.name == name) && app.tables_all.iter().any(|t| t.name == name) {
        app.table_filter.clear();
        app.table_prompt = None;
        apply_table_filter(app);
    }
    app.tables.iter().position(|t| t.name == name)
}

pub(crate) fn open_table_filter(app: &mut App) {
    if app.tables_all.is_empty() {
        app.status = t("还没有表可过滤").into();
        return;
    }
    let mut ta = TextArea::from([app.table_filter.clone()]);
    ta.move_cursor(CursorMove::End);
    app.table_prompt = Some(ta);
}

pub(crate) fn table_filter_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // Ctrl-U / Alt-Backspace clear the filter from inside the prompt (grep /
    // less muscle memory); both are free while the sidebar filter owns the keys.
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('u'))
        || (k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Backspace)
    {
        clear_table_filter(app);
        app.status = tf("已清除表过滤 · {} 个表/视图", &[&(app.tables.len())]);
        return;
    }
    match k.code {
        // Enter goes straight to the first hit: type a few letters, Enter, and
        // you are browsing. The filter stays active so Esc returns to the
        // filtered sidebar rather than a 500-row list.
        KeyCode::Enter => {
            app.table_filter = app
                .table_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.table_prompt = None;
            apply_table_filter(app);
            let (n, total) = (app.tables.len(), app.tables_all.len());
            if n == 0 {
                app.status = tf("过滤「{}」· 0 个表命中", &[&app.table_filter]);
                return;
            }
            app.table_list.select(Some(0));
            app.status = if app.table_filter.is_empty() {
                tf("{} 个表/视图", &[&(total)])
            } else {
                tf(
                    "过滤「{}」· 打开第 1 个命中 · Esc 清除",
                    &[&(app.table_filter)],
                )
            };
            open_table_data(app, tx);
        }
        KeyCode::Esc => {
            clear_table_filter(app);
            app.flash(tf("已清除表过滤 · {} 个表/视图", &[&(app.tables.len())]));
        }
        _ => {
            if let Some(t) = app.table_prompt.as_mut() {
                t.input(k);
            }
            app.table_filter = app
                .table_prompt
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            apply_table_filter(app);
        }
    }
}

// ── SQL prefix completion (Alt-/) ──

/// Keywords offered alongside table / column names. Small on purpose: a TUI
/// completion is a shortcut for long identifiers, not a SQL parser.
pub(crate) const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "SET",
    "VALUES",
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS NULL",
    "LIKE",
    "IN",
    "BETWEEN",
    "DISTINCT",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "UNION",
    "UNION ALL",
    "EXPLAIN",
    "WITH",
];

/// The identifier fragment ending at the cursor, and how many characters it is.
pub(crate) fn word_before_cursor(ta: &TextArea) -> (usize, String) {
    let (row, col) = ta.cursor();
    let line = ta.lines().get(row).cloned().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let end = col.min(chars.len());
    let mut start = end;
    while start > 0 {
        let c = chars[start - 1];
        if c.is_alphanumeric() || c == '_' || c == '.' {
            start -= 1;
        } else {
            break;
        }
    }
    (end - start, chars[start..end].iter().collect())
}

/// Everything on the editor's lines up to (not including) the cursor, joined by
/// newlines. Used to look at the keyword that precedes the fragment.
pub(crate) fn text_before_cursor(ta: &TextArea) -> String {
    let (row, col) = ta.cursor();
    let lines = ta.lines();
    let mut out = String::new();
    for l in lines.iter().take(row) {
        out.push_str(l);
        out.push('\n');
    }
    if let Some(line) = lines.get(row) {
        out.extend(line.chars().take(col));
    }
    out
}

/// Read the (possibly quoted, possibly `schema.`) identifier that precedes the
/// final `.` at the cursor — the qualifier of a `qualifier.partial` form.
/// Returns `None` when the cursor is not after such a form.
pub(crate) fn qualifier_before_cursor(ta: &TextArea) -> Option<String> {
    let (row, col) = ta.cursor();
    let line = ta.lines().get(row).cloned().unwrap_or_default();
    let chars: Vec<char> = line.chars().collect();
    let mut i = col.min(chars.len());
    // Step back over the fragment being completed to the dot.
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        i -= 1;
    }
    if i == 0 || chars[i - 1] != '.' {
        return None;
    }
    i -= 1; // the dot
    let end = i;
    if end == 0 {
        return None;
    }
    // A quoted qualifier (`` `t` ``, `"t"`, `[t]`) is read back to its opener.
    let close = chars[end - 1];
    let open = match close {
        '`' => Some('`'),
        '"' => Some('"'),
        ']' => Some('['),
        _ => None,
    };
    if let Some(open) = open {
        let mut j = end - 1;
        while j > 0 {
            j -= 1;
            if chars[j] == open {
                return Some(chars[j + 1..end - 1].iter().collect());
            }
        }
        return None;
    }
    // A bare identifier, keeping only the last dot-separated segment.
    let mut start = end;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// Work out what the cursor is completing: the context and the fragment that
/// would be replaced (after the last `.` for a qualified name).
pub(crate) fn completion_context(ta: &TextArea) -> (CompCtx, String) {
    let (n, word) = word_before_cursor(ta);
    if let Some(dot) = word.rfind('.') {
        let partial = word[dot + 1..].to_string();
        if let Some(qual) = qualifier_before_cursor(ta) {
            return (CompCtx::Qualified(qual), partial);
        }
        return (CompCtx::Any, partial);
    }
    // Drop the fragment being completed before looking at the preceding keyword.
    let before = text_before_cursor(ta);
    let keep = before.chars().count().saturating_sub(n);
    let before: String = before.chars().take(keep).collect();
    let head = before.trim_end();
    let last = head
        .split(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ';')
        .rfind(|s| !s.is_empty())
        .unwrap_or("")
        .to_ascii_uppercase();
    let ctx = match last.as_str() {
        "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE" => CompCtx::TableList,
        "WHERE" | "ON" | "SET" | "BY" | "HAVING" | "SELECT" | "AND" | "OR" => CompCtx::Column,
        _ if head.ends_with('(') => CompCtx::Column,
        _ => CompCtx::Any,
    };
    (ctx, word)
}

/// Column names known for the connection: the browsed table's metadata first,
/// then whatever columns the current result grid carries.
pub(crate) fn column_names(app: &App) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(meta) = &app.table_meta {
        for c in &meta.columns {
            if !out.contains(&c.name) {
                out.push(c.name.clone());
            }
        }
    }
    if let Some(grid) = full_grid(app) {
        for c in &grid.columns {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
    }
    out
}

/// SQL reserved words that must be quoted when they appear as an identifier —
/// an identifier named `order` / `user` / `key` is otherwise a syntax error or a
/// different object. A union of the PostgreSQL reserved keywords and the
/// MySQL-only reserved words, kept small on purpose (this is a completion
/// convenience, not a SQL parser).
pub(crate) const SQL_RESERVED_WORDS: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "binary",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
    // MySQL-only reserved words (a superset would be noise; these are the ones
    // that commonly collide with a real column name).
    "accessible",
    "auto_increment",
    "change",
    "database",
    "databases",
    "describe",
    "div",
    "dual",
    "explain",
    "force",
    "fulltext",
    "ignore",
    "index",
    "key",
    "keys",
    "kill",
    "lines",
    "load",
    "lock",
    "mod",
    "optimize",
    "outfile",
    "purge",
    "range",
    "regexp",
    "rename",
    "replace",
    "require",
    "rlike",
    "schema",
    "schemas",
    "show",
    "spatial",
    "ssl",
    "starting",
    "terminated",
    "tinyint",
    "unlock",
    "unsigned",
    "use",
    "values",
    "varbinary",
    "varchar",
    "write",
    "xor",
    "zerofill",
];

/// True when `name` must be quoted to be a valid identifier: it is not a plain
/// all-lowercase word (`_` / `a-z` / `0-9`) or it is a reserved word. A plain
/// lowercase name like `users` is left bare, so completion stays readable.
pub(crate) fn identifier_needs_quote(name: &str) -> bool {
    let mut chars = name.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_lowercase())
        && chars.all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit());
    !plain || SQL_RESERVED_WORDS.contains(&name)
}

/// R54: a completed identifier, quoted for the connection's dialect only when it
/// needs it. MySQL-family gets backticks / SQL Server brackets / everything else
/// double quotes — reuse the kernel's own dialect quoting so the spelling always
/// matches the SQL builder.
pub(crate) fn completion_quote(name: &str, dt: Option<DatabaseType>) -> String {
    if identifier_needs_quote(name) {
        quote_table_identifier(dt, name)
    } else {
        name.to_string()
    }
}

pub(crate) fn push_item(
    out: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    raw: &str,
    text: &str,
    kind: char,
    needle: &str,
) {
    let lower = raw.to_lowercase();
    if !seen.insert(lower.clone()) {
        return;
    }
    if needle.is_empty() || lower.starts_with(needle) {
        out.push(CompletionItem {
            text: text.to_string(),
            kind,
        });
    }
}

pub(crate) fn push_names(
    out: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    names: &[String],
    kind: char,
    needle: &str,
    dt: Option<DatabaseType>,
) {
    for n in names {
        // Keywords go in verbatim; identifiers are quoted on demand, so a
        // `Users` / `order` name lands insert-ready (R54). The match still runs
        // against the bare name, so a lowercase prefix finds a quoted candidate.
        let text = if kind == 'K' {
            n.clone()
        } else {
            completion_quote(n, dt)
        };
        push_item(out, seen, n, &text, kind, needle);
    }
}

/// Candidate list for the fragment before the cursor, ordered by context:
/// `table.` → that table's columns only; after `FROM`/`JOIN` → tables first;
/// after `WHERE`/`ON` → columns first; otherwise columns → tables → keywords.
/// Matching is case-insensitive.
pub(crate) fn completion_candidates(
    app: &App,
    ctx: &CompCtx,
    partial: &str,
) -> Vec<CompletionItem> {
    let needle = partial.to_lowercase();
    let dt = app.selected.as_ref().map(|c| c.db_type);
    let mut out: Vec<CompletionItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let cols = column_names(app);
    let tables: Vec<String> = app.tables_all.iter().map(|t| t.name.clone()).collect();
    let keywords: Vec<String> = SQL_KEYWORDS.iter().map(|k| (*k).to_string()).collect();
    match ctx {
        CompCtx::Qualified(q) => {
            // Match the qualifier case-insensitively, so `USERS.` still offers
            // the columns of `users`.
            let qcols: Vec<String> = if let Some(meta) = app
                .table_meta
                .as_ref()
                .filter(|m| m.table.eq_ignore_ascii_case(q))
            {
                meta.columns.iter().map(|c| c.name.clone()).collect()
            } else if app
                .page_state
                .as_ref()
                .is_some_and(|p| p.table.eq_ignore_ascii_case(q))
            {
                full_grid(app).map(|g| g.columns).unwrap_or_default()
            } else {
                cols.clone()
            };
            push_names(&mut out, &mut seen, &qcols, 'C', &needle, dt);
        }
        CompCtx::TableList => {
            // R48: after FROM / JOIN / INTO the candidate list is tables only —
            // a column or a keyword there is almost always a typo, and the
            // narrower list is faster to scan on a phone.
            push_names(&mut out, &mut seen, &tables, 'T', &needle, dt);
        }
        CompCtx::Column => {
            // R48: after WHERE / ON / SET / SELECT (and after `(`) only columns
            // are offered.
            push_names(&mut out, &mut seen, &cols, 'C', &needle, dt);
        }
        CompCtx::Any => {
            push_names(&mut out, &mut seen, &cols, 'C', &needle, dt);
            push_names(&mut out, &mut seen, &tables, 'T', &needle, dt);
            push_names(&mut out, &mut seen, &keywords, 'K', &needle, dt);
        }
    }
    // R48: a narrow phone terminal keeps the popup short (one column, 5 items)
    // so it never covers the SQL being typed; a normal terminal shows 8.
    let cap = if app.term_w > 0 && app.term_w < 40 {
        5
    } else {
        8
    };
    out.truncate(cap);
    out
}

pub(crate) fn open_completion(app: &mut App) {
    let (ctx, partial) = completion_context(&app.editor);
    let items = completion_candidates(app, &ctx, &partial);
    if items.is_empty() {
        app.status = tf("无可补全项（前缀「{}」）", &[&(partial)]);
        return;
    }
    app.completion = Some(Completion {
        items,
        sel: 0,
        replace: partial.chars().count(),
    });
}

/// Recompute the candidate list after the user typed another character.
pub(crate) fn refresh_completion(app: &mut App) {
    let (ctx, partial) = completion_context(&app.editor);
    let items = completion_candidates(app, &ctx, &partial);
    if items.is_empty() {
        app.completion = None;
        return;
    }
    let sel = app
        .completion
        .as_ref()
        .map(|c| c.sel)
        .unwrap_or(0)
        .min(items.len() - 1);
    app.completion = Some(Completion {
        items,
        sel,
        replace: partial.chars().count(),
    });
}

pub(crate) fn accept_completion(app: &mut App) {
    let Some(c) = app.completion.clone() else {
        return;
    };
    let Some(item) = c.items.get(c.sel).cloned() else {
        app.completion = None;
        return;
    };
    let back = c.replace;
    if back > 0 {
        // `delete_str` deletes *forward* from the cursor, so step back to the
        // start of the fragment first.
        let (row, col) = app.editor.cursor();
        app.editor.move_cursor(CursorMove::Jump(
            row as u16,
            col.saturating_sub(back) as u16,
        ));
        app.editor.delete_str(back);
    }
    app.editor.insert_str(&item.text);
    app.completion = None;
}

pub(crate) fn completion_key(app: &mut App, k: KeyEvent) {
    let n = app.completion.as_ref().map(|c| c.items.len()).unwrap_or(0);
    match k.code {
        KeyCode::Esc => app.completion = None,
        KeyCode::Up => {
            if let Some(c) = app.completion.as_mut() {
                c.sel = c.sel.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(c) = app.completion.as_mut() {
                c.sel = (c.sel + 1).min(n.saturating_sub(1));
            }
        }
        KeyCode::Tab | KeyCode::Enter => accept_completion(app),
        _ => {
            app.editor.input(k);
            refresh_completion(app);
        }
    }
}
