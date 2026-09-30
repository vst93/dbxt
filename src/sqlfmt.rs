use crate::prelude::*;

// ─── SQL formatter (pure text state machine, no extra dependency) ────────────

/// One lexical piece of a SQL statement. Whitespace is dropped by the
/// tokenizer; a [`SqlTok::Word`] remembers whether it was *immediately* followed
/// by `(`, so a function call (`count(`) can be told from a keyword (`IN (`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SqlTok {
    Word {
        text: String,
        call: bool,
    },
    /// A string literal or quoted identifier, kept verbatim (quotes included).
    Quoted(String),
    /// A `-- …` or `/* … */` comment, kept verbatim.
    Comment(String),
    Punct(char),
}

/// Split SQL into words, quoted regions, comments and punctuation. String
/// literals (`'…'`), quoted identifiers (`` `…` `` / `"…"`) and both comment
/// forms are captured verbatim, so the formatter can never rewrite their
/// contents (a `FROM` inside a literal or a comment is not a clause).
pub(crate) fn sql_tokenize(sql: &str) -> Vec<SqlTok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // `-- …` line comment (up to, but not including, the newline).
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            out.push(SqlTok::Comment(chars[start..i].iter().collect()));
            continue;
        }
        // `/* … */` block comment.
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let start = i;
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            if i < chars.len() {
                i += 2;
            }
            out.push(SqlTok::Comment(chars[start..i].iter().collect()));
            continue;
        }
        // Quoted literal / identifier: consume through the matching quote,
        // honouring doubled quotes and backslash escapes inside a string.
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote == '\'' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    if chars.get(i + 1) == Some(&quote) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(SqlTok::Quoted(chars[start..i].iter().collect()));
            continue;
        }
        // Bare word (keyword / identifier / function name).
        if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let call = chars.get(i) == Some(&'(');
            out.push(SqlTok::Word {
                text: chars[start..i].iter().collect(),
                call,
            });
            continue;
        }
        out.push(SqlTok::Punct(c));
        i += 1;
    }
    out
}

/// Words the formatter upper-cases. Function names are exempted separately via
/// the tokenizer's `call` flag, so `count(` keeps whatever case the user typed.
pub(crate) const SQL_FORMAT_KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "ANY",
    "ARRAY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DO",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FETCH",
    "FILTER",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NATURAL",
    "NEXT",
    "NOT",
    "NOTHING",
    "NULL",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "RECURSIVE",
    "REFERENCES",
    "RESTRICT",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROWS",
    "SELECT",
    "SET",
    "SIMILAR",
    "SOME",
    "TABLE",
    "THEN",
    "TOP",
    "TRANSACTION",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

/// Break kind for a clause: `0` none, `1` top-level clause (column 0), `2` JOIN
/// (column 0), `3` sub-clause (indent 2).
pub(crate) const BRK_NONE: u8 = 0;
pub(crate) const BRK_TOP: u8 = 1;
pub(crate) const BRK_JOIN: u8 = 2;
pub(crate) const BRK_SUB: u8 = 3;

/// Multi-word clauses, matched before the single-word rules so `GROUP BY`,
/// `ORDER BY`, `INSERT INTO` and the JOIN family stay together on one line.
pub(crate) const SQL_PHRASES: &[(&[&str], u8)] = &[
    (&["GROUP", "BY"], BRK_TOP),
    (&["ORDER", "BY"], BRK_TOP),
    (&["UNION", "ALL"], BRK_TOP),
    (&["INSERT", "INTO"], BRK_TOP),
    (&["DELETE", "FROM"], BRK_TOP),
    (&["CREATE", "TABLE"], BRK_TOP),
    (&["CREATE", "VIEW"], BRK_TOP),
    (&["CREATE", "INDEX"], BRK_TOP),
    (&["ALTER", "TABLE"], BRK_TOP),
    (&["DROP", "TABLE"], BRK_TOP),
    (&["DROP", "VIEW"], BRK_TOP),
    (&["DROP", "INDEX"], BRK_TOP),
    (&["LEFT", "JOIN"], BRK_JOIN),
    (&["RIGHT", "JOIN"], BRK_JOIN),
    (&["INNER", "JOIN"], BRK_JOIN),
    (&["OUTER", "JOIN"], BRK_JOIN),
    (&["FULL", "JOIN"], BRK_JOIN),
    (&["CROSS", "JOIN"], BRK_JOIN),
    (&["NATURAL", "JOIN"], BRK_JOIN),
    (&["LEFT", "OUTER", "JOIN"], BRK_JOIN),
    (&["RIGHT", "OUTER", "JOIN"], BRK_JOIN),
    (&["FULL", "OUTER", "JOIN"], BRK_JOIN),
    (&["IS", "NOT"], BRK_NONE),
    (&["IS", "NULL"], BRK_NONE),
    (&["NOT", "NULL"], BRK_NONE),
    (&["NOT", "IN"], BRK_NONE),
    (&["NOT", "LIKE"], BRK_NONE),
    (&["PRIMARY", "KEY"], BRK_NONE),
    (&["FOREIGN", "KEY"], BRK_NONE),
];

