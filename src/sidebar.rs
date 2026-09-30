use crate::prelude::*;
use crate::*;

pub(crate) fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub(crate) fn connect_selected(app: &mut App, tx: &Tx) {
    let Some(idx) = app.conn_list.selected() else {
        return;
    };
    let Some(cfg) = app.connections.get(idx).cloned() else {
        return;
    };
    // Connecting from the picker has no previous connection to carry a pointer
    // from, but a connection remembered from an earlier session still restores.
    let restore = app.conn_pointers.get(&cfg.id).cloned();
    let prev = app.selected.as_ref().map(|c| c.id.clone());
    app.last_conn_id = prev;
    activate_connection(app, tx, cfg, restore, None);
}

/// R41: switch to the connection at `idx` in the picker order, remembering where
/// the user was on both connections. This is the fast path shared by `Alt-<n>`
/// (direct) and `Alt-Tab` / `Alt-`` (toggle with the previous connection).
pub(crate) fn switch_connection(app: &mut App, tx: &Tx, idx: usize) {
    let Some(target) = app.connections.get(idx).cloned() else {
        return;
    };
    if app.selected.as_ref().map(|c| c.id.as_str()) == Some(target.id.as_str()) {
        app.status = tf("已在连接 {} · 无需切换", &[&(target.name)]);
        return;
    }
    let prev_id = app.selected.as_ref().map(|c| c.id.clone());
    let carry = prev_id.as_ref().map(|_| snapshot_pointer(app));
    if let (Some(pid), Some(p)) = (prev_id.clone(), carry.clone()) {
        app.conn_pointers.insert(pid, p);
    }
    // Prefer this connection's own last position; fall back to carrying the
    // current connection's database / table names (same-named restore).
    let restore = app.conn_pointers.get(&target.id).cloned().or(carry);
    let notice = (!app.editor_sql().trim().is_empty())
        .then(|| t("编辑器仍有未提交内容（切连接不会清空，Ctrl-J 可执行）").to_string());
    app.last_conn_id = prev_id;
    app.conn_list.select(Some(idx));
    activate_connection(app, tx, target, restore, notice);
}

/// `Alt-<n>`: jump straight to the Nth connection in the picker order.
pub(crate) fn quick_switch_connection(app: &mut App, tx: &Tx, n: usize) {
    if n == 0 {
        return;
    }
    if app.connections.is_empty() {
        app.status = t("还没有连接 · c 新建").into();
        return;
    }
    if n > app.connections.len() {
        app.status = tf(
            "没有第 {} 个连接（共 {} 个）",
            &[&n, &(app.connections.len())],
        );
        return;
    }
    switch_connection(app, tx, n - 1);
}

/// `Alt-Tab` / `Alt-``: toggle between the current connection and the one used
/// just before it — the fastest way to compare two connections.
pub(crate) fn toggle_last_connection(app: &mut App, tx: &Tx) {
    let Some(cur) = app.selected.as_ref().map(|c| c.id.clone()) else {
        app.status = t("先连接一个数据库").into();
        return;
    };
    let Some(prev) = app.last_conn_id.clone() else {
        app.status = t("还没有上一个连接（Alt+数字 切换一次后即可对切）").into();
        return;
    };
    if prev == cur {
        app.status = t("上一个连接就是当前连接").into();
        return;
    }
    let Some(idx) = app.connections.iter().position(|c| c.id == prev) else {
        app.status = t("上一个连接已不存在").into();
        app.last_conn_id = None;
        return;
    };
    switch_connection(app, tx, idx);
}

/// Where the user currently is, for the per-connection restore memory.
pub(crate) fn snapshot_pointer(app: &App) -> ConnPointer {
    ConnPointer {
        db: app.current_db(),
        schema: app.schema.clone(),
        table: app
            .page_state
            .as_ref()
            .map(|p| p.table.clone())
            .or_else(|| app.selected_table().map(|t| t.name.clone())),
    }
}

/// Select `cfg` as the active connection and start loading its database list.
/// `restore` is the pointer a switch wants to land on once the lists arrive;
/// `notice` is a one-shot warning folded into the landing status.
pub(crate) fn activate_connection(
    app: &mut App,
    tx: &Tx,
    cfg: ConnectionConfig,
    restore: Option<ConnPointer>,
    notice: Option<String>,
) {
    app.conn_gen = app.conn_gen.wrapping_add(1);
    let gen = app.conn_gen;
    app.selected = Some(cfg.clone());
    // R47b: the pool is opening now; the dot turns half-filled until the
    // database list (or the status refresh) confirms it is live.
    app.conn_connecting.insert(cfg.id.clone());
    app.conn_live.remove(&cfg.id);
    // R43: the active connection's tree root starts expanded so its databases
    // (or, without a database layer, its tables) are visible immediately.
    app.tree_conn_open.insert(cfg.id.clone());
    app.tree_conn_closed.remove(&cfg.id);
    app.tree_db_closed.clear();
    app.picker_open = false;
    app.backend_kind = backend_for_connection(&cfg);
    // The pointer is only meaningful for the engines whose browse state is a
    // database / table pair; Redis has no such list.
    app.pending_restore = if app.backend_kind == Backend::Redis {
        None
    } else {
        restore
    };
    app.switch_notice = notice;
    // R52: name the server once, the first time this connection is used. The
    // version is session-cached per connection id, so switching back never
    // re-queries it.
    if !app.server_versions.contains_key(&cfg.id) {
        app.spawn(tx, Op::ServerVersion(Box::new(cfg.clone())));
    }
    app.schemas.clear();
    app.schema.clear();
    app.schemas_db.clear();
    // R43: drop the previous connection's databases / tables so the tree never
    // shows them under the new root during the async reload.
    app.databases.clear();
    app.db_index = 0;
    app.tables.clear();
    app.tables_all.clear();
    app.table_list = ListState::default();
    app.tree_search.clear();
    app.tree_search_prompt = None;
    app.tree_search_prev = None;
    app.clear_grid();
    app.pinned_result = None;
    app.script = None;
    app.ddl = None;
    app.page_state = None;
    app.col_offset = 0;
    app.col_cursor = 0;
    app.cell_popup = None;
    app.row_popup = None;
    app.cmd_output.clear();
    app.redis_value = None;
    app.redis_prompt = None;
    app.redis_scan = RedisScanState::default();
    app.redis_list = ListState::default();
    app.redis_filter.clear();
    app.redis_filter_prompt = None;
    app.redis_jump_letter = None;
    app.mongo_filter.clear();
    app.mongo_page = 0;
    app.set_placeholder();
    app.loading = true;
    app.status = match first_ssh_layer(&cfg) {
        Some(ssh) => tf(
            "SSH 连接 {}@{}:{} → {}…",
            &[&(ssh.user), &(ssh.host), &(ssh.port), &(cfg.name)],
        ),
        None => tf("连接 {} ({})…", &[&(cfg.name), &(cfg.db_type.as_str())]),
    };
    if app.backend_kind == Backend::Redis {
        // Redis exposes 16 fixed logical databases; there is nothing to
        // enumerate, so go straight to the first SCAN page.
        app.databases = (0..16).map(|i| i.to_string()).collect();
        app.db_index = 0;
        app.redis_db = 0;
        start_redis_scan(app, tx, true);
        app.spawn(tx, Op::History(Box::new(cfg)));
    } else {
        app.spawn(tx, Op::Databases(Box::new(cfg), gen));
    }
    // R43: land the sidebar tree cursor on the newly active root so a switch is
    // reflected immediately, before the database list arrives.
    rebuild_side_rows(app);
    if let Some(pos) = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Conn { idx, .. } if side_is_active(app, *idx)))
    {
        app.side_sel = pos;
        side_mirror_table(app);
    }
}

/// Pick the interaction mode for a connection: a Redis connection opens the key
/// browser, a MongoDB connection the document browser, everything else SQL.
pub(crate) fn backend_for_connection(cfg: &ConnectionConfig) -> Backend {
    match cfg.db_type.as_str() {
        "redis" | "keydb" | "valkey" => Backend::Redis,
        "mongodb" | "mongo" => Backend::Mongo,
        _ => Backend::Sql,
    }
}

/// Does an SSH handshake error mean the credentials were rejected (or unusable)?
/// The kernel phrases these with `auth failed` / `authentication failed` / a
/// rejected `remaining_methods` set, or a missing credential / agent.
pub(crate) fn classify_ssh_auth_error(error: &str) -> bool {
    let l = error.to_ascii_lowercase();
    [
        "authentication failed",
        "auth failed",
        "auth probe failed",
        "password auth",
        "key auth",
        "rejected",
        "no ssh password or key",
        "ssh-agent",
        "keyboard-interactive",
        "failed to load ssh key",
    ]
    .iter()
    .any(|needle| l.contains(needle))
}

/// Does an SSH handshake error mean the jump host itself could not be reached?
pub(crate) fn classify_ssh_host_error(error: &str) -> bool {
    let l = error.to_ascii_lowercase();
    [
        "ssh connection failed",
        "ssh connection timed out",
        "connection refused",
        "no route to host",
        "network is unreachable",
        "failed to lookup",
        "name or service not known",
        "dns error",
        "connection reset",
    ]
    .iter()
    .any(|needle| l.contains(needle))
}

/// Does a driver-level (post-handshake) failure mean the forwarded channel
/// closed — i.e. the far-side database port is not serving? SSH handshake
/// errors are excluded: those are classified as auth / host failures instead.
pub(crate) fn classify_ssh_remote_error(error: &str) -> bool {
    let l = error.to_ascii_lowercase();
    if l.contains("ssh connection failed")
        || l.contains("ssh authentication")
        || l.contains("ssh auth")
        || l.contains("ssh connection timed out")
    {
        return false;
    }
    [
        "connection closed",
        "connection reset",
        "unexpected eof",
        "broken pipe",
        "input/output error",
        "os error 104",
        "os error 32",
        "server closed the connection",
        "lost connection",
    ]
    .iter()
    .any(|needle| l.contains(needle))
}

/// Name the failing stage of an SSH-tunneled connection. The kernel reports the
/// SSH handshake verbatim; a driver-level failure once the tunnel is up is
/// usually the *far-side* database endpoint being unreachable from the jump
/// host, which the raw error hides. Categories: authentication, jump host
/// unreachable, remote database unreachable.
pub(crate) fn ssh_connect_error_message(cfg: &ConnectionConfig, error: &str) -> String {
    let Some(ssh) = first_ssh_layer(cfg) else {
        return tf("无法列举数据库（{}）", &[&(error)]);
    };
    let hop = format!("{}@{}:{}", ssh.user, ssh.host, ssh.port);
    if classify_ssh_auth_error(error) {
        return tf(
            "SSH 认证失败（{}）：凭据被拒绝或不可用，请检查密码 / 密钥 / agent",
            &[&(hop)],
        );
    }
    // Checked before the host category: a closed far-side port surfaces to the
    // driver as a reset / EOF, which the host matcher would also catch.
    if classify_ssh_remote_error(error) {
        return tf(
            "隧道已建立但远端数据库不可达（{} → {}:{}）：请确认跳板机能访问该地址",
            &[&(hop), &(cfg.host), &(cfg.port)],
        );
    }
    if classify_ssh_host_error(error) {
        return tf(
            "SSH 主机不可达（{}）：无法建立连接，请检查地址 / 端口 / 网络",
            &[&(hop)],
        );
    }
    tf("SSH 隧道连接失败（{}）：{}", &[&(hop), &(error)])
}

