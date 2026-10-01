use crate::prelude::*;
use crate::*;

/// How deep the `Alt-←` / `Alt-→` history goes. Fifty nodes is far more than a
/// session ever walks back through, and bounds the memory.
pub(crate) const NAV_DEPTH: usize = 50;

/// Push a browsed table / collection / key onto the back/forward history with
/// browser semantics: re-opening the entry the cursor already points at is a
/// no-op; anything else drops the forward branch and appends, moving the cursor
/// to the new tail.
pub(crate) fn record_nav(app: &mut App, entry: NavEntry) {
    if app
        .nav_history
        .get(app.nav_pos)
        .is_some_and(|e| *e == entry)
    {
        return;
    }
    app.nav_history.truncate(app.nav_pos + 1);
    app.nav_history.push(entry);
    if app.nav_history.len() > NAV_DEPTH {
        let drop = app.nav_history.len() - NAV_DEPTH;
        app.nav_history.drain(0..drop);
    }
    app.nav_pos = app.nav_history.len().saturating_sub(1);
}

/// `Alt-←` — step back to the previously browsed node. The cursor moves first,
/// then the node is opened; the re-open does not push a second history entry
/// because [`record_nav`] sees the entry it already points at.
pub(crate) fn nav_back(app: &mut App, tx: &Tx) {
    if app.nav_pos == 0 {
        app.status = if app.nav_history.is_empty() {
            t("还没有浏览过表 / key").into()
        } else {
            t("已经是最早的表 / key").into()
        };
        return;
    }
    app.nav_pos -= 1;
    let entry = app.nav_history[app.nav_pos].clone();
    open_nav_entry(app, tx, &entry, "←");
}

/// `Alt-→` — step forward again after a back. Disabled once the cursor is at the
/// tail (the forward branch is empty).
pub(crate) fn nav_forward(app: &mut App, tx: &Tx) {
    if app.nav_history.is_empty() || app.nav_pos + 1 >= app.nav_history.len() {
        app.status = if app.nav_history.is_empty() {
            t("还没有浏览过表 / key").into()
        } else {
            t("已经是最新的表 / key").into()
        };
        return;
    }
    app.nav_pos += 1;
    let entry = app.nav_history[app.nav_pos].clone();
    open_nav_entry(app, tx, &entry, "→");
}

/// The transient status that confirms where a history step landed. The same
/// hint is kept in `nav_landing` so the status bar's context block (which a
/// narrow screen cannot truncate) can show it too.
pub(crate) fn set_nav_status(app: &mut App, arrow: &str, qualified: &str) {
    if !arrow.is_empty() {
        let hint = format!("{arrow} {qualified}");
        app.status = hint.clone();
        app.nav_landing = Some(hint);
    }
}

pub(crate) fn open_recent(app: &mut App, tx: &Tx, idx: usize) {
    let Some((db, schema, table)) = app.recent_tables.get(idx).cloned() else {
        return;
    };
    app.recent_open = false;
    open_nav_entry(app, tx, &NavEntry::Table { db, schema, table }, "");
}

/// Jump to a history node, switching backend context first when needed.
/// `arrow` (`←` / `→` / empty) prefixes the status hint so a history step
/// confirms its landing; the recent-table overlay passes empty.
pub(crate) fn open_nav_entry(app: &mut App, tx: &Tx, entry: &NavEntry, arrow: &str) {
    match entry {
        NavEntry::Table { db, schema, table } => open_nav_table(app, tx, db, schema, table, arrow),
        NavEntry::RedisKey {
            db,
            key_raw,
            key_display,
        } => open_nav_redis_key(app, tx, *db, key_raw, key_display, arrow),
    }
}

