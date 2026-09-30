use crate::prelude::*;
use crate::*;

/// R75: how long an `Esc` flash ("关闭 X" / "已清除 Y") stays on the status bar
/// before it auto-clears. Short enough to feel transient, long enough to read.
pub(crate) const FLASH_TTL: Duration = Duration::from_millis(1500);

// ─── pages & focus ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Page {
    Browse,  // sidebar (db/tables/structure) + editor + results
    NewConn, // connection form
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Backend {
    Sql,
    Redis,
    Mongo,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Focus {
    Sidebar,
    Preview,
    Editor,
    CmdInput,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LayoutMode {
    Narrow, // cols < 46  (phone portrait / tiny tmux)
    Mid,    // 46..99
    Wide,   // >= 100
}

pub(crate) fn layout_mode(cols: u16) -> LayoutMode {
    if cols < 50 {
        LayoutMode::Narrow
    } else if cols < 100 {
        LayoutMode::Mid
    } else {
        LayoutMode::Wide
    }
}

/// Effective collapse state of a pane. A manual per-pane override always wins;
/// otherwise the single `auto_collapse` master switch decides: on = unfocused
/// aux panes collapse to a one-line strip so the focused pane owns the space
/// (the old responsive behaviour); off = every pane stays expanded.
pub(crate) fn pane_eff_collapsed(app: &App, pane: usize) -> bool {
    resolve_collapse(
        app.auto_collapse,
        app.focus,
        pane,
        app.pane_override.get(pane).copied().flatten(),
    )
}

/// Pure form of the collapse rule, so it can be unit-tested without an `App`.
pub(crate) fn resolve_collapse(
    auto: bool,
    focus: Focus,
    pane: usize,
    manual: Option<bool>,
) -> bool {
    if let Some(v) = manual {
        return v;
    }
    if !auto {
        return false;
    }
    match pane {
        PANE_SIDEBAR => focus != Focus::Sidebar,
        PANE_EDITOR => !matches!(focus, Focus::Editor | Focus::CmdInput),
        _ => false,
    }
}

/// Ctrl-A — one master switch for the whole responsive-collapse behaviour.
/// On: unfocused aux panes collapse (the old behaviour). Off: everything stays
/// expanded. Per-pane overrides are cleared so the two modes never mix.
pub(crate) fn toggle_auto_collapse(app: &mut App) {
    app.auto_collapse = !app.auto_collapse;
    app.pane_override = [None; 3];
    app.status = if app.auto_collapse {
        t("自动折叠：开 · 非焦点栏收起（Ctrl-A 关闭）").into()
    } else {
        t("自动折叠：关 · 所有栏展开（Ctrl-A 开启）").into()
    };
}

pub(crate) fn pane_name(pane: usize) -> &'static str {
    match pane {
        PANE_SIDEBAR => t("侧栏"),
        PANE_EDITOR => t("编辑器"),
        _ => t("结果区"),
    }
}

pub(crate) fn toggle_pane_collapse(app: &mut App) {
    let pane = match app.focus {
        Focus::Sidebar => PANE_SIDEBAR,
        Focus::Editor | Focus::CmdInput => PANE_EDITOR,
        Focus::Preview => PANE_RESULTS,
    };
    let cur = pane_eff_collapsed(app, pane);
    app.pane_override[pane] = Some(!cur);
    app.status = if cur {
        tf("已展开{}", &[&(pane_name(pane))])
    } else {
        tf("已收起{}", &[&(pane_name(pane))])
    };
}

pub(crate) fn cycle_focus(app: &mut App, forward: bool) {
    let order: Vec<Focus> = if app.backend_kind == Backend::Sql {
        vec![Focus::Sidebar, Focus::Editor, Focus::Preview]
    } else {
        vec![
            Focus::Sidebar,
            Focus::Editor,
            Focus::CmdInput,
            Focus::Preview,
        ]
    };
    let next = match order.iter().position(|f| *f == app.focus) {
        Some(i) if forward => (i + 1) % order.len(),
        Some(i) => (i + order.len() - 1) % order.len(),
        None => 0,
    };
    app.focus = order[next];
    // R57: the results row selection belongs to the results pane; leaving it
    // ends row-select mode so a stray `d` in the sidebar is not mistaken for the
    // batch-DELETE gesture.
    if app.focus != Focus::Preview {
        app.row_sel_anchor = None;
    }
}

/// Which kind of content the results pane currently shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GridKind {
    Query,      // arbitrary SQL result
    TableData,  // paginated SELECT * of a table
    Columns,    // table structure (field list)
    RedisValue, // a Redis key's value rendered per type
    MongoDocs,  // paginated MongoDB documents
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StructView {
    Fields,
    Ddl,
}

// ─── cell values ─────────────────────────────────────────────────────────────

/// A result cell. NULL is kept distinct from the empty string so the grid can
/// render them differently.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Val {
    Null,
    Text(String),
}

impl Val {
    pub(crate) fn text(&self) -> &str {
        match self {
            Val::Null => "",
            Val::Text(s) => s,
        }
    }
    #[allow(dead_code)]
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }
}

pub(crate) fn value_to_val(v: &serde_json::Value) -> Val {
    match v {
        serde_json::Value::Null => Val::Null,
        serde_json::Value::String(s) => Val::Text(sanitize_cell(s)),
        serde_json::Value::Number(n) => Val::Text(n.to_string()),
        serde_json::Value::Bool(b) => Val::Text(b.to_string()),
        other => Val::Text(sanitize_cell(&other.to_string())),
    }
}

/// R77: one statement that failed in the last editor run, located in the editor
/// buffer by char offset (not by line, so a multi-line statement is one span).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct EditorErrorSpan {
    /// Start char offset of the statement in the editor buffer.
    pub(crate) start: usize,
    /// End char offset (exclusive).
    pub(crate) end: usize,
    /// 1-based ordinal of the statement within the executed script.
    pub(crate) ordinal: usize,
    /// Line number parsed from the driver's error message, when the engine
    /// reported one (MySQL `at line N`, PostgreSQL `LINE N:`), relative to the
    /// statement as the server received it.
    pub(crate) err_line: Option<usize>,
}

/// R88: metadata for a scoped editor run (a selection or the statement under
/// the cursor). Captured when the run is submitted so the result can label
/// `执行第 N 条` and a failure can localize back to exactly the statements that
/// ran — all client-side, zero extra queries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ScopedRun {
    /// The SQL this run sent, trimmed; `take_scope_label` matches on it so an
    /// unrelated result (a load-more, an EXPLAIN) never inherits the label.
    pub(crate) sql: String,
    /// Buffer char spans of the executed statements, one per statement (`None`
    /// when it could not be mapped back to the buffer).
    pub(crate) spans: Vec<Option<(usize, usize)>>,
    /// 1-based editor ordinals aligned with `spans` (0 = unmapped placeholder).
    pub(crate) ordinals: Vec<usize>,
    /// The bilingual status suffix, e.g. `执行第 3 条`.
    pub(crate) label: String,
}

/// Collapse control characters so a value never breaks the one-line grid layout.
pub(crate) fn sanitize_cell(s: &str) -> String {
    if !s.chars().any(|c| c == '\n' || c == '\r' || c == '\t') {
        return s.to_string();
    }
    s.chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

// ─── result grid ─────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub(crate) struct Grid {
    pub(crate) columns: Vec<String>,
    /// Driver-reported type name per column, parallel to `columns`. Empty when
    /// the source has no types (schemaless stores, some fallback paths); the
    /// big-number display layer treats an empty/unknown type as “leave alone”.
    pub(crate) types: Vec<String>,
    pub(crate) rows: Vec<Vec<Val>>,
    pub(crate) note: String,
}

impl Grid {
    pub(crate) fn from_query(
        columns: Vec<String>,
        types: Vec<String>,
        rows: &[Vec<serde_json::Value>],
        note: String,
    ) -> Self {
        Self {
            columns,
            types,
            rows: rows
                .iter()
                .map(|row| row.iter().map(value_to_val).collect())
                .collect(),
            note,
        }
    }

    /// The driver-reported type of column `ci`, if any.
    pub(crate) fn col_type(&self, ci: usize) -> Option<&str> {
        self.types.get(ci).map(String::as_str)
    }
}

/// R91: a pinned "reference" row of a result grid. The row-end `❮` marker and
/// the status-bar `Δ` offset both read from it; nothing is re-queried, and the
/// snapshot of the cell values is what lets the first differing column be named
/// even after the row has scrolled out of the loaded window.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RefRow {
    /// Absolute 1-based row number, in the same space as [`cursor_abs_row`].
    pub(crate) abs: usize,
    /// The row's unfiltered cell values (all columns) at pin time.
    pub(crate) values: Vec<Val>,
    /// Column names parallel to `values`, captured so the first differing
    /// column can still be named if the visible column set changes.
    pub(crate) columns: Vec<String>,
}

// ─── app state ───────────────────────────────────────────────────────────────

/// SSH login method offered by the connection form. The string values match
/// the kernel's [`SshTunnelConfig::auth_method`] so a saved layer round-trips.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SshAuth {
    Password,
    Key,
    Agent,
}

impl SshAuth {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SshAuth::Password => "password",
            SshAuth::Key => "key",
            SshAuth::Agent => "agent",
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            SshAuth::Password => SshAuth::Key,
            SshAuth::Key => SshAuth::Agent,
            SshAuth::Agent => SshAuth::Password,
        }
    }
    /// Map a kernel `auth_method` (possibly empty on legacy layers) back to a
    /// form value, inferring from the populated credential fields when unset.
    pub(crate) fn from_layer(layer: &SshTunnelConfig) -> Self {
        match layer.auth_method.as_str() {
            "key" | "key+password" => SshAuth::Key,
            "agent" => SshAuth::Agent,
            "password" => SshAuth::Password,
            _ => {
                if !layer.key_path.trim().is_empty() {
                    SshAuth::Key
                } else if layer.use_ssh_agent {
                    SshAuth::Agent
                } else {
                    SshAuth::Password
                }
            }
        }
    }
}

/// One focusable row of the new/edit-connection form. The row list is dynamic:
/// the SSH rows only appear once the tunnel is enabled, and the credential rows
/// follow the selected auth method.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FormRow {
    Name,
    DbType,
    Host,
    Port,
    Username,
    Password,
    Database,
    /// R58: per-connection query timeout in seconds (empty = kernel default,
    /// `0` = no limit). Mapped to `ConnectionConfig::query_timeout_secs` and, on
    /// PostgreSQL, to a `statement_timeout` connection option.
    QueryTimeout,
    Ssl,
    /// Hard-block every write statement on this connection (safety valve).
    ReadOnly,
    Color,
    SshEnabled,
    SshHost,
    SshPort,
    SshUser,
    SshAuth,
    SshPassword,
    SshKeyPath,
    SshKeyPassphrase,
    SshAgentSock,
    Save,
}

#[derive(Clone)]
pub(crate) struct ConnForm {
    pub(crate) name: String,
    pub(crate) db_type: String,
    pub(crate) host: String,
    pub(crate) port: String,
    /// True once the user has typed a port themselves: the default-port
    /// auto-fill must never clobber a hand-entered value (R51).
    pub(crate) port_touched: bool,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) database: String,
    /// R58: query timeout field text. Empty means "inherit the kernel default
    /// (60 s)"; `0` means unlimited; a positive number is used verbatim.
    pub(crate) query_timeout: String,
    pub(crate) ssl: bool,
    /// Read-only connection flag (kernel `ConnectionConfig::read_only`).
    pub(crate) read_only: bool,
    /// Connection colour as `#rrggbb` (empty = no colour, family default).
    pub(crate) color: String,
    /// Index into the `Space`-cycled colour stops (none / presets / custom).
    pub(crate) color_sel: usize,
    // ── SSH tunnel (serialized to `transport_layers`) ──
    pub(crate) ssh_enabled: bool,
    pub(crate) ssh_host: String,
    pub(crate) ssh_port: String,
    pub(crate) ssh_user: String,
    pub(crate) ssh_auth: SshAuth,
    pub(crate) ssh_password: String,
    pub(crate) ssh_key_path: String,
    pub(crate) ssh_key_passphrase: String,
    pub(crate) ssh_agent_sock: String,
    /// Id of the connection being edited (`e` in the picker); `None` = create.
    pub(crate) edit_id: Option<String>,
    pub(crate) field: usize,
    /// First visible row of the scrollable field list (kept in sync by the
    /// renderer so a tall form still works on a phone-sized terminal).
    pub(crate) scroll: usize,
    pub(crate) editing: bool,
    pub(crate) err: String,
}

impl Default for ConnForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            db_type: "mysql".into(),
            host: String::new(),
            // R51: the form opens with the default port for the preselected
            // `mysql` type already filled in; changing the type re-derives it
            // until the user types a port of their own.
            port: "3306".into(),
            port_touched: false,
            username: String::new(),
            password: String::new(),
            database: String::new(),
            query_timeout: String::new(),
            ssl: false,
            read_only: false,
            color: String::new(),
            color_sel: 0,
            ssh_enabled: false,
            ssh_host: String::new(),
            ssh_port: "22".into(),
            ssh_user: String::new(),
            ssh_auth: SshAuth::Password,
            ssh_password: String::new(),
            ssh_key_path: String::new(),
            ssh_key_passphrase: String::new(),
            ssh_agent_sock: String::new(),
            edit_id: None,
            field: 0,
            scroll: 0,
            editing: false,
            err: String::new(),
        }
    }
}

