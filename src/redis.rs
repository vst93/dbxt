use crate::prelude::*;
use crate::*;

// ─── Redis key browser ───────────────────────────────────────────────────────

/// R81: the key-browser type filter cycle (`t`). The empty string is the "all
/// types" step; every other entry matches a key's `TYPE` verbatim. Pure
/// client-side filtering of the already-loaded SCAN window.
pub(crate) const REDIS_TYPE_FILTERS: [&str; 7] =
    ["", "string", "hash", "list", "set", "zset", "stream"];

/// Server-side SCAN state for the Redis key browser. Keys are appended page by
/// page (never a full `KEYS *`), and `cursor == 0` marks the end of the keyspace.
#[derive(Clone)]
pub(crate) struct RedisScanState {
    /// The filtered view the sidebar renders / navigates (R42 client-side
    /// type-to-filter). Equal to `all` when no filter is active.
    pub(crate) keys: Vec<RedisKeyInfo>,
    /// Every key loaded so far, before the client-side filter is applied. A
    /// SCAN page appends here and the filter is re-applied.
    pub(crate) all: Vec<RedisKeyInfo>,
    /// Cursor to resume from; 0 means the scan is exhausted.
    pub(crate) cursor: u64,
    pub(crate) exhausted: bool,
    /// `MATCH` pattern applied server-side (default `*`).
    pub(crate) pattern: String,
    /// `total_keys` hint reported by the server (DBSIZE-like).
    pub(crate) total: u64,
    /// Request generation, so a stale page cannot clobber a fresh scan.
    pub(crate) gen: u64,
    /// A page request is in flight. `n` (load more) is ignored while set, so two
    /// concurrent requests cannot both start from the same cursor and append the
    /// same page twice (a fresh scan resets `cursor` to 0, so pressing `n` before
    /// its reply used to duplicate the first page).
    pub(crate) pending: bool,
}

impl Default for RedisScanState {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            all: Vec::new(),
            cursor: 0,
            exhausted: false,
            pattern: "*".to_string(),
            total: 0,
            gen: 0,
            pending: false,
        }
    }
}

/// A Redis key's value prepared for the results grid. The raw value is kept so a
/// grid row can be mapped back to its field / member for an edit or delete.
#[derive(Clone)]
pub(crate) struct RedisValueView {
    pub(crate) key_display: String,
    pub(crate) key_raw: String,
    pub(crate) redis_type: String,
    pub(crate) ttl: i64,
    pub(crate) grid: Grid,
    /// Grid row index → field / member / element id the row represents.
    pub(crate) row_keys: Vec<String>,
    /// Cursor for the next collection page, when the value is truncated.
    pub(crate) scan_cursor: Option<u64>,
    /// Raw value, used to prefill a string edit and to know the concrete type.
    pub(crate) raw: RedisValue,
}

/// Which Redis input dialog is open. Every write goes through the shared red
/// confirmation layer afterwards, so nothing mutates without a second Enter.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RedisPromptKind {
    /// `MATCH` pattern for the key browser.
    Pattern,
    /// New TTL in seconds for the focused key.
    Ttl,
    /// R81: new TTL for the focused key straight from the key list (`T`), with
    /// an optional `s` / `ms` / `m` / `h` / `d` unit suffix.
    TtlKey,
    /// New key name for a RENAME.
    Rename,
    /// New string body for the focused string key.
    StringValue,
    /// New value for a hash field.
    HashField,
    /// New TTL applied to a whole batch of selected keys.
    BatchTtl,
    /// `old=new` prefix replacement applied to a whole batch of selected keys.
    BatchRenamePrefix,
    /// Typed re-confirmation of a dangerous batch (repeat the key count / YES).
    BatchConfirm,
}

#[derive(Clone)]
pub(crate) struct RedisPrompt {
    pub(crate) kind: RedisPromptKind,
    pub(crate) title: String,
    /// Key in the form a redis-cli command needs (the decoded display name).
    pub(crate) key_display: String,
    /// Base64 raw key, used to reload the value after the write.
    pub(crate) key_raw: String,
    /// Hash field name, when the prompt edits a hash member.
    pub(crate) field: String,
    pub(crate) input: TextArea<'static>,
    /// Batch targets as `(raw, display)`, empty for single-key prompts.
    pub(crate) batch: Vec<(String, String)>,
}

/// SQL prefix-completion popup in the editor (Alt-/). Tab / Enter accept,
/// Esc cancels; typing keeps refining the candidate list.
#[derive(Clone, Debug)]
pub(crate) struct CompletionItem {
    pub(crate) text: String,
    /// `T` table, `C` column, `K` keyword — shown in the popup.
    pub(crate) kind: char,
}

#[derive(Clone)]
pub(crate) struct Completion {
    pub(crate) items: Vec<CompletionItem>,
    pub(crate) sel: usize,
    /// Characters immediately before the cursor that are replaced when a
    /// candidate is accepted (the fragment after the last `.` when qualified).
    pub(crate) replace: usize,
}