/// Open a Redis key from the round-trip stack. Switches logical db first when
/// needed (the rescan is async, so the key is stashed in
/// `pending_open_redis_key`), then selects and loads it.
pub(crate) fn open_nav_redis_key(
    app: &mut App,
    tx: &Tx,
    db: u32,
    key_raw: &str,
    key_display: &str,
    arrow: &str,
) {
    if app.backend_kind != Backend::Redis {
        app.status = t("✗ 该记录属于 Redis 连接").into();
        return;
    }
    let qualified = fix_double_encoding(key_display);
    if db != app.redis_db {
        app.redis_db = db;
        app.redis_value = None;
        app.clear_grid();
        app.redis_filter.clear();
        app.redis_filter_prompt = None;
        app.pending_open_redis_key = Some(key_raw.to_string());
        start_redis_scan(app, tx, true);
        set_nav_status(app, arrow, &qualified);
        return;
    }
    if let Some(i) = app
        .redis_scan
        .keys
        .iter()
        .position(|k| k.key_raw == key_raw)
    {
        app.redis_list.select(Some(i));
        open_redis_value(app, tx);
        set_nav_status(app, arrow, &qualified);
    } else {
        // Not in the loaded window: rescan and open it when it appears.
        app.pending_open_redis_key = Some(key_raw.to_string());
        start_redis_scan(app, tx, true);
        set_nav_status(app, arrow, &qualified);
    }
}

/// Open a `(database, schema, table)` triple from the round-trip stack.
pub(crate) fn open_nav_table(
    app: &mut App,
    tx: &Tx,
    db: &str,
    schema: &str,
    table: &str,
    arrow: &str,
) {
    if app.backend_kind == Backend::Redis {
        app.status = t("✗ 该记录属于表 / 集合").into();
        return;
    }
    let db_changed = db != app.current_db();
    let qualified = qualified_display(&fix_double_encoding(schema), &fix_double_encoding(table));
    // Same database and schema: the table is already listed, jump straight to it.
    if !db_changed && schema == app.schema {
        if let Some(pos) = focus_table_in_sidebar(app, table) {
            app.table_list.select(Some(pos));
            open_table_data(app, tx);
            set_nav_status(app, arrow, &qualified);
            return;
        }
    }
    if db_changed {
        let Some(pos) = app.databases.iter().position(|d| *d == db) else {
            app.status = tf("✗ 数据库 {} 不在当前连接中", &[&(fix_double_encoding(db))]);
            return;
        };
        app.db_index = pos;
        // The schema list belongs to the old database; refetch it.
        app.schemas.clear();
        app.schemas_db.clear();
    }
    app.schema = schema.to_string();
    app.pending_open_table = Some((schema.to_string(), table.to_string()));
    app.pending_table = None;
    app.status = tf(
        "切换到 {} 并打开 {} …",
        &[&(fix_double_encoding(db)), &qualified],
    );
    reload_tables(app, tx);
    set_nav_status(app, arrow, &qualified);
}

// ── query-history panel (Alt-H) ──