pub(crate) fn sql_break_for_word(word_upper: &str) -> u8 {
    match word_upper {
        "SELECT" | "FROM" | "WHERE" | "HAVING" | "LIMIT" | "OFFSET" | "VALUES" | "SET"
        | "UPDATE" | "INSERT" | "DELETE" | "CREATE" | "ALTER" | "DROP" | "WITH" | "RETURNING"
        | "UNION" | "EXPLAIN" => BRK_TOP,
        "JOIN" => BRK_JOIN,
        "ON" | "AND" | "OR" | "WHEN" | "ELSE" | "END" => BRK_SUB,
        _ => BRK_NONE,
    }
}

/// Append one piece to a line, choosing whether a separating space is needed.
/// `prev_was_call` is true when the previous piece was a function name, so the
/// `(` sticks to it (`count(` rather than `count (`).
pub(crate) fn push_sql_piece(out: &mut String, piece: &str, prev_was_call: bool, indent: usize) {
    if out.is_empty() {
        out.push_str(&" ".repeat(indent));
        out.push_str(piece);
        return;
    }
    if out.ends_with('\n') {
        out.push_str(piece);
        return;
    }
    let prev = out.chars().last().unwrap_or(' ');
    let first = piece.chars().next().unwrap_or(' ');
    let no_space = matches!(prev, '(' | '.' | ':' | '[')
        || matches!(first, ',' | ')' | ';' | '.' | ':')
        || (piece == "(" && prev_was_call);
    if !no_space {
        out.push(' ');
    }
    out.push_str(piece);
}

pub(crate) fn sql_flush(lines: &mut Vec<String>, cur: &mut String) {
    let trimmed = cur.trim_end().to_string();
    if !trimmed.trim().is_empty() {
        lines.push(trimmed);
    }
    cur.clear();
}