/// The context the cursor sits in, used to order / restrict candidates.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum CompCtx {
    /// After `table.` — only that table's columns.
    Qualified(String),
    /// After `FROM ` / `JOIN ` / `INTO ` — tables first.
    TableList,
    /// After `WHERE ` / `ON ` / `SET ` … — columns first.
    Column,
    /// Anything else — the historical columns → tables → keywords order.
    Any,
}

/// Execute a confirmed Redis write through the console, then refresh.
pub(crate) fn run_redis_write(
    app: &mut App,
    tx: &Tx,
    db: u32,
    cmd: &str,
    reload_value: Option<String>,
    reload_list: bool,
) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    app.loading = true;
    app.status = tf("执行 {}…", &[&(truncate_disp(&one_line(cmd), 50))]);
    app.spawn(
        tx,
        Op::RedisWrite {
            cfg: Box::new(cfg),
            db,
            cmd: cmd.to_string(),
            reload_value,
            reload_list,
        },
    );
}

/// `y` in a Redis value / Mongo document grid: copy the focused row as tab-
/// separated text (OSC52 clipboard, with the file fallback).
pub(crate) fn copy_redis_row(app: &mut App) {
    let Some(row) = focused_full_row(app) else {
        app.status = t("没有可复制的行").into();
        return;
    };
    let text = row.iter().map(|v| v.text()).collect::<Vec<_>>().join("\t");
    let n = text.chars().count();
    match clipboard_copy(&text) {
        Some(p) => app.status = tf("✓ 已复制（{} 字符）· 兜底 {}", &[&(n), &(p.display())]),
        None => app.status = tf("✓ 已复制（{} 字符）· OSC52 剪贴板", &[&(n)]),
    }
}

/// Confirm deleting the focused key (DEL). Data-destructive writes always go
/// through the red layer, like every SQL row delete.
pub(crate) fn redis_confirm_delete(app: &mut App) {
    // R68: refuse the gesture up front on a read-only connection (the confirm
    // layer's Enter would also block, but this names the connection at once).
    if readonly_conn_block(app) {
        return;
    }
    let Some(view) = app.redis_value.clone() else {
        app.status = t("先选中一个 key").into();
        return;
    };
    let cmd = format!(
        "DEL \"{}\"",
        view.key_display.replace('\\', "\\\\").replace('"', "\\\"")
    );
    app.confirm = Some(Confirm {
        sql: cmd.clone(),
        reasons: vec![
            tf("将删除 key {}（不可撤销）", &[&(view.key_display)]),
            t("DEL 不可撤销，Enter 后立即执行").into(),
        ],
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd,
            batch: Vec::new(),
            batch_keys: Vec::new(),
            reload_value: None,
            reload_list: true,
            typed_confirm: None,
            summary: String::new(),
            remove_in_place: Vec::new(),
            set_ttl_in_place: Vec::new(),
        }),
        mongo: None,
    });
    app.status = t("删除确认 · Enter 执行 · Esc 取消").into();
}

/// Run a generated batch of Redis commands in order.
pub(crate) fn run_redis_batch(
    app: &mut App,
    tx: &Tx,
    db: u32,
    cmds: Vec<String>,
    reload_list: bool,
) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let n = cmds.len();
    app.loading = true;
    app.status = tf("批量执行 {} 条命令…", &[&n]);
    app.spawn(
        tx,
        Op::RedisBatchWrite {
            cfg: Box::new(cfg),
            db,
            cmds,
            reload_list,
        },
    );
}

/// The batch targets in list order as `(raw, display)`. When nothing is
/// explicitly selected, the focused key is the single target.
pub(crate) fn redis_selection_targets(
    selected: &HashSet<String>,
    keys: &[RedisKeyInfo],
    focused: Option<usize>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = keys
        .iter()
        .filter(|k| selected.contains(&k.key_raw))
        .map(|k| (k.key_raw.clone(), k.key_display.clone()))
        .collect();
    if out.is_empty() {
        if let Some(i) = focused {
            if let Some(k) = keys.get(i) {
                out.push((k.key_raw.clone(), k.key_display.clone()));
            }
        }
    }
    out
}

pub(crate) fn redis_batch_targets(app: &App) -> Vec<(String, String)> {
    redis_selection_targets(
        &app.redis_selected,
        &app.redis_scan.keys,
        app.redis_list.selected(),
    )
}

/// True when the selection covers every loaded key (used to force the extra
/// typed confirmation before a destructive select-all delete).
pub(crate) fn redis_selection_is_all(selected: &HashSet<String>, keys: &[RedisKeyInfo]) -> bool {
    !selected.is_empty() && selected.len() == keys.len()
}

pub(crate) fn redis_all_selected(app: &App) -> bool {
    redis_selection_is_all(&app.redis_selected, &app.redis_scan.keys)
}

/// Toggle the key at `idx` in / out of the selection.
pub(crate) fn redis_selection_toggle(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
    idx: usize,
) {
    let Some(k) = keys.get(idx) else {
        return;
    };
    let raw = k.key_raw.clone();
    if !selected.remove(&raw) {
        selected.insert(raw);
    }
    *anchor = Some(idx);
}

