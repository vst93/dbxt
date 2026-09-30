//! R83: quick-open a SQLite file from the connection picker (`L`).
//!
//! The picker is a small combobox over the filesystem: type a path (with `Tab`
//! completion) or walk the listing with `↑↓`. Opening an existing
//! `.db` / `.sqlite` / `.sqlite3` file builds a *session-only*
//! [`ConnectionConfig`] and activates it. Nothing is written to the connection
//! store, so a restart never shows a ghost connection; the last five files are
//! remembered in `tui.json` and listed at the top of the picker.

use crate::prelude::*;
use crate::*;

/// Extensions the picker accepts (case-insensitive).
pub(crate) const SQLITE_EXTS: [&str; 3] = ["db", "sqlite", "sqlite3"];
/// Cap on the visible directory listing, so a huge directory cannot stall a
/// render or a `Tab`.
pub(crate) const SQLITE_LIST_MAX: usize = 500;

/// One selectable line in the picker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum SqliteRow {
    /// A remembered file (listed first, while the input is empty).
    Recent(PathBuf),
    /// A sub-directory of the current listing (Enter descends into it).
    Dir(PathBuf),
    /// A matching SQLite file (Enter opens it).
    File(PathBuf),
}

impl SqliteRow {
    pub(crate) fn path(&self) -> &std::path::Path {
        match self {
            SqliteRow::Recent(p) | SqliteRow::Dir(p) | SqliteRow::File(p) => p,
        }
    }
}

/// The open picker's state. `rows` is the combined recent + directory listing
/// and `sel` indexes into it.
pub(crate) struct SqliteOpen {
    /// Directory the listing (and an empty input) is relative to.
    pub(crate) cwd: PathBuf,
    /// The editable path/name field.
    pub(crate) input: TextArea<'static>,
    /// Directory the current listing was read from.
    pub(crate) dir: PathBuf,
    /// The literal directory prefix of `input` (kept so `Tab` completion can
    /// rebuild the typed form instead of forcing an absolute path).
    pub(crate) dir_prefix: String,
    /// The name fragment the listing is filtered by.
    pub(crate) filter: String,
    pub(crate) rows: Vec<SqliteRow>,
    pub(crate) sel: usize,
    /// A path / directory error to show under the input.
    pub(crate) error: Option<String>,
}

/// Is `path` a file the picker will open? A pure predicate so the validation is
/// unit-testable without touching the filesystem.
pub(crate) fn is_sqlite_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SQLITE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The file name of `path` as a display string (lossy).
pub(crate) fn sqlite_file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// A textarea for the picker's path field.
pub(crate) fn sqlite_textarea(text: &str) -> TextArea<'static> {
    let mut ta = TextArea::from(vec![text.to_string()]);
    ta.set_placeholder_text(t("路径或文件名…"));
    ta
}

/// Split the typed input into the directory to list, the literal prefix to
/// re-prepend on completion, and the name fragment to filter by. Pure.
pub(crate) fn sqlite_split_input(cwd: &std::path::Path, raw: &str) -> (PathBuf, String, String) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (cwd.to_path_buf(), String::new(), String::new());
    }
    let expanded = expand_tilde(raw);
    if raw.ends_with('/') || expanded.is_dir() {
        let prefix = if raw.ends_with('/') {
            raw.to_string()
        } else {
            format!("{raw}/")
        };
        return (expanded, prefix, String::new());
    }
    match raw.rfind('/') {
        Some(idx) => {
            let prefix = raw[..=idx].to_string();
            let filter = raw[idx + 1..].to_string();
            (expand_tilde(&prefix), prefix, filter)
        }
        None => (cwd.to_path_buf(), String::new(), raw.to_string()),
    }
}