/// Open the history overlay: show what is already loaded, then refresh from
/// DBX's shared store (recent 300, newest first, unique SQL).
pub(crate) fn open_history(app: &mut App, tx: &Tx) {
    if app.backend_kind != Backend::Sql {
        app.status = t("查询历史仅用于 SQL 编辑器").into();
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.history_open = true;
    app.history_needle.clear();
    app.history_filter = None;
    app.history_confirm = None;
    // Start at the newest entry, then clamp to the (possibly empty) view.
    app.history_list.select(Some(0));
    recompute_history_view(app);
    app.status = t("加载查询历史…").into();
    app.spawn(tx, Op::HistoryPanel(Box::new(cfg)));
}

/// Rebuild the filtered view (`history_view`) from the needle, keeping the
/// cursor on a valid row. Favourites come first (the panel's “收藏” section),
/// then the rest in time order (R45).
pub(crate) fn recompute_history_view(app: &mut App) {
    let needle = app.history_needle.trim().to_lowercase();
    // R51: the `/` filter matches the statement text, the source connection name
    // and the run-origin badge (`编` / `脚` / `直`), so `编` narrows to
    // editor-run statements and a host name narrows to its connection.
    let matches = |r: &HistoryRow| {
        needle.is_empty()
            || r.sql.to_lowercase().contains(&needle)
            || r.connection_name.to_lowercase().contains(&needle)
            || history_origin_badge(&r.origin).is_some_and(|b| b.to_lowercase().contains(&needle))
    };
    let mut favs: Vec<usize> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    for (i, r) in app.history_rows.iter().enumerate() {
        if !matches(r) {
            continue;
        }
        if app.history_favorites.contains(&r.sql) {
            favs.push(i);
        } else {
            rest.push(i);
        }
    }
    favs.extend(rest);
    app.history_view = favs;
    let n = app.history_view.len();
    if n == 0 {
        app.history_list.select(None);
    } else {
        let sel = app.history_list.selected().unwrap_or(0).min(n - 1);
        app.history_list.select(Some(sel));
    }
}

/// Recompute the view after a favourite toggle, keeping the cursor on the same
/// statement even though it just moved between sections (R45).
pub(crate) fn recompute_history_view_keep(app: &mut App, sql: &str) {
    recompute_history_view(app);
    if let Some(pos) = app
        .history_view
        .iter()
        .position(|&ri| app.history_rows.get(ri).is_some_and(|r| r.sql == sql))
    {
        app.history_list.select(Some(pos));
    }
}

/// How many leading entries of the view are favourites: the panel draws a
/// “收藏” header before them and a “时间序” header before the rest.
pub(crate) fn history_fav_count(app: &App) -> usize {
    app.history_view
        .iter()
        .take_while(|&&ri| {
            app.history_rows
                .get(ri)
                .is_some_and(|r| app.history_favorites.contains(&r.sql))
        })
        .count()
}

/// The row under the panel cursor.
pub(crate) fn history_selected_row(app: &App) -> Option<&HistoryRow> {
    let sel = app.history_list.selected()?;
    let idx = *app.history_view.get(sel)?;
    app.history_rows.get(idx)
}

/// `2000-01-01T00:00:00Z` → `01-01 00:00`, so the list stays narrow.
pub(crate) fn history_time_label(executed_at: &str) -> String {
    let b = executed_at.as_bytes();
    // Require an all-ASCII prefix so the byte slices below can never split a
    // multi-byte character (real DBX timestamps are always RFC3339 ASCII).
    if b.len() >= 16 && b[..16].is_ascii() && b.get(10) == Some(&b'T') {
        format!(
            "{}-{} {}",
            &executed_at[5..7],
            &executed_at[8..10],
            &executed_at[11..16]
        )
    } else {
        truncate_disp(executed_at, 11)
    }
}

/// First non-empty line of a statement, whitespace-collapsed, for the list.
pub(crate) fn history_summary(sql: &str) -> String {
    let first = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    one_line(first)
}

/// The `HH:MM` tail of a history timestamp, used on a very narrow panel so the
/// statement summary keeps a usable width (R41).
pub(crate) fn history_time_label_short(executed_at: &str) -> String {
    let b = executed_at.as_bytes();
    if b.len() >= 16 && b[..16].is_ascii() && b.get(10) == Some(&b'T') {
        executed_at[11..16].to_string()
    } else {
        truncate_disp(executed_at, 5)
    }
}

pub(crate) fn history_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // The `/` input is modal on top of the panel.
    if app.history_filter.is_some() {
        history_filter_key(app, k);
        return;
    }
    // Ctrl-Enter / p: run the selected statement directly (R45), bypassing the
    // editor. Enter keeps its recall-into-editor behaviour below.
    if (k.code == KeyCode::Enter && k.modifiers.contains(KeyModifiers::CONTROL))
        || (k.modifiers.is_empty() && k.code == KeyCode::Char('p'))
    {
        history_run_selected(app, tx);
        return;
    }
    let n = app.history_view.len();
    // Vim count prefix: `3j` moves the cursor three entries.
    if count_pre(app, tx, k, count_motion(k.code)) {
        return;
    }
    let step = |app: &mut App, delta: i32| {
        if n == 0 {
            return;
        }
        let cur = app.history_list.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        app.history_list.select(Some(next));
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.history_open = false;
            app.history_filter = None;
            app.flash(t("已关闭查询历史").into());
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let times = take_count(app) as i32;
            step(app, -times);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let times = take_count(app) as i32;
            step(app, times);
        }
        KeyCode::PageUp => {
            let times = take_count(app) as i32;
            step(app, -10 * times);
        }
        KeyCode::PageDown => {
            let times = take_count(app) as i32;
            step(app, 10 * times);
        }
        KeyCode::Home => {
            take_count(app);
            if n > 0 {
                app.history_list.select(Some(0));
            }
        }
        KeyCode::End => {
            take_count(app);
            if n > 0 {
                app.history_list.select(Some(n - 1));
            }
        }
        KeyCode::Char('/') => {
            let mut ta = TextArea::from([app.history_needle.clone()]);
            ta.move_cursor(CursorMove::End);
            app.history_filter = Some(ta);
            app.status = t("按语句内容过滤 · Enter 保留 · Esc 清除").into();
        }
        KeyCode::Enter => {
            let Some(row) = history_selected_row(app) else {
                app.status = t("没有可回填的历史").into();
                return;
            };
            let sql = row.sql.clone();
            app.history_open = false;
            app.history_filter = None;
            app.set_editor_text(&sql);
            app.focus = Focus::Editor;
            app.status = tf("已回填历史语句（{} 字符）", &[&(sql.chars().count())]);
        }
        KeyCode::Char('f') => history_toggle_favorite(app, tx),
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(row) = history_selected_row(app) else {
                app.status = t("没有可复制的历史").into();
                return;
            };
            let sql = row.sql.clone();
            let n = sql.chars().count();
            match clipboard_copy(&sql) {
                Some(p) => {
                    app.status = tf(
                        "✓ 已复制整条语句（{} 字符）· 兜底 {}",
                        &[&n, &(p.display())],
                    )
                }
                None => app.status = tf("✓ 已复制整条语句（{} 字符）· OSC52 剪贴板", &[&n]),
            }
        }
        KeyCode::Delete | KeyCode::Char('x') => {
            let Some(row) = history_selected_row(app) else {
                app.status = t("没有可删除的历史").into();
                return;
            };
            // R84: session rows live in memory only, so there is nothing to
            // delete from the store; they clear themselves when dbxt exits.
            if row.session {
                app.status = t("会话记录仅在内存中（退出即清空），无需删除").into();
                return;
            }
            app.history_confirm = Some(HistoryConfirm {
                id: row.id.clone(),
                sql: row.sql.clone(),
            });
            app.status = t("删除历史确认 · Enter 执行 · Esc 取消").into();
        }
        _ => {}
    }
}