/// Extend the selection from the anchor to `to` (additive).
pub(crate) fn redis_selection_range(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
    to: usize,
) {
    let n = keys.len();
    if n == 0 {
        return;
    }
    let to = to.min(n - 1);
    let a = anchor.unwrap_or(to).min(n - 1);
    let (lo, hi) = if a <= to { (a, to) } else { (to, a) };
    for k in keys.iter().take(hi + 1).skip(lo) {
        selected.insert(k.key_raw.clone());
    }
    *anchor = Some(a);
}

/// Select every loaded key (the `a` gesture).
pub(crate) fn redis_selection_all(
    selected: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    keys: &[RedisKeyInfo],
) {
    for k in keys {
        selected.insert(k.key_raw.clone());
    }
    if !keys.is_empty() {
        *anchor = Some(0);
    }
}

/// Toggle the focused key's selection.
pub(crate) fn redis_toggle_select(app: &mut App) {
    let Some(i) = app.redis_list.selected() else {
        return;
    };
    redis_selection_toggle(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
        i,
    );
}

/// Extend the selection from the anchor to `to` (additive).
pub(crate) fn redis_select_range(app: &mut App, to: usize) {
    redis_selection_range(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
        to,
    );
    let n = app.redis_scan.keys.len();
    if n > 0 {
        app.redis_list.select(Some(to.min(n - 1)));
    }
}

/// Select every loaded key (the `a` gesture).
pub(crate) fn redis_select_all(app: &mut App) {
    if app.redis_scan.keys.is_empty() {
        return;
    }
    redis_selection_all(
        &mut app.redis_selected,
        &mut app.redis_anchor,
        &app.redis_scan.keys,
    );
    app.status = tf("已全选 {} 个 key", &[&app.redis_selected.len()]);
}

/// `y` in the key browser: copy the selected key names (or the focused one).
pub(crate) fn redis_copy_selection(app: &mut App) {
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    let text = targets
        .iter()
        .map(|(_, d)| d.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let n = targets.len();
    match clipboard_copy(&text) {
        Some(p) => app.status = tf("✓ 已复制 {} 个 key 名 · 兜底 {}", &[&n, &(p.display())]),
        None => app.status = tf("✓ 已复制 {} 个 key 名 · OSC52 剪贴板", &[&n]),
    }
}

/// A concise confirmation preview: the first commands plus a count summary.
pub(crate) fn redis_batch_preview(cmds: &[String], keys: usize) -> String {
    let mut s = String::new();
    for c in cmds.iter().take(4) {
        s.push_str(c);
        s.push('\n');
    }
    if cmds.len() > 4 {
        s.push_str(&tf("… 其余 {} 条命令", &[&(cmds.len() - 4)]));
        s.push('\n');
    }
    s.push_str(&tf("共 {} 个 key · {} 条命令", &[&keys, &cmds.len()]));
    s
}

/// Open the red confirmation layer for a generated Redis batch.
pub(crate) fn redis_open_batch_confirm(
    app: &mut App,
    commands: Vec<String>,
    targets: Vec<(String, String)>,
    typed_confirm: Option<usize>,
    summary: String,
) {
    let n = targets.len();
    let pattern = app.redis_scan.pattern.clone();
    app.confirm = Some(Confirm {
        sql: redis_batch_preview(&commands, n),
        reasons: vec![
            tf("将影响 {} 个 key（模式 {}）", &[&n, &pattern]),
            t("Enter 后按顺序执行，不可撤销").into(),
        ],
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd: String::new(),
            batch: commands,
            batch_keys: targets,
            reload_value: None,
            reload_list: true,
            typed_confirm,
            summary,
            remove_in_place: Vec::new(),
            set_ttl_in_place: Vec::new(),
        }),
        mongo: None,
    });
    app.status = t("批量确认 · Enter 执行 · Esc 取消").into();
}

/// R57: drop just-deleted keys from the loaded SCAN window without rescanning,
/// so the cursor and the already-loaded pages stay put (a full `SCAN` reset
/// jumps the list back to the first page). The filtered `keys` view and the
/// loaded `all` window are both pruned, and the selection / open value follow.
pub(crate) fn redis_remove_keys_in_place(app: &mut App, raws: &[String]) {
    if raws.is_empty() {
        return;
    }
    let gone: HashSet<&str> = raws.iter().map(|s| s.as_str()).collect();
    app.redis_scan
        .all
        .retain(|k| !gone.contains(k.key_raw.as_str()));
    app.redis_scan
        .keys
        .retain(|k| !gone.contains(k.key_raw.as_str()));
    for r in raws {
        app.redis_selected.remove(r);
    }
    let n = app.redis_scan.keys.len();
    let sel = app.redis_list.selected().unwrap_or(0);
    app.redis_list
        .select(if n == 0 { None } else { Some(sel.min(n - 1)) });
    app.redis_anchor = None;
    // The open value belonged to a deleted key: close it and hand focus back.
    if let Some(v) = &app.redis_value {
        if gone.contains(v.key_raw.as_str()) {
            app.redis_value = None;
            app.clear_grid();
            app.focus = Focus::Sidebar;
        }
    }
}