/// Preset connection colours cycled by `Space` on the `color` form row. Index 0
/// is "no colour" (the database-family default applies), the middle entries are
/// terminal-safe RGB presets, and the final index is the free-form hex stop.
pub(crate) const CONN_COLOR_PRESETS: [&str; 10] = [
    "#e06c75", // red
    "#e5c07b", // amber
    "#98c379", // green
    "#56b6c2", // cyan
    "#61afef", // blue
    "#c678dd", // purple
    "#ff8800", // orange
    "#00d7af", // teal
    "#ffffff", // white
    "#808080", // gray
];
/// Number of `color_sel` stops: none + presets + custom.
pub(crate) const CONN_COLOR_STOPS: usize = CONN_COLOR_PRESETS.len() + 2;
/// `color_sel` index of the free-form hex stop.
pub(crate) const CONN_COLOR_CUSTOM: usize = CONN_COLOR_PRESETS.len() + 1;

/// `color_sel` for a stored hex value: 0 when unset, the preset index when it
/// matches a preset exactly, else the custom stop.
pub(crate) fn color_sel_for(value: &str) -> usize {
    let v = value.trim();
    if v.is_empty() {
        return 0;
    }
    if let Some(i) = CONN_COLOR_PRESETS
        .iter()
        .position(|p| p.eq_ignore_ascii_case(v))
    {
        return i + 1;
    }
    CONN_COLOR_CUSTOM
}

/// Canonical colour value for a `color_sel` stop (the custom stop keeps the text
/// the user typed).
pub(crate) fn color_value_for(sel: usize, custom: &str) -> String {
    if sel == 0 {
        String::new()
    } else if sel <= CONN_COLOR_PRESETS.len() {
        CONN_COLOR_PRESETS[sel - 1].to_string()
    } else {
        custom.to_string()
    }
}

/// Next `color_sel` stop, wrapping none → presets → custom → none.
pub(crate) fn color_next_sel(sel: usize) -> usize {
    (sel + 1) % CONN_COLOR_STOPS
}

/// Validate / normalise a form colour into the kernel's `#rrggbb` form. `Ok(None)`
/// means "no colour"; `Err(())` means the text is not a valid hex colour.
pub(crate) fn normalize_conn_color(raw: &str) -> Result<Option<String>, ()> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if parse_hex_color(v).is_none() {
        return Err(());
    }
    Ok(Some(format!(
        "#{}",
        v.trim_start_matches('#').to_ascii_lowercase()
    )))
}

/// R58: hard ceiling for a per-connection query timeout (one day). Anything
/// larger is almost certainly a typo and would make the setting useless.
pub(crate) const MAX_QUERY_TIMEOUT_SECS: u64 = 86_400;

/// R58: parse the connection form's query-timeout field. `Ok(None)` means the
/// field is blank and the saved / kernel default (60 s) applies; `Ok(Some(0))`
/// means unlimited; `Ok(Some(n))` is a finite limit. `Err(())` is a user error
/// (non-numeric or over [`MAX_QUERY_TIMEOUT_SECS`]).
pub(crate) fn parse_query_timeout(raw: &str) -> Result<Option<u64>, ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let secs: u64 = s.parse().map_err(|_| ())?;
    if secs > MAX_QUERY_TIMEOUT_SECS {
        return Err(());
    }
    Ok(Some(secs))
}

/// R58: apply the connection form's query-timeout field to a config. A blank
/// field keeps the kernel default (60 s); `0` disables the limit. On engines
/// whose URL builder reaches `tokio_postgres`, the limit is also mirrored into
/// the `statement_timeout` connection option so every pooled session enforces it
/// server-side (`SHOW statement_timeout` reflects it) with no per-query `SET`.
/// Pure so the mapping is unit-testable.
pub(crate) fn apply_form_query_timeout(cfg: &mut ConnectionConfig, field: &str) -> Result<(), ()> {
    let secs = parse_query_timeout(field)?
        .unwrap_or_else(dbx_core::models::connection::default_query_timeout_secs);
    cfg.query_timeout_secs = secs;
    if postgres_url_params_engine(cfg.db_type.as_str()) {
        cfg.url_params =
            with_pg_statement_timeout(cfg.url_params.as_deref(), Some(secs.saturating_mul(1000)));
    }
    Ok(())
}

/// The engines whose kernel URL builder passes `url_params` through to
/// `tokio_postgres` (Postgres / Redshift / CockroachDB). GaussDB, Kingbase, DM
/// and friends use their own driver scheme and would ignore an `options` entry.
pub(crate) fn postgres_url_params_engine(db_type: &str) -> bool {
    matches!(
        db_type.to_ascii_lowercase().as_str(),
        "postgres" | "postgresql" | "redshift" | "cockroachdb"
    )
}

/// Decode a `%XX`-escaped URL parameter value. `+` is left literal (libpq-style
/// URLs use `%20`), and a stray `%` is kept as-is.
pub(crate) fn percent_decode_url_value(input: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// R58: set (or clear) `statement_timeout` inside a PostgreSQL `url_params`
/// string, leaving every other parameter and `-c` option untouched.
///
/// The kernel's `options` URL parameter is the one connection-level channel that
/// reaches `tokio_postgres` — it becomes the session's `options` startup
/// argument, so `SHOW statement_timeout` reflects it on every pooled session
/// without a per-query `SET`. `Some(ms)` writes `-c statement_timeout=<ms>`
/// (`0` = unlimited); `None` drops any existing `statement_timeout` pair while
/// preserving the rest of the options — an all-empty result becomes `None`.
pub(crate) fn with_pg_statement_timeout(
    params: Option<&str>,
    millis: Option<u64>,
) -> Option<String> {
    let raw = params.unwrap_or("").trim().trim_start_matches('?');
    let mut kept: Vec<String> = Vec::new();
    let mut option_tokens: Vec<String> = Vec::new();
    for part in raw.split('&').filter(|p| !p.trim().is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        if !key.eq_ignore_ascii_case("options") {
            kept.push(part.to_string());
            continue;
        }
        let decoded = percent_decode_url_value(value);
        let tokens: Vec<&str> = decoded.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() {
            let is_pair = tokens[i] == "-c"
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| t.to_ascii_lowercase().starts_with("statement_timeout="));
            if is_pair {
                i += 2;
                continue;
            }
            option_tokens.push(tokens[i].to_string());
            i += 1;
        }
    }
    if let Some(ms) = millis {
        option_tokens.push("-c".to_string());
        option_tokens.push(format!("statement_timeout={ms}"));
    }
    if option_tokens.is_empty() {
        return if kept.is_empty() {
            None
        } else {
            Some(kept.join("&"))
        };
    }
    let base = kept.join("&");
    Some(dbx_core::connection::upsert_connection_url_param(
        Some(&base),
        "options",
        &option_tokens.join(" "),
    ))
}

/// Order the connection picker `s` cycles through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnSort {
    Name,
    Type,
    Color,
}

impl ConnSort {
    pub(crate) fn next(self) -> Self {
        match self {
            ConnSort::Name => ConnSort::Type,
            ConnSort::Type => ConnSort::Color,
            ConnSort::Color => ConnSort::Name,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            ConnSort::Name => t("名称"),
            ConnSort::Type => t("类型"),
            ConnSort::Color => t("颜色"),
        }
    }
}

/// Grouping key used by the colour sort: connections that share an explicit
/// colour stay together, and uncoloured ones group by database family so the
/// default badge colours still line up.
pub(crate) fn conn_color_group(cfg: &ConnectionConfig) -> String {
    match cfg
        .color
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(hex) => format!("c:{}", hex.trim_start_matches('#').to_ascii_lowercase()),
        None => format!("t:{}", cfg.db_type.as_str().to_ascii_lowercase()),
    }
}

/// Sort a connection list in place for the picker's `s` modes. Pure so it can be
/// tested without an `App`.
pub(crate) fn sort_connection_list(list: &mut [ConnectionConfig], mode: ConnSort) {
    match mode {
        ConnSort::Name => list.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        }),
        ConnSort::Type => list.sort_by(|a, b| {
            a.db_type
                .as_str()
                .cmp(b.db_type.as_str())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        }),
        ConnSort::Color => list.sort_by(|a, b| {
            conn_color_group(a)
                .cmp(&conn_color_group(b))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        }),
    }
}

/// Order the sidebar table list `s` cycles through. Size is deliberately not a
/// mode: the kernel exposes no table size, and counting every table would cost
/// one query per table — the sidebar is zero-query by design.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TableSort {
    Name,
    Type,
}

impl TableSort {
    pub(crate) fn next(self) -> Self {
        match self {
            TableSort::Name => TableSort::Type,
            TableSort::Type => TableSort::Name,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            TableSort::Name => t("名称"),
            TableSort::Type => t("类型"),
        }
    }
}

/// Views sort after tables so the type mode groups each kind together.
pub(crate) fn table_kind_rank(table_type: &str) -> u8 {
    if table_type.eq_ignore_ascii_case("VIEW") {
        1
    } else {
        0
    }
}

/// Sort the sidebar table list in place for the `s` modes. Case-insensitive and
/// stable-shaped (name breaks type ties, type breaks name ties), so the order is
/// identical in both languages and across runs.
pub(crate) fn sort_table_list(list: &mut [TableInfo], mode: TableSort) {
    match mode {
        TableSort::Name => list.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| table_kind_rank(&a.table_type).cmp(&table_kind_rank(&b.table_type)))
        }),
        TableSort::Type => list.sort_by(|a, b| {
            table_kind_rank(&a.table_type)
                .cmp(&table_kind_rank(&b.table_type))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
    }
}

/// The focusable rows for the current form state, in display order. Labels are
/// technical keycaps kept identical in both languages (like the base fields).
pub(crate) fn form_rows(f: &ConnForm) -> Vec<(FormRow, &'static str)> {
    let mut rows = vec![
        // R51: field order follows the real creation flow — pick the type first
        // (which fills the default port), then name, host, credentials, database.
        (FormRow::DbType, "db_type"),
        (FormRow::Name, "name"),
        (FormRow::Host, "host"),
        (FormRow::Port, "port"),
        (FormRow::Username, "username"),
        (FormRow::Password, "password"),
        (FormRow::Database, "database"),
        (FormRow::QueryTimeout, "query_timeout"),
        (FormRow::Ssl, "ssl"),
        (FormRow::ReadOnly, "read_only"),
        (FormRow::Color, "color"),
        (FormRow::SshEnabled, "ssh_tunnel"),
    ];
    if f.ssh_enabled {
        rows.push((FormRow::SshHost, "ssh_host"));
        rows.push((FormRow::SshPort, "ssh_port"));
        rows.push((FormRow::SshUser, "ssh_user"));
        rows.push((FormRow::SshAuth, "ssh_auth"));
        match f.ssh_auth {
            SshAuth::Password => rows.push((FormRow::SshPassword, "ssh_password")),
            SshAuth::Key => {
                rows.push((FormRow::SshKeyPath, "ssh_key"));
                rows.push((FormRow::SshKeyPassphrase, "ssh_passphrase"));
            }
            SshAuth::Agent => rows.push((FormRow::SshAgentSock, "ssh_agent")),
        }
    }
    rows.push((FormRow::Save, t("保存")));
    rows
}