/// Longest common character prefix of `names` (empty for an empty slice).
pub(crate) fn common_prefix(names: &[String]) -> String {
    let mut it = names.iter();
    let mut prefix: Vec<char> = it.next().map(|s| s.chars().collect()).unwrap_or_default();
    for n in it {
        let cs: Vec<char> = n.chars().collect();
        let len = prefix
            .iter()
            .zip(cs.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(len);
    }
    prefix.into_iter().collect()
}

/// Read `dir`, keeping sub-directories and SQLite files whose name starts with
/// `filter` (case-insensitive). Dot-files are hidden unless the filter itself
/// starts with a dot. Directories sort before files.
pub(crate) fn sqlite_scan(dir: &std::path::Path, filter: &str) -> Result<Vec<SqliteRow>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let fl = filter.to_lowercase();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !fl.starts_with('.') {
            continue;
        }
        if !fl.is_empty() && !name.to_lowercase().starts_with(&fl) {
            continue;
        }
        let path = ent.path();
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(path);
        } else if is_sqlite_file(&path) {
            files.push(path);
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| {
        sqlite_file_name(a)
            .to_lowercase()
            .cmp(&sqlite_file_name(b).to_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    let mut rows: Vec<SqliteRow> = dirs.into_iter().map(SqliteRow::Dir).collect();
    rows.extend(files.into_iter().map(SqliteRow::File));
    rows.truncate(SQLITE_LIST_MAX);
    Ok(rows)
}

/// Recompute the listing for the picker's current input, keeping the recent
/// list on top while the input is empty.
pub(crate) fn sqlite_refresh(app: &App, o: &mut SqliteOpen) {
    let raw = o.input.lines().join(" ").trim().to_string();
    let (dir, prefix, filter) = sqlite_split_input(&o.cwd, &raw);
    o.dir = dir.clone();
    o.dir_prefix = prefix;
    o.filter = filter.clone();
    o.error = None;
    let mut rows: Vec<SqliteRow> = Vec::new();
    let recents: Vec<PathBuf> = if raw.is_empty() {
        app.config.sqlite_recent.clone()
    } else {
        Vec::new()
    };
    for p in &recents {
        rows.push(SqliteRow::Recent(p.clone()));
    }
    match sqlite_scan(&dir, &filter) {
        Ok(mut listing) => {
            // Never show a file twice (once as a recent, once in the listing).
            if !recents.is_empty() {
                listing.retain(|r| !recents.iter().any(|p| p.as_path() == r.path()));
            }
            rows.append(&mut listing);
        }
        Err(e) => o.error = Some(tf("无法读取目录：{}", &[&e])),
    }
    o.rows = rows;
    o.sel = o.sel.min(o.rows.len().saturating_sub(1));
}

/// `L` in the connection picker (and anywhere on the browse page): open the
/// SQLite quick-open overlay, starting from the process working directory.
pub(crate) fn open_sqlite_picker(app: &mut App) {
    if app.sqlite_open.is_some() {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut o = SqliteOpen {
        cwd,
        input: sqlite_textarea(""),
        dir: PathBuf::new(),
        dir_prefix: String::new(),
        filter: String::new(),
        rows: Vec::new(),
        sel: 0,
        error: None,
    };
    sqlite_refresh(app, &mut o);
    app.sqlite_open = Some(o);
    app.status = t("打开 SQLite 文件 · 输入路径或 ↑↓ 选择 · Enter 打开 · Esc 取消").into();
}

/// Descend into `dir` (Enter on a directory row).
fn sqlite_enter_dir(app: &App, o: &mut SqliteOpen, dir: PathBuf) {
    o.cwd = dir;
    o.input = sqlite_textarea("");
    o.sel = 0;
    sqlite_refresh(app, o);
}

/// `Tab`: complete the input to the single match, or to the common prefix of
/// several. Never fails — an ambiguous or empty listing is simply left alone.
pub(crate) fn sqlite_complete(app: &App, o: &mut SqliteOpen) {
    let mut names: Vec<String> = Vec::new();
    let mut is_dir: Vec<bool> = Vec::new();
    for r in &o.rows {
        match r {
            SqliteRow::Dir(p) => {
                names.push(sqlite_file_name(p));
                is_dir.push(true);
            }
            SqliteRow::File(p) => {
                names.push(sqlite_file_name(p));
                is_dir.push(false);
            }
            SqliteRow::Recent(_) => {}
        }
    }
    if names.is_empty() {
        return;
    }
    let completion = if names.len() == 1 {
        let mut s = format!("{}{}", o.dir_prefix, names[0]);
        if is_dir[0] {
            s.push('/');
        }
        Some(s)
    } else {
        let cp = common_prefix(&names);
        (cp.chars().count() > o.filter.chars().count()).then(|| format!("{}{}", o.dir_prefix, cp))
    };
    if let Some(s) = completion {
        o.input = sqlite_textarea(&s);
    }
    sqlite_refresh(app, o);
}

/// Key handling for the picker. Enter opens the highlighted row (or the typed
/// path), `Tab` completes, `↑↓` move, `Del` forgets a recent file (only while
/// the input is empty, so it never shadows text editing), `Esc` cancels.
pub(crate) fn sqlite_open_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    let Some(mut o) = app.sqlite_open.take() else {
        return;
    };
    match k.code {
        KeyCode::Esc => {
            app.flash(t("已取消打开 SQLite 文件").into());
        }
        // The picker is a text field, so `?` would otherwise be a literal; while
        // the field is empty (and always on F1) it opens the help layers above
        // the picker instead. The footer's pinned `? 帮助` hint stays truthful.
        KeyCode::F(1) => {
            app.sqlite_open = Some(o);
            open_help(app);
        }
        KeyCode::Char('?')
            if o.input.lines().join(" ").trim().is_empty()
                && !k.modifiers.contains(KeyModifiers::CONTROL)
                && !k.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.sqlite_open = Some(o);
            open_help(app);
        }
        KeyCode::Enter => {
            let picked = o.rows.get(o.sel).map(|r| r.path().to_path_buf());
            let raw = o.input.lines().join(" ").trim().to_string();
            if picked.is_none() && raw.is_empty() {
                o.error = Some(t("请输入路径或选择文件").to_string());
                app.sqlite_open = Some(o);
                return;
            }
            let target = picked.unwrap_or_else(|| expand_tilde(&raw));
            if target.is_dir() {
                sqlite_enter_dir(app, &mut o, target);
                app.sqlite_open = Some(o);
                return;
            }
            app.sqlite_open = Some(o);
            open_sqlite_file(app, tx, target);
        }
        KeyCode::Up => {
            o.sel = o.sel.saturating_sub(1);
            app.sqlite_open = Some(o);
        }
        KeyCode::Down => {
            if o.sel + 1 < o.rows.len() {
                o.sel += 1;
            }
            app.sqlite_open = Some(o);
        }
        KeyCode::Tab => {
            sqlite_complete(app, &mut o);
            app.sqlite_open = Some(o);
        }
        KeyCode::Delete if o.input.lines().join(" ").trim().is_empty() => {
            if let Some(SqliteRow::Recent(p)) = o.rows.get(o.sel).cloned() {
                app.config.remove_sqlite_recent(&p);
                app.persist();
                app.status = tf("已移除最近文件 {}", &[&sqlite_file_name(&p)]);
                sqlite_refresh(app, &mut o);
            }
            app.sqlite_open = Some(o);
        }
        _ => {
            o.input.input(k);
            sqlite_refresh(app, &mut o);
            app.sqlite_open = Some(o);
        }
    }
}

/// Re-attach this session's temporary connections after a reload of the saved
/// list, so a `L` quick-open never looks like it vanished (and, because nothing
/// was saved, a *restart* still shows no ghost connection).
pub(crate) fn merge_temp_connections(
    loaded: &mut Vec<ConnectionConfig>,
    temp: &[ConnectionConfig],
) {
    for t in temp {
        match loaded.iter().position(|c| c.id == t.id) {
            Some(i) => loaded[i] = t.clone(),
            None => loaded.push(t.clone()),
        }
    }
}

/// Build the session-only config for a SQLite file. The kernel treats the
/// config's `host` as the database path and `port` as unused, matching how a
/// saved SQLite connection is stored. Pure so a test can assert the shape.
pub(crate) fn sqlite_connection_config(path: &std::path::Path) -> Result<ConnectionConfig, String> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let mut cfg = new_connection_config(
        format!("sqlite-file-{}", Uuid::new_v4()),
        name,
        DatabaseType::Sqlite,
        path.to_string_lossy().into_owned(),
        0,
        String::new(),
        String::new(),
        None,
        false,
        None,
    )?;
    cfg.read_only = false;
    Ok(cfg)
}