/// `Ctrl-Enter` / `p` in the history panel: run the selected statement straight
/// through the normal query pipeline (multi-statement included), closing the
/// panel so the result lands in the results pane (R45).
pub(crate) fn history_run_selected(app: &mut App, tx: &Tx) {
    let Some(row) = history_selected_row(app) else {
        app.status = t("没有可执行的历史").into();
        return;
    };
    let sql = row.sql.clone();
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.history_open = false;
    app.history_filter = None;
    // R88: a direct history run is not an editor scope — no `执行第 N 条` label.
    app.pending_scope = None;
    // The same per-statement danger check the editor uses, so a re-run of a
    // DELETE / DROP still stops at the red confirmation layer.
    let statements = dbx_core::sql::split_sql_statements_for_database(&sql, cfg.db_type);
    let mut reasons: Vec<String> = Vec::new();
    for st in &statements {
        if let Some(r) = detect_danger(st) {
            if !reasons.contains(&r) {
                reasons.push(r);
            }
        }
    }
    if !reasons.is_empty() {
        app.pending_run_origin = "direct";
        app.confirm = Some(Confirm {
            sql,
            reasons,
            refresh: false,
            clear_batch: false,
            conn: None,
            redis: None,
            mongo: None,
        });
        app.status = t("危险语句确认 · Enter 执行 · Esc 取消").into();
        return;
    }
    app.status = t("直跑历史语句…").into();
    execute_sql(app, tx, sql, "direct");
}