/// R41: abbreviated form labels for a very narrow terminal. The SSH section
/// (which expands dynamically with the tunnel toggle) keeps a recognisable
/// `ssh.*` shape while the value column gains the reclaimed width.
pub(crate) fn form_label_short(label: &'static str) -> &'static str {
    match label {
        "db_type" => "type",
        "username" => "user",
        "password" => "pass",
        "database" => "db",
        "query_timeout" => "timeout",
        "ssh_tunnel" => "ssh",
        "ssh_host" => "ssh.host",
        "ssh_port" => "ssh.port",
        "ssh_user" => "ssh.user",
        "ssh_auth" => "ssh.auth",
        "ssh_password" => "ssh.pass",
        "ssh_key" => "ssh.key",
        "ssh_passphrase" => "ssh.pass",
        "ssh_agent" => "ssh.agent",
        "read_only" => "ro",
        other => other,
    }
}

/// The editable string behind a text row, if any (toggles return `None`).
pub(crate) fn form_text_mut(f: &mut ConnForm, row: FormRow) -> Option<&mut String> {
    match row {
        FormRow::Name => Some(&mut f.name),
        FormRow::DbType => Some(&mut f.db_type),
        FormRow::Host => Some(&mut f.host),
        FormRow::Port => Some(&mut f.port),
        FormRow::Username => Some(&mut f.username),
        FormRow::Password => Some(&mut f.password),
        FormRow::Database => Some(&mut f.database),
        FormRow::QueryTimeout => Some(&mut f.query_timeout),
        FormRow::Color => Some(&mut f.color),
        FormRow::SshHost => Some(&mut f.ssh_host),
        FormRow::SshPort => Some(&mut f.ssh_port),
        FormRow::SshUser => Some(&mut f.ssh_user),
        FormRow::SshPassword => Some(&mut f.ssh_password),
        FormRow::SshKeyPath => Some(&mut f.ssh_key_path),
        FormRow::SshKeyPassphrase => Some(&mut f.ssh_key_passphrase),
        FormRow::SshAgentSock => Some(&mut f.ssh_agent_sock),
        FormRow::Ssl
        | FormRow::ReadOnly
        | FormRow::SshEnabled
        | FormRow::SshAuth
        | FormRow::Save => None,
    }
}

/// The default port for a fresh connection of `db_type`, as an editable string.
/// `0` (local-file drivers like SQLite / DuckDB) and unknown types both render
/// as an empty field rather than a bogus `0`.
pub(crate) fn default_form_port(db_type: &str) -> String {
    parse_database_type(db_type.trim())
        .ok()
        .and_then(|dt| dbx_core::database_manifest::default_port(&dt))
        .filter(|p| *p > 0)
        .map(|p| p.to_string())
        .unwrap_or_default()
}

/// R51: keep the `port` field in sync with the selected `db_type` while the user
/// has not typed a port of their own. Called after every `db_type` change, so
/// picking MySQL / PostgreSQL / Redis / MongoDB fills 3306 / 5432 / 6379 / 27017
/// instantly; once `port_touched` is set the user's value is never overwritten.
pub(crate) fn apply_default_port(f: &mut ConnForm) {
    if f.port_touched {
        return;
    }
    f.port = default_form_port(&f.db_type);
}

/// R51: connection name generated when the user leaves `name` blank, shaped
/// `host-db_type` (e.g. `localhost-postgres`) so the picker stays scannable
/// without a manual name.
pub(crate) fn auto_conn_name(host: &str, db_type: &str) -> String {
    format!("{}-{}", host.trim(), db_type.trim())
}

/// The first SSH transport layer of a saved connection, if any. Used to prefill
/// the form when editing / duplicating a tunneled connection.
pub(crate) fn first_ssh_layer(cfg: &ConnectionConfig) -> Option<&SshTunnelConfig> {
    cfg.transport_layers.iter().find_map(|layer| match layer {
        TransportLayerConfig::Ssh(ssh) => Some(ssh),
        _ => None,
    })
}

/// Copy a saved connection into the form (duplicate / edit), including the SSH
/// tunnel section so a desktop-configured tunnel round-trips through the TUI.
pub(crate) fn form_from_connection(
    cfg: &ConnectionConfig,
    name: String,
    edit_id: Option<String>,
) -> ConnForm {
    let mut form = ConnForm {
        name,
        db_type: cfg.db_type.as_str().to_string(),
        host: cfg.host.clone(),
        port: if cfg.port == 0 {
            String::new()
        } else {
            cfg.port.to_string()
        },
        // A saved connection's port is authoritative: changing the type while
        // editing must not silently re-derive it (R51).
        port_touched: true,
        username: cfg.username.clone(),
        password: cfg.password.clone(),
        database: cfg.database.clone().unwrap_or_default(),
        // R58: an unset default (60 s) renders as an empty "inherit" field; an
        // explicit 0 (DBX's "no limit") or any custom value round-trips.
        query_timeout: if cfg.query_timeout_secs == 0 {
            "0".into()
        } else if cfg.query_timeout_secs
            == dbx_core::models::connection::default_query_timeout_secs()
        {
            String::new()
        } else {
            cfg.query_timeout_secs.to_string()
        },
        ssl: cfg.ssl,
        read_only: cfg.read_only,
        color: cfg.color.clone().unwrap_or_default(),
        color_sel: color_sel_for(cfg.color.as_deref().unwrap_or("")),
        edit_id,
        ..ConnForm::default()
    };
    if let Some(ssh) = first_ssh_layer(cfg) {
        form.ssh_enabled = ssh.enabled;
        form.ssh_host = ssh.host.clone();
        form.ssh_port = if ssh.port == 0 {
            "22".into()
        } else {
            ssh.port.to_string()
        };
        form.ssh_user = ssh.user.clone();
        form.ssh_auth = SshAuth::from_layer(ssh);
        form.ssh_password = ssh.password.clone();
        form.ssh_key_path = ssh.key_path.clone();
        form.ssh_key_passphrase = ssh.key_passphrase.clone();
        form.ssh_agent_sock = ssh.ssh_agent_sock_path.clone();
    }
    form
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Rects {
    pub(crate) sidebar: Rect,
    pub(crate) editor: Rect,
    pub(crate) cmd: Rect,
    pub(crate) results: Rect,
    pub(crate) picker: Rect,
    pub(crate) picker_visible: bool,
    pub(crate) db_picker: Rect,
    pub(crate) db_picker_visible: bool,
    /// Clickable horizontal scrollbar track (bottom border of the result grid).
    pub(crate) hbar: Rect,
    pub(crate) hbar_visible: bool,
    /// Tap targets for the `◀` / `▶` pan buttons drawn at either end of the bar.
    pub(crate) hbar_prev: Rect,
    pub(crate) hbar_next: Rect,
    /// Clickable `[ 执行 ]  [ 取消 ]` button row of the two confirmation layers
    /// (`confirm` and `history_confirm`), captured during the last render.
    pub(crate) confirm_ok: Rect,
    pub(crate) confirm_cancel: Rect,
    pub(crate) hist_ok: Rect,
    pub(crate) hist_cancel: Rect,
    /// The error box: a click is the `Enter` of the key path (widen a compact
    /// box; page through a long expanded one and close at the end).
    pub(crate) error_box: Rect,
    pub(crate) error_inner: Rect,
    pub(crate) error_max_scroll: u16,
    /// The row-detail popup. `inner` is the wrapped-line viewport, `scroll` its
    /// origin; `app.row_popup_hit` maps a physical line back to an entry.
    pub(crate) row_popup_inner: Rect,
    pub(crate) row_popup_scroll: u16,
    pub(crate) row_popup_visible: bool,
    /// The full-cell popup box (kept for the click geometry tests).
    pub(crate) cell_popup: Rect,
}

// ─── touch / swipe gesture layer ─────────────────────────────────────────────

/// How a horizontal swipe is recognised, from `DBXT_DRAG_PAN`:
///
/// * `button` (default) — a held left button that moves (`MouseEventKind::Drag`),
///   which is how phone terminals encode a left/right swipe when they do not
///   emit a horizontal wheel at all;
/// * `any` — also treat bare motion (`MouseEventKind::Moved`) as a swipe, for
///   terminals that report a touch drag without a button; a desktop mouse also
///   sends `Moved` constantly, so this is opt-in only;
/// * `off` — no swipe handling (wheel and keys only).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DragPan {
    Off,
    Button,
    Any,
}

impl DragPan {
    pub(crate) fn from_env() -> Self {
        Self::parse(&std::env::var("DBXT_DRAG_PAN").unwrap_or_default())
    }

    pub(crate) fn parse(v: &str) -> Self {
        match v.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "no" | "none" | "false" => DragPan::Off,
            "any" | "all" | "motion" | "moved" | "touch" => DragPan::Any,
            _ => DragPan::Button,
        }
    }
}

/// Finger columns of travel that make up one pan step (2:1 keeps a slow swipe
/// moving without making a fast flick jump the whole table away).
pub(crate) const DRAG_COLS_PER_STEP: i32 = 2;
/// A gesture that travelled this far is a swipe, not a tap.
pub(crate) const DRAG_TAP_SLOP: i32 = 2;
/// Cap on the columns one (possibly coalesced) drag event may pan.
pub(crate) const DRAG_MAX_STEPS: i32 = 4;

/// Turn finger travel into whole pan steps, carrying the remainder so a slow
/// swipe still moves the window instead of being rounded away.
pub(crate) fn pan_steps(accum: &mut i32, dx: i32) -> i32 {
    let cap = DRAG_MAX_STEPS * DRAG_COLS_PER_STEP;
    *accum = (*accum + dx).clamp(-cap, cap);
    let steps = *accum / DRAG_COLS_PER_STEP;
    *accum -= steps * DRAG_COLS_PER_STEP;
    steps
}

/// State machine that turns "button held + moved horizontally" into column pans.
/// A touch left/right swipe reaches us as `Drag(Left)` (button-event tracking) or
/// `Moved` (any-event tracking); a horizontal wheel (`ScrollLeft`/`ScrollRight`)
/// is a different, much rarer encoding, so both paths exist.
#[derive(Default, Clone, Copy)]
pub(crate) struct PanGesture {
    /// position of the previous mouse event, for the travel delta
    pub(crate) last: Option<(u16, u16)>,
    /// button currently held (a `Down` was seen)
    pub(crate) held: Option<MouseButton>,
    /// travel not yet converted into pan steps
    pub(crate) accum: i32,
    /// the gesture has travelled far enough to be a swipe, not a tap
    pub(crate) moved: bool,
    /// the terminal has sent at least one `Up`; only then may a tap be deferred
    pub(crate) saw_up: bool,
}

impl PanGesture {
    /// Feed one mouse event.
    ///
    /// `None` means "not part of a swipe, handle it normally"; `Some(steps)` means
    /// the event belongs to a swipe and the caller should swallow it, panning the
    /// column window by `steps` columns (`0` = swipe, but no step completed yet).
    pub(crate) fn feed(
        &mut self,
        kind: MouseEventKind,
        col: u16,
        row: u16,
        mode: DragPan,
    ) -> Option<i32> {
        let prev = self.last.replace((col, row));
        match kind {
            MouseEventKind::Down(b) => {
                self.held = Some(b);
                self.accum = 0;
                self.moved = false;
                None
            }
            MouseEventKind::Up(_) => {
                self.held = None;
                self.accum = 0;
                self.moved = false;
                self.saw_up = true;
                None
            }
            MouseEventKind::Drag(b) => {
                if mode == DragPan::Off || b != MouseButton::Left {
                    return None;
                }
                match self.held {
                    Some(MouseButton::Left) => {}
                    // another button owns this gesture (e.g. desktop text selection)
                    Some(_) => return None,
                    // Some terminals drop the press and only report the drag; start
                    // the gesture from the first drag event in that case.
                    None => {
                        self.held = Some(MouseButton::Left);
                        self.accum = 0;
                        self.moved = false;
                    }
                }
                self.travel(prev, col, row)
            }
            MouseEventKind::Moved => {
                let touching = match mode {
                    DragPan::Off => return None,
                    DragPan::Any => true,
                    DragPan::Button => self.held == Some(MouseButton::Left),
                };
                if !touching {
                    return None;
                }
                self.travel(prev, col, row)
            }
            _ => None,
        }
    }

    pub(crate) fn travel(&mut self, prev: Option<(u16, u16)>, col: u16, row: u16) -> Option<i32> {
        let (pc, pr) = prev?;
        let dx = col as i32 - pc as i32;
        let dy = row as i32 - pr as i32;
        if dx.abs() >= DRAG_TAP_SLOP || dy.abs() >= DRAG_TAP_SLOP {
            // The finger travelled: whatever happens next, this was not a tap.
            self.moved = true;
        }
        if dy.abs() > dx.abs() {
            // Vertical-dominant travel is not a horizontal pan: rows keep moving
            // through the wheel, which every terminal reports.
            self.accum = 0;
            return Some(0);
        }
        Some(pan_steps(&mut self.accum, dx))
    }

    /// True once the gesture travelled far enough that it must not also click.
    pub(crate) fn is_swipe(&self) -> bool {
        self.moved
    }

    /// A tap may only be deferred to its `Up` on a terminal that sends `Up`
    /// events; otherwise the press itself has to click or tapping would break.
    pub(crate) fn can_defer_tap(&self) -> bool {
        self.saw_up
    }
}

/// Two presses at the same spot within this many milliseconds are a double tap
/// (the mouse / finger equivalent of `Enter` on the focused row). 400 ms is the
/// value every desktop toolkit settles on and it is short enough that two
/// deliberate single taps never merge into one.
pub(crate) const DOUBLE_TAP_MS: u64 = 400;
/// A finger is not pixel-accurate, so the second tap may drift this far and
/// still count as the same spot.
pub(crate) const DOUBLE_TAP_SLOP: i32 = 1;

/// Double-tap / double-click detector.
///
/// `feed` is called on every *press* with the session's monotonic millisecond
/// clock and answers whether that press is the second of a double. Both tap
/// paths funnel through it: a terminal that reports `Up` feeds on `Down` and
/// acts on `Up` (so a swipe can still cancel the tap), while a touch terminal
/// that never reports `Up` feeds and acts on `Down` — either way the two presses
/// of a double are the two `Down`s, which is exactly what makes tap-tap work on
/// glass.
#[derive(Default, Clone, Copy)]
pub(crate) struct DoubleTap {
    /// `(clock, column, row)` of the previous, unpaired press.
    pub(crate) last: Option<(u64, u16, u16)>,
    /// True once this pair has been reported, so a third press starts a fresh
    /// pair instead of firing a second double straight away.
    pub(crate) fired: bool,
}

impl DoubleTap {
    pub(crate) fn feed(&mut self, now_ms: u64, col: u16, row: u16) -> bool {
        let double = matches!(self.last, Some((t, c, r))
            if !self.fired
                && now_ms.saturating_sub(t) < DOUBLE_TAP_MS
                && (col as i32 - c as i32).abs() <= DOUBLE_TAP_SLOP
                && (row as i32 - r as i32).abs() <= DOUBLE_TAP_SLOP);
        if double {
            self.fired = true;
            self.last = None;
        } else {
            self.fired = false;
            self.last = Some((now_ms, col, row));
        }
        double
    }
}

/// Mirror of tui-textarea's viewport, so a click inside the editor can be mapped
/// back to a `(row, col)` even after the text scrolled horizontally.
///
/// The widget keeps its scroll origin private, so this replays the same rule it
/// uses (`next_scroll_top` in tui-textarea's `widget.rs`): `render_main_area`
/// re-derives it after every frame that draws the editor, and the page-scroll
/// keys apply their delta here because they move the viewport without moving the
/// cursor out of it (which the render-time cursor-follow could not see).
#[derive(Default, Clone, Copy)]
pub(crate) struct EditorViewport {
    pub(crate) row: u16,
    pub(crate) col: u16,
    /// Size of the viewport on the last frame (0 before the first draw).
    pub(crate) w: u16,
    pub(crate) h: u16,
}

impl EditorViewport {
    /// The widget's own scroll-origin rule, reproduced exactly (including the
    /// degenerate zero-size case, so the mirror never drifts from the widget).
    pub(crate) fn next_top(prev: u16, cursor: u16, len: u16) -> u16 {
        if cursor < prev {
            cursor
        } else if prev.saturating_add(len) <= cursor {
            cursor + 1 - len
        } else {
            prev
        }
    }