/// R81: reflect a just-confirmed single-key TTL write in the loaded key window
/// in place, so the list keeps its SCAN cursor / position instead of rescanning
/// from page one. Pure local state, no extra query.
pub(crate) fn redis_apply_ttl_in_place(app: &mut App, updates: &[(String, i64)]) {
    for (raw, ttl) in updates {
        for k in app.redis_scan.all.iter_mut().filter(|k| &k.key_raw == raw) {
            k.ttl = *ttl;
        }
        for k in app.redis_scan.keys.iter_mut().filter(|k| &k.key_raw == raw) {
            k.ttl = *ttl;
        }
        if let Some(v) = app.redis_value.as_mut() {
            if &v.key_raw == raw {
                v.ttl = *ttl;
            }
        }
    }
}

/// `Del` in the key browser: batch delete the selected keys.
pub(crate) fn redis_batch_delete(app: &mut App) {
    if readonly_conn_block(app) {
        return;
    }
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    if targets.len() > REDIS_BATCH_LIMIT {
        app.status = tf(
            "选中 {} 个 key 超过单页上限 {}，请分批操作（space 取消部分选择）",
            &[&(targets.len()), &(REDIS_BATCH_LIMIT)],
        );
        return;
    }
    let all_loaded = redis_all_selected(app);
    let Ok(plan) = redis_plan_batch(RedisBatchKind::Delete, &targets, all_loaded, "") else {
        return;
    };
    let n = targets.len();
    let pattern = app.redis_scan.pattern.clone();
    let single = targets.first().map(|(raw, _)| raw.clone());
    redis_open_batch_confirm(
        app,
        plan.commands,
        targets,
        plan.typed_confirm,
        plan.summary,
    );
    if let Some(c) = app.confirm.as_mut() {
        c.reasons = vec![
            tf("将批量删除 {} 个 key（模式 {}）", &[&n, &pattern]),
            t("DEL 不可撤销，Enter 后立即执行").into(),
        ];
        // R57: a single-key delete stays put — remove it from the loaded list in
        // place instead of rescanning from cursor 0 (which would jump the list
        // back to the first page). Confirmed exactly like any other delete.
        if n == 1 {
            if let (Some(rc), Some(raw)) = (c.redis.as_mut(), single) {
                rc.reload_list = false;
                rc.remove_in_place = vec![raw];
            }
        }
    }
}

/// `x` in the key browser: open the batch TTL prompt.
pub(crate) fn open_redis_batch_ttl_prompt(app: &mut App) {
    if readonly_conn_block(app) {
        return;
    }
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    let mut ta = TextArea::default();
    ta.set_placeholder_text(t("秒数（-1 = 持久化，0 = 立即删除）"));
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchTtl,
        title: tf("批量设置 TTL · {} 个 key", &[&targets.len()]),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: targets,
        input: ta,
    });
}

/// `m` in the key browser: open the batch prefix-rename prompt.
pub(crate) fn open_redis_batch_rename_prompt(app: &mut App) {
    if readonly_conn_block(app) {
        return;
    }
    let targets = redis_batch_targets(app);
    if targets.is_empty() {
        app.status = t("先选中一个 key").into();
        return;
    }
    // Prefill the old prefix from the SCAN pattern when it ends with `*`.
    let old = app
        .redis_scan
        .pattern
        .strip_suffix('*')
        .filter(|p| !p.is_empty() && *p != "*")
        .unwrap_or("");
    let mut ta = TextArea::from(vec![format!("{old}=")]);
    ta.set_placeholder_text(t("旧前缀=新前缀，例: app: = new:"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::BatchRenamePrefix,
        title: tf("批量前缀重命名 · {} 个 key", &[&targets.len()]),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: targets,
        input: ta,
    });
}

/// Toggle the first-column pin and flash the new state. Shared by the result,
/// Redis and MongoDB grids so `z` reads the same everywhere.
pub(crate) fn toggle_freeze_first(app: &mut App) {
    app.freeze_first = !app.freeze_first;
    app.status = if app.freeze_first {
        t("首列已钉住 · z 取消").into()
    } else {
        t("首列已取消钉住 · z 钉住").into()
    };
}