pub(crate) fn history_filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            app.history_filter = None;
            app.status = tf(
                "历史过滤「{}」· 命中 {}",
                &[&(app.history_needle), &(app.history_view.len())],
            );
        }
        KeyCode::Esc => {
            app.history_filter = None;
            app.history_needle.clear();
            recompute_history_view(app);
            app.flash(t("已清除历史过滤").into());
        }
        _ => {
            if let Some(ta) = app.history_filter.as_mut() {
                ta.input(k);
            }
            app.history_needle = app
                .history_filter
                .as_ref()
                .and_then(|ta| ta.lines().first().cloned())
                .unwrap_or_default();
            recompute_history_view(app);
            // The list is the filter view, so the cursor is only valid when
            // something matched (an empty view keeps it unset).
            if !app.history_view.is_empty() {
                app.history_list.select(Some(0));
            }
        }
    }
}

/// Toggle the focused statement in / out of DBX's `saved_sql_files` favourites.
pub(crate) fn history_toggle_favorite(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let Some(row) = history_selected_row(app) else {
        app.status = t("没有可收藏的历史").into();
        return;
    };
    let sql = row.sql.clone();
    let name = {
        let s = history_summary(&sql);
        if s.trim().is_empty() {
            "query".to_string()
        } else {
            truncate_disp(&s, 60)
        }
    };
    app.status = t("更新收藏…").into();
    app.spawn(
        tx,
        Op::HistoryFavorite {
            cfg: Box::new(cfg),
            sql,
            name,
        },
    );
}

pub(crate) fn history_confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(c) = app.history_confirm.take() else {
                return;
            };
            app.status = t("删除该条历史…").into();
            app.spawn(tx, Op::HistoryDelete { id: c.id });
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.history_confirm = None;
            app.flash(t("已取消").into());
        }
        _ => {}
    }
}

// ── global database search (Alt-G) ──

/// Open the global-search term prompt. Redis / Mongo have no information schema
/// to enumerate, so the feature is SQL-only and says so.
pub(crate) fn open_global_search(app: &mut App) {
    if app.backend_kind != Backend::Sql {
        app.status = t("全库搜索仅支持 SQL（MySQL / PostgreSQL）").into();
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    if is_postgres_family(cfg.db_type.as_str()) || is_mysql_family(cfg.db_type.as_str()) {
        let mut ta = TextArea::default();
        ta.set_placeholder_text(t("搜索词（所有表的文本列，大小写不敏感）"));
        app.search_input = Some(ta);
        app.status = tf(
            "全库搜索 {} · 输入搜索词 · Enter 开始 · Esc 取消",
            &[&(cfg.name)],
        );
    } else {
        app.status = t("全库搜索仅支持 MySQL / PostgreSQL 连接").into();
    }
}

pub(crate) fn search_input_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter => {
            let needle = app
                .search_input
                .as_ref()
                .map(|t| t.lines().join(" ").trim().to_string())
                .unwrap_or_default();
            app.search_input = None;
            if needle.is_empty() {
                app.status = t("搜索词不能为空").into();
                return;
            }
            start_global_search(app, tx, needle);
        }
        KeyCode::Esc => {
            app.search_input = None;
            app.flash(t("已取消全库搜索").into());
        }
        _ => {
            if let Some(t) = app.search_input.as_mut() {
                t.input(k);
            }
        }
    }
}

/// Kick off a background scan, cancelling any previous one. Results land in the
/// overlay as they arrive and can be aborted with Esc.
pub(crate) fn start_global_search(app: &mut App, tx: &Tx, needle: String) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    // Cancel the previous scan before replacing the shared flag.
    app.search_cancel.store(true, Ordering::Relaxed);
    app.search_gen += 1;
    let gen = app.search_gen;
    let cancel = Arc::new(AtomicBool::new(false));
    app.search_cancel = cancel.clone();
    app.search_query = needle.clone();
    app.search_hits.clear();
    app.search_skipped.clear();
    app.search_truncated = false;
    app.search_list.select(None);
    app.search_running = true;
    app.search_progress = Some((0, 0));
    app.search_open = true;
    app.status = tf("全库搜索「{}」…", &[&needle]);
    app.loading = true;
    app.spawn(
        tx,
        Op::GlobalSearch {
            cfg: Box::new(cfg),
            db: app.current_db(),
            schema: app.schema.clone(),
            needle,
            scan_limit: search_scan_limit(),
            max_rows: search_max_rows(),
            gen,
            cancel,
        },
    );
}