    pub(crate) fn resize(&mut self, w: u16, h: u16) {
        self.w = w;
        self.h = h;
    }

    /// Follow the cursor after it moved (what the widget does at render time).
    pub(crate) fn follow(&mut self, cursor: (usize, usize)) {
        self.row = Self::next_top(self.row, cursor.0 as u16, self.h);
        self.col = Self::next_top(self.col, cursor.1 as u16, self.w);
    }

    /// A page scroll moves the origin by a full viewport height.
    pub(crate) fn page(&mut self, down: bool) {
        self.row = if down {
            self.row.saturating_add(self.h)
        } else {
            self.row.saturating_sub(self.h)
        };
    }

    /// R62: a half-page scroll moves the origin by half the viewport height,
    /// mirroring tui-textarea's `Scrolling::HalfPageDown` / `HalfPageUp`
    /// (`height / 2`, truncating). The widget then pulls the cursor back into the
    /// new viewport; the next render's `follow` lands the mirror on the same top.
    pub(crate) fn half_page(&mut self, down: bool) {
        let delta = self.h / 2;
        self.row = if down {
            self.row.saturating_add(delta)
        } else {
            self.row.saturating_sub(delta)
        };
    }

    /// Replay the viewport delta of the page-scroll keys that reach the widget
    /// (`PageUp` / `PageDown`, plus tui-textarea's built-in `Ctrl-V` = page down).
    pub(crate) fn note_key(&mut self, k: &KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::PageDown => self.page(true),
            KeyCode::PageUp => self.page(false),
            KeyCode::Char('v') if ctrl && !alt => self.page(true),
            _ => {}
        }
    }

    /// The text position under a click `(rel_x, rel_y)` inside the viewport.
    /// `CursorMove::Jump` clamps the result, so a click past the last line or
    /// past the end of a line lands on the nearest character (clicking the empty
    /// space below the text jumps to the end of the buffer).
    pub(crate) fn text_pos(&self, rel_x: u16, rel_y: u16) -> (u16, u16) {
        (
            self.row.saturating_add(rel_y),
            self.col.saturating_add(rel_x),
        )
    }
}

/// Panes that can be collapsed in the responsive layout.
pub(crate) const PANE_SIDEBAR: usize = 0;
pub(crate) const PANE_EDITOR: usize = 1;
pub(crate) const PANE_RESULTS: usize = 2;

/// A pending kernel SSH prompt awaiting a TUI answer. The handshake task is
/// suspended on `responder`; dropping it (or answering) resumes it.
pub(crate) struct SshPromptState {
    pub(crate) request: SshPromptRequest,
    pub(crate) responder: Option<tokio::sync::oneshot::Sender<SshPromptAnswer>>,
    /// Typed secret for a [`SshPromptKind::SecretInput`] challenge.
    pub(crate) input: String,
}

/// One row of the query-history browser (`Alt-H`): the statement plus the
/// metadata DBX stores next to it, so the panel can show when and where it ran.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryRow {
    pub(crate) id: String,
    pub(crate) sql: String,
    /// RFC3339 timestamp as DBX writes it (`2026-06-27T00:00:00Z`).
    pub(crate) executed_at: String,
    pub(crate) connection_name: String,
    /// Whether the statement succeeded, shown as a subtle marker.
    pub(crate) success: bool,
    /// Execution time in milliseconds, when dbxt recorded one (> 0). Desktop /
    /// CLI entries and multi-statement scripts may carry 0, which means
    /// "unknown" and is not shown (R45).
    pub(crate) duration_ms: u64,
    /// Where dbxt ran it from: `editor` / `script` / `direct`. Empty for
    /// entries another client wrote (no origin to show).
    pub(crate) origin: String,
    /// R51: how many times this statement appears in the loaded history window.
    /// The panel keeps one row per statement (newest first) and shows `×n` when
    /// it ran more than once, so a re-run loop does not flood the list.
    pub(crate) count: usize,
    /// R84: this row comes from the in-memory *session* run log (a statement
    /// dbxt itself just sent to the server), not from DBX's persisted history.
    /// Session rows carry no store id, so the delete gesture declines them.
    pub(crate) session: bool,
}

/// R84: one statement dbxt itself sent to the server during this session
/// (newest first, at most [`SESSION_RUN_MAX`]). Kept purely in memory — the
/// panel can show it instantly, before the persisted store answers, and it is
/// never written to disk.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionRun {
    pub(crate) sql: String,
    /// RFC3339 timestamp, so the panel reuses its `MM-DD HH:MM` label.
    pub(crate) executed_at: String,
    pub(crate) duration_ms: u64,
    pub(crate) success: bool,
    /// `editor` / `script` / `direct`.
    pub(crate) origin: &'static str,
    pub(crate) connection_name: String,
}

/// R84: how many session runs the in-memory log keeps (LRU window).
pub(crate) const SESSION_RUN_MAX: usize = 20;

impl SessionRun {
    /// Render one session run as the history row the panel draws: a synthetic
    /// `session:` id makes it recognisable without touching the store.
    pub(crate) fn to_row(&self, idx: usize) -> HistoryRow {
        HistoryRow {
            id: format!("session:{idx}"),
            sql: self.sql.clone(),
            executed_at: self.executed_at.clone(),
            connection_name: self.connection_name.clone(),
            success: self.success,
            duration_ms: self.duration_ms,
            origin: self.origin.to_string(),
            count: 1,
            session: true,
        }
    }
}

/// R51: collapse repeated statements in a newest-first history window into a
/// single row per statement, counting the repeats (`×n`). The newest occurrence
/// wins, so its timestamp / duration are what the row shows, and the survivors
/// keep their original order. This is the adjacency merge the panel needs: a
/// statement re-run back-to-back lands as consecutive entries, and the count
/// also folds in non-adjacent repeats.
pub(crate) fn merge_history_rows(rows: Vec<HistoryRow>) -> Vec<HistoryRow> {
    let mut out: Vec<HistoryRow> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    for mut r in rows {
        if let Some(&i) = index_of.get(&r.sql) {
            out[i].count += 1;
        } else {
            r.count = 1;
            index_of.insert(r.sql.clone(), out.len());
            out.push(r);
        }
    }
    out
}

/// Short badge for a [`HistoryRow::origin`] value, or `None` when unknown.
pub(crate) fn history_origin_badge(origin: &str) -> Option<&'static str> {
    match origin {
        "editor" => Some(t("编")),
        "script" => Some(t("脚")),
        "direct" => Some(t("直")),
        _ => None,
    }
}

/// `12` → `12ms`, `1234` → `1.2s`. Kept small so the panel stays narrow (R45).
pub(crate) fn history_duration_label(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// R63: the connect-time latency shown in the status bar. `12ms` under a
/// second, `1.2s` above it, so a slow link is obvious without a wide field.
pub(crate) fn format_rtt(d: Duration) -> String {
    history_duration_label(d.as_millis() as u64)
}

/// The red confirmation for deleting a single history entry. Deleting the
/// history row never touches the database's data.
#[derive(Clone)]
pub(crate) struct HistoryConfirm {
    pub(crate) id: String,
    pub(crate) sql: String,
}

/// Where the user was browsing on one connection (R41 smart restore): the
/// database / schema plus the table open in the data browser. Restoring it on a
/// later switch lands back on the same spot when the new connection still has a
/// database / table with those names.
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct ConnPointer {
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: Option<String>,
}

/// A compact execution-error overlay (R41). The first line plus a line count is
/// shown on a small screen; `Enter` widens it to the full, scrollable text.
#[derive(Clone)]
pub(crate) struct ErrorPopup {
    pub(crate) lines: Vec<String>,
    pub(crate) expanded: bool,
    pub(crate) scroll: u16,
}

/// One stop on the `Alt-←` / `Alt-→` round-trip stack (R42). Tables, Mongo
/// collections and Redis keys are all first-class navigation nodes; a value /
/// document *detail* view is deliberately not a node, so a back step lands on
/// the list entry that opened it. Mongo collections are stored as [`Self::Table`]
/// because they live in the same sidebar list as SQL tables.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum NavEntry {
    Table {
        db: String,
        schema: String,
        table: String,
    },
    RedisKey {
        db: u32,
        key_raw: String,
        key_display: String,
    },
}

/// One visible row of the sidebar connection tree (R43). The sidebar used to be
/// a flat connection header + database row + table list; it is now a real tree:
/// group → connection → database → table, with the same rows navigable by one
/// cursor. `depth` counts the ancestor *groups* (0 when a node is not inside a
/// group), so R48's nested desktop groups indent the whole subtree under them.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum SideRow {
    /// R48: a desktop sidebar group (`▾ name [n]`). `count` is the number of
    /// live connections inside it (nested groups included).
    Group {
        id: String,
        name: String,
        depth: usize,
        count: usize,
        open: bool,
    },
    /// A connection root (every saved connection is a root).
    Conn { idx: usize, depth: usize },
    /// A non-current connection's database list is being fetched.
    ConnLoading { idx: usize, depth: usize },
    /// A non-current connection's database list could not be fetched.
    ConnError {
        idx: usize,
        msg: String,
        depth: usize,
    },
    /// A database under a connection. `idx` is the connection's index.
    Db {
        idx: usize,
        db: String,
        depth: usize,
    },
    /// A table / collection under the current connection's current database.
    /// `table` indexes `App::tables`.
    Table {
        idx: usize,
        table: usize,
        depth: usize,
    },
}

/// R54: a stable identity for one tree node, so the quick search can find the
/// same node again after the tree is rebuilt around it (Enter / Esc restore the
/// full tree and re-seat the cursor).
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum SideHit {
    Group(String),
    Conn(String),
    Db { conn: String, db: String },
    Table(String),
}

// ── R55: in-place tree rename / reorder ──

/// Longest accepted connection / group display name. The field is a single-line
/// sidebar label, so anything longer would only ever be truncated; rejecting it
/// at save time keeps a typo (a stuck key) from silently becoming the name.
pub(crate) const MAX_CONN_NAME_LEN: usize = 64;

/// Upper bound for a manual per-column width override (R55).
pub(crate) const COL_W_MAX: usize = 80;

/// In-place rename state for a tree row (R55). Deliberately a plain string
/// buffer — the same editing model the connection form uses (append /
/// Backspace) — so there is no new text widget and no modal prompt: the name is
/// edited on the row itself, Enter saves and Esc cancels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenameEdit {
    pub(crate) target: RenameTarget,
    /// The live edit buffer; starts as the current name.
    pub(crate) text: String,
}

/// What an in-place rename is editing. Each target is addressed by a stable id
/// (not a list index) so a background refresh / re-sort cannot retarget the edit
/// at the wrong row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RenameTarget {
    /// A connection root.
    Conn { id: String },
    /// A desktop sidebar group.
    Group { id: String },
}

/// Per-column width overrides for result grids (R55, persisted in R72). Keyed by
/// a scope string (connection + database + schema + table, or a query bucket)
/// and then by column *name*, so page turns, re-queries and reopening the same
/// table in one session keep the widths. A browsed table's widths are additionally
/// mirrored into [`TuiConfig`] so they survive a restart; a plain query result
/// stays session-only.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ColWidthMemory {
    pub(crate) scopes: HashMap<String, HashMap<String, usize>>,
}

impl ColWidthMemory {
    pub(crate) fn get(&self, scope: &str, col: &str) -> Option<usize> {
        self.scopes.get(scope).and_then(|m| m.get(col)).copied()
    }

    /// Drop one column's session override, so the renderer falls back to the
    /// natural width (or a persisted width, which the caller clears too).
    pub(crate) fn reset(&mut self, scope: &str, col: &str) {
        if let Some(m) = self.scopes.get_mut(scope) {
            m.remove(col);
            if m.is_empty() {
                self.scopes.remove(scope);
            }
        }
    }

    /// Drop every session override for one scope. Returns how many columns were
    /// dropped.
    pub(crate) fn clear_scope(&mut self, scope: &str) -> usize {
        self.scopes.remove(scope).map(|m| m.len()).unwrap_or(0)
    }

    /// R85: set `col`'s override to an absolute width (the auto-fit `g w` / `g W`
    /// gesture), clamped like a manual adjustment, and return the stored value.
    pub(crate) fn set(&mut self, scope: &str, col: &str, width: usize) -> usize {
        let w = width.clamp(MIN_CELL_WIDTH, COL_W_MAX);
        self.scopes
            .entry(scope.to_string())
            .or_default()
            .insert(col.to_string(), w);
        w
    }

    /// Widen / narrow `col` by `delta` display cells and return the new width.
    /// The first adjustment starts from `current` (the width on screen), so a key
    /// press is always a small step from what the user sees.
    pub(crate) fn adjust(&mut self, scope: &str, col: &str, current: usize, delta: i32) -> usize {
        let base = self
            .get(scope, col)
            .unwrap_or(current)
            .clamp(MIN_CELL_WIDTH, COL_W_MAX) as i32;
        let next = (base + delta).clamp(MIN_CELL_WIDTH as i32, COL_W_MAX as i32) as usize;
        self.scopes
            .entry(scope.to_string())
            .or_default()
            .insert(col.to_string(), next);
        next
    }

    /// The overrides for one scope, applied by the renderer after the natural
    /// content widths.
    pub(crate) fn overrides(&self, scope: &str) -> Option<&HashMap<String, usize>> {
        self.scopes.get(scope)
    }
}

// ── desktop sidebar groups (R48) ──

/// One entry inside a group (or at the top of the desktop sidebar tree): either
/// a nested group or a connection id. The order is the desktop's own order, so
/// the dbxt tree mirrors it exactly (a group may hold `conn A`, `group B`,
/// `conn C` in any interleaving).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LayoutNode {
    Group(LayoutGroup),
    Conn(String),
}