/// R91: `g f` — pin / unpin the *focused* column at the left edge. The wide-table
/// twin of `z` (which only ever pins column 0): at most [`MAX_FROZEN_COLS`]
/// columns are pinned in total, and pressing the key on an already-pinned column
/// unfreezes it. Purely a render-time layout choice, zero queries.
pub(crate) fn toggle_freeze_col(app: &mut App) {
    let Some(ncols) = active_col_count(app) else {
        app.status = t("没有可冻结的列").into();
        return;
    };
    if ncols < 3 {
        app.status = t("列太少 · 无需冻结").into();
        return;
    }
    let col = app.col_cursor.min(ncols - 1);
    if col == 0 {
        if !app.freeze_first && app.frozen_cols.len() >= MAX_FROZEN_COLS {
            app.status = tf(
                "最多冻结 {} 列 · 先对已冻结列再按 g f 解冻",
                &[&(MAX_FROZEN_COLS)],
            );
            return;
        }
        toggle_freeze_first(app);
        return;
    }
    if let Some(pos) = app.frozen_cols.iter().position(|&c| c == col) {
        app.frozen_cols.remove(pos);
        app.flash(tf("已解冻第 {} 列", &[&(col + 1)]));
        return;
    }
    let total = app.frozen_cols.len() + usize::from(app.freeze_first);
    if total >= MAX_FROZEN_COLS {
        app.status = tf(
            "最多冻结 {} 列 · 先对已冻结列再按 g f 解冻",
            &[&(MAX_FROZEN_COLS)],
        );
        return;
    }
    app.frozen_cols.push(col);
    app.frozen_cols.sort_unstable();
    app.flash(tf("已冻结第 {} 列 · 横向滚动时始终可见", &[&(col + 1)]));
}

/// Keys for a Redis value grid: edit the string / hash field, expire, rename,
/// delete, plus the shared search / copy / popup infrastructure.
pub(crate) fn redis_value_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    // R91: the `g` chord lives here too — `gf` freezes the focused column and
    // `gs` pins the reference row, matching the SQL / MongoDB grids.
    if app.pending_g {
        app.pending_g = false;
        if k.modifiers.is_empty() && k.code == KeyCode::Char('f') {
            toggle_freeze_col(app);
        } else if k.modifiers.is_empty() && k.code == KeyCode::Char('s') {
            toggle_ref_row(app);
        }
        return;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('e') => app.focus = Focus::Editor,
            // R101: leave `Ctrl-Shift-D` (result snapshot) alone — a shifted
            // lowercase `d` must never be read as the delete shortcut.
            KeyCode::Char('d') if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                redis_confirm_delete(app)
            }
            _ => {}
        }
        return;
    }
    if k.code == KeyCode::Esc && !app.cell_find_needle.is_empty() {
        app.clear_cell_find();
        app.flash(t("已清除单元格查找").into());
        return;
    }
    if k.code == KeyCode::Esc && !app.result_needle.is_empty() {
        app.result_needle.clear();
        app.rebuild_view();
        app.sel = 0;
        app.flash(t("已清除结果搜索").into());
        return;
    }
    match k.code {
        KeyCode::Esc => {
            app.focus = Focus::Sidebar;
            app.flash(t("已回到侧栏").into());
        }
        KeyCode::Char('e') => open_redis_edit(app),
        // `n` loads the next page of a large hash / list / set / zset value.
        KeyCode::Char('n') => redis_load_more(app, tx),
        // `x` = expire (TTL), `m` = move / rename.
        KeyCode::Char('x') => open_redis_ttl_prompt(app),
        KeyCode::Char('m') => open_redis_rename_prompt(app),
        KeyCode::Delete => redis_confirm_delete(app),
        KeyCode::Char('o') => open_row_popup(app),
        KeyCode::Char('v') => open_cell_popup(app),
        KeyCode::Char('/') => open_result_filter(app),
        KeyCode::Char('\\') => open_cell_find(app),
        KeyCode::Char('y') => copy_redis_row(app),
        KeyCode::Char('Y') => copy_cell_value(app),
        KeyCode::Char(':') => open_goto_row(app),
        KeyCode::Char('z') => toggle_freeze_first(app),
        // R91: `g` starts the chord — `gf` freezes the focused column, `gs` pins
        // the reference row.
        KeyCode::Char('g') => {
            app.pending_g = true;
            app.status = t("g… f=冻结列 s=钉行").into();
        }
        KeyCode::Up | KeyCode::Char('k') => move_cursor(app, tx, -1),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(app, tx, 1),
        KeyCode::Left | KeyCode::Char('h') => move_col_cursor(app, -1),
        KeyCode::Right | KeyCode::Char('l') => move_col_cursor(app, 1),
        KeyCode::PageUp => screen_move(app, tx, -1),
        KeyCode::PageDown => screen_move(app, tx, 1),
        KeyCode::Home => app.sel = 0,
        KeyCode::End => {
            let n = result_row_count(app);
            if n > 0 {
                app.sel = n - 1;
            }
        }
        // Enter always opens the whole row (R42b); `v` opens the cell directly.
        KeyCode::Enter => open_row_popup(app),
        _ => {}
    }
}