/// Validate `path` and, on success, open it as a temporary SQLite connection.
/// The file is remembered in the recent list and never written to the store.
pub(crate) fn open_sqlite_file(app: &mut App, tx: &Tx, path: PathBuf) {
    let path = expand_tilde(&path.to_string_lossy());
    if !path.exists() {
        app.status = tf("文件不存在：{}", &[&(path.display())]);
        return;
    }
    if path.is_dir() {
        app.status = tf("这是目录：{}", &[&(path.display())]);
        return;
    }
    if !path.is_file() {
        app.status = tf("不是普通文件：{}", &[&(path.display())]);
        return;
    }
    if !is_sqlite_file(&path) {
        app.status = tf(
            "不是 SQLite 文件（需 .db / .sqlite / .sqlite3）：{}",
            &[&(path.display())],
        );
        return;
    }
    let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let cfg = match sqlite_connection_config(&abs) {
        Ok(c) => c,
        Err(e) => {
            app.status = tf("无法打开 SQLite 文件：{}", &[&e]);
            return;
        }
    };
    app.config.push_sqlite_recent(&abs);
    app.persist();
    // Register as a session-only connection: in the in-memory list (so it shows
    // in the picker and can be switched back to) and in `temp_conns` (so a
    // reload re-attaches it), but never persisted.
    app.temp_conns.retain(|c| c.id != cfg.id);
    app.temp_conns.push(cfg.clone());
    app.connections.retain(|c| c.id != cfg.id);
    app.connections.push(cfg.clone());
    sort_connection_list(&mut app.connections, app.conn_sort);
    if let Some(i) = app.connections.iter().position(|c| c.id == cfg.id) {
        app.conn_list.select(Some(i));
    }
    app.sqlite_open = None;
    app.status = tf("打开 SQLite 文件 {}…", &[&sqlite_file_name(&abs)]);
    // Register in the kernel's runtime cache first; its reply activates the
    // connection, so the first store-backed query already resolves the id.
    app.spawn(tx, Op::RegisterTempConn(Box::new(cfg), true));
}