pub(crate) fn search_selected_hit(app: &App) -> Option<&SearchHit> {
    let sel = app.search_list.selected()?;
    app.search_hits.get(sel)
}

/// Copy the focused hit's matched value, and record the status.
pub(crate) fn search_copy_hit(app: &mut App) {
    let Some(hit) = search_selected_hit(app) else {
        app.status = t("没有可复制的命中").into();
        return;
    };
    let text = hit.matched.clone();
    let n = text.chars().count();
    match clipboard_copy(&text) {
        Some(p) => app.status = tf("✓ 已复制命中值（{} 字符）· 兜底 {}", &[&n, &(p.display())]),
        None => app.status = tf("✓ 已复制命中值（{} 字符）· OSC52 剪贴板", &[&n]),
    }
}

/// Enter on a hit: open its table and pre-filter to the row that matched.
pub(crate) fn open_search_hit(app: &mut App, tx: &Tx, hit: &SearchHit) {
    let label = qualified_display(&hit.schema, &hit.table);
    app.search_open = false;
    app.search_input = None;
    app.pending_table_filter = Some(hit.filter.clone());
    // The global search scans every table, so a leftover sidebar `/` filter that
    // hides the hit must not turn the jump into "table not found".
    let found = focus_table_in_sidebar(app, &hit.table);
    match found {
        Some(pos) => {
            app.table_list.select(Some(pos));
            app.status = tf("定位 {} 的命中行…", &[&(fix_double_encoding(&label))]);
            open_table_data(app, tx);
        }
        None => {
            // The sidebar list is stale; refresh and open once it lands.
            app.pending_open_table = Some((hit.schema.clone(), hit.table.clone()));
            app.status = tf("加载表列表后定位 {}…", &[&(fix_double_encoding(&label))]);
            spawn_table_list(app, tx);
        }
    }
}

pub(crate) fn search_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let n = app.search_hits.len();
    let step = |app: &mut App, delta: i32| {
        if n == 0 {
            return;
        }
        let cur = app.search_list.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        app.search_list.select(Some(next));
    };
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            if app.search_running {
                app.search_cancel.store(true, Ordering::Relaxed);
                app.status = t("正在中止全库搜索…").into();
            } else {
                app.search_open = false;
                app.flash(t("已关闭全库搜索").into());
            }
        }
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home => {
            if n > 0 {
                app.search_list.select(Some(0));
            }
        }
        KeyCode::End => {
            if n > 0 {
                app.search_list.select(Some(n - 1));
            }
        }
        KeyCode::Char('y') => search_copy_hit(app),
        KeyCode::Char('r') => {
            if app.search_query.trim().is_empty() {
                app.status = t("没有可重搜的关键词").into();
            } else {
                let q = app.search_query.clone();
                start_global_search(app, tx, q);
            }
        }
        KeyCode::Enter => {
            let Some(hit) = search_selected_hit(app).cloned() else {
                app.status = t("没有可定位的命中").into();
                return;
            };
            open_search_hit(app, tx, &hit);
        }
        _ => {}
    }
}

// ── R100: full-database data dictionary (`E` on the connection tree) ──