/// Handle a Redis input dialog. Enter turns the input into a command and routes
/// it through the confirmation layer; the pattern dialog is read-only-safe and
/// applies immediately.
pub(crate) fn redis_prompt_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut p) = app.redis_prompt.take() else {
        return;
    };
    if k.code == KeyCode::Esc {
        app.redis_pending_batch = None;
        app.flash(t("已取消").into());
        return;
    }
    if k.code != KeyCode::Enter {
        p.input.input(k);
        app.redis_prompt = Some(p);
        return;
    }
    let input = p.input.lines().join("\n");
    match p.kind {
        RedisPromptKind::Pattern => {
            let pat = input.trim();
            app.redis_scan.pattern = if pat.is_empty() {
                "*".to_string()
            } else {
                pat.to_string()
            };
            app.redis_value = None;
            app.clear_grid();
            app.redis_selected.clear();
            app.redis_anchor = None;
            app.status = tf("匹配模式 → {}", &[&(app.redis_scan.pattern)]);
            start_redis_scan(app, tx, true);
            return;
        }
        RedisPromptKind::BatchConfirm => {
            let need = app
                .redis_pending_batch
                .as_ref()
                .and_then(|r| r.typed_confirm)
                .unwrap_or(0);
            let typed = input.trim();
            if typed != need.to_string() && !typed.eq_ignore_ascii_case("YES") {
                app.status = tf("输入不匹配（需 {} 或 YES）", &[&need]);
                app.redis_prompt = Some(p);
                return;
            }
            if let Some(rc) = app.redis_pending_batch.take() {
                if !rc.batch.is_empty() {
                    if !rc.remove_in_place.is_empty() {
                        redis_remove_keys_in_place(app, &rc.remove_in_place);
                    }
                    run_redis_batch(app, tx, rc.db, rc.batch, rc.reload_list);
                }
            }
            return;
        }
        RedisPromptKind::BatchTtl => {
            let ttl = input.trim().to_string();
            let plan = match redis_plan_batch(RedisBatchKind::Ttl, &p.batch, false, &ttl) {
                Ok(plan) => plan,
                Err(e) => {
                    app.status = format!("✗ {e}");
                    app.redis_prompt = Some(p);
                    return;
                }
            };
            let n = p.batch.len();
            let pattern = app.redis_scan.pattern.clone();
            redis_open_batch_confirm(
                app,
                plan.commands,
                p.batch.clone(),
                plan.typed_confirm,
                plan.summary,
            );
            if let Some(c) = app.confirm.as_mut() {
                c.reasons = vec![
                    tf(
                        "将对 {} 个 key 设置 TTL={}s（模式 {}）",
                        &[&n, &ttl, &pattern],
                    ),
                    t("Enter 后立即执行").into(),
                ];
            }
            return;
        }
        RedisPromptKind::BatchRenamePrefix => {
            let plan = match redis_plan_batch(RedisBatchKind::RenamePrefix, &p.batch, false, &input)
            {
                Ok(plan) => plan,
                Err(e) => {
                    app.status = format!("✗ {e}");
                    app.redis_prompt = Some(p);
                    return;
                }
            };
            let Some((old, new)) = input.split_once('=') else {
                return;
            };
            let renamed = redis_prefix_rename_plan(
                &p.batch.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
                old,
                new,
            )
            .len();
            let pattern = app.redis_scan.pattern.clone();
            redis_open_batch_confirm(
                app,
                plan.commands,
                p.batch.clone(),
                plan.typed_confirm,
                plan.summary,
            );
            if let Some(c) = app.confirm.as_mut() {
                c.reasons = vec![
                    tf(
                        "将重命名 {} 个 key：{} → {}（模式 {}）",
                        &[&renamed, &old, &new, &pattern],
                    ),
                    t("Enter 后立即执行").into(),
                ];
            }
            return;
        }
        // R81: `T` on the key list parses seconds / `ms` / `m` / `h` / `d` and
        // opens the red layer. The new TTL is also reflected in the loaded list
        // in place, so the browser never rescans from page one.
        RedisPromptKind::TtlKey => {
            let plan = match redis_ttl_command(&p.key_display, &input) {
                Ok(plan) => plan,
                Err(e) => {
                    app.status = format!("✗ {e}");
                    app.redis_prompt = Some(p);
                    return;
                }
            };
            app.confirm = Some(Confirm {
                sql: plan.command.clone(),
                reasons: vec![
                    tf(
                        "将 key {} 的 TTL 设为 {}（覆盖当前 TTL，不可撤销）",
                        &[&(p.key_display), &(plan.label)],
                    ),
                    t("Enter 执行 · Esc 取消").into(),
                ],
                refresh: false,
                clear_batch: false,
                conn: None,
                redis: Some(RedisConfirm {
                    db: app.redis_db,
                    cmd: plan.command,
                    batch: Vec::new(),
                    batch_keys: Vec::new(),
                    reload_value: None,
                    reload_list: false,
                    typed_confirm: None,
                    summary: String::new(),
                    remove_in_place: Vec::new(),
                    set_ttl_in_place: vec![(p.key_raw.clone(), plan.ttl_secs)],
                }),
                mongo: None,
            });
            app.status = t("确认写入 · Enter 执行 · Esc 取消").into();
            return;
        }
        _ => {}
    }
    let cmd = redis_prompt_command(p.kind, &p.key_display, &p.field, &input);
    if cmd.is_empty() {
        return;
    }
    let (reload_value, reload_list) = match p.kind {
        // A renamed key has a new name, so just refresh the list.
        RedisPromptKind::Rename => (None, true),
        RedisPromptKind::StringValue | RedisPromptKind::HashField => {
            (Some(p.key_raw.clone()), false)
        }
        RedisPromptKind::Ttl => (Some(p.key_raw.clone()), true),
        RedisPromptKind::Pattern
        | RedisPromptKind::TtlKey
        | RedisPromptKind::BatchTtl
        | RedisPromptKind::BatchRenamePrefix
        | RedisPromptKind::BatchConfirm => (None, false),
    };
    app.confirm = Some(Confirm {
        sql: cmd.clone(),
        reasons: vec![
            tf("将执行 {}", &[&(truncate_disp(&one_line(&cmd), 60))]),
            t("Enter 执行 · Esc 取消").into(),
        ],
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: Some(RedisConfirm {
            db: app.redis_db,
            cmd,
            batch: Vec::new(),
            batch_keys: Vec::new(),
            reload_value,
            reload_list,
            typed_confirm: None,
            summary: String::new(),
            remove_in_place: Vec::new(),
            set_ttl_in_place: Vec::new(),
        }),
        mongo: None,
    });
    app.status = t("确认写入 · Enter 执行 · Esc 取消").into();
}