/// One group in DBX Desktop's persisted sidebar tree, parsed from
/// `sidebar_layout.layout_json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LayoutGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    /// Members (nested groups and connections) in desktop order.
    pub(crate) nodes: Vec<LayoutNode>,
}

/// The parsed desktop sidebar tree. Empty = no grouping: every connection is
/// shown flat, exactly as before this round.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidebarLayout {
    /// Top-level groups, in desktop order. Ungrouped connections are not stored
    /// here — they are drawn flat from `App::connections`, after the groups.
    pub(crate) groups: Vec<LayoutGroup>,
}

/// Parse `sidebar_layout.layout_json` into [`SidebarLayout`]. Only `groups`
/// (metadata) and `order` (the tree) matter; every other desktop field is
/// ignored. Parsed by hand from `serde_json::Value` rather than through a
/// derive: `serde` is only a transitive dependency of this crate (it depends on
/// `serde_json`), and the round's rule is not to touch the dependency set. A
/// malformed tree, an unknown group id, or a non-JSON value degrades to the
/// empty layout (flat list), so a corrupt desktop file never breaks the sidebar.
/// Semantics mirror DBX's own `mcp_policy::connection_group_paths`.
pub(crate) fn parse_sidebar_layout(value: &serde_json::Value) -> SidebarLayout {
    let Some(obj) = value.as_object() else {
        return SidebarLayout::default();
    };
    // Group metadata: id → display name.
    let mut names: HashMap<String, String> = HashMap::new();
    if let Some(groups) = obj.get("groups").and_then(|v| v.as_array()) {
        for g in groups {
            let (Some(id), Some(name)) = (
                g.get("id").and_then(|v| v.as_str()),
                g.get("name").and_then(|v| v.as_str()),
            ) else {
                return SidebarLayout::default();
            };
            names.insert(id.to_string(), name.to_string());
        }
    }
    let Some(order) = obj.get("order").and_then(|v| v.as_array()) else {
        // No `order` at all is a valid “no groups” layout (an empty sidebar).
        return SidebarLayout::default();
    };
    let mut out = SidebarLayout::default();
    for entry in order {
        if entry.get("type").and_then(|v| v.as_str()) != Some("group") {
            // A top-level connection: drawn flat from `App::connections`.
            continue;
        }
        match build_layout_group(entry, &names) {
            Some(group) => out.groups.push(group),
            // A group with no metadata is structurally invalid: fall back to the
            // flat list rather than guessing a name.
            None => return SidebarLayout::default(),
        }
    }
    out
}

/// Build one group entry (and, recursively, its members). `children` and the
/// legacy flat `connectionIds` are both accepted; `children` (when present)
/// wins, matching DBX's parser. Members keep their desktop order.
pub(crate) fn build_layout_group(
    entry: &serde_json::Value,
    names: &HashMap<String, String>,
) -> Option<LayoutGroup> {
    let id = entry.get("id").and_then(|v| v.as_str())?;
    let name = names.get(id)?.clone();
    let mut group = LayoutGroup {
        id: id.to_string(),
        name,
        nodes: Vec::new(),
    };
    if let Some(children) = entry.get("children").and_then(|v| v.as_array()) {
        for child in children {
            match child.get("type").and_then(|v| v.as_str()) {
                Some("group") => group
                    .nodes
                    .push(LayoutNode::Group(build_layout_group(child, names)?)),
                Some("connection") => group.nodes.push(LayoutNode::Conn(
                    child.get("id").and_then(|v| v.as_str())?.to_string(),
                )),
                _ => return None,
            }
        }
    } else if let Some(ids) = entry.get("connectionIds").and_then(|v| v.as_array()) {
        for id in ids {
            group.nodes.push(LayoutNode::Conn(id.as_str()?.to_string()));
        }
    }
    Some(group)
}

/// Per-connection state of the sidebar tree's lazy database fetch.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum TreeDbState {
    Loading,
    Error(String),
}

/// One database's lazily fetched size metadata (R45). Fetched only when the
/// user presses `s` on a database row, then cached for the session. `rows` and
/// `sizes` are keyed by lowercased table name (the engines differ in case).
#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct DbSizeInfo {
    /// `SUM(data_length + index_length)` (MySQL) or `pg_database_size` (PG).
    pub(crate) total_bytes: Option<u64>,
    /// Per-table row estimate (`information_schema.tables.table_rows` on MySQL,
    /// `pg_class.reltuples` on PG). Empty when the engine has no such metadata.
    pub(crate) rows: HashMap<String, u64>,
    /// Per-table on-disk bytes, when the metadata exposes it.
    pub(crate) sizes: HashMap<String, u64>,
}

/// Parse the raw JSON cells of one metadata query into a `u64`, tolerating the
/// numeric shape drivers report (`number` or a numeric `string`).
pub(crate) fn json_u64(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => {
            n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
        }
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f.max(0.0) as u64),
        _ => None,
    }
}

/// Build a [`DbSizeInfo`] from `(name, row_estimate, bytes)` rows (MySQL shape:
/// `data_length + index_length`). The total is the sum of the per-table bytes.
pub(crate) fn parse_db_size_info(rows: &[Vec<serde_json::Value>]) -> DbSizeInfo {
    let mut info = DbSizeInfo::default();
    let mut total: u64 = 0;
    for row in rows {
        let name = row.first().and_then(|v| v.as_str()).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if let Some(r) = row.get(1).and_then(json_u64) {
            info.rows.insert(key.clone(), r);
        }
        if let Some(b) = row.get(2).and_then(json_u64) {
            info.sizes.insert(key, b);
            total = total.saturating_add(b);
        }
    }
    if !info.sizes.is_empty() {
        info.total_bytes = Some(total);
    }
    info
}

/// `2.1 GB` / `512 MB` / `0 B` — human-readable byte size for the sidebar (R45).
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0usize;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[unit])
    }
}

/// `12` / `1.2k` / `3.4M` — a compact row-count estimate for the sidebar (R45).
pub(crate) fn human_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    }
}