/// Draw the quick-open overlay: a title, the path field, the recent + directory
/// listing, and any error under the field.
pub(crate) fn render_sqlite_open(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width < 12 || area.height < 5 {
        return;
    }
    let w = area.width.min(if app.layout_mode == LayoutMode::Narrow {
        area.width
    } else {
        64
    });
    // Title (1 line) + input (1 line) + up to N rows + borders.
    let want =
        (app.sqlite_open.as_ref().map(|o| o.rows.len()).unwrap_or(0) as u16).saturating_add(4);
    let h = want.min(area.height).max(5.min(area.height));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let box_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(overlay_hint_title(
            box_area.width,
            &format!(" {} ", t("打开 SQLite 文件")),
            &[
                ("Enter", t("打开")),
                ("Tab", t("补全")),
                ("Del", t("移除最近")),
                ("Esc", t("取消")),
            ],
            " ",
        ))
        .border_set(border::ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    if inner.height == 0 {
        return;
    }
    let mut rows_area = inner;
    // First inner row = the path field.
    let input_area = Rect { height: 1, ..inner };
    if let Some(o) = app.sqlite_open.as_mut() {
        o.input.set_block(Block::default());
        f.render_widget(&o.input, input_area);
    }
    rows_area.y += 1;
    rows_area.height = rows_area.height.saturating_sub(1);
    if rows_area.height == 0 {
        return;
    }
    let Some(o) = app.sqlite_open.as_ref() else {
        return;
    };
    let items: Vec<ListItem> = o
        .rows
        .iter()
        .map(|r| {
            let (icon, color) = match r {
                SqliteRow::Recent(_) => ("⟲ ", Color::Yellow),
                SqliteRow::Dir(_) => ("▸ ", Color::Cyan),
                SqliteRow::File(_) => ("  ", Color::Green),
            };
            let label = match r {
                SqliteRow::Dir(p) => format!("{}/", sqlite_file_name(p)),
                _ => sqlite_file_name(r.path()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(icon.to_string(), Style::default().fg(color)),
                Span::raw(truncate_disp(
                    &label,
                    rows_area.width.saturating_sub(3) as usize,
                )),
            ]))
        })
        .collect();
    // Scroll the selection into view with a tiny local state.
    let mut ls = ListState::default();
    ls.select(Some(o.sel));
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, rows_area, &mut ls);
    // Empty listing / error hint.
    if o.rows.is_empty() || o.error.is_some() {
        let hint = o.error.clone().unwrap_or_else(|| {
            t("没有 .db / .sqlite / .sqlite3 文件 · 输入完整路径或 Tab 补全").to_string()
        });
        let hint_area = Rect {
            x: rows_area.x,
            y: rows_area.y,
            width: rows_area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(truncate_disp(&hint, rows_area.width as usize))
                .style(Style::default().fg(Color::DarkGray)),
            hint_area,
        );
    }
}