/// Translate the DBX kernel's secret-store failure codes (v0.6.27+) into an
/// actionable hint. From v0.6.27 the kernel encrypts connection / plugin / AI /
/// tunnel secrets with a key that lives *outside* the database (OS keychain, or
/// `DBX_SECRET_KEY_FILE` / `DBX_SECRET_KEY`); a headless CLI can only read it
/// when that provider is reachable, and it never provisions a key itself. dbxt
/// passes the key resolution straight through to the kernel — this only turns
/// the raw code into something a TUI user can act on.
pub(crate) fn secret_store_error_hint(error: &str) -> Option<&'static str> {
    if error.contains("DATA_MIGRATION_REQUIRED") {
        return Some(t(
            "DBX 数据安全升级未完成：请先打开 DBX 桌面端并完成「数据安全升级向导」（dbxt 不会迁移数据）；无桌面环境可用 DBX_SECRET_KEY_FILE 提供密钥",
        ));
    }
    if error.contains("SECRET_KEY_UNAVAILABLE") || error.contains("KEY_PROVIDER_UNAVAILABLE") {
        return Some(t(
            "读不到 DBX 数据加密密钥：桌面端把密钥存放在系统钥匙串，本进程无法访问；请改用带系统钥匙串支持的构建，或用 DBX_SECRET_KEY_FILE / DBX_SECRET_KEY 提供密钥",
        ));
    }
    if error.contains("ENCRYPTED_DATA_KEY_MISSING")
        || error.contains("SECRET_KEY_MISMATCH")
        || error.contains("SECRET_KEY_INVALID")
        || error.contains("MISSING_EXTERNAL_KEY")
        || error.contains("KEY_FILE_UNAVAILABLE")
    {
        return Some(t(
            "DBX 数据加密密钥缺失或不匹配：请提供创建该库时所用的密钥（DBX_SECRET_KEY_FILE / DBX_SECRET_KEY），或重新运行桌面端升级向导",
        ));
    }
    None
}

/// Replace a raw kernel secret-store error with its hint, leaving every other
/// error untouched.
pub(crate) fn humanize_backend_error(error: &str) -> String {
    secret_store_error_hint(error)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

/// R58: true when a query error is a timeout rather than a real SQL / data
/// error. Covers the kernel's client-side cancel ("Query timed out after N
/// seconds"), a PostgreSQL server `statement_timeout`, and MySQL's
/// `max_execution_time` interrupt. Shared so the `Op::Query` path can rewrite a
/// raw driver message into a readable hint.
pub(crate) fn is_query_timeout_error(error: &str) -> bool {
    let l = error.to_ascii_lowercase();
    l.contains("query timed out after")
        || l.contains("canceling statement due to statement timeout")
        || l.contains("cancelling statement due to statement timeout")
        || l.contains("statement timeout")
        || l.contains("maximum statement execution time exceeded")
        || l.contains("max_execution_time")
        || l.contains("query execution was interrupted")
}

/// R58: a readable, bilingual timeout message. `timeout_secs == 0` means the
/// connection has no limit, so a timeout there is generic (a driver / watchdog
/// bound), otherwise the configured limit is named and the user is told how to
/// react. Non-timeout errors pass through unchanged.
pub(crate) fn query_error_text(error: &str, timeout_secs: u64) -> String {
    if !is_query_timeout_error(error) {
        return error.to_string();
    }
    if timeout_secs > 0 {
        tf("查询超时（{}s），可调大超时或优化语句", &[&timeout_secs])
    } else {
        t("查询超时，可调大超时或优化语句").to_string()
    }
}

/// Human-readable text for a best-effort host-key notice.
pub(crate) fn ssh_notice_text(notice: &SshHostKeyNotice) -> String {
    match notice.kind {
        SshHostKeyNoticeKind::Changed => tf(
            "⚠ SSH 主机密钥已变化（{}:{}），可能被中间人攻击",
            &[&(notice.host), &(notice.port)],
        ),
        SshHostKeyNoticeKind::Rejected => tf(
            "SSH 主机密钥被拒绝（{}:{}）",
            &[&(notice.host), &(notice.port)],
        ),
        SshHostKeyNoticeKind::LearnFailed => tf(
            "SSH 主机密钥已接受但无法保存（{}:{}）：仅本次会话信任",
            &[&(notice.host), &(notice.port)],
        ),
    }
}

/// Answer a pending kernel SSH prompt. The handshake task is suspended on the
/// one-shot responder, so answering (or dropping it) resumes it.
pub(crate) fn ssh_prompt_key(app: &mut App, k: KeyEvent) {
    let Some(mut state) = app.ssh_prompt.take() else {
        return;
    };
    let answer = match state.request.kind {
        SshPromptKind::HostKeyVerify | SshPromptKind::HostKeyChanged => match k.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(SshPromptAnswer::Accept { remember: true })
            }
            // Trust the key for this session only (do not write known_hosts).
            KeyCode::Char('s') | KeyCode::Char('S') => {
                Some(SshPromptAnswer::Accept { remember: false })
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(SshPromptAnswer::Reject),
            _ => None,
        },
        SshPromptKind::SecretInput => match k.code {
            KeyCode::Enter => Some(SshPromptAnswer::Secret(state.input.clone())),
            KeyCode::Esc => Some(SshPromptAnswer::Reject),
            KeyCode::Backspace => {
                state.input.pop();
                None
            }
            KeyCode::Char(c) => {
                state.input.push(c);
                None
            }
            _ => None,
        },
        // A generic user-input prompt (e.g. a plugin bastion MFA code). With
        // fixed options a digit picks one; otherwise it is free-form text.
        SshPromptKind::UserInput => match k.code {
            KeyCode::Enter => Some(SshPromptAnswer::Secret(state.input.clone())),
            KeyCode::Esc => Some(SshPromptAnswer::Reject),
            KeyCode::Backspace => {
                state.input.pop();
                None
            }
            KeyCode::Char(c) if !state.request.options.is_empty() => {
                match c
                    .to_digit(10)
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| state.request.options.get(i as usize))
                {
                    Some(option) => Some(SshPromptAnswer::Secret(option.value.clone())),
                    None => {
                        state.input.push(c);
                        None
                    }
                }
            }
            KeyCode::Char(c) => {
                state.input.push(c);
                None
            }
            _ => None,
        },
        // dbxt does not use the SQLite worker, so this consent is always denied.
        SshPromptKind::WorkerUploadConsent => Some(SshPromptAnswer::Reject),
    };
    match answer {
        Some(ans) => {
            if let Some(tx) = state.responder.take() {
                let _ = tx.send(ans);
            }
            app.ssh_prompt = None;
        }
        None => app.ssh_prompt = Some(state),
    }
}

/// How many keys one `SCAN` page asks for.
pub(crate) const REDIS_SCAN_PAGE: usize = 100;
/// How many server-side SCAN cycles a single page performs. `SCAN` is a hint, so
/// a selective `MATCH` can return zero keys for several cycles; iterating a few
/// times server-side keeps an empty first page rare without loading the whole
/// keyspace.
pub(crate) const REDIS_SCAN_ITERATIONS: usize = 5;
/// How many documents one MongoDB page shows.
pub(crate) const MONGO_PAGE: usize = 50;

/// Request the next page of the Redis key browser. `reset` starts a fresh scan
/// (used after a pattern change / logical-db switch / write) instead of
/// appending; a reset also bumps the generation so a late reply is dropped.
pub(crate) fn start_redis_scan(app: &mut App, tx: &Tx, reset: bool) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    if reset {
        app.redis_scan.keys.clear();
        app.redis_scan.all.clear();
        app.redis_scan.cursor = 0;
        app.redis_scan.exhausted = false;
        app.redis_scan.gen = app.redis_scan.gen.wrapping_add(1);
        // R57: a fresh scan re-reads TTLs, so restart the local countdown clock.
        app.redis_ttl_clock = Instant::now();
    } else if app.redis_scan.exhausted || app.redis_scan.pending {
        // A page is already in flight: a second request would start from the
        // same cursor and append a duplicate page (the fresh-scan race).
        return;
    }
    let gen = app.redis_scan.gen;
    let cursor = app.redis_scan.cursor;
    let pattern = app.redis_scan.pattern.clone();
    app.redis_scan.pending = true;
    app.loading = true;
    app.spawn(
        tx,
        Op::RedisScan {
            cfg: Box::new(cfg),
            db: app.redis_db,
            cursor,
            pattern,
            count: REDIS_SCAN_PAGE,
            gen,
            append: !reset,
        },
    );
}

/// Load the selected key's typed value into the results pane.
pub(crate) fn open_redis_value(app: &mut App, tx: &Tx) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(key) = app
        .redis_list
        .selected()
        .and_then(|i| app.redis_scan.keys.get(i))
        .map(|k| (k.key_raw.clone(), k.key_display.clone()))
    else {
        app.status = t("先选中一个 key").into();
        return;
    };
    // R42: the key value view is a round-trip node (the detail is not).
    remember_redis_key(app, app.redis_db, &key.0, &key.1);
    app.loading = true;
    app.status = tf("加载 key {}…", &[&(fix_double_encoding(&key.1))]);
    app.spawn(
        tx,
        Op::RedisValue {
            cfg: Box::new(cfg),
            db: app.redis_db,
            key_raw: key.0,
        },
    );
}

/// Append the next page of a large Redis collection value.
pub(crate) fn redis_load_more(app: &mut App, tx: &Tx) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let Some(cursor) = view.scan_cursor else {
        app.status = t("已全部加载").into();
        return;
    };
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    app.loading = true;
    app.status = t("加载更多…").into();
    app.spawn(
        tx,
        Op::RedisMore {
            cfg: Box::new(cfg),
            db: app.redis_db,
            key_raw: view.key_raw,
            key_type: view.redis_type,
            cursor,
            count: 200,
        },
    );
}

/// Reload one MongoDB collection page.
pub(crate) fn reload_mongo_docs(app: &mut App, tx: &Tx, page: usize) {
    let Some(cfg) = app.selected.clone() else {
        return;
    };
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    app.mongo_gen = app.mongo_gen.wrapping_add(1);
    let gen = app.mongo_gen;
    app.loading = true;
    app.status = tf("加载 {} 文档…", &[&(fix_double_encoding(&coll))]);
    app.spawn(
        tx,
        Op::MongoDocs {
            cfg: Box::new(cfg),
            db: app.current_db(),
            collection: coll,
            page,
            page_size: MONGO_PAGE,
            filter: app.mongo_filter.clone(),
            gen,
        },
    );
}

/// Open the selected collection in the document browser (Mongo mode).
pub(crate) fn open_mongo_collection(app: &mut App, tx: &Tx) {
    app.mongo_page = 0;
    app.mongo_filter.clear();
    reload_mongo_docs(app, tx, 0);
}

/// Drop the current connection and show the connection picker again.
pub(crate) fn back_to_picker(app: &mut App) {
    // Remember where the user was so a later picker connect can restore it, and
    // invalidate any database list still in flight for the connection left.
    if let Some(cur) = app.selected.as_ref().map(|c| c.id.clone()) {
        let p = snapshot_pointer(app);
        app.conn_pointers.insert(cur, p);
    }
    app.conn_gen = app.conn_gen.wrapping_add(1);
    app.pending_restore = None;
    app.switch_notice = None;
    app.selected = None;
    app.tables.clear();
    app.tables_all.clear();
    app.table_filter.clear();
    app.tree_search.clear();
    app.tree_search_prompt = None;
    app.tree_search_prev = None;
    app.columns.clear();
    app.databases.clear();
    app.schemas.clear();
    app.schema.clear();
    app.schemas_db.clear();
    app.clear_grid();
    app.script = None;
    app.ddl = None;
    app.page_state = None;
    app.col_offset = 0;
    app.col_cursor = 0;
    app.cell_popup = None;
    app.redis_value = None;
    app.redis_scan = RedisScanState::default();
    app.redis_list = ListState::default();
    app.redis_selected.clear();
    app.redis_anchor = None;
    app.redis_pending_batch = None;
    app.redis_filter.clear();
    app.redis_filter_prompt = None;
    app.redis_jump_letter = None;
    app.picker_open = true;
}

/// Switch the Redis logical database and rescan it.
pub(crate) fn cycle_redis_db(app: &mut App, tx: &Tx, forward: bool) {
    app.redis_db = if forward {
        (app.redis_db + 1) % 16
    } else {
        (app.redis_db + 15) % 16
    };
    app.redis_value = None;
    app.clear_grid();
    app.set_placeholder();
    app.redis_selected.clear();
    app.redis_anchor = None;
    app.redis_filter.clear();
    app.redis_filter_prompt = None;
    app.redis_jump_letter = None;
    app.status = tf("redis db → {}", &[&(app.redis_db)]);
    start_redis_scan(app, tx, true);
}