/// Parse dbxt's run-origin stamp out of a history entry's `details_json`.
pub(crate) fn history_origin_from_details(details: Option<&str>) -> String {
    details
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .and_then(|v| {
            v.get("dbxt_origin")
                .and_then(|o| o.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// MySQL: one cheap metadata scan returns both the per-table row estimate and
/// the on-disk bytes (the caller sums the latter into the database total).
pub(crate) fn mysql_db_size_sql(db: &str) -> String {
    format!(
        "SELECT table_name, table_rows, \
         COALESCE(data_length, 0) + COALESCE(index_length, 0) \
         FROM information_schema.tables WHERE table_schema = '{}'",
        db.replace('\'', "''")
    )
}

/// PostgreSQL: the whole-database on-disk size.
pub(crate) fn pg_db_size_sql(db: &str) -> String {
    format!("SELECT pg_database_size('{}')", db.replace('\'', "''"))
}

/// PostgreSQL: per-relation row estimate (`reltuples`) and total size. An empty
/// `schema` falls back to the connection's visible schema.
pub(crate) fn pg_table_size_sql(schema: &str) -> String {
    let ns = if schema.trim().is_empty() {
        "n.nspname = current_schema()".to_string()
    } else {
        format!("n.nspname = '{}'", schema.replace('\'', "''"))
    };
    format!(
        "SELECT c.relname, c.reltuples::bigint, pg_total_relation_size(c.oid) \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p') AND {ns}"
    )
}

/// R63: ordering of the recency panel (`t` / `Alt-R`). `Recent` keeps the
/// canonical most-recently-browsed-first order; `Name` re-sorts the same list
/// client-side, by `schema.table` then database, so a jump is easy to scan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RecentSort {
    Recent,
    Name,
}

/// R81: ordering of the loaded Redis keys (`Ctrl-T`). `Scan` keeps the order
/// SCAN delivered; the two TTL modes re-sort the already-loaded window in place
/// (never a query), so a keyspace can be scanned by expiry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RedisSort {
    Scan,
    TtlAsc,
    TtlDesc,
}

impl RedisSort {
    pub(crate) fn next(self) -> Self {
        match self {
            RedisSort::Scan => RedisSort::TtlAsc,
            RedisSort::TtlAsc => RedisSort::TtlDesc,
            RedisSort::TtlDesc => RedisSort::Scan,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            RedisSort::Scan => t("扫描顺序"),
            RedisSort::TtlAsc => t("TTL 升序"),
            RedisSort::TtlDesc => t("TTL 降序"),
        }
    }
}

/// R82: ordering of the loaded MongoDB documents (`Ctrl-S`). `Natural` keeps the
/// order the page arrived in; the size modes re-sort the already-loaded window
/// in place (never a query), so a collection can be eyeballed by document size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MongoSizeSort {
    Natural,
    SizeAsc,
    SizeDesc,
}

impl MongoSizeSort {
    pub(crate) fn next(self) -> Self {
        match self {
            MongoSizeSort::Natural => MongoSizeSort::SizeDesc,
            MongoSizeSort::SizeDesc => MongoSizeSort::SizeAsc,
            MongoSizeSort::SizeAsc => MongoSizeSort::Natural,
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            MongoSizeSort::Natural => t("原始顺序"),
            MongoSizeSort::SizeAsc => t("大小升序"),
            MongoSizeSort::SizeDesc => t("大小降序"),
        }
    }
}

pub(crate) struct App {
    pub(crate) backend: Arc<LocalBackend>,
    pub(crate) page: Page,
    pub(crate) focus: Focus,
    pub(crate) quit: bool,

    pub(crate) connections: Vec<ConnectionConfig>,
    /// R83: session-only connections opened through the SQLite quick-open
    /// (`L`). They live in `connections` for the picker but are deliberately
    /// never written to the connection store, so a restart shows no ghost.
    pub(crate) temp_conns: Vec<ConnectionConfig>,
    pub(crate) conn_list: ListState,
    pub(crate) picker_open: bool,
    /// Order of the connection picker (`s` cycles name / type / colour).
    pub(crate) conn_sort: ConnSort,
    /// R92: index into the static tip pool shown on the connection-list page.
    /// Seeded from the day-of-year at startup so it rotates daily; `T` advances
    /// it. Session-only — nothing is persisted.
    pub(crate) tip_idx: usize,
    /// Bumped on every connection switch; a `list_databases` reply that carries
    /// an older id is dropped so a slow enumeration cannot clobber the new
    /// connection's database list (R41 makes switching a one-keystroke affair).
    pub(crate) conn_gen: u64,
    /// The connection the user was on before the current one (R41 `Alt-Tab` /
    /// `Alt-`` toggles the two). Updated by every switch, including `Alt-<n>`.
    pub(crate) last_conn_id: Option<String>,
    /// R87: the connections this session has activated, most-recent first
    /// (LRU, capped at [`CONN_RECENT_MAX`]). Session-only bookkeeping; `Alt-Shift-H`
    /// lists them and Enter switches straight back.
    pub(crate) conn_recent: Vec<String>,
    /// R87: the recent-connection overlay (`Alt-Shift-H`) is open.
    pub(crate) conn_recent_open: bool,
    pub(crate) conn_recent_list: ListState,
    /// Per-connection memory of where the user was browsing (`database` /
    /// `schema` / open table), so switching back lands on the same spot when it
    /// still exists. Keyed by connection id.
    pub(crate) conn_pointers: HashMap<String, ConnPointer>,
    /// The pointer a switch in flight wants to restore once the new
    /// connection's database / table lists arrive.
    pub(crate) pending_restore: Option<ConnPointer>,
    /// One-shot notice appended to the landing status after a switch (e.g. the
    /// editor still holds uncommitted text).
    pub(crate) switch_notice: Option<String>,

    pub(crate) selected: Option<ConnectionConfig>,
    pub(crate) databases: Vec<String>,
    pub(crate) db_index: usize,
    /// Schemas of the current database (empty for engines without a schema
    /// layer, e.g. MySQL). Fetched once per database.
    pub(crate) schemas: Vec<String>,
    /// Currently browsed schema; empty when the engine has no schema layer.
    pub(crate) schema: String,
    /// Which database `schemas` belongs to, so a database switch refetches.
    pub(crate) schemas_db: String,
    /// Monotonic id of the latest table-list request; a stale reply is discarded.
    pub(crate) tables_gen: u64,

    pub(crate) tables: Vec<TableInfo>,
    pub(crate) table_list: ListState,
    /// Unfiltered table list; `tables` is this list with the `/` filter applied.
    pub(crate) tables_all: Vec<TableInfo>,
    /// Active sidebar table-name filter (`/`, filter-as-you-type).
    pub(crate) table_filter: String,
    pub(crate) table_prompt: Option<TextArea<'static>>,
    /// R54: sidebar tree quick search (`f`). A transient needle over the
    /// *loaded* tree cache (connection / database / table names); unlike the
    /// persistent `/` filter it never talks to the server, forces hit groups
    /// open, and Enter jumps to the first hit then clears the needle.
    pub(crate) tree_search: String,
    pub(crate) tree_search_prompt: Option<TextArea<'static>>,
    /// Row the cursor sat on when the quick search opened, so Esc can put it
    /// back on that node after the tree is restored.
    pub(crate) tree_search_prev: Option<SideHit>,
    /// Sidebar table-list order (`s` cycles name / type).
    pub(crate) table_sort: TableSort,
    /// Last first-letter jump (R39): `;` / `,` repeat it forward / backward.
    pub(crate) table_jump_letter: Option<char>,

    // ── sidebar connection tree (R43) ──
    /// Connection roots the user expanded (session memory). The active
    /// connection is inserted on every switch so it starts open.
    pub(crate) tree_conn_open: std::collections::HashSet<String>,
    /// Connection roots the user explicitly collapsed. The active root is open
    /// unless it is listed here (so it starts open without being un-collapsible).
    pub(crate) tree_conn_closed: std::collections::HashSet<String>,
    /// Database nodes of the *current* connection the user explicitly collapsed,
    /// so the active database's tables can be hidden (session memory).
    pub(crate) tree_db_closed: std::collections::HashSet<String>,
    /// Lazily fetched database lists for connections other than the active one,
    /// keyed by connection id. Filled when a root is expanded.
    pub(crate) tree_dbs: std::collections::HashMap<String, Vec<String>>,
    /// Per-connection state of that lazy fetch (`None` = idle / loaded).
    pub(crate) tree_db_state: std::collections::HashMap<String, TreeDbState>,
    /// Monotonic request id per connection, so a stale reply is dropped.
    pub(crate) tree_gen: std::collections::HashMap<String, u64>,
    /// R48: DBX Desktop's sidebar groups (parsed from `sidebar_layout`). Empty
    /// when the desktop never grouped anything, in which case the tree stays
    /// exactly as it was: every connection flat.
    pub(crate) sidebar_layout: SidebarLayout,
    /// R55: the raw `sidebar_layout` JSON the tree was parsed from. Kept so a
    /// group rename / row move can patch one entry and save the whole tree back
    /// verbatim (every desktop-only field included).
    pub(crate) sidebar_layout_raw: Option<serde_json::Value>,
    /// R55: the tree row currently being renamed in place (connection or group).
    pub(crate) rename_edit: Option<RenameEdit>,
    /// R48: group ids the user collapsed this session (groups default open).
    pub(crate) group_closed: std::collections::HashSet<String>,
    /// The flattened visible tree rows, rebuilt whenever the tree can change.
    pub(crate) side_rows: Vec<SideRow>,
    /// Cursor into `side_rows` (the sidebar's single selection).
    pub(crate) side_sel: usize,
    /// Last `table_list` selection mirrored into `side_sel`; a change means an
    /// external caller moved the table cursor, so the tree follows it.
    pub(crate) side_table_seen: Option<usize>,
    /// Session cache of lazily fetched database sizes (R45), keyed by database
    /// name on the *active* connection. Populated only by an explicit `s`.
    pub(crate) db_sizes: std::collections::HashMap<String, DbSizeInfo>,
    /// Per-database state of that lazy fetch (`None` = idle / loaded).
    pub(crate) db_size_state: std::collections::HashMap<String, TreeDbState>,
    /// Monotonic request id per database, so a stale reply is dropped.
    pub(crate) db_size_gen: std::collections::HashMap<String, u64>,
    /// R47b: cached kernel liveness per connection id — `true` when DBX holds a
    /// pool for it. Refreshed by `Op::ConnStatus` (a pure registry read) and
    /// updated optimistically on connect / disconnect so the tree dot reacts at
    /// once. The kernel is the source of truth; this is only a cache.
    pub(crate) conn_live: HashMap<String, bool>,
    /// R47b: connections whose pool is being opened right now (a switch or a
    /// lazy tree expand). Drives the half-filled `◐` status dot.
    pub(crate) conn_connecting: HashSet<String>,
    /// R52: server version per connection id (session cache). Read once when a
    /// connection becomes active — a single free metadata query, never on the
    /// browse hot path — so the status bar can name the environment without
    /// another terminal. A failed read stays uncached, so a later switch retries.
    pub(crate) server_versions: HashMap<String, String>,
    /// R63: the connect-time latency of the version probe above, per connection
    /// id. Measured once (the same read, no extra query) and shown muted in the
    /// status bar; a failed probe leaves no entry, so nothing is reported.
    pub(crate) server_rtts: HashMap<String, Duration>,
    /// R63: recency panel ordering — most-recently-browsed first, or by name.
    pub(crate) recent_sort: RecentSort,

    /// Client-side substring filter over the loaded Redis keys (R42 one-step
    /// type-to-filter, mirrors the sidebar table filter). `redis_scan.keys` is
    /// the filtered view; `redis_scan.all` keeps the full loaded window.
    pub(crate) redis_filter: String,
    pub(crate) redis_filter_prompt: Option<TextArea<'static>>,
    /// R81: client-side type filter (`t`) over the loaded keys; `None` = all.
    pub(crate) redis_type_filter: Option<String>,
    /// R81: scan-order / TTL ordering of the loaded keys (`Ctrl-T`).
    pub(crate) redis_sort: RedisSort,
    /// Last Redis first-letter jump, so `;` / `,` repeat it forward / backward.
    pub(crate) redis_jump_letter: Option<char>,

    /// R42: the results pane shows a statement separator line plus a `12.3ms`
    /// prefix per statement (console feel). Off by default; `Alt-O` toggles it.
    pub(crate) show_stmt_timing: bool,

    // table structure
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) ddl: Option<String>,
    pub(crate) struct_view: StructView,
    pub(crate) ddl_scroll: u16,

    pub(crate) editor: TextArea<'static>,
    pub(crate) history: Vec<String>,
    pub(crate) history_idx: Option<usize>,
    pub(crate) history_draft: String,
    /// One-shot snapshot of the editor text before the last `Alt-F` reformat, so
    /// a single `Ctrl-U` can undo it (tui-textarea's own undo needs two steps
    /// for a whole-buffer replace).
    pub(crate) editor_undo: Option<String>,

    // ── editor buffer find (R61 `Ctrl-F`) ──
    /// The modal one-line input while the find needle is being typed (bottom
    /// bar). `None` once the input is closed, even while the highlight stays.
    pub(crate) editor_find: Option<TextArea<'static>>,
    /// Active find needle. Non-empty keeps every match highlighted in the
    /// editor; `Esc` closes the input but keeps the highlight until the next
    /// edit. Case-insensitive substring, entirely client-side (no query).
    pub(crate) editor_find_needle: String,
    /// Index into the current hit list of the match the caret last jumped to,
    /// so the status bar can show `3/7`.
    pub(crate) editor_find_idx: Option<usize>,
    /// Editor buffer snapshot from when the needle was computed; any edit makes
    /// the matches stale, so the highlight is dropped.
    pub(crate) editor_find_snapshot: Vec<String>,

    // ── R77: execution-error statement location (Alt-E / F8) ──
    /// Char-offset spans in the editor buffer of the statements that failed in
    /// the most recent editor run, sorted by position. Non-empty paints the
    /// whole statement(s) and lets `Alt-E` / `F8` cycle them. Client-side only
    /// (text location, zero queries).
    pub(crate) editor_error_spans: Vec<EditorErrorSpan>,
    /// Index into [`App::editor_error_spans`] of the failing statement the
    /// caret currently sits on (the accent-coloured one).
    pub(crate) editor_error_idx: usize,
    /// Editor buffer snapshot when the errors were located; any edit drops the
    /// highlight (the offsets would no longer address the same text).
    pub(crate) editor_error_snapshot: Vec<String>,
    /// The status line that carried the error, kept verbatim so a jump can
    /// append `第 N 条语句` without losing the message.
    pub(crate) editor_error_base: String,

    // ── query-history panel (Alt-H) ──
    /// The overlay is open (it owns the keyboard until Esc / Enter).
    pub(crate) history_open: bool,
    /// Cursor inside the *filtered* view (`history_view`), not `history_rows`.
    pub(crate) history_list: ListState,
    /// Recent entries loaded from DBX's shared history (newest first, unique SQL).
    pub(crate) history_rows: Vec<HistoryRow>,
    /// R84: the raw persisted rows as loaded from the store (unmerged), so a
    /// session run can be re-merged on top without losing the `×n` counts.
    pub(crate) history_persisted: Vec<HistoryRow>,
    /// R84: statements dbxt sent to the server during this session (newest
    /// first, LRU [`SESSION_RUN_MAX`]). In-memory only; the panel shows them
    /// instantly and they are never written to disk by dbxt.
    pub(crate) session_runs: Vec<SessionRun>,
    /// SQL texts already saved as DBX favourites, for the `★` marker and the `f`
    /// toggle.
    pub(crate) history_favorites: HashSet<String>,
    /// Active `/` filter needle (case-insensitive substring of the statement).
    pub(crate) history_needle: String,
    /// The `/` input while it is being typed (modal on top of the panel).
    pub(crate) history_filter: Option<TextArea<'static>>,
    /// Indices into `history_rows` that pass the needle: the list is the view.
    pub(crate) history_view: Vec<usize>,
    /// The red layer for deleting one history entry.
    pub(crate) history_confirm: Option<HistoryConfirm>,
    /// Where the next [`execute_sql`] came from, so a run that goes through the
    /// danger-confirm layer still records its origin (R45).
    pub(crate) pending_run_origin: &'static str,
    /// R88: the scope of the last editor run when it was narrowed to a selection
    /// or the statement under the cursor. `None` for a whole-buffer run.
    pub(crate) pending_scope: Option<ScopedRun>,
    /// The in-flight query was started by `Ctrl-Enter` in the history panel;
    /// the result status names it a direct run plus its elapsed time (R45).
    pub(crate) direct_run: bool,

    // results
    pub(crate) grid: Option<Grid>,
    pub(crate) grid_kind: GridKind,
    pub(crate) page_state: Option<PageState>,
    pub(crate) script: Option<ScriptView>,
    pub(crate) sel: usize, // cursor row inside the current page / result set
    /// R57: row-selection anchor for the results grid (`V`). `None` = not in
    /// row-select mode; otherwise `min(anchor, sel)..=max(anchor, sel)` is the
    /// highlighted block (anchor == sel is a single row).
    pub(crate) row_sel_anchor: Option<usize>,
    pub(crate) col_offset: usize, // leftmost column of the scrollable window
    pub(crate) col_cursor: usize, // focused column (cell cursor)
    /// R91: `g s` — the pinned reference row of the current result grid, if any.
    pub(crate) ref_row: Option<RefRow>,
    /// R47b: while `Instant::now() < deadline` the horizontal scroll bar is
    /// drawn. A horizontal scroll (wheel / drag / pan / column jump) refreshes
    /// it; at rest the bar hides so the bottom border is not a permanent thick
    /// band. `None` means "never poked yet" (hidden).
    pub(crate) hbar_until: Option<Instant>,
    pub(crate) vis_cols: usize, // columns currently visible (set while rendering)
    /// Width cap actually used for the last render (compact mode aware).
    pub(crate) grid_max_cell: usize,
    pub(crate) freeze_first: bool, // pin the first data column (row-number gutter is always pinned)
    /// R91: `g f` — additional pinned columns (ascending, deduped). The first
    /// column keeps its own `z` toggle; together they pin at most
    /// [`MAX_FROZEN_COLS`]. Out-of-range indices are ignored at render time.
    pub(crate) frozen_cols: Vec<usize>,
    pub(crate) cell_popup: Option<CellPopup>,
    pub(crate) row_popup: Option<RowPopup>,
    /// Compact / expandable execution-error overlay (R41).
    pub(crate) error_popup: Option<ErrorPopup>,

    // ── mobile efficiency ──
    /// Compact column-width mode (`None` = automatic for a narrow terminal).
    pub(crate) compact: Option<bool>,
    /// R76: big-number display mode for result cells (`#` cycles it).
    pub(crate) num_fmt: NumFmt,
    /// R76: alternate-row banding in the result grid (config, default on).
    pub(crate) stripe: bool,
    /// R79: auto-indent the new line after `Enter` inside a SQL block (input
    /// behaviour, default on; `tui.json` can turn it off).
    pub(crate) editor_indent: bool,
    /// R79: auto-close `(` / `[` and skip over an existing closer (input
    /// behaviour, default on; `tui.json` can turn it off).
    pub(crate) editor_pairs: bool,
    /// R88: show statement ordinals in the editor's left gutter (render-only,
    /// default off; `tui.json` `stmt_gutter`, toggled with `F2`).
    pub(crate) stmt_gutter: bool,
    /// R88: the gutter width used on the last frame (0 = no gutter). Read by
    /// the click mapping and the editor paint layers so text stays aligned.
    pub(crate) editor_gutter: u16,
    /// Column names hidden for this browsing session (Ctrl-Shift-H).
    pub(crate) col_hidden: HashSet<String>,
    pub(crate) col_picker_open: bool,
    pub(crate) col_picker_list: ListState,
    /// R48: the `gc` column-structure popup (name / type / nullable / comment
    /// from the cached `table_meta`, no extra query). `cols_popup_scroll` is the
    /// top visible line.
    pub(crate) cols_popup_open: bool,
    pub(crate) cols_popup_scroll: u16,
    /// R75: the sidebar table-node info card (`i`). Read-only, sourced entirely
    /// from metadata already in the session cache (the tree's `TableInfo`, the
    /// open table's `table_meta`, and the per-database `db_sizes` row / size
    /// estimates). Opening it never issues a query.
    pub(crate) table_info_open: bool,
    pub(crate) table_info_scroll: u16,
    /// R56: `/` inside the popup filters the column list by name.
    /// `cols_popup_needle` is the active needle (empty = show every column) and
    /// `cols_popup_filter` is the modal one-line input while it is being typed.
    pub(crate) cols_popup_needle: String,
    pub(crate) cols_popup_filter: Option<TextArea<'static>>,
    /// R65: the highlighted row of the `gc` popup, as an index into the
    /// *filtered* column rows. Enter jumps the cell cursor to that column.
    pub(crate) cols_popup_sel: usize,
    /// The unfiltered grid backing the filtered `grid` (needed to re-show a
    /// hidden column without re-querying).
    pub(crate) grid_full: Option<Grid>,
    /// The five most recently browsed `(database, schema, table)` triples.
    pub(crate) recent_tables: Vec<(String, String, String)>,
    pub(crate) recent_open: bool,
    pub(crate) recent_list: ListState,
    /// R65: the in-data-view table switcher (`g b`). The needle is edited inline
    /// (type to filter) over the current database's *cached* table list — never
    /// a query. Enter opens the highlighted table's data view.
    pub(crate) table_jump_open: bool,
    pub(crate) table_jump_needle: String,
    pub(crate) table_jump_list: ListState,
    /// Browser-style back/forward history of browsed tables / collections /
    /// Redis keys (`Alt-←` / `Alt-→`). `nav_pos` is the cursor into it; opening
    /// a node truncates the forward branch and appends, exactly like a browser.
    pub(crate) nav_history: Vec<NavEntry>,
    pub(crate) nav_pos: usize,
    /// A Redis key a history step wants to open once its key list has loaded
    /// (the db switch rescans asynchronously).
    pub(crate) pending_open_redis_key: Option<String>,
    /// The transient `← table` / `→ table` landing hint from the last history
    /// step. Shown at the head of the status bar's context block (where a narrow
    /// screen cannot truncate it) and cleared by the next key press.
    pub(crate) nav_landing: Option<String>,
    /// One-shot discovery hint shown the first time a data grid appears this
    /// session (`Enter 看整行 · v 看单元格`). Any key clears it, and a deadline
    /// fades it after a few seconds so it never lingers.
    pub(crate) row_hint_shown: bool,
    pub(crate) row_hint_until: Option<Instant>,
    /// Memoised wrap for the cell / row / error text popups (R42).
    pub(crate) popup_cache: Option<PopupCache>,
    /// A `(schema, table)` to open as soon as the (new) table list arrives.
    pub(crate) pending_open_table: Option<(String, String)>,
    /// A `WHERE` predicate to apply when the next table opens (a search-hit
    /// jump). Cleared once consumed by `open_table_data`.
    pub(crate) pending_table_filter: Option<String>,
    /// SQL prefix-completion popup in the editor (Alt-/).
    pub(crate) completion: Option<Completion>,

    // ── persistent preferences (db.table granularity) ──
    /// Column visibility / compact / sort restored per `(database, table)`.
    pub(crate) config: TuiConfig,
    pub(crate) config_path: Option<PathBuf>,

    // ── result-grid search (`/` in the results pane) ──
    /// The modal input while `/` is being typed (filter-as-you-type).
    pub(crate) result_filter: Option<TextArea<'static>>,
    /// Active search needle; non-empty hides every non-matching row.
    pub(crate) result_needle: String,
    /// Displayed row index → row index in the unfiltered grid (row filter map).
    pub(crate) result_rows: Vec<usize>,
    // ── results-pane column filter (`*`) ──
    /// Column the `*` filter is scoped to, stored by *name* so a hidden-column
    /// toggle cannot silently retarget it. `None` = no column filter.
    pub(crate) col_filter_name: Option<String>,
    /// Active column-filter needle; non-empty keeps only rows whose cell in the
    /// named column contains it (case-insensitive, client-side).
    pub(crate) col_filter_needle: String,
    /// The modal input while the `*` prompt is being typed.
    pub(crate) col_filter_prompt: Option<TextArea<'static>>,
    // ── grid value locate (`gv` in the results pane) ──
    /// The modal input while `gv` is being typed.
    pub(crate) locate_prompt: Option<TextArea<'static>>,
    /// Active locate needle; unlike `result_needle` it never hides rows — the
    /// cursor jumps to the match and `n`/`N` cycle the hits in place.
    pub(crate) locate_needle: String,
    /// The grid column the locate searched (sort / primary-key / first), kept
    /// so `n`/`N` do not have to re-derive it after a page change.
    pub(crate) locate_col: Option<usize>,
    // ── result-set cell find (`\` in the results pane, R64) ──
    /// The modal input while `\` is being typed.
    pub(crate) cell_find_prompt: Option<TextArea<'static>>,
    /// Active cell-find needle; non-empty highlights every matching cell on the
    /// loaded page and lets `n`/`N` step through them. Unlike `/` it never
    /// hides a row, and unlike `gv` it searches *every* column.
    pub(crate) cell_find_needle: String,
    /// `(row, col)` of every matching cell in the *displayed* grid, in reading
    /// order (row-major). Indices address [`App::grid`].
    pub(crate) cell_find_hits: Vec<(usize, usize)>,
    /// Index into [`App::cell_find_hits`] of the match the cursor last landed
    /// on, so the render can paint it with the accent colour.
    pub(crate) cell_find_idx: usize,
    /// True when the hit list reached [`DEFAULT_CELL_FIND_LIMIT`] and stopped.
    pub(crate) cell_find_capped: bool,
    // ── grid column jump (`|` in the results pane) ──
    /// The modal input for `|` (column number or name prefix).
    pub(crate) col_jump: Option<TextArea<'static>>,
    /// R56: `:` in the results pane — jump to a row by number (`:12`) or to the
    /// last row (`:$`). The modal input while the row number is typed.
    pub(crate) goto_prompt: Option<TextArea<'static>>,
    /// Last SQL sent to the backend, used to guess a table for `y`.
    pub(crate) last_sql: Option<String>,
    /// Last SQL actually executed (set by [`execute_sql`]), so the quit guard
    /// can tell unrun editor text from an already-executed statement.
    pub(crate) last_executed: Option<String>,
    /// Two-stage quit: the first quit key press arms this (when there is unrun
    /// editor text / an active filter); a second press quits.
    pub(crate) quit_armed: bool,

    // ── global database search (Alt-G) ──
    /// The modal search-term input.
    pub(crate) search_input: Option<TextArea<'static>>,
    /// Committed term (kept for the overlay title / rescan).
    pub(crate) search_query: String,
    /// Results overlay open (the list owns the keyboard).
    pub(crate) search_open: bool,
    pub(crate) search_list: ListState,
    pub(crate) search_hits: Vec<SearchHit>,
    /// `(done, total)` tables while a scan runs.
    pub(crate) search_progress: Option<(usize, usize)>,
    /// A scan is in flight.
    pub(crate) search_running: bool,
    /// Tables skipped as too large: `(table, estimate)`.
    pub(crate) search_skipped: Vec<(String, u64)>,
    /// Monotonic scan id; only the newest reply lands.
    pub(crate) search_gen: u64,
    /// Cancellation flag shared with the running scan (Esc aborts).
    pub(crate) search_cancel: Arc<AtomicBool>,
    /// True when the hit list was capped at [`SEARCH_MAX_HITS`].
    pub(crate) search_truncated: bool,

    // ── schema diff (Alt-D / Shift+Alt-D) ──
    /// The `Alt-D` target picker (source is the focused table / current database).
    pub(crate) diff_picker: Option<DiffPicker>,
    /// The open two-table diff overlay.
    pub(crate) diff: Option<Box<SchemaDiffState>>,
    /// The open two-database table-list diff overlay.
    pub(crate) db_diff: Option<Box<DbDiffState>>,
    /// Monotonic id of the latest diff request; a stale reply is discarded.
    pub(crate) diff_gen: u64,

    // ── data compare (Alt-K / picker `m`) ──
    /// The open two-table data-diff overlay.
    pub(crate) data_diff: Option<Box<DataDiffState>>,
    /// The optional `WHERE` input for a data compare (modal, on top of the
    /// picker).
    pub(crate) data_where: Option<TextArea<'static>>,
    /// Monotonic id of the latest data-compare request.
    pub(crate) data_diff_gen: u64,
    /// `(done, total)` chunks while a data compare runs.
    pub(crate) data_progress: Option<(usize, usize)>,
    /// Cancellation flag shared with the running compare (Esc aborts).
    pub(crate) data_cancel: Arc<AtomicBool>,

    // ── SQL file execution (Alt-L) ──
    /// The modal file-path input.
    pub(crate) file_load_prompt: Option<TextArea<'static>>,
    /// The preview / confirmation layer for a read `.sql` file.
    pub(crate) file_load_plan: Option<Box<FileLoadPlan>>,

    // ── SQLite quick-open (`L`) ──
    /// The modal file picker overlay (recent files + directory listing).
    pub(crate) sqlite_open: Option<SqliteOpen>,

    // WHERE filter prompt (modal text input)
    pub(crate) filter_prompt: Option<TextArea<'static>>,

    // cell edit dialog: diff-style confirmation before any write is sent
    pub(crate) edit_dialog: Option<EditDialog>,
    // a write is in flight; on success refresh the current page instead of
    // replacing the grid with the DML result
    pub(crate) pending_write: bool,
    // success message kept until the refreshed page lands so it is not lost
    pub(crate) pending_write_msg: Option<String>,
    // queued edits for one transactional batch commit (Ctrl-S)
    pub(crate) batch: Vec<String>,
    // manual per-pane collapse override (None = follow `auto_collapse`)
    pub(crate) pane_override: [Option<bool>; 3],
    // master switch for responsive collapse: on = unfocused panes collapse
    pub(crate) auto_collapse: bool,

    // help overlay
    pub(crate) help_open: bool,
    pub(crate) help_scroll: u16,
    /// R60: `/` filter needle for the full `?` cheat-sheet (key or feature
    /// name; pure client-side, never touches the database).
    pub(crate) help_needle: String,
    /// The modal input while `/` is being typed inside the help overlay.
    pub(crate) help_filter: Option<TextArea<'static>>,

    // touch / terminal fallbacks
    //   pan_mode: vertical wheel pans columns instead of rows (for phone terminals
    //   that never emit a horizontal wheel for a left/right swipe).
    pub(crate) pan_mode: bool,
    //   drag_pan: how a swipe is recognised (see `DragPan`).
    pub(crate) drag_pan: DragPan,
    //   gesture: swipe state for the drag → column-pan path.
    pub(crate) gesture: PanGesture,
    //   pending_tap: a results-pane click waiting for its `Up`, so the press that
    //   starts a swipe does not also select a row / jump the scrollbar. The bool
    //   marks the second press of a double tap, which opens the row detail.
    pub(crate) pending_tap: Option<(u16, u16, bool)>,
    //   tap / popup_tap: double-tap detectors for the result grid (double click =
    //   row detail) and for the row-detail popup (double click = drill into the
    //   cell). Kept apart so a click that opened the popup cannot pair with the
    //   first click inside it.
    pub(crate) tap: DoubleTap,
    pub(crate) popup_tap: DoubleTap,
    //   mouse_epoch: session monotonic clock the tap detectors time against.
    pub(crate) mouse_epoch: Instant,
    //   editor_vp: mirror of tui-textarea's viewport for click-to-place-cursor.
    pub(crate) editor_vp: EditorViewport,
    //   row_popup_hit: physical (wrapped) line → entry position, captured by the
    //   row-popup render so a click selects the entry under the pointer.
    pub(crate) row_popup_hit: Vec<usize>,
    //   When `DBXT_EVENT_TRACE` is set, mouse/resize events are appended here so a
    //   user can report exactly what their terminal sends. Keystrokes are never
    //   traced (a password field would leak).
    pub(crate) trace_path: Option<PathBuf>,
    pub(crate) last_event: Option<String>,
    //   `DBXT_MOUSE_DEBUG`: same log plus a live on-screen event readout, so a
    //   phone user can see what a swipe is encoded as without leaving the TUI.
    pub(crate) mouse_debug: bool,
    pub(crate) mouse_log: VecDeque<String>,

    // successive query results, switchable with `[` / `]`
    pub(crate) result_tabs: Vec<ResultTab>,
    pub(crate) result_tab: usize,
    /// R48: a pinned results-pane snapshot drawn above the live grid (`Alt-F`).
    pub(crate) pinned_result: Option<PinnedResult>,
    // the last query that hit the row cap, for `Ctrl-N` "load more"
    pub(crate) query_more: Option<(String, usize)>,

    // saved-SQL snippet overlay (DBX's `saved_sql_files`)
    pub(crate) snippet_open: bool,
    pub(crate) snippet_list: ListState,
    pub(crate) snippets: Vec<SnippetRow>,
    /// `/` filter needle for the favourites list (name or SQL text).
    pub(crate) snippet_needle: String,
    /// The modal input while `/` is being typed.
    pub(crate) snippet_filter: Option<TextArea<'static>>,
    /// Filtered display index → index into `snippets`.
    pub(crate) snippet_view: Vec<usize>,
    /// Store id awaiting a `d` delete confirmation.
    pub(crate) snippet_confirm: Option<String>,
    /// True when the snippet overlay was opened with `Alt-P` (quick paste at the
    /// cursor) instead of `Ctrl-O` (append to the editor).
    pub(crate) snippet_insert: bool,
    /// Name prompt shown when saving the editor's SQL as a DBX favourite.
    pub(crate) snippet_name: Option<TextArea<'static>>,

    // R71: built-in SQL template panel (`Alt-T` in the editor; read-only, pure
    // text — never touches the connection). The panel is the single-statement
    // counterpart to the R57 batch TSV/DELETE/UPDATE generators.
    pub(crate) template_open: bool,
    pub(crate) template_list: ListState,
    /// `/` filter needle for the template list (label or SQL text).
    pub(crate) template_needle: String,
    /// The modal input while `/` is being typed.
    pub(crate) template_filter: Option<TextArea<'static>>,
    /// Filtered display index → index into `TEMPLATES`.
    pub(crate) template_view: Vec<usize>,
    /// True while a just-inserted template still has `{{…}}` placeholders, so
    /// `Tab` jumps between them and the placeholder paint is on.
    pub(crate) template_active: bool,
    /// Start (row, col) of the placeholder the caret last selected, so `Tab`
    /// can find the *next* one even right after the token was replaced.
    pub(crate) template_ph_start: Option<(usize, usize)>,

    // column metadata for the table currently open in the data browser
    pub(crate) table_meta: Option<TableMeta>,
    // session cache of row counts, keyed by db/table/filter; the bool marks a
    // lower bound (the row-count sample cap was hit).
    pub(crate) count_cache: HashMap<String, (u64, bool)>,

    // pagination hand-off between key handling and the async page load
    pub(crate) pending_sel: Option<usize>,
    // focus to apply when the next page arrives (None = do not steal focus)
    pub(crate) pending_focus: Option<Focus>,
    pub(crate) page_pending: bool,
    // monotonically increasing id of the latest table-data request
    pub(crate) page_gen: u64,
    // The first page is deferred until the table's columns arrive, so the
    // browser knows the primary key before it picks keyset-vs-OFFSET ordering.
    pub(crate) pending_open_page: bool,
    // Deep OFFSET paging (no primary key to seek by) is slow; show the hint once
    // per table session.
    pub(crate) deep_page_hint_shown: bool,
    pub(crate) pending_deep_hint: bool,

    // database switcher overlay
    pub(crate) db_picker_open: bool,
    pub(crate) db_list: ListState,
    pub(crate) pending_table: Option<String>,

    // grid geometry captured while rendering, used to map clicks back to cells
    pub(crate) grid_gutter: u16,
    pub(crate) grid_frozen: usize,
    /// R91: the actual pinned columns captured at the last render, so click
    /// mapping / panning / the header readout know *which* columns are frozen
    /// (not merely how many).
    pub(crate) grid_frozen_cols: Vec<usize>,
    pub(crate) grid_widths: Vec<usize>,
    /// width available to the scrollable column window, captured while rendering.
    /// `pan_columns` recomputes the visible-column count from it so the cell
    /// cursor lands inside the window the renderer will actually draw (a stale
    /// count would let `window_for_cursor` undo the pan).
    pub(crate) grid_avail: usize,
    /// Version counter for the displayed grid, bumped whenever `grid` is
    /// replaced. The column-width cache below is keyed on it so a scroll never
    /// rescans the whole result set.
    pub(crate) grid_epoch: u64,
    /// Cached natural column widths for `(grid_epoch, max_cell)`. Building a
    /// 20k-row grid's widths is O(cells); caching turns the per-frame cost into
    /// a lookup.
    pub(crate) width_cache: Option<(u64, usize, Vec<usize>)>,
    /// R55: session-only per-column width overrides for result grids (`<` / `>`).
    pub(crate) col_width_mem: ColWidthMemory,

    pub(crate) confirm: Option<Confirm>,

    pub(crate) loading: bool,
    /// Backend calls currently in flight. The spinner only stops when this hits
    /// zero, so a fast secondary result (e.g. the history fetch) cannot make a
    /// slow primary one (the table list) look finished.
    pub(crate) pending_ops: usize,
    /// When the oldest in-flight call started, shown as elapsed seconds so a slow
    /// query is visibly progressing rather than apparently hung.
    pub(crate) loading_since: Option<Instant>,
    pub(crate) spinner: usize,
    pub(crate) status: String,
    /// R75: a transient status message ("关闭 X" / "已清除 Y") that auto-clears
    /// after [`FLASH_TTL`] unless a newer message replaces it. `flash_text`
    /// guards the clear so a message set in the meantime is never wiped.
    pub(crate) flash_until: Option<Instant>,
    pub(crate) flash_text: String,

    pub(crate) backend_kind: Backend,
    pub(crate) cmd_input: TextArea<'static>,
    pub(crate) cmd_output: Vec<String>,
    pub(crate) redis_db: u32,
    // ── Redis key browser ──
    pub(crate) redis_scan: RedisScanState,
    pub(crate) redis_list: ListState,
    pub(crate) redis_value: Option<RedisValueView>,
    pub(crate) redis_prompt: Option<RedisPrompt>,
    /// Raw keys multi-selected in the browser (space / shift-range / a).
    pub(crate) redis_selected: HashSet<String>,
    /// Anchor index for a shift range selection.
    pub(crate) redis_anchor: Option<usize>,
    /// A batch confirm awaiting its typed re-confirmation.
    pub(crate) redis_pending_batch: Option<RedisConfirm>,
    /// R57: wall clock of the last local TTL countdown step for the loaded key
    /// browser, so the `…s` badge refreshes without a server round-trip.
    pub(crate) redis_ttl_clock: Instant,
    // ── MongoDB document browser ──
    pub(crate) mongo_page: usize,
    pub(crate) mongo_filter: String,
    pub(crate) mongo_gen: u64,
    /// Documents of the current page, so `e` / `Del` can map a grid row back to
    /// its source document.
    pub(crate) mongo_docs: Vec<serde_json::Value>,
    /// R82: the page's documents in arrival order, so `Ctrl-S` can re-sort the
    /// loaded window by size and restore the natural order without a query.
    pub(crate) mongo_docs_base: Vec<serde_json::Value>,
    /// R82: active client-side size ordering of the loaded documents.
    pub(crate) mongo_size_sort: MongoSizeSort,
    /// R82: the `gf` field-name jump prompt (client-side over the loaded page).
    pub(crate) mongo_field_prompt: Option<TextArea<'static>>,
    /// R82: the `c` dotted-path extraction prompt (client-side, then copy).
    pub(crate) mongo_path_prompt: Option<TextArea<'static>>,
    /// The JSON editor dialog for a MongoDB document edit / insert.
    pub(crate) mongo_dialog: Option<MongoDocDialog>,

    pub(crate) form: ConnForm,
    /// A pending kernel SSH prompt (host-key TOFU / keyboard-interactive) the
    /// handshake task is suspended on, plus the one-shot responder.
    pub(crate) ssh_prompt: Option<SshPromptState>,
    /// Last host-key notice, surfaced in the status line (best-effort).
    pub(crate) ssh_notice: Option<String>,

    // ── CSV import ──
    /// The file-path step, before the CSV is read.
    pub(crate) import_prompt: Option<ImportPrompt>,
    /// The parsed preview / confirmation layer.
    pub(crate) import_plan: Option<Box<ImportPlan>>,
    /// Monotonic id of the latest plan request; a stale reply is discarded.
    pub(crate) import_gen: u64,
    /// Preview scroll offset.
    pub(crate) import_scroll: u16,
    /// `(done, total)` while an import runs.
    pub(crate) import_progress: Option<(usize, usize)>,
    /// The completion overlay.
    pub(crate) import_report: Option<Box<ImportReport>>,

    // ── data transfer (Alt-T) ──
    /// The three-step transfer wizard (target connection → name → options).
    pub(crate) transfer: Option<Box<TransferWizard>>,
    /// The completion summary overlay.
    pub(crate) transfer_report: Option<Box<TransferReport>>,
    /// Monotonic id of the latest transfer request; a stale reply is discarded.
    pub(crate) transfer_gen: u64,
    /// Live progress: `(rows, chunks, elapsed_ms, total_rows)`.
    pub(crate) transfer_progress: Option<(u64, usize, u128, Option<u64>)>,
    /// Cancellation flag shared with the running transfer (Esc aborts).
    pub(crate) transfer_cancel: Arc<AtomicBool>,

    // ── result export (Ctrl-Y) ──
    /// The format picker.
    pub(crate) export_open: bool,
    pub(crate) export_list: ListState,
    /// The chosen format, waiting for a destination.
    pub(crate) export_pending: Option<ExportPending>,
    /// The destination prompt (blank = clipboard, else a file path).
    pub(crate) export_path: Option<TextArea<'static>>,

    // ── connection import / export (Alt-E / Alt-I) ──
    /// The `Alt-E` bundle export overlay (destination, passwords, confirm).
    pub(crate) conn_export: Option<Box<ConnExport>>,
    /// The `Alt-I` file-path prompt, before the bundle is read.
    pub(crate) conn_import_path: Option<TextArea<'static>>,
    /// The parsed preview / duplicate-resolution layer.
    pub(crate) conn_import_plan: Option<Box<ConnImportPlan>>,

    pub(crate) layout_mode: LayoutMode,
    pub(crate) term_h: u16,
    /// Terminal width captured each frame. Overlay titles and the results header
    /// read it to pick a compressed layout on narrow screens (<56 cols).
    pub(crate) term_w: u16,
    /// `?` opens a context mini cheat-sheet first (`help_mini`), and a second `?`
    /// (or `Enter`) widens it to the full `help_open` overlay.
    pub(crate) help_mini: bool,
    /// Pending second key of a `gd` / `gt` (goto definition / goto data) chord.
    pub(crate) pending_g: bool,
    /// Vim-style count prefix: the digits typed before a motion (`5n`, `3j`).
    /// Flushed to a direct list jump when no motion follows within
    /// [`COUNT_JUMP_TIMEOUT`].
    pub(crate) count_buf: String,
    pub(crate) count_deadline: Option<Instant>,
    pub(crate) rects: Rects,
}