// ─── R104: per-key memory usage sampling ─────────────────────────────────────
//
// An explicit `M` on the key list measures the *loaded* window with
// `MEMORY USAGE <key> SAMPLES 0`, at most [`REDIS_MEM_SAMPLE_LIMIT`] keys and
// [`REDIS_MEM_CONCURRENCY`] in flight. The result is a session-only
// `HashMap<key_raw, Option<u64>>`: `None` is a failed / unsupported key and
// renders as `· ?`. Nothing here polls — a sample runs only on the keystroke,
// and the cache survives page loads / rescans until `Shift-M` (or a connection
// / db switch) clears it.

/// R104: how many keys one `M` sample covers. The list is sampled in its
/// current on-screen order, so the first 500 rows — the ones being looked at —
/// are measured; a longer list reports the truncation in the status bar.
pub(crate) const REDIS_MEM_SAMPLE_LIMIT: usize = 500;

/// R104: `MEMORY USAGE` probes in flight at once. Reuses the R96 `Ctrl-P`
/// fan-out so a long list stays quick without flooding the server.
pub(crate) const REDIS_MEM_CONCURRENCY: usize = 4;

/// R104: per-probe ceiling. `SAMPLES 0` walks a whole collection, so this is
/// more generous than the R96 ping; a key past it is reported as `· ?` rather
/// than holding up the batch.
pub(crate) const REDIS_MEM_TIMEOUT: Duration = Duration::from_secs(10);

/// R104: live progress of an `M` sample. `gen` drops a late partial from a
/// sample the user already replaced (a connection / db switch bumps it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RedisMemProbe {
    pub(crate) gen: u64,
    pub(crate) done: usize,
    pub(crate) total: usize,
    pub(crate) truncated: bool,
}

/// R104: the `MEMORY USAGE` command for one key. The key is quoted / escaped
/// exactly like `DEL`, so a name with a quote or backslash cannot split the
/// command.
pub(crate) fn redis_memory_command(key_display: &str) -> String {
    format!(
        "MEMORY USAGE \"{}\" SAMPLES 0",
        key_display.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// R104: parse a `MEMORY USAGE` reply into bytes. A JSON integer (or a quoted /
/// bare integer string) is a value; `null` (nil), a non-numeric string and
/// every other shape are `None` (rendered as `· ?`). A driver / server error
/// never reaches here — the caller maps it to `None` too.
pub(crate) fn parse_memory_usage(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        serde_json::Value::String(s) => {
            let trimmed = s.trim().trim_matches('"').trim();
            trimmed.parse::<u64>().ok()
        }
        _ => None,
    }
}

/// R104: a compact memory size (`12.3KB` / `45.6MB` / `210MB` / `512B`). One
/// decimal, base 1024, no space before the unit, and a `.0` fraction dropped so
/// a round value stays short. Pure display — the raw byte count is what the
/// summary sums.
pub(crate) fn redis_mem_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0usize;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        let num = format!("{v:.1}");
        let num = num.strip_suffix(".0").unwrap_or(&num);
        format!("{num}{}", UNITS[unit])
    }
}

/// R104: the grey tail appended to a key row. `None` when the key was never
/// sampled (no tail); `Some("· ?")` when the sample failed / is unsupported;
/// `Some("· 12.3KB")` otherwise.
pub(crate) fn redis_mem_tail(mem: Option<&Option<u64>>) -> Option<String> {
    match mem {
        None => None,
        Some(None) => Some("· ?".to_string()),
        Some(Some(bytes)) => Some(format!("· {}", redis_mem_size(*bytes))),
    }
}