/// Pretty-print a SQL statement: keywords upper-cased, main clauses on their own
/// line, JOIN on its own line, sub-clauses indented two spaces, runs of
/// whitespace collapsed. Literals, quoted identifiers and comments are copied
/// verbatim. The result is a fixed point (`format_sql(format_sql(x)) ==
/// format_sql(x)`).
pub(crate) fn format_sql(sql: &str) -> String {
    let toks = sql_tokenize(sql);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut indent = 0usize;
    let mut last_call = false;
    let mut i = 0usize;
    while i < toks.len() {
        // A multi-word clause wins over the single-word rule.
        let mut matched_phrase: Option<usize> = None;
        if matches!(toks[i], SqlTok::Word { .. }) {
            for (phrase, brk) in SQL_PHRASES {
                if i + phrase.len() <= toks.len()
                    && phrase.iter().enumerate().all(|(k, w)| match &toks[i + k] {
                        SqlTok::Word { text, .. } => text.eq_ignore_ascii_case(w),
                        _ => false,
                    })
                {
                    if *brk != BRK_NONE {
                        sql_flush(&mut lines, &mut cur);
                        indent = if *brk == BRK_SUB { 2 } else { 0 };
                    }
                    for w in phrase.iter() {
                        push_sql_piece(&mut cur, w, false, indent);
                    }
                    last_call = false;
                    matched_phrase = Some(phrase.len());
                    break;
                }
            }
        }
        if let Some(n) = matched_phrase {
            i += n;
            continue;
        }
        match &toks[i] {
            SqlTok::Word { text, call } => {
                let upper = text.to_ascii_uppercase();
                let brk = sql_break_for_word(&upper);
                if brk != BRK_NONE {
                    sql_flush(&mut lines, &mut cur);
                    indent = if brk == BRK_SUB { 2 } else { 0 };
                }
                // A known keyword keeps upper-casing even when it is immediately
                // followed by `(` (`IN(`, `VALUES(`); only a real *function* name
                // (a word that is not a keyword) keeps the user's case.
                let is_keyword = SQL_FORMAT_KEYWORDS.contains(&upper.as_str());
                let piece = if is_keyword {
                    upper.as_str()
                } else {
                    text.as_str()
                };
                push_sql_piece(&mut cur, piece, false, indent);
                last_call = *call;
                i += 1;
            }
            SqlTok::Quoted(q) => {
                push_sql_piece(&mut cur, q, false, indent);
                last_call = false;
                i += 1;
            }
            SqlTok::Comment(c) => {
                push_sql_piece(&mut cur, c, false, indent);
                last_call = false;
                // A `--` comment runs to end of line, so force a newline after it.
                if c.starts_with("--") {
                    sql_flush(&mut lines, &mut cur);
                }
                i += 1;
            }
            SqlTok::Punct(p) => {
                let mut buf = [0u8; 4];
                let s = p.encode_utf8(&mut buf);
                push_sql_piece(&mut cur, s, last_call, indent);
                last_call = false;
                if *p == ';' {
                    sql_flush(&mut lines, &mut cur);
                    indent = 0;
                }
                i += 1;
            }
        }
    }
    sql_flush(&mut lines, &mut cur);
    lines.join("\n")
}

/// Collapse a statement back to a single line (the inverse of [`format_sql`]).
/// A `--` line comment keeps its terminating newline so the rest of the
/// statement is never swallowed by the comment.
pub(crate) fn compress_sql(sql: &str) -> String {
    let toks = sql_tokenize(sql);
    let mut out = String::new();
    let mut last_call = false;
    for t in &toks {
        match t {
            SqlTok::Word { text, call } => {
                push_sql_piece(&mut out, text, last_call, 0);
                last_call = *call;
            }
            SqlTok::Quoted(q) => {
                push_sql_piece(&mut out, q, last_call, 0);
                last_call = false;
            }
            SqlTok::Comment(c) => {
                push_sql_piece(&mut out, c, last_call, 0);
                last_call = false;
                if c.starts_with("--") {
                    out.push('\n');
                }
            }
            SqlTok::Punct(p) => {
                let mut buf = [0u8; 4];
                let s = p.encode_utf8(&mut buf);
                push_sql_piece(&mut out, s, last_call, 0);
                last_call = false;
            }
        }
    }
    out.trim_end().to_string()
}

/// True when `sql` is already in canonical formatted form (so the next `Alt-F`
/// should compress it rather than format it again).
pub(crate) fn is_sql_formatted(sql: &str) -> bool {
    let trimmed = sql.trim();
    !trimmed.is_empty() && format_sql(trimmed) == trimmed
}

// ─── dangerous-statement detection ───────────────────────────────────────────