impl App {
    /// Session monotonic clock in milliseconds, used to time double taps.
    pub(crate) fn now_ms(&self) -> u64 {
        self.mouse_epoch.elapsed().as_millis() as u64
    }
    /// R47b: keep the horizontal scroll bar on screen for the next
    /// [`HBAR_VISIBLE_MS`] after a horizontal scroll.
    pub(crate) fn poke_hbar(&mut self) {
        self.hbar_until = Some(Instant::now() + Duration::from_millis(HBAR_VISIBLE_MS));
    }
    /// R75: set a transient status message that auto-clears after [`FLASH_TTL`]
    /// (unless another message replaces it first). Used by `Esc` so its
    /// "关闭 X / 已清除 Y" feedback is visible but never lingers.
    pub(crate) fn flash(&mut self, text: String) {
        self.flash_text = text.clone();
        self.flash_until = Some(Instant::now() + FLASH_TTL);
        self.status = text;
    }
    pub(crate) fn selected_name(&self) -> String {
        self.selected
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }
    /// Queue a backend call, keeping the spinner up until *every* in-flight call
    /// has answered.
    pub(crate) fn spawn(&mut self, tx: &Tx, op: Op) {
        self.pending_ops = self.pending_ops.saturating_add(1);
        if self.pending_ops == 1 {
            self.loading_since = Some(Instant::now());
        }
        self.loading = true;
        spawn_op(&self.backend, tx, op);
    }
    pub(crate) fn current_db(&self) -> String {
        self.databases
            .get(self.db_index)
            .cloned()
            .unwrap_or_default()
    }
    pub(crate) fn set_placeholder(&mut self) {
        let t = match self.backend_kind {
            Backend::Redis => tf("redis 命令… (db={}) · Ctrl-L 切换", &[&(self.redis_db)]),
            Backend::Mongo => tf(
                "mongo shell… (db={}) · Ctrl-L 切换",
                &[&(self.current_db())],
            ),
            Backend::Sql => String::new(),
        };
        self.cmd_input.set_placeholder_text(t);
    }
    pub(crate) fn set_editor_text(&mut self, text: &str) {
        let mut ta = TextArea::from(text.split('\n'));
        ta.set_placeholder_text(t("SQL … (Ctrl-J 当前句/选区 · F5 全部 · ↑ 历史)"));
        ta.move_cursor(CursorMove::Bottom);
        ta.move_cursor(CursorMove::End);
        self.editor = ta;
    }
    pub(crate) fn editor_sql(&self) -> String {
        self.editor.lines().join("\n")
    }
    /// R84: record one statement dbxt just sent to the server in the session
    /// run log. Dedup-promote: a re-run of an earlier statement removes the old
    /// entry and floats the fresh one to the top (LRU, most recent first),
    /// capped at [`SESSION_RUN_MAX`]. Purely in memory — nothing is persisted.
    pub(crate) fn push_session_run(&mut self, mut run: SessionRun) {
        let sql = run.sql.trim().to_string();
        if sql.is_empty() {
            return;
        }
        run.sql = sql.clone();
        if let Some(pos) = self.session_runs.iter().position(|r| r.sql == sql) {
            self.session_runs.remove(pos);
        }
        if self.session_runs.len() >= SESSION_RUN_MAX {
            self.session_runs.truncate(SESSION_RUN_MAX - 1);
        }
        self.session_runs.insert(0, run);
        self.rebuild_history_rows();
    }