/// R104: re-order the loaded keys by memory descending. A key with a value
/// ranks above an unsampled / failed one; within each group the original order
/// is preserved (`sort_by` is stable), so an unsampled tail never shuffles.
/// `on == false` is a no-op, which is how the `Ctrl-M` twin restores the order.
pub(crate) fn redis_mem_sort_keys(
    keys: &mut [RedisKeyInfo],
    mem: &HashMap<String, Option<u64>>,
    on: bool,
) {
    if !on {
        return;
    }
    keys.sort_by(|a, b| {
        let av = mem.get(&a.key_raw).and_then(|v| *v);
        let bv = mem.get(&b.key_raw).and_then(|v| *v);
        match (av, bv) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

/// R104: the `Top: … · 共采样 N 键 · 合计 …` summary over the keys currently in
/// the list that carry a cached sample. `None` when nothing was sampled.
pub(crate) fn redis_mem_summary(
    mem: &HashMap<String, Option<u64>>,
    keys: &[RedisKeyInfo],
) -> Option<String> {
    let mut count = 0usize;
    let mut total = 0u64;
    let mut top: Option<(String, u64)> = None;
    for k in keys {
        let Some(v) = mem.get(&k.key_raw) else {
            continue;
        };
        count += 1;
        if let Some(bytes) = *v {
            total = total.saturating_add(bytes);
            if top.as_ref().map(|(_, tb)| bytes > *tb).unwrap_or(true) {
                top = Some((fix_double_encoding(&k.key_display), bytes));
            }
        }
    }
    if count == 0 {
        return None;
    }
    match top {
        Some((name, bytes)) => Some(tf(
            "Top: {} {} · 共采样 {} 键 · 合计 {}",
            &[
                &name,
                &redis_mem_size(bytes),
                &count,
                &redis_mem_size(total),
            ],
        )),
        // Every sampled key failed (e.g. Redis < 4.0): name the count, not a
        // bogus top.
        None => Some(tf(
            "共采样 {} 键 · 均不可用（MEMORY USAGE 需要 Redis 4.0+）",
            &[&count],
        )),
    }
}

/// R104: `M` on the key list — sample the loaded window with bounded
/// concurrency. A no-op (with a hint) on an empty list; otherwise it spawns the
/// op and seeds the progress line. The cache is *not* cleared: a re-press
/// overwrites entries as results arrive.
pub(crate) fn start_redis_mem_probe(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        app.status = t("✗ 未选择连接").into();
        return;
    };
    let all = &app.redis_scan.keys;
    if all.is_empty() {
        app.status = t("还没有 key 可采样").into();
        return;
    }
    let truncated = all.len() > REDIS_MEM_SAMPLE_LIMIT;
    let targets: Vec<(String, String)> = all
        .iter()
        .take(REDIS_MEM_SAMPLE_LIMIT)
        .map(|k| (k.key_raw.clone(), k.key_display.clone()))
        .collect();
    let total = targets.len();
    app.redis_mem_gen = app.redis_mem_gen.wrapping_add(1);
    let gen = app.redis_mem_gen;
    app.redis_mem_probe = Some(RedisMemProbe {
        gen,
        done: 0,
        total,
        truncated,
    });
    app.status = if truncated {
        tf(
            "采样中 0/{} · 仅前 {}（共 {} key）",
            &[&total, &(REDIS_MEM_SAMPLE_LIMIT), &(all.len())],
        )
    } else {
        tf("采样中 0/{}", &[&total])
    };
    app.spawn(
        tx,
        Op::RedisMemProbe {
            cfg: Box::new(cfg),
            db: app.redis_db,
            keys: targets,
            gen,
            truncated,
        },
    );
}

/// R104: `Shift-M` (and the connection / db switch) — drop the session cache and
/// its ordering, then restore the list's natural order.
pub(crate) fn clear_redis_mem_cache(app: &mut App) {
    let had = !app.redis_mem.is_empty() || app.redis_mem_sort;
    app.redis_mem.clear();
    app.redis_mem_sort = false;
    app.redis_mem_probe = None;
    apply_redis_filter(app);
    app.status = if had {
        t("已清除内存采样缓存").into()
    } else {
        t("内存采样缓存为空").into()
    };
}

/// R104: `Ctrl-M` — re-order the loaded keys by memory descending, again to
/// restore the scan / TTL order (the R96 `O` two-state twin). A no-op before the
/// first sample.
pub(crate) fn toggle_redis_mem_sort(app: &mut App) {
    if app.redis_mem.is_empty() {
        app.status = t("先按 M 采样内存").into();
        return;
    }
    app.redis_mem_sort = !app.redis_mem_sort;
    apply_redis_filter(app);
    app.status = if app.redis_mem_sort {
        tf(
            "内存排序：降序 · {} 个 key",
            &[&(app.redis_scan.keys.len())],
        )
    } else {
        tf(
            "内存排序：原序 · {} 个 key",
            &[&(app.redis_scan.keys.len())],
        )
    };
}