/// Open the `/` pattern prompt (server-side SCAN MATCH).
pub(crate) fn open_redis_pattern_prompt(app: &mut App) {
    let mut ta = TextArea::from(vec![app.redis_scan.pattern.clone()]);
    ta.set_placeholder_text(t("例: app:*（留空回车 = 全部 *）"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Pattern,
        title: t("key 匹配模式（SCAN MATCH）").to_string(),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open a TTL edit prompt for the focused key.
pub(crate) fn open_redis_ttl_prompt(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let initial = if view.ttl >= 0 {
        view.ttl.to_string()
    } else {
        String::new()
    };
    let mut ta = TextArea::from(vec![initial]);
    ta.set_placeholder_text(t("秒数（-1 = 持久化，0 = 立即删除）"));
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Ttl,
        title: tf("设置 TTL · {}", &[&(view.key_display)]),
        key_display: view.key_display.clone(),
        key_raw: view.key_raw.clone(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open a rename prompt for the focused key.
pub(crate) fn open_redis_rename_prompt(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    let mut ta = TextArea::from(vec![view.key_display.clone()]);
    ta.move_cursor(CursorMove::End);
    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Rename,
        title: tf("重命名 key · {}", &[&(view.key_display)]),
        key_display: view.key_display.clone(),
        key_raw: view.key_raw.clone(),
        field: String::new(),
        batch: Vec::new(),
        input: ta,
    });
}

/// Open an edit prompt for the focused grid cell: a string key's whole body, or
/// a hash field's value. Other Redis types are read-only for now.
pub(crate) fn open_redis_edit(app: &mut App) {
    let Some(view) = app.redis_value.clone() else {
        return;
    };
    match &view.raw.data {
        RedisValueData::String { content, .. } => {
            let initial = redis_blob_editable_text(content).unwrap_or_default();
            let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
            ta.move_cursor(CursorMove::End);
            app.redis_prompt = Some(RedisPrompt {
                kind: RedisPromptKind::StringValue,
                title: tf("编辑 string · {}", &[&(view.key_display)]),
                key_display: view.key_display.clone(),
                key_raw: view.key_raw.clone(),
                field: String::new(),
                batch: Vec::new(),
                input: ta,
            });
        }
        RedisValueData::Hash { .. } => {
            let Some(field) = view.row_keys.get(app.sel).cloned() else {
                return;
            };
            if field.is_empty() {
                return;
            }
            let initial = app
                .grid
                .as_ref()
                .and_then(|g| g.rows.get(app.sel))
                .and_then(|r| r.get(1))
                .map(|v| v.text().to_string())
                .unwrap_or_default();
            let mut ta = TextArea::from(initial.split('\n').collect::<Vec<_>>());
            ta.move_cursor(CursorMove::End);
            app.redis_prompt = Some(RedisPrompt {
                kind: RedisPromptKind::HashField,
                title: tf("编辑 hash 字段 · {} · {}", &[&(view.key_display), &(field)]),
                key_display: view.key_display.clone(),
                key_raw: view.key_raw.clone(),
                field,
                batch: Vec::new(),
                input: ta,
            });
        }
        _ => {
            app.status = t("该类型暂不支持直接编辑，可用命令行修改").into();
        }
    }
}

/// Which write a Redis prompt will generate. Kept as data so the confirmation
/// layer shows the exact command before anything runs.
pub(crate) fn redis_prompt_command(
    kind: RedisPromptKind,
    key: &str,
    field: &str,
    input: &str,
) -> String {
    let q = |s: &str| {
        // Quote with double quotes and escape so spaces / quotes survive the
        // redis-cli tokenizer the backend uses.
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    };
    match kind {
        RedisPromptKind::Ttl => format!("EXPIRE {} {}", q(key), input.trim()),
        RedisPromptKind::Rename => format!("RENAME {} {}", q(key), q(input.trim())),
        RedisPromptKind::StringValue => format!("SET {} {}", q(key), q(input)),
        RedisPromptKind::HashField => format!("HSET {} {} {}", q(key), q(field), q(input)),
        RedisPromptKind::Pattern
        | RedisPromptKind::BatchTtl
        | RedisPromptKind::BatchRenamePrefix
        | RedisPromptKind::BatchConfirm => String::new(),
    }
}

pub(crate) fn result_row_count(app: &App) -> usize {
    if let Some(s) = &app.script {
        if s.drilled.is_none() {
            return s.outcomes.len();
        }
        return active_grid(app).map(|g| g.rows.len()).unwrap_or(0);
    }
    if app.struct_view == StructView::Ddl && app.ddl.is_some() {
        return 0;
    }
    app.grid.as_ref().map(|g| g.rows.len()).unwrap_or(0)
}

pub(crate) fn scroll(app: &mut App, tx: &Tx, delta: i32) {
    // wheel never steals keyboard focus
    match app.focus {
        Focus::Sidebar => {
            if app.selected.is_none() {
                let n = app.connections.len();
                if n > 0 {
                    let cur = app.conn_list.selected().unwrap_or(0) as i32;
                    let next = (cur + delta).clamp(0, n as i32 - 1);
                    app.conn_list.select(Some(next as usize));
                }
            } else {
                rebuild_side_rows(app);
                side_step(app, delta.unsigned_abs() as usize, delta > 0);
            }
        }
        Focus::Preview => {
            if app.struct_view == StructView::Ddl && app.ddl.is_some() {
                let d = app.ddl_scroll as i32 + delta;
                app.ddl_scroll = d.max(0) as u16;
            } else {
                move_cursor(app, tx, delta);
            }
        }
        Focus::Editor => {
            // let tui-textarea scroll itself
            let code = if delta < 0 {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            app.editor.input(KeyEvent::new(code, KeyModifiers::NONE));
        }
        Focus::CmdInput => {}
    }
}

pub(crate) fn mouse(app: &mut App, tx: &Tx, m: MouseEvent) {
    let r = app.rects;
    // Forensics first: every event the terminal actually delivered is recorded,
    // including the ones an overlay swallows, so `DBXT_MOUSE_DEBUG` shows the
    // truth rather than what we happened to act on.
    if app.mouse_debug {
        let desc = describe_mouse(&m);
        app.mouse_log.push_back(desc);
        while app.mouse_log.len() > MOUSE_DEBUG_LINES {
            app.mouse_log.pop_front();
        }
    }
    // Overlays own the wheel while they are open; scrolling the grid underneath a
    // modal would be invisible and confusing.
    let overlay_open = app.confirm.is_some()
        || app.edit_dialog.is_some()
        || app.cell_popup.is_some()
        || app.row_popup.is_some()
        || app.error_popup.is_some()
        || app.filter_prompt.is_some()
        || app.db_picker_open
        || app.snippet_open
        || app.template_open
        || app.col_picker_open
        || app.recent_open
        || app.table_jump_open
        || app.table_prompt.is_some()
        || app.tree_search_prompt.is_some()
        || app.help_open
        || app.help_mini;
    match m.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = m.kind == MouseEventKind::ScrollUp;
            if let Some(p) = app.cell_popup.as_mut() {
                p.scroll = if up {
                    p.scroll.saturating_sub(1)
                } else {
                    p.scroll.saturating_add(1)
                };
                return;
            }
            if let Some(p) = app.row_popup.as_mut() {
                // The row popup keeps its cursor on screen, so the wheel moves
                // the selection rather than a raw scroll offset.
                let visible = row_popup_visible(p).len();
                let last = visible.saturating_sub(1);
                let cur = p.cursor.min(last);
                p.cursor = if up {
                    cur.saturating_sub(1)
                } else {
                    (cur + 1).min(last)
                };
                return;
            }
            if app.help_open {
                app.help_scroll = if up {
                    app.help_scroll.saturating_sub(1)
                } else {
                    app.help_scroll.saturating_add(1)
                };
                return;
            }
            if app.help_mini {
                return;
            }
            if overlay_open {
                return;
            }
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if overlay_open => return,
        _ => {}
    }

    // ── swipe layer: a held button moving horizontally pans the columns ──
    //
    // A phone terminal that never emits a horizontal wheel encodes a left/right
    // swipe as `Drag(Left)` (button-event tracking) or `Moved` (any-event
    // tracking) — a gesture the wheel-only code could not see at all. Panning
    // ignores focus, exactly like the horizontal wheel.
    if !overlay_open {
        // A tap is only confirmed on its `Up`, so the press that starts a swipe
        // does not also select a row / jump the scrollbar. A gesture that turned
        // into a swipe drops the pending tap instead.
        if let MouseEventKind::Up(MouseButton::Left) = m.kind {
            let swipe = app.gesture.is_swipe();
            let _ = app.gesture.feed(m.kind, m.column, m.row, app.drag_pan);
            let tap = app.pending_tap.take();
            if let Some((cx, cy, double)) = tap {
                if !swipe {
                    result_tap(app, cx, cy, double);
                }
            }
            return;
        }
        if let Some(steps) = app.gesture.feed(m.kind, m.column, m.row, app.drag_pan) {
            if steps != 0 {
                pan_columns(app, steps);
            }
            if app.gesture.is_swipe() {
                app.pending_tap = None;
            }
            return;
        }
    }

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // ── the topmost modal owns the press ──
            //
            // Every layer below the pointer is hit-tested in the same order the
            // key router uses, so a click and a key can never disagree about
            // which surface is in front.
            if app.ssh_prompt.is_some() || app.edit_dialog.is_some() {
                return;
            }
            if app.confirm.is_some() {
                confirm_click(app, tx, m.column, m.row);
                return;
            }
            if app.history_confirm.is_some() {
                history_confirm_click(app, tx, m.column, m.row);
                return;
            }
            if app.error_popup.is_some() {
                error_popup_click(app, m.column, m.row);
                return;
            }
            // The cell popup sits on top of a drilled row popup; a click closes
            // it (exactly like `Enter` / `Esc`) and reveals the row underneath.
            if app.cell_popup.is_some() {
                app.cell_popup = None;
                return;
            }
            if app.row_popup.is_some() {
                row_popup_click(app, m.column, m.row);
                return;
            }
            if app.snippet_open
                || app.template_open
                || app.col_picker_open
                || app.recent_open
                || app.table_jump_open
                || app.table_prompt.is_some()
                || app.tree_search_prompt.is_some()
                || app.filter_prompt.is_some()
                || app.help_open
                || app.help_mini
            {
                return;
            }
            if r.db_picker_visible && rect_contains(r.db_picker, m.column, m.row) {
                let row_index = m.row as i32 - r.db_picker.y as i32 - 1;
                if row_index >= 0 {
                    let idx = row_index as usize;
                    if idx < db_entries(app).len() {
                        if app.db_list.selected() == Some(idx) {
                            db_picker_apply(app, tx, idx);
                        } else {
                            app.db_list.select(Some(idx));
                        }
                    }
                }
                return;
            }
            // hit-test order: picker > cmd > editor > results > sidebar
            if r.picker_visible && rect_contains(r.picker, m.column, m.row) {
                let row_index = m.row as i32 - r.picker.y as i32 - 1;
                if row_index >= 0 {
                    let idx = row_index as usize;
                    if idx < app.connections.len() {
                        if app.conn_list.selected() == Some(idx) {
                            connect_selected(app, tx);
                        } else {
                            app.conn_list.select(Some(idx));
                        }
                    }
                }
                return;
            }
            if rect_contains(r.cmd, m.column, m.row) {
                app.focus = Focus::CmdInput;
                return;
            }
            if rect_contains(r.editor, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_EDITOR) {
                    app.pane_override[PANE_EDITOR] = Some(false);
                }
                app.focus = Focus::Editor;
                // A click in the editor also places the caret where it landed
                // (the pane only has to be on screen for that; a collapsed pane
                // is opened above and gets its geometry on the next frame).
                editor_click(app, m.column, m.row);
                return;
            }
            if rect_contains(r.results, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_RESULTS) {
                    app.pane_override[PANE_RESULTS] = Some(false);
                    app.focus = Focus::Preview;
                    return;
                }
                app.focus = Focus::Preview;
                // A second press at the same spot is a double click: it opens the
                // row detail, the mouse equivalent of `Enter`.
                let now = app.now_ms();
                let double = app.tap.feed(now, m.column, m.row);
                // A touch swipe starts with the same press as a tap, so defer the
                // click to the matching `Up` and drop it when the gesture becomes a
                // swipe. Terminals that never send `Up` keep press-to-click
                // (`can_defer_tap`), so tapping can never regress.
                if app.gesture.can_defer_tap() {
                    app.pending_tap = Some((m.column, m.row, double));
                } else {
                    result_tap(app, m.column, m.row, double);
                }
                return;
            }
            if rect_contains(r.sidebar, m.column, m.row) {
                if pane_eff_collapsed(app, PANE_SIDEBAR) {
                    app.pane_override[PANE_SIDEBAR] = Some(false);
                    app.focus = Focus::Sidebar;
                    return;
                }
                app.focus = Focus::Sidebar;
                if app.selected.is_some() {
                    sidebar_click(app, tx, m.column, m.row);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if wheel_pans_columns(app, &m) {
                pan_columns(app, -1);
            } else {
                scroll(app, tx, -1);
            }
        }
        MouseEventKind::ScrollDown => {
            if wheel_pans_columns(app, &m) {
                pan_columns(app, 1);
            } else {
                scroll(app, tx, 1);
            }
        }
        // A touch screen's left/right swipe arrives as a horizontal wheel. Pan the
        // columns regardless of which pane has focus, so a swipe works even after
        // tapping the sidebar or the editor; when every column already fits the
        // event is simply ignored.
        MouseEventKind::ScrollLeft => {
            pan_columns(app, -1);
        }
        MouseEventKind::ScrollRight => {
            pan_columns(app, 1);
        }
        _ => {}
    }
}

/// Map a click inside the results pane back to a column index using the geometry
/// captured during the last render.
pub(crate) fn col_at_x(app: &App, rel_x: i32) -> Option<usize> {
    if rel_x < 0 {
        return None;
    }
    let mut x = app.grid_gutter as i32;
    if rel_x < x {
        return None; // row-number gutter
    }
    for ci in 0..app.grid_frozen {
        x += 1; // column spacing
        let w = app.grid_widths.get(ci).copied().unwrap_or(MIN_CELL_WIDTH) as i32;
        if rel_x < x + w {
            return Some(ci);
        }
        x += w;
    }
    x += 1; // gap between the pinned block and the scrollable window
    for k in 0..app.vis_cols {
        let ci = app.col_offset + k;
        let w = app.grid_widths.get(ci).copied().unwrap_or(MIN_CELL_WIDTH) as i32;
        if rel_x < x + w {
            return Some(ci);
        }
        x += w + 1;
    }
    None
}

pub(crate) fn result_click(app: &mut App, x: u16, y: u16) {
    if hbar_click(app, x, y) {
        return;
    }
    let area = app.rects.results;
    let rel = y as i32 - area.y as i32 - 2; // skip border + header row
    let rel_x = x as i32 - area.x as i32 - 1;
    if rel < 0 {
        return;
    }
    if app.grid_kind != GridKind::Columns {
        if let Some(ci) = col_at_x(app, rel_x) {
            app.col_cursor = ci;
        }
    }
    let h = (area.height as usize).saturating_sub(3).max(1);
    if rel as usize >= h {
        return;
    }
    let n = result_row_count(app);
    if n == 0 {
        return;
    }
    let start = app
        .sel
        .saturating_sub(h / 2)
        .min(n.saturating_sub(h.min(n)));
    let idx = start + rel as usize;
    if idx >= n {
        return;
    }
    if let Some(s) = &mut app.script {
        if s.drilled.is_none() {
            if s.sel == idx {
                drill_script(app, idx);
            } else {
                s.sel = idx;
            }
            return;
        }
    }
    app.sel = idx;
}

/// A completed tap (or, on a touch terminal that never sends `Up`, a press) in
/// the results pane. A double tap opens the row detail — the mouse / finger
/// equivalent of `Enter` — instead of only selecting the row. The grid kinds
/// (query, table data, Redis value, Mongo documents) all share this path, so the
/// value views behave exactly like the SQL side.
pub(crate) fn result_tap(app: &mut App, x: u16, y: u16, double: bool) {
    result_click(app, x, y);
    if double {
        open_row_popup(app);
    }
}

/// A press on a red confirmation layer: the two button rectangles act as the
/// `Enter` (confirm) and `Esc` (cancel) branches. A press anywhere else is
/// ignored, so the layer cannot be dismissed by a stray tap.
pub(crate) fn confirm_click(app: &mut App, tx: &Tx, x: u16, y: u16) {
    let r = app.rects;
    if rect_contains(r.confirm_ok, x, y) {
        confirm_key(app, tx, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    } else if rect_contains(r.confirm_cancel, x, y) {
        confirm_key(app, tx, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
}

/// The same two buttons for the query-history deletion layer.
pub(crate) fn history_confirm_click(app: &mut App, tx: &Tx, x: u16, y: u16) {
    let r = app.rects;
    if rect_contains(r.hist_ok, x, y) {
        history_confirm_key(app, tx, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    } else if rect_contains(r.hist_cancel, x, y) {
        history_confirm_key(app, tx, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
}

/// A press while the error box is open, with the same meaning as `Enter`
/// regardless of where it lands (the box owns the screen like it owns the
/// keyboard): the compact box widens to the full text, and an already-expanded
/// one pages down through a long error — closing only once the end is on screen,
/// so a tap can never close a box the user has not finished reading.
pub(crate) fn error_popup_click(app: &mut App, x: u16, y: u16) {
    let r = app.rects;
    let _ = (x, y);
    let expanded = app.error_popup.as_ref().is_some_and(|p| p.expanded);
    if !expanded {
        if let Some(p) = app.error_popup.as_mut() {
            p.expanded = true;
        }
        return;
    }
    let scroll = app.error_popup.as_ref().map(|p| p.scroll).unwrap_or(0);
    if scroll < r.error_max_scroll {
        let step = r.error_inner.height.max(1);
        if let Some(p) = app.error_popup.as_mut() {
            p.scroll = scroll.saturating_add(step).min(r.error_max_scroll);
        }
    } else {
        app.error_popup = None;
    }
}

/// A press inside the row-detail popup: select the entry under the pointer, and
/// on the second press at the same spot drill into the full cell (the mouse
/// equivalent of `Enter` / `v`).
pub(crate) fn row_popup_click(app: &mut App, x: u16, y: u16) {
    let r = app.rects;
    if !r.row_popup_visible || !rect_contains(r.row_popup_inner, x, y) {
        return;
    }
    // Physical (wrapped) line → entry position, so clicking a value that wrapped
    // over three rows still selects that value.
    let line = r.row_popup_scroll as usize + (y - r.row_popup_inner.y) as usize;
    let Some(&pos) = app.row_popup_hit.get(line) else {
        return;
    };
    if let Some(p) = app.row_popup.as_mut() {
        p.cursor = pos;
        p.count.clear();
    }
    let now = app.now_ms();
    if app.popup_tap.feed(now, x, y) {
        drill_row_popup_cell(app);
    }
}

/// Place the editor caret where the click landed. The click is translated
/// through the mirrored viewport, so it stays correct after the text scrolled
/// horizontally; `CursorMove::Jump` clamps the result, so a click past the last
/// line or past the end of a line lands on the nearest character.
pub(crate) fn editor_click(app: &mut App, x: u16, y: u16) {
    let area = app.rects.editor;
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };
    if !rect_contains(inner, x, y) {
        return;
    }
    let (row, col) = app.editor_vp.text_pos(x - inner.x, y - inner.y);
    if row as usize >= app.editor.lines().len() {
        // Clicking the empty space below the text jumps to the end of the buffer
        // (`Jump` with an out-of-range position clamps to the last character).
        app.editor.move_cursor(CursorMove::Jump(u16::MAX, u16::MAX));
    } else {
        app.editor.move_cursor(CursorMove::Jump(row, col));
    }
}

/// Clicking the horizontal progress bar jumps the column window to the clicked
/// position. Returns true when the click was on the bar (and handled).
pub(crate) fn hbar_click(app: &mut App, x: u16, y: u16) -> bool {
    if !app.rects.hbar_visible {
        return false;
    }
    // The `◀` / `▶` end buttons pan one window — the touch-friendly control for
    // phones whose terminal never sends a horizontal wheel.
    if rect_contains(app.rects.hbar_prev, x, y) {
        let step = app.vis_cols.max(1) as i32;
        pan_columns(app, -step);
        return true;
    }
    if rect_contains(app.rects.hbar_next, x, y) {
        let step = app.vis_cols.max(1) as i32;
        pan_columns(app, step);
        return true;
    }
    let r = app.rects.hbar;
    if r.width == 0 || y != r.y || x < r.x || x >= r.x + r.width {
        return false;
    }
    let Some(grid) = active_grid(app) else {
        return true;
    };
    let ncols = grid.columns.len();
    if ncols == 0 {
        return true;
    }
    let frozen = app.grid_frozen;
    let total = ncols.saturating_sub(frozen).max(1);
    let rel = (x - r.x) as usize;
    let frac = if r.width > 1 {
        rel as f64 / (r.width - 1) as f64
    } else {
        0.0
    };
    let target = frozen + (frac * (total.saturating_sub(1)) as f64).round() as usize;
    app.col_cursor = target.min(ncols - 1);
    app.col_offset = app.col_cursor;
    // R47b: a tap on the track is a horizontal scroll, so keep the bar up.
    app.poke_hbar();
    true
}

pub(crate) fn sidebar_click(app: &mut App, tx: &Tx, x: u16, y: u16) {
    let area = app.rects.sidebar;
    let rel = y as i32 - area.y as i32 - 1; // skip top border
    if rel < 0 {
        return;
    }
    // Redis: row 0 connection, row 1 logical DB, row 2 pattern, then keys.
    if app.backend_kind == Backend::Redis && app.selected.is_some() {
        if rel == 1 {
            open_db_picker(app);
            return;
        }
        let key_row = rel - 3;
        if key_row < 0 {
            return;
        }
        let _ = x;
        let n = app.redis_scan.keys.len();
        let cap = (area.height as usize).saturating_sub(5).max(1);
        let sel = app.redis_list.selected();
        let start = sel
            .unwrap_or(0)
            .saturating_sub(cap / 2)
            .min(n.saturating_sub(cap.min(n)));
        let idx = start + key_row as usize;
        if idx >= n {
            return;
        }
        if app.redis_list.selected() == Some(idx) {
            open_redis_value(app, tx);
        } else {
            app.redis_list.select(Some(idx));
        }
        return;
    }
    // SQL / MongoDB: the connection tree. Row 0 is the `/` filter row when the
    // connection has tables; the tree rows follow.
    rebuild_side_rows(app);
    let filter_rows: usize = if app.tables_all.is_empty() { 0 } else { 1 };
    let tree_row = rel - filter_rows as i32;
    if tree_row < 0 {
        return;
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
    let idx = start + tree_row as usize;
    if idx >= n {
        return;
    }
    // The `▸` / `▾` expander is its own hit target: clicking it folds or unfolds
    // that node even when the row is not the selected one, so a phone can tap a
    // triangle without first moving the tree cursor. The row's remaining cells
    // keep the two-tap select-then-activate behaviour.
    let rel_x = x as i32 - area.x as i32 - 1;
    if let Some(cols) = side_tri_cols(&app.side_rows[idx]) {
        if rel_x >= 0 && cols.contains(&(rel_x as u16)) {
            let open = side_row_open(app, &app.side_rows[idx].clone());
            app.side_sel = idx;
            side_mirror_table(app);
            if open {
                side_collapse(app);
            } else {
                side_expand(app, tx);
            }
            return;
        }
    }
    if app.side_sel == idx {
        side_activate(app, tx);
    } else {
        app.side_sel = idx;
        side_mirror_table(app);
    }
}

/// True when the tree row draws a `▸` / `▾` expander that can be clicked
/// (connections and databases have children; tables are leaves).
pub(crate) fn side_row_has_tri(row: &SideRow) -> bool {
    matches!(
        row,
        SideRow::Group { .. } | SideRow::Conn { .. } | SideRow::Db { .. }
    )
}

/// The two columns the expander occupies on a tree row, relative to the
/// sidebar's inner (bordered) area — the row is indented two cells per level and
/// the triangle is drawn before the node's symbol.
pub(crate) fn side_tri_cols(row: &SideRow) -> Option<std::ops::Range<u16>> {
    if !side_row_has_tri(row) {
        return None;
    }
    let off = 2 * side_row_depth(row) as u16;
    Some(off..off + 2)
}

/// True when this tree row's node is currently expanded (its children are
/// listed), i.e. it draws `▾`. Mirrors the rule `side_row_line` uses so a click
/// on the triangle folds exactly what the glyph promises.
pub(crate) fn side_row_open(app: &App, row: &SideRow) -> bool {
    match row {
        SideRow::Group { id, open, .. } => {
            // The stored `open` flag and the session collapse set must agree;
            // they do because the set is the single source of truth.
            let _ = open;
            !app.group_closed.contains(id)
        }
        SideRow::Conn { idx, .. } => side_conn_open(app, *idx),
        SideRow::Db { idx, db, .. } => {
            side_is_active(app, *idx)
                && *db == app.current_db()
                && side_root_cfg(app, *idx)
                    .is_some_and(|c| !app.tree_db_closed.contains(&db_node_key(&c.id, db)))
        }
        _ => false,
    }
}

/// Whether the sidebar shows a database row under the connection header.
pub(crate) fn sidebar_db_row(app: &App) -> bool {
    app.selected.is_some() && !app.databases.is_empty()
}

pub(crate) fn sidebar_db_label(app: &App) -> String {
    if app.backend_kind == Backend::Redis {
        tf(
            "redis db {} · {} keys · d 切换",
            &[&(app.redis_db), &(app.redis_scan.keys.len())],
        )
    } else if !app.schemas.is_empty() {
        tf(
            "{} · schema {} · d 切换",
            &[
                &(fix_double_encoding(&app.current_db())),
                &(fix_double_encoding(&app.schema)),
            ],
        )
    } else {
        tf("{} · d 切换", &[&(fix_double_encoding(&app.current_db()))])
    }
}

// ── sidebar connection tree (R43) ──

/// Index of the active connection in `app.connections` (matched by id, so a
/// re-sorted picker does not confuse the tree).
pub(crate) fn current_conn_index(app: &App) -> Option<usize> {
    let id = app.selected.as_ref()?.id.as_str();
    app.connections.iter().position(|c| c.id == id)
}

/// The connection a tree root index refers to. Indices `0..connections.len()`
/// are the saved list; one extra index past the end stands for the active
/// connection when it is not in that list (a test fixture, or a connection that
/// has just been created and not yet reloaded).
pub(crate) fn side_root_cfg(app: &App, idx: usize) -> Option<&ConnectionConfig> {
    if let Some(c) = app.connections.get(idx) {
        return Some(c);
    }
    if idx == app.connections.len() {
        return app.selected.as_ref();
    }
    None
}

/// Whether tree root `idx` is the active connection.
pub(crate) fn side_is_active(app: &App, idx: usize) -> bool {
    match current_conn_index(app) {
        Some(i) => i == idx,
        None => idx == app.connections.len() && app.selected.is_some(),
    }
}

/// Number of tree roots: the saved connections plus, when the active connection
/// is missing from that list, one synthetic root for it.
pub(crate) fn side_root_count(app: &App) -> usize {
    app.connections.len() + usize::from(current_conn_index(app).is_none() && app.selected.is_some())
}

/// Whether a connection root is expanded. The active root defaults open (the
/// user has to collapse it explicitly); a non-active root opens on demand.
pub(crate) fn side_conn_open(app: &App, idx: usize) -> bool {
    let Some(c) = side_root_cfg(app, idx) else {
        return false;
    };
    if side_is_active(app, idx) {
        !app.tree_conn_closed.contains(&c.id)
    } else {
        app.tree_conn_open.contains(&c.id)
    }
}

/// R47b: cached kernel liveness — `true` when DBX holds a pool for this
/// connection. Unknown ids read as idle.
pub(crate) fn conn_is_live(app: &App, id: &str) -> bool {
    app.conn_live.get(id).copied().unwrap_or(false)
}

/// R47b: the status dot state for one connection. Local optimistic marks
/// (`conn_connecting`) win, then the cached kernel liveness.
pub(crate) fn conn_status_for(app: &App, id: &str) -> ConnStatus {
    if app.conn_connecting.contains(id) {
        ConnStatus::Connecting
    } else if conn_is_live(app, id) {
        ConnStatus::Active
    } else {
        ConnStatus::Idle
    }
}

/// R47b: the status dot state for tree root `idx`.
pub(crate) fn side_conn_status(app: &App, idx: usize) -> ConnStatus {
    side_root_cfg(app, idx)
        .map(|c| conn_status_for(app, &c.id))
        .unwrap_or(ConnStatus::Idle)
}

/// R47b: refresh the cached liveness from the kernel. Spawned as an
/// intermediate op (it never drives the spinner), and only after a call that
/// could have changed a pool, so a registry read is never on the hot path.
pub(crate) fn refresh_conn_status(app: &mut App, tx: &Tx) {
    let mut ids: Vec<String> = app.connections.iter().map(|c| c.id.clone()).collect();
    if let Some(c) = &app.selected {
        if !ids.iter().any(|id| id == &c.id) {
            ids.push(c.id.clone());
        }
    }
    if ids.is_empty() {
        return;
    }
    spawn_op(&app.backend, tx, Op::ConnStatus { ids });
}

/// R47b: optimistically mark the active connection live. Called when a result
/// that could only have come from a live connection lands, so a query that
/// silently re-opened a disconnected pool flips the dot back without waiting
/// for a status refresh.
pub(crate) fn mark_active_live(app: &mut App) {
    if let Some(c) = &app.selected {
        let id = c.id.clone();
        app.conn_connecting.remove(&id);
        app.conn_live.insert(id, true);
    }
}

/// R47b: move the tree cursor to the connection root nearest `from_id`,
/// excluding that root. Used after the active connection is disconnected so the
/// cursor never sits on a dead root while other roots exist.
pub(crate) fn side_focus_nearest_root(app: &mut App, from_id: &str) {
    let from_idx = app.connections.iter().position(|c| c.id == from_id);
    let cur = app.side_sel;
    let candidates: Vec<usize> = app
        .side_rows
        .iter()
        .enumerate()
        .filter(|(_, r)| match r {
            SideRow::Conn { idx, .. } => Some(*idx) != from_idx,
            _ => false,
        })
        .map(|(i, _)| i)
        .collect();
    if let Some(target) = candidates.iter().copied().min_by_key(|&i| i.abs_diff(cur)) {
        app.side_sel = target;
        side_mirror_table(app);
    }
}

/// Stable key for one database node of one connection (session collapse memory).
pub(crate) fn db_node_key(conn_id: &str, db: &str) -> String {
    format!("{conn_id}\u{0}{db}")
}

/// Depth of a tree row: connections at 0, databases at 1, tables at 2. Used by
/// `h` (collapse / step to parent) and the indent.
pub(crate) fn side_row_depth(r: &SideRow) -> usize {
    match r {
        SideRow::Group { depth, .. }
        | SideRow::Conn { depth, .. }
        | SideRow::ConnLoading { depth, .. }
        | SideRow::ConnError { depth, .. }
        | SideRow::Db { depth, .. }
        | SideRow::Table { depth, .. } => *depth,
    }
}

/// Build the visible sidebar tree rows. Pure over `app`, so navigation, click
/// hit-testing and rendering all agree on what is on screen.
///
/// R48: DBX Desktop's groups come first, in desktop order, with their
/// connections nested (and nested groups indented). Connections the layout does
/// not mention stay flat after the groups, in the picker's own order. Every
/// connection is still a root: the active one expands on switch, so its
/// databases (or, for engines without a database layer, its tables) show
/// immediately; other connections expand on demand and lazily load their
/// database list, so a failure shows an error row instead of breaking the tree.
pub(crate) fn compute_side_rows(app: &App) -> Vec<SideRow> {
    let mut rows = Vec::new();
    if app.selected.is_none() {
        return rows;
    }
    // `f` quick search narrows the whole tree to its hits; `/` keeps the older
    // filter semantics (see `push_conn_subtree`). Only one is ever active at a
    // time — `f` and `/` both open a modal prompt.
    let searching = app.tree_search_prompt.is_some();
    let needle = if searching {
        app.tree_search.trim().to_lowercase()
    } else {
        app.table_filter.trim().to_lowercase()
    };
    let mut placed: HashSet<usize> = HashSet::new();
    for group in &app.sidebar_layout.groups {
        rows.extend(build_group_rows(
            app,
            group,
            0,
            &needle,
            &mut placed,
            searching,
        ));
    }
    for idx in 0..side_root_count(app) {
        if placed.contains(&idx) {
            continue;
        }
        // A *search* is a result set, so it filters ungrouped roots too; the
        // persistent `/` filter keeps them as anchors (grouped = false).
        push_conn_subtree(app, idx, 0, &needle, searching, searching, &mut rows);
    }
    rows
}

/// Resolve a connection id from the desktop layout to a tree-root index (a
/// saved connection, or the synthetic active root). `None` for a deleted
/// connection, which is simply skipped.
pub(crate) fn side_root_index_for_id(app: &App, id: &str) -> Option<usize> {
    if let Some(i) = app.connections.iter().position(|c| c.id == id) {
        return Some(i);
    }
    if side_root_count(app) > app.connections.len()
        && app.selected.as_ref().is_some_and(|c| c.id == id)
    {
        return Some(app.connections.len());
    }
    None
}

/// Number of live connections a group holds, nested groups included. Counts
/// only connections that still exist, so the `[n]` badge matches the tree.
pub(crate) fn layout_group_count(app: &App, group: &LayoutGroup) -> usize {
    group
        .nodes
        .iter()
        .map(|node| match node {
            LayoutNode::Conn(id) => usize::from(side_root_index_for_id(app, id).is_some()),
            LayoutNode::Group(sub) => layout_group_count(app, sub),
        })
        .sum()
}

/// True when a *grouped* connection should be shown under an active filter. The
/// active connection is always kept (it is the user's working anchor); every
/// other one has to match by name or by a loaded database name. A group whose
/// connections all miss is hidden with them, which is what “组内连接命中即整组
/// 可见” means in practice.
pub(crate) fn conn_matches_filter(app: &App, idx: usize, needle: &str) -> bool {
    if side_is_active(app, idx) {
        return true;
    }
    let Some(c) = side_root_cfg(app, idx) else {
        return false;
    };
    if c.name.to_lowercase().contains(needle) {
        return true;
    }
    app.tree_dbs
        .get(&c.id)
        .is_some_and(|dbs| dbs.iter().any(|d| d.to_lowercase().contains(needle)))
}

/// Flatten one desktop group (and its nested groups) into tree rows, following
/// the group's own member order. The group header is emitted only when the
/// group has at least one visible connection under the active filter; `placed`
/// guards against a connection listed twice.
pub(crate) fn build_group_rows(
    app: &App,
    group: &LayoutGroup,
    depth: usize,
    needle: &str,
    placed: &mut HashSet<usize>,
    searching: bool,
) -> Vec<SideRow> {
    let mut inner: Vec<SideRow> = Vec::new();
    for node in &group.nodes {
        match node {
            LayoutNode::Group(sub) => inner.extend(build_group_rows(
                app,
                sub,
                depth + 1,
                needle,
                placed,
                searching,
            )),
            LayoutNode::Conn(cid) => {
                let Some(idx) = side_root_index_for_id(app, cid) else {
                    continue;
                };
                if !placed.insert(idx) {
                    continue;
                }
                push_conn_subtree(app, idx, depth + 1, needle, true, searching, &mut inner);
            }
        }
    }
    // A group with no live connections (every member deleted, or an empty
    // desktop group) is pure noise in a terminal: draw nothing for it.
    let count = layout_group_count(app, group);
    if count == 0 {
        return Vec::new();
    }
    if !needle.is_empty() && inner.is_empty() {
        return Vec::new();
    }
    // A quick search force-opens every group that has a hit, so a match hidden
    // inside a collapsed group is revealed without an extra key.
    let open = (searching && !needle.is_empty()) || !app.group_closed.contains(&group.id);
    let mut out = Vec::with_capacity(inner.len() + 1);
    out.push(SideRow::Group {
        id: group.id.clone(),
        name: group.name.clone(),
        depth,
        count,
        open,
    });
    if open {
        out.extend(inner);
    }
    out
}

/// Append one connection root and, when it is open, its databases / tables (or
/// the lazy-loading placeholder). `grouped` connections are subject to the
/// active filter; ungrouped roots stay as anchors, as they were before R48.
/// `searching` additionally filters table rows (a quick search is a result set;
/// the `/` filter relies on `App::tables` already being pre-filtered).
pub(crate) fn push_conn_subtree(
    app: &App,
    idx: usize,
    depth: usize,
    needle: &str,
    grouped: bool,
    searching: bool,
    rows: &mut Vec<SideRow>,
) {
    let Some(c) = side_root_cfg(app, idx) else {
        return;
    };
    if grouped && !needle.is_empty() && !conn_matches_filter(app, idx, needle) {
        return;
    }
    let is_active = side_is_active(app, idx);
    rows.push(SideRow::Conn { idx, depth });
    if !side_conn_open(app, idx) {
        return;
    }
    if is_active {
        if app.databases.is_empty() {
            // No database layer (SQLite / a test fixture): tables hang
            // directly under the connection.
            for ti in 0..app.tables.len() {
                if searching && !table_matches_needle(app, ti, needle) {
                    continue;
                }
                rows.push(SideRow::Table {
                    idx,
                    table: ti,
                    depth: depth + 1,
                });
            }
        } else {
            let cur_db = app.current_db();
            for db in &app.databases {
                let is_cur_db = *db == cur_db;
                // A filter keeps a database row when its name matches or it
                // is the active one with visible tables; ancestors of a
                // match survive, which is the whole point of a tree filter.
                if !needle.is_empty()
                    && !db.to_lowercase().contains(needle)
                    && !(is_cur_db && !app.tables.is_empty())
                {
                    continue;
                }
                rows.push(SideRow::Db {
                    idx,
                    db: db.clone(),
                    depth: depth + 1,
                });
                if is_cur_db && !app.tree_db_closed.contains(&db_node_key(&c.id, db)) {
                    for ti in 0..app.tables.len() {
                        if searching && !table_matches_needle(app, ti, needle) {
                            continue;
                        }
                        rows.push(SideRow::Table {
                            idx,
                            table: ti,
                            depth: depth + 2,
                        });
                    }
                }
            }
        }
    } else {
        match app.tree_db_state.get(&c.id) {
            Some(TreeDbState::Loading) => rows.push(SideRow::ConnLoading { idx, depth }),
            Some(TreeDbState::Error(msg)) => rows.push(SideRow::ConnError {
                idx,
                msg: msg.clone(),
                depth,
            }),
            _ => match app.tree_dbs.get(&c.id) {
                Some(dbs) => {
                    for db in dbs {
                        if !needle.is_empty() && !db.to_lowercase().contains(needle) {
                            continue;
                        }
                        rows.push(SideRow::Db {
                            idx,
                            db: db.clone(),
                            depth: depth + 1,
                        });
                    }
                }
                // Not fetched yet (or a synthetic root): a loading row stands
                // in until the lazy list arrives.
                None => rows.push(SideRow::ConnLoading { idx, depth }),
            },
        }
    }
}

// ── sidebar tree quick search (`f`, R54) ──

/// Whether table `ti` matches the quick-search needle, by the same qualified
/// `schema.table` spelling the sidebar draws (so `/inv` finds the `inv` schema).
pub(crate) fn table_matches_needle(app: &App, ti: usize, needle: &str) -> bool {
    needle.is_empty()
        || app.tables.get(ti).is_some_and(|t| {
            qualified_display(&app.schema, &t.name)
                .to_lowercase()
                .contains(needle)
        })
}

/// A row is a *direct* search hit when its own label matches the needle — a row
/// kept only as an ancestor (a group header, the active database) is not.
pub(crate) fn side_row_matches_needle(app: &App, row: &SideRow, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    match row {
        SideRow::Group { name, .. } => name.to_lowercase().contains(needle),
        SideRow::Conn { idx, .. } => {
            side_root_cfg(app, *idx).is_some_and(|c| c.name.to_lowercase().contains(needle))
        }
        SideRow::Db { db, .. } => fix_double_encoding(db).to_lowercase().contains(needle),
        SideRow::Table { table, .. } => table_matches_needle(app, *table, needle),
        SideRow::ConnLoading { .. } | SideRow::ConnError { .. } => false,
    }
}

/// Stable identity of a row, used to re-seat the cursor after a rebuild.
pub(crate) fn side_row_hit(app: &App, row: &SideRow) -> Option<SideHit> {
    match row {
        SideRow::Group { id, .. } => Some(SideHit::Group(id.clone())),
        SideRow::Conn { idx, .. } => side_root_cfg(app, *idx).map(|c| SideHit::Conn(c.id.clone())),
        SideRow::Db { idx, db, .. } => side_root_cfg(app, *idx).map(|c| SideHit::Db {
            conn: c.id.clone(),
            db: db.clone(),
        }),
        SideRow::Table { table, .. } => app
            .tables
            .get(*table)
            .map(|t| SideHit::Table(t.name.clone())),
        SideRow::ConnLoading { .. } | SideRow::ConnError { .. } => None,
    }
}

/// Human label for a row, for the landing status message.
pub(crate) fn side_row_label(app: &App, row: &SideRow) -> String {
    match row {
        SideRow::Group { name, .. } => name.clone(),
        SideRow::Conn { idx, .. } => side_root_cfg(app, *idx)
            .map(|c| c.name.clone())
            .unwrap_or_default(),
        SideRow::Db { db, .. } => fix_double_encoding(db),
        SideRow::Table { table, .. } => app
            .tables
            .get(*table)
            .map(|t| qualified_display(&app.schema, &t.name))
            .unwrap_or_default(),
        SideRow::ConnLoading { .. } | SideRow::ConnError { .. } => String::new(),
    }
}

pub(crate) fn find_side_hit(app: &App, rows: &[SideRow], hit: &SideHit) -> Option<usize> {
    rows.iter()
        .position(|r| side_row_hit(app, r).as_ref() == Some(hit))
}

/// Index of the first direct hit in the current (already filtered) tree rows.
pub(crate) fn tree_search_first_pos(app: &App) -> Option<usize> {
    let needle = app.tree_search.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    app.side_rows
        .iter()
        .position(|r| side_row_matches_needle(app, r, &needle))
}

/// Number of direct hits in the current tree rows.
pub(crate) fn tree_search_hits(app: &App) -> usize {
    let needle = app.tree_search.trim().to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    app.side_rows
        .iter()
        .filter(|r| side_row_matches_needle(app, r, &needle))
        .count()
}

/// Open the `f` tree quick search: a fresh, empty needle over the loaded tree.
pub(crate) fn open_tree_search(app: &mut App) {
    if app.selected.is_none() {
        return;
    }
    app.tree_search.clear();
    app.tree_search_prompt = Some(TextArea::default());
    app.tree_search_prev = app
        .side_rows
        .get(app.side_sel)
        .and_then(|r| side_row_hit(app, r));
    rebuild_side_rows(app);
    app.status = t("搜索连接 / 库 / 表：输入关键字 · Enter 跳首个命中 · Esc 清除").into();
}

/// Pull the needle out of the prompt (single line), refilter the tree, and put
/// the cursor on the first hit so Enter's landing is always visible.
pub(crate) fn apply_tree_search(app: &mut App) {
    app.tree_search = app
        .tree_search_prompt
        .as_ref()
        .map(|t| t.lines().join(" ").trim().to_string())
        .unwrap_or_default();
    rebuild_side_rows(app);
    if let Some(pos) = tree_search_first_pos(app) {
        app.side_sel = pos;
        side_mirror_table(app);
    }
    if app.tree_search.is_empty() {
        app.status = t("搜索连接 / 库 / 表：输入关键字 · Enter 跳首个命中 · Esc 清除").into();
    } else {
        let hits = tree_search_hits(app);
        app.status = if hits == 0 {
            tf("搜索「{}」· 0 个命中", &[&app.tree_search])
        } else {
            tf(
                "搜索「{}」· {} 个命中 · Enter 跳首个 · Esc 清除",
                &[&app.tree_search, &hits],
            )
        };
    }
}

/// The group / connection ancestors of the row at `pos`, so a jump can un-fold
/// them and actually reveal the hit.
pub(crate) fn side_hit_ancestors(app: &App, pos: usize) -> (Option<String>, Option<String>) {
    let Some(_) = app.side_rows.get(pos) else {
        return (None, None);
    };
    let mut group_id = None;
    let mut conn_id = None;
    let mut depth = side_row_depth(&app.side_rows[pos]);
    let mut i = pos;
    while i > 0 && depth > 0 {
        i -= 1;
        let d = side_row_depth(&app.side_rows[i]);
        if d < depth {
            match &app.side_rows[i] {
                SideRow::Group { id, .. } if group_id.is_none() => group_id = Some(id.clone()),
                SideRow::Conn { idx, .. } if conn_id.is_none() => {
                    conn_id = side_root_cfg(app, *idx).map(|c| c.id.clone());
                }
                _ => {}
            }
            depth = d;
        }
    }
    (group_id, conn_id)
}

/// Enter: jump to the first hit, restore the full tree, drop the needle.
pub(crate) fn tree_search_confirm(app: &mut App) {
    let hit = tree_search_first_pos(app).map(|pos| {
        let row = app.side_rows[pos].clone();
        (
            side_row_hit(app, &row),
            side_row_label(app, &row),
            side_hit_ancestors(app, pos),
        )
    });
    app.tree_search_prompt = None;
    app.tree_search.clear();
    app.tree_search_prev = None;
    rebuild_side_rows(app);
    match hit {
        Some((Some(h), label, (group, conn))) if !label.is_empty() => {
            // Un-fold the hit's ancestors so the landing is actually visible
            // (the search had force-opened them only for the duration).
            if let Some(g) = group {
                app.group_closed.remove(&g);
            }
            if let Some(cid) = conn {
                app.tree_conn_open.insert(cid.clone());
                app.tree_conn_closed.remove(&cid);
            }
            rebuild_side_rows(app);
            if let Some(pos) = find_side_hit(app, &app.side_rows, &h) {
                app.side_sel = pos;
                side_mirror_table(app);
            }
            app.status = tf("✓ 跳到 {} · 已清除搜索", &[&label]);
        }
        _ => {
            app.status = t("没有匹配的连接 / 库 / 表").into();
        }
    }
}

/// Esc: restore the full tree and put the cursor back on the node it started on.
pub(crate) fn tree_search_cancel(app: &mut App) {
    let prev = app.tree_search_prev.take();
    app.tree_search_prompt = None;
    app.tree_search.clear();
    rebuild_side_rows(app);
    if let Some(prev) = prev {
        if let Some(pos) = find_side_hit(app, &app.side_rows, &prev) {
            app.side_sel = pos;
            side_mirror_table(app);
        }
    }
    app.status = t("已清除搜索").into();
}

/// Modal key handler for the quick search while its prompt is open.
pub(crate) fn tree_search_key(app: &mut App, k: KeyEvent) {
    // Ctrl-U / Alt-Backspace clear the needle, like the other filter prompts.
    if (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('u'))
        || (k.modifiers.contains(KeyModifiers::ALT) && k.code == KeyCode::Backspace)
    {
        app.tree_search_prompt = Some(TextArea::default());
        app.tree_search.clear();
        apply_tree_search(app);
        app.status = t("已清除搜索").into();
        return;
    }
    match k.code {
        KeyCode::Enter => tree_search_confirm(app),
        KeyCode::Esc => tree_search_cancel(app),
        _ => {
            if let Some(t) = app.tree_search_prompt.as_mut() {
                t.input(k);
            }
            apply_tree_search(app);
        }
    }
}

// ── R55: in-place rename of a tree row ──

/// Open the in-place rename for the tree row under the cursor. Works for a
/// connection root and for a desktop group; any other row keeps its own `r`
/// meaning (table structure).
pub(crate) fn open_rename_edit(app: &mut App) {
    let Some(row) = app.side_rows.get(app.side_sel).cloned() else {
        return;
    };
    let edit = match &row {
        SideRow::Conn { idx, .. } => side_root_cfg(app, *idx).map(|c| RenameEdit {
            target: RenameTarget::Conn { id: c.id.clone() },
            text: c.name.clone(),
        }),
        SideRow::Group { id, name, .. } => Some(RenameEdit {
            target: RenameTarget::Group { id: id.clone() },
            text: name.clone(),
        }),
        _ => None,
    };
    match edit {
        Some(edit) => {
            let label = match edit.target {
                RenameTarget::Conn { .. } => t("连接"),
                RenameTarget::Group { .. } => t("分组"),
            };
            app.rename_edit = Some(edit);
            app.status = tf("重命名{}：改好后 Enter 保存 · Esc 取消", &[&label]);
        }
        None => app.status = t("把光标移到连接或分组行上再按 r 重命名").into(),
    }
}

/// Key handling while a tree row is being renamed. A plain text field: append /
/// Backspace edit, Ctrl-U clears, Enter saves and Esc cancels. Any other key is
/// swallowed so a global shortcut (`q`, `j`, …) cannot fire mid-edit.
pub(crate) fn rename_edit_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('u') {
        if let Some(e) = app.rename_edit.as_mut() {
            e.text.clear();
        }
        return;
    }
    match k.code {
        KeyCode::Enter => commit_rename(app, tx),
        KeyCode::Esc => {
            app.rename_edit = None;
            app.status = t("已取消重命名").into();
        }
        KeyCode::Backspace => {
            if let Some(e) = app.rename_edit.as_mut() {
                e.text.pop();
            }
        }
        KeyCode::Char(c)
            if !k.modifiers.contains(KeyModifiers::CONTROL)
                && !k.modifiers.contains(KeyModifiers::ALT)
                && !c.is_ascii_control() =>
        {
            if let Some(e) = app.rename_edit.as_mut() {
                // Cap the buffer well above the save-time limit so the length
                // check can still explain itself instead of silently dropping
                // keys.
                if e.text.chars().count() < 256 {
                    e.text.push(c);
                }
            }
        }
        _ => {}
    }
}

/// Validate a rename buffer: trimmed, non-empty and within the length cap.
/// Returns the cleaned name or a user-facing error (bilingual via `t` / `tf`).
pub(crate) fn validate_rename_name(text: &str) -> Result<String, String> {
    let name = text.trim().to_string();
    if name.is_empty() {
        return Err(t("✗ 名称不能为空").into());
    }
    if name.chars().count() > MAX_CONN_NAME_LEN {
        return Err(tf("✗ 名称过长（最多 {} 字符）", &[&(MAX_CONN_NAME_LEN)]));
    }
    Ok(name)
}

/// Validate and apply the in-place rename. A connection is saved with a
/// single-row store upsert (`Op::RenameConn`); a group patches only its name in
/// the raw sidebar tree, which is then persisted as a whole via the kernel store.
pub(crate) fn commit_rename(app: &mut App, tx: &Tx) {
    let Some(edit) = app.rename_edit.take() else {
        return;
    };
    let name = match validate_rename_name(&edit.text) {
        Ok(name) => name,
        Err(e) => {
            app.status = e;
            app.rename_edit = Some(edit);
            return;
        }
    };
    match &edit.target {
        RenameTarget::Conn { id } => {
            let Some(mut cfg) = app.connections.iter().find(|c| &c.id == id).cloned() else {
                app.status = t("✗ 连接已不存在").into();
                return;
            };
            if cfg.name == name {
                app.status = tf("连接名未变：{}", &[&name]);
                return;
            }
            cfg.name = name.clone();
            app.status = tf("重命名连接 → {}…", &[&name]);
            app.spawn(tx, Op::RenameConn(Box::new(cfg)));
        }
        RenameTarget::Group { id } => {
            let Some(mut raw) = app.sidebar_layout_raw.clone() else {
                app.status = t("✗ 没有可保存的桌面分组布局").into();
                return;
            };
            if !patch_group_name(&mut raw, id, &name) {
                app.status = t("✗ 分组已不存在").into();
                return;
            }
            app.sidebar_layout = parse_sidebar_layout(&raw);
            app.sidebar_layout_raw = Some(raw.clone());
            rebuild_side_rows(app);
            app.status = tf("已重命名分组为 {}", &[&name]);
            app.spawn(tx, Op::SaveSidebarLayout(Box::new(raw)));
        }
    }
}

/// Patch one group's display name inside the raw `sidebar_layout` metadata,
/// leaving every other desktop field untouched.
pub(crate) fn patch_group_name(raw: &mut serde_json::Value, id: &str, name: &str) -> bool {
    let Some(groups) = raw.get_mut("groups").and_then(|g| g.as_array_mut()) else {
        return false;
    };
    for g in groups {
        if g.get("id").and_then(|v| v.as_str()) == Some(id) {
            g["name"] = serde_json::Value::String(name.to_string());
            return true;
        }
    }
    false
}

// ── R55: reorder a tree row (Shift+↑/↓) ──

/// One node in the raw desktop layout, used as a move target.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LayoutTarget {
    Group(String),
    Conn(String),
}

/// True when a connection id appears inside some group (at any nesting depth).
/// A top-level connection is not part of the persisted order — it is drawn flat
/// from the picker — so it has nothing to reorder.
pub(crate) fn layout_conn_is_grouped(layout: &SidebarLayout, id: &str) -> bool {
    fn in_nodes(nodes: &[LayoutNode], id: &str) -> bool {
        nodes.iter().any(|n| match n {
            LayoutNode::Conn(c) => c == id,
            LayoutNode::Group(g) => in_nodes(&g.nodes, id),
        })
    }
    layout.groups.iter().any(|g| in_nodes(&g.nodes, id))
}

pub(crate) fn layout_entry_matches(entry: &serde_json::Value, target: &LayoutTarget) -> bool {
    let ty = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match target {
        LayoutTarget::Group(id) => {
            ty == "group" && entry.get("id").and_then(|v| v.as_str()) == Some(id)
        }
        LayoutTarget::Conn(id) => {
            ty == "connection" && entry.get("id").and_then(|v| v.as_str()) == Some(id)
        }
    }
}

/// Swap the entry matching `target` with the previous (`dir < 0`) / next
/// (`dir > 0`) sibling in one entry array. At the top level only groups take
/// part in the persisted order, so a group swaps with the nearest other group;
/// inside a group every sibling (connection or nested group) is movable.
pub(crate) fn swap_entry_in_array(
    entries: &mut [serde_json::Value],
    target: &LayoutTarget,
    dir: i32,
    top_level: bool,
) -> bool {
    let Some(i) = entries.iter().position(|e| layout_entry_matches(e, target)) else {
        return false;
    };
    let candidates: Vec<usize> = if top_level {
        (0..entries.len())
            .filter(|&k| entries[k].get("type").and_then(|v| v.as_str()) == Some("group"))
            .collect()
    } else {
        (0..entries.len()).collect()
    };
    let Some(pos) = candidates.iter().position(|&k| k == i) else {
        return false;
    };
    let nb = if dir < 0 {
        pos.checked_sub(1)
    } else {
        Some(pos + 1).filter(|p| *p < candidates.len())
    };
    match nb {
        Some(p) => {
            entries.swap(i, candidates[p]);
            true
        }
        None => false,
    }
}

/// Depth-first through the raw layout until the array holding `target` is found,
/// then swap within it. Handles the modern `children` tree, the legacy flat
/// `connectionIds` list, and top-level groups in `order`.
pub(crate) fn swap_in_layout(raw: &mut serde_json::Value, target: &LayoutTarget, dir: i32) -> bool {
    /// Try each group's member list, then recurse into nested groups.
    fn walk(entries: &mut [serde_json::Value], target: &LayoutTarget, dir: i32) -> bool {
        for e in entries.iter_mut() {
            if e.get("type").and_then(|v| v.as_str()) != Some("group") {
                continue;
            }
            if let Some(children) = e.get_mut("children").and_then(|c| c.as_array_mut()) {
                if swap_entry_in_array(children, target, dir, false) || walk(children, target, dir)
                {
                    return true;
                }
            } else if let LayoutTarget::Conn(id) = target {
                if let Some(ids) = e.get_mut("connectionIds").and_then(|c| c.as_array_mut()) {
                    if let Some(i) = ids.iter().position(|v| v.as_str() == Some(id.as_str())) {
                        let j = if dir < 0 {
                            i.checked_sub(1)
                        } else {
                            Some(i + 1).filter(|j| *j < ids.len())
                        };
                        if let Some(j) = j {
                            ids.swap(i, j);
                            return true;
                        }
                        return false;
                    }
                }
            }
        }
        false
    }
    let Some(order) = raw.get_mut("order").and_then(|o| o.as_array_mut()) else {
        return false;
    };
    // Top level: a group swaps with the nearest other top-level group; if it is
    // already at that end the move reports failure (walk must not then swap it
    // past a bare connection entry, which the tree does not draw in place).
    if swap_entry_in_array(order, target, dir, true) {
        return true;
    }
    walk(order, target, dir)
}

/// Shift+↑/↓ on a tree row: move it one slot within its sibling list, re-parse
/// and persist the desktop tree. The tree cursor follows the row.
pub(crate) fn move_side_row(app: &mut App, tx: &Tx, dir: i32) {
    let Some(row) = app.side_rows.get(app.side_sel).cloned() else {
        return;
    };
    let (target, label) = match &row {
        SideRow::Conn { idx, .. } => {
            let Some(c) = side_root_cfg(app, *idx) else {
                return;
            };
            (LayoutTarget::Conn(c.id.clone()), c.name.clone())
        }
        SideRow::Group { id, name, .. } => (LayoutTarget::Group(id.clone()), name.clone()),
        _ => {
            app.status = t("Shift+↑/↓ 只能移动连接根或分组行").into();
            return;
        }
    };
    // A top-level connection has no persisted order (it follows the picker's
    // sort), so point at the fix instead of silently swapping nothing.
    if let LayoutTarget::Conn(id) = &target {
        if !layout_conn_is_grouped(&app.sidebar_layout, id) {
            app.status = t("顶层未分组连接按名称排序；先放进分组再调整顺序").into();
            return;
        }
    }
    let Some(mut raw) = app.sidebar_layout_raw.clone() else {
        app.status = t("✗ 没有可调整的桌面分组布局").into();
        return;
    };
    if !swap_in_layout(&mut raw, &target, dir) {
        app.status = t("已经在同层的最上 / 最下").into();
        return;
    }
    app.sidebar_layout = parse_sidebar_layout(&raw);
    app.sidebar_layout_raw = Some(raw.clone());
    rebuild_side_rows(app);
    let hit = match &target {
        LayoutTarget::Conn(id) => SideHit::Conn(id.clone()),
        LayoutTarget::Group(id) => SideHit::Group(id.clone()),
    };
    if let Some(pos) = find_side_hit(app, &app.side_rows, &hit) {
        app.side_sel = pos;
        side_mirror_table(app);
    }
    let arrow = if dir < 0 { "↑" } else { "↓" };
    app.status = tf("已移动 {} {}", &[&label, &arrow]);
    app.spawn(tx, Op::SaveSidebarLayout(Box::new(raw)));
}

// ── R55: session column-width memory (`<` / `>`) ──

/// Scope a manual column-width override is remembered under. A browsed table
/// keys on connection + database + schema + table (stable across page turns and
/// re-queries); every other result grid shares one bucket per connection.
pub(crate) fn col_width_scope(app: &App) -> String {
    let conn = app
        .selected
        .as_ref()
        .map(|c| c.id.clone())
        .unwrap_or_default();
    if app.grid_kind == GridKind::TableData {
        if let Some(ps) = &app.page_state {
            return format!(
                "t\u{0}{conn}\u{0}{}\u{0}{}\u{0}{}",
                app.current_db(),
                ps.schema,
                ps.table
            );
        }
    }
    format!("q\u{0}{conn}")
}

/// `<` / `>` on the focused result column: widen / narrow it, remembered for
/// the session. Never persisted.
pub(crate) fn adjust_col_width(app: &mut App, delta: i32) {
    let Some(grid) = active_grid(app) else {
        app.status = t("没有可调整列宽的结果").into();
        return;
    };
    // The drilled script list has no grid column under the cursor.
    if app.script.as_ref().is_some_and(|s| s.drilled.is_none()) {
        app.status = t("展开一条语句结果后再调列宽").into();
        return;
    }
    let Some(name) = grid.columns.get(app.col_cursor).cloned() else {
        return;
    };
    let current = app
        .grid_widths
        .get(app.col_cursor)
        .copied()
        .unwrap_or(MIN_CELL_WIDTH);
    let scope = col_width_scope(app);
    let next = app.col_width_mem.adjust(&scope, &name, current, delta);
    let disp = fix_double_encoding(&name);
    app.status = tf(
        "列宽 {} → {} 格 · 会话内记忆（< 收窄 / > 加宽）",
        &[&disp, &next],
    );
}

/// Apply the session overrides to the natural widths of `grid`, in place.
pub(crate) fn apply_col_width_overrides(app: &App, grid: &Grid, widths: &mut [usize]) {
    let scope = col_width_scope(app);
    let Some(map) = app.col_width_mem.overrides(&scope) else {
        return;
    };
    for (ci, name) in grid.columns.iter().enumerate() {
        if ci >= widths.len() {
            break;
        }
        if let Some(w) = map.get(name) {
            widths[ci] = (*w).clamp(MIN_CELL_WIDTH, COL_W_MAX);
        }
    }
}

/// Rebuild the flattened tree and keep the cursor in sync. An external move of
/// `table_list` (first-letter jump, recent tables, filter Enter, count jump, a
/// global-search hit) drags the tree cursor onto that table; a tree move drags
/// `table_list` with it, so `selected_table()` stays authoritative for the rest
/// of the app.
pub(crate) fn rebuild_side_rows(app: &mut App) {
    let rows = compute_side_rows(app);
    let cur_table = app.table_list.selected();
    if cur_table != app.side_table_seen {
        if let Some(pos) = rows
            .iter()
            .position(|r| matches!(r, SideRow::Table { table, .. } if Some(*table) == cur_table))
        {
            app.side_sel = pos;
        }
        app.side_table_seen = cur_table;
    }
    app.side_rows = rows;
    let n = app.side_rows.len();
    if n == 0 {
        app.side_sel = 0;
    } else {
        app.side_sel = app.side_sel.min(n - 1);
    }
    side_mirror_table(app);
}

/// Mirror the tree cursor onto `table_list` when it sits on a table row.
pub(crate) fn side_mirror_table(app: &mut App) {
    if let Some(SideRow::Table { table, .. }) = app.side_rows.get(app.side_sel) {
        if app.table_list.selected() != Some(*table) {
            app.table_list.select(Some(*table));
        }
        app.side_table_seen = Some(*table);
    }
}

/// Move the tree cursor by `step` rows, clamped to the visible tree.
pub(crate) fn side_step(app: &mut App, step: usize, forward: bool) {
    let n = app.side_rows.len();
    if n == 0 {
        return;
    }
    let cur = app.side_sel.min(n - 1);
    app.side_sel = if forward {
        (cur + step).min(n - 1)
    } else {
        cur.saturating_sub(step)
    };
    side_mirror_table(app);
    // Name the position so a count move (`3j`) replaces the `3·` prefix readout
    // with something useful instead of leaving it on screen.
    app.status = tf("树 {}/{}", &[&(app.side_sel + 1), &(n)]);
}

/// Move the tree cursor up to the nearest row shallower than the current one
/// (a table's database, a database's connection).
pub(crate) fn side_focus_parent(app: &mut App) {
    let Some(cur) = app.side_rows.get(app.side_sel) else {
        return;
    };
    let d = side_row_depth(cur);
    let mut i = app.side_sel;
    while i > 0 {
        i -= 1;
        if side_row_depth(&app.side_rows[i]) < d {
            app.side_sel = i;
            side_mirror_table(app);
            return;
        }
    }
}

/// `l` / `→`: expand the tree row under the cursor (a group unfolds, a
/// connection loads its databases, a database reveals its tables). On an
/// already-expanded node it steps into its first child, vim-tree style. Every
/// node type follows the same rule: an expanded parent walks into its first
/// child, a collapsed parent opens in place.
pub(crate) fn side_expand(app: &mut App, tx: &Tx) {
    let Some(row) = app.side_rows.get(app.side_sel).cloned() else {
        return;
    };
    match row {
        SideRow::Group { id, .. } => {
            if app.group_closed.contains(&id) {
                app.group_closed.remove(&id);
                rebuild_side_rows(app);
            } else {
                side_step_into_child(app);
            }
        }
        SideRow::Conn { idx, .. } => {
            if side_conn_open(app, idx) {
                side_step_into_child(app);
            } else {
                expand_conn(app, tx, idx);
            }
        }
        SideRow::Db { idx, db, .. } => {
            // Only the active connection's *current* database can reveal tables;
            // any other database node selects it (switching connection when
            // needed).
            if side_is_active(app, idx) && db == app.current_db() {
                let id = side_root_cfg(app, idx)
                    .map(|c| c.id.clone())
                    .unwrap_or_default();
                let key = db_node_key(&id, &db);
                if app.tree_db_closed.remove(&key) {
                    rebuild_side_rows(app);
                } else {
                    side_step_into_child(app);
                }
            } else {
                switch_to_db(app, tx, idx, &db);
            }
        }
        _ => {}
    }
}

/// Move the tree cursor onto the first child of the row it sits on, but only
/// when that child is actually on screen. An expanded parent whose children are
/// still loading (or an empty one) stays put instead of jumping to a sibling —
/// the same rule for every node type.
pub(crate) fn side_step_into_child(app: &mut App) {
    let Some(cur) = app.side_rows.get(app.side_sel) else {
        return;
    };
    let d = side_row_depth(cur);
    if app
        .side_rows
        .get(app.side_sel + 1)
        .is_some_and(|r| side_row_depth(r) > d)
    {
        side_step(app, 1, true);
    }
}

/// `h` / `←`: collapse the tree row under the cursor, or step up to its parent
/// when it is already collapsed / a leaf. Groups, connections, databases and
/// tables (and nested groups) all answer `h` the same way: the first press on
/// an expanded node folds it in place, a second press climbs to the parent.
pub(crate) fn side_collapse(app: &mut App) {
    let Some(row) = app.side_rows.get(app.side_sel).cloned() else {
        return;
    };
    match row {
        SideRow::Group { id, .. } => {
            if !app.group_closed.contains(&id) {
                app.group_closed.insert(id);
                rebuild_side_rows(app);
            } else {
                side_focus_parent(app);
            }
        }
        SideRow::Conn { idx, .. } => {
            if side_conn_open(app, idx) {
                let id = side_root_cfg(app, idx)
                    .map(|c| c.id.clone())
                    .unwrap_or_default();
                if side_is_active(app, idx) {
                    app.tree_conn_closed.insert(id);
                } else {
                    app.tree_conn_open.remove(&id);
                }
                rebuild_side_rows(app);
            } else {
                // R50: an already-collapsed connection climbs to its parent,
                // exactly like a group / database / table — it used to swallow
                // the key.
                side_focus_parent(app);
            }
        }
        SideRow::Db { idx, db, .. } => {
            if side_is_active(app, idx) && db == app.current_db() {
                let id = side_root_cfg(app, idx)
                    .map(|c| c.id.clone())
                    .unwrap_or_default();
                let key = db_node_key(&id, &db);
                if !app.tree_db_closed.contains(&key) {
                    app.tree_db_closed.insert(key);
                    rebuild_side_rows(app);
                } else {
                    side_focus_parent(app);
                }
            } else {
                side_focus_parent(app);
            }
        }
        _ => side_focus_parent(app),
    }
}

/// Expand a connection root, lazily loading its database list when it is not the
/// active connection. A failure is remembered and drawn as an error row.
pub(crate) fn expand_conn(app: &mut App, tx: &Tx, idx: usize) {
    let Some(c) = side_root_cfg(app, idx).cloned() else {
        return;
    };
    let id = c.id.clone();
    if side_is_active(app, idx) {
        app.tree_conn_closed.remove(&id);
        // R47b: expanding a disconnected active root reconnects it — a fresh
        // activate re-runs the database load and turns the dot green again.
        if !conn_is_live(app, &id) {
            let restore = app.conn_pointers.get(&id).cloned();
            activate_connection(app, tx, c, restore, None);
            return;
        }
        rebuild_side_rows(app);
        return;
    }
    app.tree_conn_open.insert(id.clone());
    // A synthetic root (not in the saved list) cannot be lazily connected.
    if idx >= app.connections.len() {
        rebuild_side_rows(app);
        return;
    }
    let live = conn_is_live(app, &id);
    let loading = matches!(app.tree_db_state.get(&id), Some(TreeDbState::Loading));
    // A live root with a cached list just opens (no refetch). A disconnected
    // root always re-fetches, so its cached rows are refreshed and the pool
    // reopens ("点开时重新懒连接").
    if loading || (live && app.tree_dbs.contains_key(&id)) {
        rebuild_side_rows(app);
        return;
    }
    let gen = {
        let g = app.tree_gen.entry(id.clone()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    };
    app.tree_db_state.insert(id.clone(), TreeDbState::Loading);
    app.conn_connecting.insert(id.clone());
    rebuild_side_rows(app);
    app.spawn(tx, Op::TreeDatabases(Box::new(c), gen));
}

/// R47b: open the red confirmation for a manual disconnect. Shared by the SQL /
/// Mongo tree (`x` on a connection root) and the Redis browser (`X`).
pub(crate) fn open_disconnect_confirm(app: &mut App, cfg: &ConnectionConfig) {
    if !conn_is_live(app, &cfg.id) && !app.conn_connecting.contains(&cfg.id) {
        app.status = tf("连接 {} 已断开", &[&cfg.name]);
        return;
    }
    app.confirm = Some(Confirm {
        sql: String::new(),
        reasons: Vec::new(),
        refresh: false,
        clear_batch: false,
        conn: Some(ConnConfirm {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            db_type: cfg.db_type.as_str().to_string(),
            disconnect: true,
            readonly: None,
        }),
        redis: None,
        mongo: None,
    });
    app.status = tf("断开连接 {} · Enter 确认 · Esc 取消", &[&cfg.name]);
}

/// R47b: `x` on a connection root opens the disconnect confirmation. A
/// synthetic root (an active connection missing from the saved list) has no
/// saved config to reconnect with, so it is refused with a hint instead.
pub(crate) fn request_disconnect(app: &mut App, idx: usize) {
    if idx >= app.connections.len() {
        app.status = t("该连接不在已保存列表中，无法断开").into();
        return;
    }
    let Some(c) = side_root_cfg(app, idx).cloned() else {
        return;
    };
    open_disconnect_confirm(app, &c);
}

/// R68: `!` on the tree flips the read-only policy of the connection under the
/// cursor (or the active connection when the cursor is on a db / table row),
/// behind the same red confirmation layer as a disconnect. Read-only is a
/// connection-level config field persisted through LocalBackend's single-row
/// upsert, so the policy survives a restart and every write guard reads it.
pub(crate) fn open_readonly_toggle_confirm(app: &mut App) {
    let target = match app.side_rows.get(app.side_sel) {
        Some(SideRow::Conn { idx, .. }) => side_root_cfg(app, *idx).cloned(),
        _ => app.selected.clone(),
    };
    let Some(cfg) = target else {
        app.status = t("没有可切换的连接").into();
        return;
    };
    // Only a saved connection can be persisted; a synthetic root (an active
    // connection missing from the store) has no row to write.
    if !app.connections.iter().any(|c| c.id == cfg.id) {
        app.status = t("该连接不在已保存列表中，无法保存只读设置").into();
        return;
    }
    let new_value = !cfg.read_only;
    app.confirm = Some(Confirm {
        sql: String::new(),
        reasons: Vec::new(),
        refresh: false,
        clear_batch: false,
        conn: Some(ConnConfirm {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            db_type: cfg.db_type.as_str().to_string(),
            disconnect: false,
            readonly: Some(new_value),
        }),
        redis: None,
        mongo: None,
    });
    app.status = if new_value {
        tf("将连接 {} 设为只读 · Enter 确认 · Esc 取消", &[&cfg.name])
    } else {
        tf("将连接 {} 恢复为可写 · Enter 确认 · Esc 取消", &[&cfg.name])
    };
}