    /// R84: rebuild the panel's display rows from the in-memory session log plus
    /// the raw persisted rows. Session runs come first (they are the newest) and
    /// a repeated statement collapses into one row with a `×n` count, so the
    /// session entry wins and the persisted duplicate folds into it.
    pub(crate) fn rebuild_history_rows(&mut self) {
        let mut rows: Vec<HistoryRow> = self
            .session_runs
            .iter()
            .enumerate()
            .map(|(i, r)| r.to_row(i))
            .collect();
        rows.extend(self.history_persisted.iter().cloned());
        self.history_rows = merge_history_rows(rows);
    }

    pub(crate) fn push_history(&mut self, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        self.history_idx = None;
        if self.history.last().map(|s| s.as_str()) == Some(sql) {
            return;
        }
        self.history.push(sql.to_string());
        if self.history.len() > 500 {
            self.history.remove(0);
        }
    }
    pub(crate) fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.history_idx {
            None => {
                self.history_draft = self.editor_sql();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        self.history_idx = Some(next);
        let text = self.history[next].clone();
        self.set_editor_text(&text);
        true
    }
    pub(crate) fn history_next(&mut self) -> bool {
        let Some(i) = self.history_idx else {
            return false;
        };
        if i + 1 >= self.history.len() {
            self.history_idx = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_editor_text(&draft);
            return true;
        }
        self.history_idx = Some(i + 1);
        let text = self.history[i + 1].clone();
        self.set_editor_text(&text);
        true
    }
    pub(crate) fn selected_table(&self) -> Option<&TableInfo> {
        let idx = self.table_list.selected()?;
        self.tables.get(idx)
    }
}