/// R100: `E` on the connection list / tree — start a whole-database data
/// dictionary export. A SQL-only, read-only, explicitly triggered walk; a
/// database with more than [`DICT_CONFIRM_TABLES`] cached tables first asks
/// through the red confirmation layer so a fat-finger cannot launch a huge scan.
/// The table count is read from the sidebar's already-cached list (no query).
pub(crate) fn open_data_dictionary(app: &mut App, tx: &Tx) {
    if app.backend_kind != Backend::Sql {
        app.status = t("数据字典仅支持 SQL 连接").into();
        return;
    }
    let Some(cfg) = app.selected.clone() else {
        app.status = t("先连接一个 SQL 连接再按 E 导出数据字典").into();
        return;
    };
    let db = app.current_db();
    if db.trim().is_empty() {
        app.status = t("请先选择一个数据库").into();
        return;
    }
    let schema = app.schema.clone();
    let n = app.tables_all.len();
    if n > DICT_CONFIRM_TABLES {
        app.dict_confirm = Some(DictConfirm {
            cfg: Box::new(cfg),
            db,
            schema,
            tables: n,
        });
        app.status = tf("数据字典将遍历 {} 张表 · Enter 继续 · Esc 取消", &[&n]);
        return;
    }
    start_data_dictionary(app, tx, cfg, db, schema);
}

/// Keys for the `>200`-table confirmation layer.
pub(crate) fn dict_confirm_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    match k.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(c) = app.dict_confirm.take() else {
                return;
            };
            start_data_dictionary(app, tx, *c.cfg, c.db, c.schema);
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.dict_confirm = None;
            app.flash(t("已取消导出数据字典").into());
        }
        _ => {}
    }
}

/// Kick off the background walk, cancelling any previous one. Progress lands on
/// the status bar; Esc (handled in the global router) soft-cancels it.
pub(crate) fn start_data_dictionary(
    app: &mut App,
    tx: &Tx,
    cfg: ConnectionConfig,
    db: String,
    schema: String,
) {
    app.dict_cancel.store(true, Ordering::Relaxed);
    app.dict_gen = app.dict_gen.wrapping_add(1);
    let gen = app.dict_gen;
    let cancel = Arc::new(AtomicBool::new(false));
    app.dict_cancel = cancel.clone();
    app.dict_running = true;
    app.dict_progress = Some((0, 0));
    app.dict_content = None;
    app.dict_prompt = None;
    app.dict_db = db.clone();
    app.status = t("生成数据字典…正在枚举表").into();
    app.loading = true;
    app.spawn(
        tx,
        Op::DataDictionary {
            cfg: Box::new(cfg),
            db,
            schema,
            gen,
            cancel,
        },
    );
}

/// Esc while a dictionary walk is in flight: ask the worker to stop between
/// tables (the partial document is discarded). The state is reset by the
/// worker's [`OpResult::DictCancelled`] reply, exactly like a search abort.
pub(crate) fn soft_cancel_data_dictionary(app: &mut App) -> bool {
    if !app.dict_running {
        return false;
    }
    app.dict_cancel.store(true, Ordering::Relaxed);
    app.status = t("正在中止数据字典…").into();
    true
}

/// Destination prompt for a generated dictionary. Blank copies to the clipboard
/// (the `Ctrl-Y` rule); a path writes the file.
pub(crate) fn dict_prompt_key(app: &mut App, k: KeyEvent) {
    let Some(mut ta) = app.dict_prompt.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.dict_content = None;
        app.flash(t("已取消导出数据字典").into());
        return;
    }
    if k.code != KeyCode::Enter {
        ta.input(k);
        app.dict_prompt = Some(ta);
        return;
    }
    let Some(content) = app.dict_content.take() else {
        return;
    };
    let input = ta.lines().join("\n");
    let path = input.trim();
    let n = content.chars().count();
    if path.is_empty() {
        match clipboard_copy(&content) {
            Some(p) => {
                app.status = tf(
                    "✓ 数据字典已复制到剪贴板（{} 字符）· 兜底 {}",
                    &[&n, &(p.display())],
                )
            }
            None => app.status = tf("✓ 数据字典已复制到剪贴板（{} 字符）", &[&n]),
        }
        return;
    }
    let expanded = expand_home(path);
    match std::fs::write(&expanded, content.as_bytes()) {
        Ok(()) => {
            app.status = tf(
                "✓ 数据字典已写入 {}（{} 字符）",
                &[&(expanded.display()), &n],
            )
        }
        Err(e) => app.status = tf("✗ 数据字典写入失败：{}", &[&e]),
    }
}

// ── schema diff (Alt-D / Shift+Alt-D) ──