/// Strip SQL string literals and comments so keyword scans cannot be fooled by
/// `'where'` inside a literal or a commented-out clause.
pub(crate) fn strip_sql_noise(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' | '"' | '`' => {
                let quote = c;
                out.push(' ');
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' && quote != '`' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if i + 1 < chars.len() && chars[i + 1] == '-' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                out.push(' ');
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
                out.push(' ');
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

pub(crate) fn has_keyword(cleaned_lower: &str, keyword: &str) -> bool {
    cleaned_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == keyword)
}

/// Returns a human-readable reason when a statement is destructive enough to
/// deserve a confirmation prompt before it runs.
pub(crate) fn detect_danger(statement: &str) -> Option<String> {
    let cleaned = strip_sql_noise(statement);
    let lower = cleaned.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let first = lower
        .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .find(|w| !w.is_empty())
        .unwrap_or("");
    match first {
        "drop" => Some(t("DROP 会永久删除对象").to_string()),
        "truncate" => Some(t("TRUNCATE 会清空整张表且不可回滚").to_string()),
        // `ALTER TABLE … DROP COLUMN` / `DROP CONSTRAINT` destroy data or
        // constraints even though the leading keyword is ALTER.
        "alter" if has_keyword(&lower, "drop") => {
            Some(t("ALTER … DROP 会删除列 / 约束及其数据").to_string())
        }
        "update" | "delete" => {
            if !has_keyword(&lower, "where") {
                Some(tf(
                    "{} 没有 WHERE 子句，会作用于整张表",
                    &[&(first.to_ascii_uppercase())],
                ))
            } else {
                None
            }
        }
        // A common-table-expression statement can hide a destructive DELETE
        // (`WITH x AS (...) DELETE FROM t`), which has no leading DELETE keyword.
        "with" if has_keyword(&lower, "delete") && !has_keyword(&lower, "where") => {
            Some(t("DELETE 没有 WHERE 子句，会作用于整张表").to_string())
        }
        _ => None,
    }
}

// ─── read-only connection guard ──────────────────────────────────────────────

/// The first SQL word, lowercased, skipping leading whitespace, `;` and `(`.
/// Returns the word plus the byte offset just past it.
pub(crate) fn first_sql_word(sql: &str) -> (String, usize) {
    let trimmed = sql.trim_start_matches(|c: char| c.is_whitespace() || c == ';' || c == '(');
    let off = sql.len() - trimmed.len();
    let end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(trimmed.len());
    (trimmed[..end].to_ascii_lowercase(), off + end)
}

/// Unwrap `EXPLAIN [ANALYZE] [VERBOSE] [(opts)] …` to the statement it wraps, so
/// `EXPLAIN ANALYZE DELETE` is classified as the DELETE it really runs.
pub(crate) fn explain_inner(rest: &str) -> String {
    let mut rest = rest;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ';');
        // Skip a whole `(ANALYZE, FORMAT JSON, …)` option group in one step.
        if rest.starts_with('(') {
            let mut depth = 0i32;
            let mut end = None;
            for (i, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => {
                    rest = &rest[e..];
                    continue;
                }
                None => return String::new(),
            }
        }
        let (w, after) = first_sql_word(rest);
        if w.is_empty() {
            return String::new();
        }
        match w.as_str() {
            "analyze" | "analyse" | "verbose" => rest = &rest[after..],
            _ => return rest.to_string(),
        }
    }
}

/// The main verb of a `WITH …` statement: scan at paren depth 0 (CTE bodies are
/// inside parentheses, so their `SELECT` is skipped) and return the first
/// statement verb. `None` when nothing recognisable follows the CTE list.
pub(crate) fn with_main_verb(rest: &str) -> Option<String> {
    let mut rest = rest.trim_start();
    let (w, after) = first_sql_word(rest);
    if w == "recursive" {
        rest = &rest[after..];
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < chars.len() {
        let c = chars[i];
        if c == '(' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == ')' {
            depth -= 1;
            i += 1;
            continue;
        }
        if depth == 0 && (c.is_alphanumeric() || c == '_') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            match word.as_str() {
                "select" | "values" | "table" | "insert" | "update" | "delete" | "merge"
                | "replace" | "call" | "execute" => return Some(word),
                _ => {}
            }
            continue;
        }
        i += 1;
    }
    None
}

/// The statement's main verb, lowercased. `WITH …` is reduced to its top-level
/// verb and `EXPLAIN` unwrapped; an empty string means undetermined.
pub(crate) fn statement_main_verb(statement: &str) -> String {
    let cleaned = strip_sql_noise(statement);
    let lower = cleaned.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return String::new();
    }
    let (first, after) = first_sql_word(&lower);
    match first.as_str() {
        "explain" => statement_main_verb(&explain_inner(&lower[after..])),
        "with" => with_main_verb(&lower[after..]).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// A statement is read-only only when its verb is unambiguously a read. Every
/// other verb — DML, DDL, transaction control, session `SET`, an unrecognised
/// word — counts as a write, so a read-only connection fails closed.
pub(crate) fn statement_is_read_only(statement: &str) -> bool {
    matches!(
        statement_main_verb(statement).as_str(),
        "select" | "show" | "values" | "table" | "describe" | "desc"
    )
}

/// The first write verb in `sql` for a read-only connection, or `None` when the
/// whole batch is read-only. `?` marks a statement whose verb could not be
/// determined (which still counts as a violation).
pub(crate) fn readonly_violation(cfg: &ConnectionConfig, sql: &str) -> Option<String> {
    if !cfg.read_only {
        return None;
    }
    let statements = dbx_core::sql::split_sql_statements_for_database(sql, cfg.db_type);
    for st in &statements {
        if !statement_is_read_only(st) {
            let verb = statement_main_verb(st);
            return Some(if verb.is_empty() {
                "?".to_string()
            } else {
                verb.to_ascii_uppercase()
            });
        }
    }
    None
}

/// Hard-block a write on a read-only connection (zero extra queries: the check
/// is pure text). Returns true when the statement was refused.
pub(crate) fn readonly_block(app: &mut App, sql: &str) -> bool {
    let Some(cfg) = app.selected.clone() else {
        return false;
    };
    if !cfg.read_only {
        return false;
    }
    match readonly_violation(&cfg, sql) {
        Some(verb) => {
            // R54: name the connection, so with several open the user knows
            // *which* one refused the write.
            app.status = tf("✗ 只读连接「{}」：拒绝写语句（{}）", &[&cfg.name, &verb]);
            true
        }
        None => false,
    }
}

/// Refuse a row-level write gesture (edit / insert / delete) on a read-only
/// connection before any SQL is generated.
pub(crate) fn readonly_conn_block(app: &mut App) -> bool {
    if let Some(cfg) = app.selected.as_ref().filter(|c| c.read_only) {
        let name = cfg.name.clone();
        app.status = tf("✗ 只读连接「{}」：拒绝写语句", &[&name]);
        return true;
    }
    false
}

/// Find a top-level (paren depth 0) keyword, ignoring string literals, quoted
/// identifiers and comments. Returns the byte offset just past the keyword.
pub(crate) fn find_top_level_keyword(sql: &str, keyword: &str) -> Option<usize> {
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote == '\'' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        if c == '(' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == ')' {
            depth -= 1;
            i += 1;
            continue;
        }
        if depth == 0 && (c.is_alphanumeric() || c == '_') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            if word == keyword {
                let byte_off: usize = chars[..i].iter().map(|c| c.len_utf8()).sum();
                return Some(byte_off);
            }
            continue;
        }
        i += 1;
    }
    None
}

/// The `WHERE` predicate text of every UPDATE / DELETE in `sql`, collapsed to
/// one line and truncated to 80 display columns. Purely textual — no query is
/// sent, the statement the user already sees is just parsed back.
pub(crate) fn where_predicates(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for st in dbx_core::sql::split_sql_statements(sql) {
        let verb = statement_main_verb(&st);
        if !matches!(verb.as_str(), "update" | "delete") {
            continue;
        }
        if let Some(off) = find_top_level_keyword(&st, "where") {
            let pred = one_line(st[off..].trim().trim_end_matches(';'));
            if !pred.is_empty() {
                out.push(truncate_disp(&pred, 80));
            }
        }
    }
    out
}

/// The largest `LIMIT n` (n > 10000) in a read statement, for the non-blocking
/// "large result set" heads-up. MySQL's `LIMIT off, n` counts the second number.
pub(crate) fn large_limit_hint(sql: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    for st in dbx_core::sql::split_sql_statements(sql) {
        let verb = statement_main_verb(&st);
        if verb != "select" && verb != "values" {
            continue;
        }
        if let Some(n) = top_level_limit(&st) {
            if n > 10_000 {
                best = Some(best.map_or(n, |b| b.max(n)));
            }
        }
    }
    best
}

/// The `LIMIT` value of one statement (paren-depth 0 only), if any.
pub(crate) fn top_level_limit(statement: &str) -> Option<u64> {
    let mut rest = statement;
    loop {
        let off = find_top_level_keyword(rest, "limit")?;
        let after = &rest[off..];
        let s = after.trim_start();
        let first_len = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if first_len == 0 {
            rest = after;
            continue;
        }
        let first: u64 = s[..first_len].parse().unwrap_or(0);
        let tail = s[first_len..].trim_start();
        if let Some(comma) = tail.strip_prefix(',') {
            let r = comma.trim_start();
            let l = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            return Some(r[..l].parse().unwrap_or(first));
        }
        return Some(first);
    }
}
