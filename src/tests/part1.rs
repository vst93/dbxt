use super::*;

#[test]
pub(crate) fn extreme_sizes_do_not_panic() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    // 40×12 (phone-ish), 250×70 (huge), and a few degenerate heights where
    // the results pane is squeezed to zero rows.
    for (w, h) in [(40u16, 12u16), (250, 70), (20, 6), (40, 2), (18, 1), (1, 1)] {
        draw(&mut app, w, h);
    }
}

pub(crate) fn redis_sample_view() -> RedisValueView {
    let hash = RedisValue {
        key_display: "app:user:42".into(),
        key_raw: base64_encode(b"app:user:42"),
        ttl: 60,
        redis_type: "hash".into(),
        data: RedisValueData::Hash {
            items: (0..30)
                .map(|i| dbx_core::db::redis_driver::RedisHashItem {
                    field: RedisBlob {
                        raw_base64: base64_encode(format!("field_{i}").as_bytes()),
                        encoding: RedisBlobEncoding::Utf8,
                    },
                    value: RedisBlob {
                        raw_base64: base64_encode(format!("value_{i}").as_bytes()),
                        encoding: RedisBlobEncoding::Utf8,
                    },
                    field_ttl: Some(-1),
                })
                .collect(),
            total: 30,
            scan_cursor: Some(30),
        },
    };
    redis_value_view(hash)
}

/// The R20–R22 views (Redis key list / value grid, Mongo document grid) at the
/// two acceptance sizes plus degenerate ones. The Redis sidebar is long enough
/// to need SCAN paging, the value grid is wide enough to need the horizontal
/// scrollbar, and the Mongo grid has nested-object cells.
#[test]
pub(crate) fn redis_and_mongo_views_render_at_extreme_sizes() {
    let sizes = [(40u16, 12u16), (250, 70), (20, 6), (40, 2), (1, 1)];
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("redis"));
    app.backend_kind = Backend::Redis;
    app.redis_scan.keys = (0..40)
        .map(|i| RedisKeyInfo {
            key_display: format!("app:key:{i}"),
            key_raw: base64_encode(format!("app:key:{i}").as_bytes()),
            key_type: "hash".into(),
            ttl: -1,
            size: 12,
            value_preview: String::new(),
        })
        .collect();
    app.redis_scan.total = 40;
    app.redis_scan.exhausted = false;
    app.focus = Focus::Sidebar;
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }

    let view = redis_sample_view();
    app.grid_kind = GridKind::RedisValue;
    app.set_grid(view.grid.clone());
    app.redis_value = Some(view);
    app.focus = Focus::Preview;
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }

    let docs: Vec<serde_json::Value> = (0..30)
        .map(|i| {
            serde_json::json!({
                "_id": i,
                "name": format!("n{i}"),
                "nested": {"a": 1, "b": "x"},
                "tags": [1, 2, 3],
            })
        })
        .collect();
    app.backend_kind = Backend::Mongo;
    app.selected = Some(test_conn("mongodb"));
    app.grid_kind = GridKind::MongoDocs;
    app.set_grid(mongo_docs_grid(&docs));
    app.mongo_docs = docs;
    app.page_state = Some(PageState {
        table: "coll".into(),
        schema: String::new(),
        table_type: None,
        page: 0,
        page_size: MONGO_PAGE,
        total: Some(30),
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: None,
        keyset: None,
    });
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
}

pub(crate) fn mk_redis_key(name: &str) -> RedisKeyInfo {
    RedisKeyInfo {
        key_display: name.to_string(),
        key_raw: base64_encode(name.as_bytes()),
        key_type: "string".into(),
        ttl: -1,
        size: 1,
        value_preview: String::new(),
    }
}

/// R42: the KV list filter narrows `keys` without touching the loaded window
/// (`all`), keeps a still-matching selection, and restores on clear.
#[test]
pub(crate) fn redis_filter_state_machine_narrows_and_restores() {
    let mut app = test_app();
    app.redis_scan.all = vec![
        mk_redis_key("app:a"),
        mk_redis_key("app:b"),
        mk_redis_key("user:x"),
    ];
    apply_redis_filter(&mut app);
    assert_eq!(app.redis_scan.keys.len(), 3);
    app.redis_list.select(Some(1));
    // Substring filter keeps the selected `app:b` selected.
    app.redis_filter = "app:".into();
    apply_redis_filter(&mut app);
    assert_eq!(app.redis_scan.keys.len(), 2);
    assert_eq!(app.redis_list.selected(), Some(1));
    // A filter that excludes the selection falls back to the first hit.
    app.redis_filter = "user".into();
    apply_redis_filter(&mut app);
    assert_eq!(app.redis_scan.keys.len(), 1);
    assert_eq!(app.redis_list.selected(), Some(0));
    // No hit empties the view, never the loaded window.
    app.redis_filter = "zzz".into();
    apply_redis_filter(&mut app);
    assert!(app.redis_scan.keys.is_empty());
    assert_eq!(app.redis_list.selected(), None);
    assert_eq!(app.redis_scan.all.len(), 3);
    // Clearing restores the full window.
    clear_redis_filter(&mut app);
    assert_eq!(app.redis_scan.keys.len(), 3);
}

/// R42: `Alt+<letter>` cycles through the loaded keys by first letter and
/// wraps, and the repeat key remembers the letter.
#[test]
pub(crate) fn redis_first_letter_jump_cycles_by_letter() {
    let mut app = test_app();
    app.redis_scan.all = vec![
        mk_redis_key("alpha"),
        mk_redis_key("beta"),
        mk_redis_key("apex"),
    ];
    apply_redis_filter(&mut app);
    app.redis_list.select(Some(0));
    assert_eq!(redis_jump_by_letter(&mut app, 'a', 1), Some(2));
    assert_eq!(app.redis_jump_letter, Some('a'));
    // Forward from the last `a` wraps to the first.
    assert_eq!(redis_jump_by_letter(&mut app, 'a', 1), Some(0));
    // Backward from index 0 wraps to the last `a`.
    assert_eq!(redis_jump_by_letter(&mut app, 'a', -1), Some(2));
    assert_eq!(redis_jump_by_letter(&mut app, 'z', 1), None);
}

/// R42: the script console timing prefix stays compact across magnitudes.
#[test]
pub(crate) fn elapsed_prefix_format_is_compact() {
    assert_eq!(format_elapsed_ms(0), "0ms");
    assert_eq!(format_elapsed_ms(12), "12ms");
    assert_eq!(format_elapsed_ms(999), "999ms");
    assert_eq!(format_elapsed_ms(1_234), "1.23s");
    assert_eq!(format_elapsed_ms(120_000), "2.0m");
}

/// R42: the narrow Redis badge fuses type and TTL into one token.
#[test]
pub(crate) fn redis_badge_fuses_type_and_ttl_when_narrow() {
    assert_eq!(redis_badge_token(true, "S", Some(12)), "S·12s");
    assert_eq!(redis_badge_token(true, "H", None), "H");
    assert_eq!(redis_badge_token(false, "S", Some(12)), "S");
}

pub(crate) fn sample_script(n: usize) -> ScriptView {
    let outcomes = (0..n)
        .map(|i| StmtOutcome {
            sql: format!("SELECT {i}"),
            grid: Grid {
                columns: vec!["x".into()],
                rows: vec![vec![Val::Text(format!("{i}"))]],
                note: String::new(),
            },
            error: None,
            affected: 0,
            ms: 12,
        })
        .collect();
    ScriptView {
        outcomes,
        sel: 1,
        drilled: None,
    }
}

/// R42: `Home` / `End` / `gg` / `G` route to the statement list or the grid,
/// whichever owns the results pane.
#[test]
pub(crate) fn preview_home_end_routes_to_script_list_and_grid() {
    let mut app = test_app();
    app.script = Some(sample_script(4));
    preview_end(&mut app);
    assert_eq!(app.script.as_ref().unwrap().sel, 3);
    preview_home(&mut app);
    assert_eq!(app.script.as_ref().unwrap().sel, 0);
    // Drilled into a statement: the grid cursor moves instead.
    app.script.as_mut().unwrap().drilled = Some(0);
    app.script.as_mut().unwrap().outcomes[0].grid = sample_grid();
    app.set_grid(sample_grid());
    app.sel = 2;
    preview_home(&mut app);
    assert_eq!(app.sel, 0);
    preview_end(&mut app);
    assert_eq!(app.sel, 3);
}

/// R51: `Home` / `End` in a grid also park the cell cursor back on the first
/// column, so a row sweep never leaves a stale column highlight.
#[test]
pub(crate) fn preview_home_end_reset_the_cell_cursor_column() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.sel = 2;
    app.col_cursor = 5;
    app.col_offset = 3;
    preview_home(&mut app);
    assert_eq!(app.sel, 0);
    assert_eq!(app.col_cursor, 0);
    assert_eq!(app.col_offset, 0);
    app.col_cursor = 7;
    app.col_offset = 4;
    preview_end(&mut app);
    assert_eq!(app.sel, 3);
    assert_eq!(app.col_cursor, 0);
    assert_eq!(app.col_offset, 0);
}

/// R42: the `gg` / `G` / `Home` / `End` chord actually reaches the script
/// list through the global key dispatch (the `g` chord is resolved in
/// `browse_key` before the pane handler).
#[test]
pub(crate) fn script_list_gg_and_g_move_to_top_and_bottom() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Preview;
    app.script = Some(sample_script(5));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE),
    );
    assert_eq!(app.script.as_ref().unwrap().sel, 4);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert_eq!(app.script.as_ref().unwrap().sel, 0, "gg goes to the top");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
    );
    assert_eq!(app.script.as_ref().unwrap().sel, 4);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
    );
    assert_eq!(app.script.as_ref().unwrap().sel, 0);
}

/// R42: `Alt-O` swaps the plain script table for the separator stream.
#[test]
pub(crate) fn script_timing_toggle_renders_separator_stream() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Preview;
    app.script = Some(sample_script(3));
    let plain = draw(&mut app, 80, 20);
    assert!(
        plain.iter().any(|l| l.contains("SELECT 0")),
        "plain script list missing: {plain:?}"
    );
    assert!(
        !plain.iter().any(|l| l.contains("#1")),
        "separators must be off by default"
    );
    app.show_stmt_timing = true;
    let stream = draw(&mut app, 80, 20);
    assert!(
        stream.iter().any(|l| l.contains("#1")),
        "separator stream missing: {stream:?}"
    );
    assert!(
        stream.iter().any(|l| l.contains("12ms")),
        "timing prefix missing: {stream:?}"
    );
}

/// R42: a 100 KB cell is wrapped once and the wrap is reused across frames
/// and scrolls; only a width change rebuilds it.
#[test]
pub(crate) fn large_cell_popup_wraps_once_and_reuses_cache() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    let big = "x".repeat(100_000);
    app.set_grid(Grid {
        columns: vec!["v".into()],
        rows: vec![vec![Val::Text(big)]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    app.col_cursor = 0;
    open_cell_popup(&mut app);
    assert!(app.popup_cache.is_none(), "cache is cleared on open");
    draw(&mut app, 42, 22);
    let cache = app
        .popup_cache
        .as_ref()
        .expect("cache filled on first draw");
    let width = cache.width;
    let ptr = cache.lines.as_ptr();
    // Same width: the wrap is reused (same allocation), even when scrolled.
    draw(&mut app, 42, 22);
    app.cell_popup.as_mut().unwrap().scroll = 20;
    draw(&mut app, 42, 22);
    let cache = app.popup_cache.as_ref().unwrap();
    assert_eq!(cache.lines.as_ptr(), ptr, "wrap reused across frames");
    // A width change rebuilds it.
    draw(&mut app, 70, 22);
    let cache = app.popup_cache.as_ref().unwrap();
    assert_ne!(cache.width, width, "resize recomputes the wrap");
}

/// R42b: `Enter` in a data grid opens the whole row in *both* layout modes
/// (the old compact/wide split is gone), while `v` still opens the cell
/// popup directly.
#[test]
pub(crate) fn enter_opens_the_whole_row_in_both_layout_modes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    for mode in [LayoutMode::Mid, LayoutMode::Narrow, LayoutMode::Wide] {
        let mut app = test_app();
        app.picker_open = false;
        app.selected = Some(test_conn("mysql"));
        app.backend_kind = Backend::Sql;
        app.grid_kind = GridKind::TableData;
        app.set_grid(sample_grid());
        app.focus = Focus::Preview;
        app.layout_mode = mode;
        app.sel = 0;
        app.col_cursor = 0;
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(
            app.row_popup.is_some(),
            "{mode:?}: Enter should open the row"
        );
        assert!(
            app.cell_popup.is_none(),
            "{mode:?}: Enter should not open the cell"
        );
        // Esc closes the row, then `v` opens the cell directly.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(app.row_popup.is_none());
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        assert!(app.cell_popup.is_some());
        assert!(app.row_popup.is_none());
    }
}

/// R42b: `/` inside the row popup filters by column name, so a 40+ column
/// table can be narrowed without scanning. Digits are filter text while
/// typing, never a count prefix.
#[test]
pub(crate) fn row_popup_filters_columns_by_name() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid()); // column_0 … column_7
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    assert_eq!(row_popup_visible(app.row_popup.as_ref().unwrap()).len(), 8);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert!(app.row_popup.as_ref().unwrap().filtering);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE),
    );
    let popup = app.row_popup.as_ref().unwrap();
    assert_eq!(popup.filter, "5", "digits are filter text while typing");
    assert_eq!(row_popup_visible(popup).len(), 1);
    // Enter keeps the filter and leaves the input mode.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(!app.row_popup.as_ref().unwrap().filtering);
    assert_eq!(row_popup_visible(app.row_popup.as_ref().unwrap()).len(), 1);
    // Esc now closes the popup (the filter is no longer being typed).
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.row_popup.is_none());
}

/// R42b: the row-popup title locates the row by primary key when the
/// browsed table's metadata is known (`第 1 行 · id=4821`).
#[test]
pub(crate) fn row_popup_title_carries_the_primary_key() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Val::Text("4821".into()), Val::Text("ada".into())]],
        note: String::new(),
    });
    app.page_state = Some(page_of("orders"));
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![
            ColumnInfo {
                name: "id".into(),
                data_type: "int".into(),
                is_primary_key: true,
                ..Default::default()
            },
            ColumnInfo {
                name: "name".into(),
                data_type: "text".into(),
                ..Default::default()
            },
        ],
        indexes: Vec::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    let title = &app.row_popup.as_ref().unwrap().title;
    assert!(title.contains("id=4821"), "title missing the key: {title}");
    assert!(title.contains('1'), "title missing the row number: {title}");
}

/// R42b: Enter inside the row popup drills into the cell popup, which stays
/// stacked over the row; Esc unwinds cell → row → grid.
#[test]
pub(crate) fn row_popup_drill_returns_to_the_row_then_the_grid() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.sel = 0;
    app.col_cursor = 0;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.row_popup.is_some());
    // Move to the second column and drill in.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.cell_popup.is_some());
    assert!(app.row_popup.is_some(), "row stays under the drilled cell");
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 1);
    // Esc unwinds to the row, then closes it.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.cell_popup.is_none());
    assert!(app.row_popup.is_some());
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.row_popup.is_none());
}

/// R42b: a vim count prefix moves the row-popup cursor (`5j`).
#[test]
pub(crate) fn row_popup_count_prefix_jumps_entries() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    for c in ["5", "j"] {
        let ch = c.chars().next().unwrap();
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 5);
    for c in ["2", "k"] {
        let ch = c.chars().next().unwrap();
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 3);
}

/// R67: `n` / `p` move the row-popup cursor just like `j` / `k`, count
/// prefix included, so the page-turn keys work one level in as well.
#[test]
pub(crate) fn row_popup_n_and_p_move_the_cursor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    for c in ["3", "n"] {
        let ch = c.chars().next().unwrap();
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 3);
    for c in ["2", "p"] {
        let ch = c.chars().next().unwrap();
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 1);
}

/// R67: the row-popup `/` filter matches the column name *or* the displayed
/// value, so a wide row can be narrowed by either.
#[test]
pub(crate) fn row_popup_filter_matches_name_or_value() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into(), "city".into()],
        rows: vec![vec![
            Val::Text("1".into()),
            Val::Text("ada".into()),
            Val::Text("berlin".into()),
        ]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    let set = |app: &mut App, f: &str| {
        app.row_popup.as_mut().unwrap().filter = f.to_string();
        row_popup_visible(app.row_popup.as_ref().unwrap())
    };
    assert_eq!(set(&mut app, ""), vec![0, 1, 2]);
    // A name match keeps only that field.
    assert_eq!(set(&mut app, "nam"), vec![1]);
    // A value match finds the field holding it even when the name does not.
    assert_eq!(set(&mut app, "berl"), vec![2]);
    // Nothing matches → the empty state.
    assert!(set(&mut app, "zzz").is_empty());
}

/// R67: the row-popup body keeps `name = value` on one line when wide and
/// stacks the value under the name on a narrow popup; `hit` maps every
/// physical line back to its field so a click still selects the right one.
#[test]
pub(crate) fn row_popup_body_stacks_on_narrow_widths() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Val::Text("1".into()), Val::Text("ada".into())]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    let popup = app.row_popup.as_ref().unwrap();
    // Wide (>= ROW_POPUP_STACK_W): one physical line per field.
    let (body, hit, sel) = row_popup_body(popup, ROW_POPUP_STACK_W);
    assert_eq!(body.len(), 2);
    assert_eq!(hit, vec![0, 1]);
    assert_eq!(sel, 0);
    // Narrow: name line + indented value line per field.
    let (body, hit, sel) = row_popup_body(popup, ROW_POPUP_STACK_W - 1);
    assert_eq!(body.len(), 4);
    assert_eq!(hit, vec![0, 0, 1, 1]);
    assert_eq!(sel, 0);
}

/// R67: `y` and `Y` inside the row popup both copy the selected field's
/// value through the grid's copy path, naming the column in the status.
#[test]
pub(crate) fn row_popup_y_and_shift_y_copy_the_selected_field() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Val::Text("7".into()), Val::Text("seven".into())]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    open_row_popup(&mut app);
    // Move to `name`, then `Y` copies it.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    app.status.clear();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
    );
    assert!(app.status.contains("已复制"), "{}", app.status);
    assert!(app.status.contains("name"), "{}", app.status);
    assert!(app.status.contains("seven"), "{}", app.status);
    // `y` is the same gesture.
    app.status.clear();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(app.status.contains("name"), "{}", app.status);
}

/// R67: `z` pins the first column and flashes the same bilingual state in
/// the result, Redis and MongoDB grids — the Redis / Mongo keymaps used to
/// toggle silently.
#[test]
pub(crate) fn z_flashes_the_pin_state_in_every_grid() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let z = || KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    let cases: [(Backend, GridKind, &str); 3] = [
        (Backend::Sql, GridKind::Query, "mysql"),
        (Backend::Redis, GridKind::RedisValue, "redis"),
        (Backend::Mongo, GridKind::MongoDocs, "mongodb"),
    ];
    for (backend, kind, conn) in cases {
        let mut app = test_app();
        app.picker_open = false;
        app.selected = Some(test_conn(conn));
        app.backend_kind = backend;
        app.grid_kind = kind;
        app.set_grid(sample_grid());
        app.focus = Focus::Preview;
        app.freeze_first = false;
        key(&mut app, &tx, z());
        assert!(app.freeze_first, "{conn}: z pins the first column");
        assert_eq!(app.status, t("首列已钉住 · z 取消"), "{conn}");
        key(&mut app, &tx, z());
        assert!(!app.freeze_first, "{conn}: z unpins");
        assert_eq!(app.status, t("首列已取消钉住 · z 钉住"), "{conn}");
    }
}

/// One overlay fixture for [`overlays_render_at_extreme_sizes`].
pub(crate) type OverlayCase = (&'static str, Box<dyn Fn(&mut App)>);

/// Every modal overlay that can sit over a browse page, rendered at the two
/// acceptance sizes and at a degenerate one. `Clear` panics on an out-of-
/// bounds rect, so this is the guard for the whole overlay family.
#[test]
pub(crate) fn overlays_render_at_extreme_sizes() {
    // (42, 22) is the R39 acceptance size for the overlay small-screen
    // sweep; the rest are the historical phone / huge / degenerate cases.
    let sizes = [(40u16, 12u16), (42, 22), (250, 70), (20, 6), (1, 1)];
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;

    let reset = |app: &mut App| {
        app.help_open = false;
        app.help_mini = false;
        app.pending_g = false;
        app.clear_count();
        app.export_open = false;
        app.export_path = None;
        app.export_pending = None;
        app.import_prompt = None;
        app.import_plan = None;
        app.import_report = None;
        app.redis_prompt = None;
        app.mongo_dialog = None;
        app.confirm = None;
        app.ssh_prompt = None;
        app.edit_dialog = None;
        app.completion = None;
        app.cell_popup = None;
        app.row_popup = None;
        app.db_picker_open = false;
        app.col_picker_open = false;
        app.cols_popup_open = false;
        app.recent_open = false;
        app.table_jump_open = false;
        app.table_jump_needle.clear();
        app.history_open = false;
        app.history_filter = None;
        app.history_confirm = None;
        app.snippet_open = false;
        app.snippet_name = None;
        app.snippet_needle.clear();
        app.snippet_filter = None;
        app.snippet_view.clear();
        app.snippet_confirm = None;
        app.template_open = false;
        app.template_filter = None;
        app.template_needle.clear();
        app.template_view.clear();
        app.template_active = false;
        app.template_ph_start = None;
        app.table_prompt = None;
        app.result_filter = None;
        app.locate_prompt = None;
        app.col_jump = None;
        app.filter_prompt = None;
        app.search_open = false;
        app.search_input = None;
        app.file_load_prompt = None;
        app.file_load_plan = None;
        app.conn_export = None;
        app.conn_import_path = None;
        app.conn_import_plan = None;
        app.help_filter = None;
        app.help_needle.clear();
    };

    let cases: Vec<OverlayCase> = vec![
        ("help", Box::new(|a| a.help_open = true)),
        (
            "help-filter",
            Box::new(|a| {
                a.help_open = true;
                a.help_needle = "y".into();
                a.help_filter = Some(TextArea::from(["y"]));
            }),
        ),
        ("help-mini", Box::new(|a| a.help_mini = true)),
        (
            "locate-prompt",
            Box::new(|a| a.locate_prompt = Some(TextArea::default())),
        ),
        (
            "col-jump",
            Box::new(|a| a.col_jump = Some(TextArea::default())),
        ),
        ("export-picker", Box::new(|a| a.export_open = true)),
        (
            "export-path",
            Box::new(|a| {
                a.export_pending = Some(ExportPending {
                    format: ExportFormat::Csv,
                    table: None,
                });
                a.export_path = Some(TextArea::default());
            }),
        ),
        (
            "import-prompt",
            Box::new(|a| {
                a.import_prompt = Some(ImportPrompt {
                    input: TextArea::default(),
                    table: "t".into(),
                    schema: String::new(),
                    db: "d".into(),
                    error: None,
                })
            }),
        ),
        (
            "import-plan",
            Box::new(|a| {
                a.import_plan = Some(Box::new(ImportPlan {
                    path: PathBuf::from("/tmp/x.csv"),
                    file_size: 123,
                    encoding: "UTF-8".into(),
                    delimiter: ',',
                    headers: vec!["a".into(), "b".into()],
                    rows: vec![vec!["1".into(), "2".into()]],
                    table: "t".into(),
                    schema: String::new(),
                    db: "d".into(),
                    columns: vec![ImportCol {
                        name: "a".into(),
                        src: Some(0),
                        ty: ColType::Int,
                        data_type: "int".into(),
                    }],
                    extra: Vec::new(),
                    missing: vec!["b".into()],
                    mode: ImportMode::Append,
                    on_error: ImportOnError::Stop,
                    error: None,
                }))
            }),
        ),
        (
            "import-report",
            Box::new(|a| {
                a.import_report = Some(Box::new(ImportReport {
                    table: "t".into(),
                    schema: String::new(),
                    mode: ImportMode::Append,
                    total: 1,
                    inserted: 1,
                    skipped: vec![(1, "bad".into())],
                    aborted: None,
                    elapsed_ms: 5,
                }))
            }),
        ),
        (
            "redis-prompt",
            Box::new(|a| {
                a.redis_prompt = Some(RedisPrompt {
                    kind: RedisPromptKind::Rename,
                    title: "t".into(),
                    key_display: "k".into(),
                    key_raw: "aw==".into(),
                    field: String::new(),
                    batch: Vec::new(),
                    input: TextArea::default(),
                })
            }),
        ),
        (
            "mongo-dialog",
            Box::new(|a| {
                a.mongo_dialog = Some(MongoDocDialog {
                    mode: MongoDocMode::Insert,
                    db: "d".into(),
                    collection: "c".into(),
                    original: serde_json::json!({}),
                    id: String::new(),
                    editor: TextArea::from(vec!["{", "}"]),
                    error: None,
                })
            }),
        ),
        (
            "confirm",
            Box::new(|a| {
                a.confirm = Some(Confirm {
                    sql: "DELETE FROM t".into(),
                    reasons: vec!["no WHERE".into()],
                    refresh: false,
                    clear_batch: false,
                    conn: None,
                    redis: None,
                    mongo: None,
                })
            }),
        ),
        (
            "edit-dialog",
            Box::new(|a| {
                a.edit_dialog = Some(EditDialog {
                    kind: EditKind::Update,
                    cfg: Box::new(test_conn("mysql")),
                    db: "d".into(),
                    schema: String::new(),
                    table: "t".into(),
                    column: "c".into(),
                    data_type: Some("int".into()),
                    old: Val::Text("1".into()),
                    new_input: TextArea::default(),
                    where_clause: "id = 1".into(),
                    keys: vec!["id".into()],
                    no_pk: false,
                    insert_sql: String::new(),
                    insert_preview: Vec::new(),
                })
            }),
        ),
        (
            "completion",
            Box::new(|a| {
                a.completion = Some(Completion {
                    items: vec![CompletionItem {
                        text: "users".into(),
                        kind: 'T',
                    }],
                    sel: 0,
                    replace: 0,
                })
            }),
        ),
        (
            "cell-popup",
            Box::new(|a| {
                a.cell_popup = Some(CellPopup {
                    title: "cell".into(),
                    lines: vec![PopupLine {
                        text: "x".into(),
                        style: Style::default(),
                    }],
                    scroll: 0,
                })
            }),
        ),
        (
            "row-popup",
            Box::new(|a| {
                a.row_popup = Some(row_popup_from_lines(
                    "row".into(),
                    vec![PopupLine {
                        text: "x".into(),
                        style: Style::default(),
                    }],
                ))
            }),
        ),
        ("db-picker", Box::new(|a| a.db_picker_open = true)),
        ("col-picker", Box::new(|a| a.col_picker_open = true)),
        ("recent", Box::new(|a| a.recent_open = true)),
        // R65: the in-data-view table switcher and its filter-as-you-type
        // needle render at every size too.
        (
            "table-jump",
            Box::new(|a| {
                a.tables_all = vec![table_info("orders", "TABLE"), table_info("items", "TABLE")];
                a.table_jump_open = true;
                a.table_jump_needle = "ord".into();
                a.table_jump_list.select(Some(0));
            }),
        ),
        ("cols-popup", Box::new(open_cols_popup)),
        (
            "search",
            Box::new(|a| {
                a.search_query = "ali".into();
                a.search_hits = vec![SearchHit {
                    schema: "public".into(),
                    table: "users".into(),
                    column: "name".into(),
                    matched: "Alice".into(),
                    filter: "id = 1".into(),
                }];
                a.search_list.select(Some(0));
                a.search_open = true;
            }),
        ),
        (
            "search-input",
            Box::new(|a| a.search_input = Some(TextArea::from(["ali"]))),
        ),
        (
            "file-load-prompt",
            Box::new(|a| a.file_load_prompt = Some(TextArea::from(["seed.sql"]))),
        ),
        (
            "file-load-plan",
            Box::new(|a| {
                a.file_load_plan = Some(Box::new(FileLoadPlan {
                    path: PathBuf::from("/tmp/seed.sql"),
                    sql: "DELETE FROM t;\nSELECT 1;".into(),
                    bytes: 123,
                    statements: 2,
                    connection: "prod-mysql".into(),
                    db: "shop".into(),
                    danger: vec!["DELETE 没有 WHERE 子句，会作用于整张表".into()],
                    warning: None,
                }))
            }),
        ),
        (
            "history",
            Box::new(|a| {
                a.history_rows = (0..40)
                        .map(|i| HistoryRow {
                            id: format!("h{i}"),
                            sql: format!("SELECT column_{i} FROM a_very_long_table_name_{i} WHERE id = {i} AND ok = 1"),
                            executed_at: "2026-06-27T12:34:56Z".into(),
                            connection_name: "prod-mysql".into(),
                            success: i % 5 != 0,
                            duration_ms: if i % 3 == 0 { 12 } else { 0 },
                            origin: if i % 2 == 0 { "editor".into() } else { String::new() },
                            count: 1,
                        })
                        .collect();
                a.history_view = (0..a.history_rows.len()).collect();
                a.history_list.select(Some(0));
                a.history_open = true;
            }),
        ),
        (
            "history-filter",
            Box::new(|a| {
                a.history_rows = vec![HistoryRow {
                    id: "h0".into(),
                    sql: "SELECT 1".into(),
                    executed_at: "2026-06-27T12:34:56Z".into(),
                    connection_name: "prod".into(),
                    success: true,
                    duration_ms: 0,
                    origin: String::new(),
                    count: 1,
                }];
                a.history_view = vec![0];
                a.history_list.select(Some(0));
                a.history_open = true;
                a.history_filter = Some(TextArea::from(["sel"]));
            }),
        ),
        (
            "history-confirm",
            Box::new(|a| {
                a.history_open = true;
                a.history_confirm = Some(HistoryConfirm {
                    id: "h0".into(),
                    sql: "DELETE FROM users WHERE id = 1".into(),
                });
            }),
        ),
        (
            "snippets",
            Box::new(|a| {
                a.snippets = vec![
                    SnippetRow {
                        id: "s0".into(),
                        label: "recent.sql".into(),
                        sql: "SELECT 1".into(),
                    },
                    SnippetRow {
                        id: "s1".into(),
                        label: "users.sql".into(),
                        sql: "SELECT * FROM users".into(),
                    },
                ];
                a.snippet_view = vec![0, 1];
                a.snippet_list.select(Some(0));
                a.snippet_open = true;
            }),
        ),
        (
            "snippet-filter",
            Box::new(|a| {
                a.snippets = vec![SnippetRow {
                    id: "s0".into(),
                    label: "users.sql".into(),
                    sql: "SELECT * FROM users".into(),
                }];
                a.snippet_view = vec![0];
                a.snippet_list.select(Some(0));
                a.snippet_open = true;
                a.snippet_needle = "users".into();
                a.snippet_filter = Some(TextArea::from(["users"]));
            }),
        ),
        (
            "snippet-confirm",
            Box::new(|a| {
                a.snippets = vec![SnippetRow {
                    id: "s0".into(),
                    label: "users.sql".into(),
                    sql: "SELECT * FROM users".into(),
                }];
                a.snippet_view = vec![0];
                a.snippet_list.select(Some(0));
                a.snippet_open = true;
                a.snippet_confirm = Some("s0".into());
            }),
        ),
        (
            "snippet-name",
            Box::new(|a| a.snippet_name = Some(TextArea::default())),
        ),
        (
            "templates",
            Box::new(|a| {
                open_template_panel(a);
                a.template_list.select(Some(1));
            }),
        ),
        (
            "template-filter",
            Box::new(|a| {
                open_template_panel(a);
                a.template_needle = "update".into();
                recompute_template_view(a);
                a.template_filter = Some(TextArea::from(["update"]));
            }),
        ),
        (
            "table-prompt",
            Box::new(|a| a.table_prompt = Some(TextArea::default())),
        ),
        (
            "result-filter",
            Box::new(|a| a.result_filter = Some(TextArea::default())),
        ),
        (
            "filter-prompt",
            Box::new(|a| a.filter_prompt = Some(TextArea::default())),
        ),
        (
            "ssh-hostkey",
            Box::new(|a| {
                let (tx, _rx) = tokio::sync::oneshot::channel();
                a.ssh_prompt = Some(SshPromptState {
                    request: ssh_prompt::host_key_changed_request(
                        "jump.example.com",
                        22,
                        Some("ssh-ed25519".into()),
                        Some("SHA256:new".into()),
                        Some("SHA256:old".into()),
                    ),
                    responder: Some(tx),
                    input: String::new(),
                });
            }),
        ),
        (
            "ssh-secret",
            Box::new(|a| {
                let (tx, _rx) = tokio::sync::oneshot::channel();
                a.ssh_prompt = Some(SshPromptState {
                    request: ssh_prompt::secret_input_request(
                        "jump.example.com",
                        22,
                        "Verification code".into(),
                        false,
                    ),
                    responder: Some(tx),
                    input: "123456".into(),
                });
            }),
        ),
        (
            "conn-export",
            Box::new(|a| {
                let mut path = TextArea::default();
                path.insert_str(CONN_EXPORT_DEFAULT_PATH);
                a.conn_export = Some(Box::new(ConnExport {
                    path,
                    field: 0,
                    editing: false,
                    include_passwords: false,
                    confirm_pw: false,
                }));
            }),
        ),
        (
            "conn-export-confirm",
            Box::new(|a| {
                let mut path = TextArea::default();
                path.insert_str(CONN_EXPORT_DEFAULT_PATH);
                a.conn_export = Some(Box::new(ConnExport {
                    path,
                    field: 1,
                    editing: false,
                    include_passwords: false,
                    confirm_pw: true,
                }));
            }),
        ),
        (
            "conn-import-path",
            Box::new(|a| a.conn_import_path = Some(TextArea::from(["~/dbeaver.json"]))),
        ),
        (
            "conn-import-plan",
            Box::new(|a| {
                a.conn_import_plan = Some(Box::new(ConnImportPlan {
                    source: ConnSource::DBeaver,
                    origin: "~/.local/share/DBeaverData/…/data-sources.json".into(),
                    rows: vec![
                        ConnImportRow {
                            conn: ImportConn {
                                name: "prod-mysql".into(),
                                driver: "mysql8".into(),
                                db_type: Some("mysql".into()),
                                host: "db.internal".into(),
                                port: Some(3306),
                                user: "root".into(),
                                ssl: true,
                                color: Some("#e06c75".into()),
                                needs_password: true,
                                ..ImportConn::default()
                            },
                            dup: true,
                            policy: DupPolicy::Overwrite,
                            selected: true,
                        },
                        ConnImportRow {
                            conn: ImportConn {
                                name: "jump-pg".into(),
                                driver: "postgresql".into(),
                                db_type: Some("postgres".into()),
                                host: "10.0.0.5".into(),
                                port: Some(5432),
                                user: "pg".into(),
                                ssh: Some(ImportSsh {
                                    host: "jump.example.com".into(),
                                    port: 22,
                                    user: "ops".into(),
                                    auth_method: "key".into(),
                                    ..ImportSsh::default()
                                }),
                                needs_password: true,
                                ..ImportConn::default()
                            },
                            dup: false,
                            policy: DupPolicy::Skip,
                            selected: true,
                        },
                    ],
                    skipped: vec!["derby · legacy".into()],
                    cursor: 1,
                    confirm: None,
                }));
            }),
        ),
        (
            "conn-import-confirm",
            Box::new(|a| {
                a.conn_import_plan = Some(Box::new(ConnImportPlan {
                    source: ConnSource::Navicat,
                    origin: "/tmp/conns.ncx".into(),
                    rows: vec![ConnImportRow {
                        conn: ImportConn {
                            name: "hero".into(),
                            driver: "MYSQL".into(),
                            db_type: Some("mysql".into()),
                            host: "127.0.0.1".into(),
                            port: Some(3306),
                            ..ImportConn::default()
                        },
                        dup: true,
                        policy: DupPolicy::Overwrite,
                        selected: true,
                    }],
                    skipped: Vec::new(),
                    cursor: 0,
                    confirm: Some(ConnOverwriteScope::All),
                }));
            }),
        ),
    ];

    for (name, set) in &cases {
        reset(&mut app);
        set(&mut app);
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
        let _ = name;
    }
}

/// The new-connection form on its own page, at tiny sizes (it clamps its own
/// box, so this is the regression guard for that path).
#[test]
pub(crate) fn new_connection_form_renders_at_extreme_sizes() {
    let mut app = test_app();
    app.page = Page::NewConn;
    for (w, h) in [(40u16, 12u16), (250, 70), (20, 6), (1, 1)] {
        draw(&mut app, w, h);
    }
}

/// The SSH-expanded form must render (and scroll) at phone and desktop sizes
/// for every auth method, with the forward target shown and the save row
/// reachable by moving the cursor.
#[test]
pub(crate) fn ssh_form_renders_and_scrolls_at_extreme_sizes() {
    let mut app = test_app();
    app.page = Page::NewConn;
    app.form = password_ssh_form();
    for auth in [SshAuth::Password, SshAuth::Key, SshAuth::Agent] {
        app.form.ssh_auth = auth;
        for (w, h) in [(40u16, 12u16), (30, 20), (80, 24), (20, 6), (1, 1)] {
            draw(&mut app, w, h);
        }
    }
    // With the cursor on the auth row the SSH section (and forward target)
    // shows. A tall-enough terminal is needed for the row to be on screen.
    app.form.ssh_auth = SshAuth::Password;
    let auth_idx = form_rows(&app.form)
        .iter()
        .position(|(r, _)| *r == FormRow::SshAuth)
        .unwrap();
    app.form.field = auth_idx;
    app.form.scroll = 0;
    let top = draw(&mut app, 60, 24).join("\n");
    let flat_top: String = top.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat_top.contains("10.0.0.5:3306"),
        "forward target missing:\n{top}"
    );
    // Moving to the last row scrolls the save button into view.
    app.form.field = form_rows(&app.form).len() - 1;
    let bottom = draw(&mut app, 60, 14).join("\n");
    let flat_bottom: String = bottom.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        flat_bottom.contains("保存连接"),
        "save row not scrolled into view:\n{bottom}"
    );
}

/// `y` in a Redis / Mongo grid must copy the focused row even when a result
/// search is active: the search keeps a display→source map, and reading the
/// row from the filtered grid with the full-grid index used to fail.
#[test]
pub(crate) fn focused_row_survives_an_active_result_search() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["name".into()],
        rows: vec![
            vec![Val::Text("ada".into())],
            vec![Val::Text("admin".into())],
            vec![Val::Text("bob".into())],
        ],
        note: String::new(),
    });
    assert_eq!(focused_full_row(&app).unwrap()[0].text(), "ada");

    app.result_needle = "admin".into();
    app.rebuild_view();
    app.sel = 0;
    assert_eq!(app.grid.as_ref().unwrap().rows.len(), 1);
    assert_eq!(focused_full_row(&app).unwrap()[0].text(), "admin");

    // A broader search (two matches) still resolves the cursor's row.
    app.result_needle = "a".into();
    app.rebuild_view();
    app.sel = 1;
    assert_eq!(focused_full_row(&app).unwrap()[0].text(), "admin");
}

/// `n` before the first SCAN page lands must not queue a second request from
/// the same cursor: that used to append the first page twice (the fresh-scan
/// race). The `pending` flag makes the second call a no-op until the reply.
#[test]
pub(crate) fn redis_load_more_is_ignored_while_a_page_is_in_flight() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.selected = Some(test_conn("redis"));
        app.backend_kind = Backend::Redis;
        start_redis_scan(&mut app, &tx, true);
        assert!(app.redis_scan.pending, "a reset scan is in flight");
        let spawned = app.pending_ops;
        start_redis_scan(&mut app, &tx, false);
        assert_eq!(app.pending_ops, spawned, "the second `n` must not spawn");
        // The reply releases the guard...
        app.redis_scan.pending = false;
        // ...and a reset always supersedes (bumps gen) and re-arms it.
        start_redis_scan(&mut app, &tx, true);
        assert!(app.redis_scan.pending);
    });
}

/// A >1 MB text cell and a binary column in all six export formats: none may
/// panic, and the binary column must become a hex literal in the INSERT
/// formats so the row round-trips instead of being mangled as a string.
#[test]
pub(crate) fn export_handles_huge_and_binary_cells_in_every_format() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.table_meta = Some(TableMeta {
        table: "t".into(),
        schema: String::new(),
        columns: vec![
            ColumnInfo {
                name: "id".into(),
                data_type: "int".into(),
                ..Default::default()
            },
            ColumnInfo {
                name: "payload".into(),
                data_type: "longblob".into(),
                ..Default::default()
            },
            ColumnInfo {
                name: "big".into(),
                data_type: "text".into(),
                ..Default::default()
            },
        ],
        indexes: Vec::new(),
    });
    let huge = "x".repeat(1_100_000);
    let grid = Grid {
        columns: vec!["id".into(), "payload".into(), "big".into()],
        rows: vec![vec![
            Val::Text("1".into()),
            Val::Text("\u{0}\u{1}raw".into()),
            Val::Text(huge.clone()),
        ]],
        note: String::new(),
    };
    for format in EXPORT_FORMATS {
        let out = render_export_content(
            &app,
            &grid,
            *format,
            Some(&("".to_string(), "t".to_string())),
        );
        assert!(!out.is_empty(), "{format:?} produced nothing");
    }
    // The blob column is copied as `X'…'`, never as a quoted string.
    let insert = render_export_content(
        &app,
        &grid,
        ExportFormat::Insert,
        Some(&("".to_string(), "t".to_string())),
    );
    assert!(insert.contains("X'0001726177'"), "{insert}");
    // The 1 MB text cell survives verbatim in CSV and Markdown.
    let csv = render_export_content(
        &app,
        &grid,
        ExportFormat::Csv,
        Some(&("".to_string(), "t".to_string())),
    );
    assert!(csv.contains(&huge));
    let md = render_export_content(
        &app,
        &grid,
        ExportFormat::Markdown,
        Some(&("".to_string(), "t".to_string())),
    );
    assert!(md.contains(&huge));
}

/// Build an `App` whose table metadata matches `(schema, table)` so the
/// INSERT generators resolve column types exactly as they would in the TUI.
pub(crate) fn export_meta_app(
    cfg: ConnectionConfig,
    schema: &str,
    table: &str,
    cols: &[(&str, &str)],
) -> App {
    let mut app = test_app();
    app.selected = Some(cfg);
    app.table_meta = Some(TableMeta {
        table: table.to_string(),
        schema: schema.to_string(),
        columns: cols
            .iter()
            .map(|(n, t)| ColumnInfo {
                name: (*n).to_string(),
                data_type: (*t).to_string(),
                ..Default::default()
            })
            .collect(),
        indexes: Vec::new(),
    });
    app
}

/// The hard requirement for the streaming export: every format must produce
/// exactly the bytes the legacy in-memory string builder produces. Compared
/// over a grid that exercises NULLs, empty strings, quotes, commas, embedded
/// newlines/tabs, unicode, canonical numbers, booleans and binary/array
/// columns (MySQL + PostgreSQL dialect paths).
pub(crate) fn assert_stream_matches(app: &App, grid: &Grid, schema: &str, table: &str) {
    let cfg = app.selected.as_ref().expect("a selected connection");
    let types = grid_column_types(app, schema, table, grid);
    let table_ref = Some((schema.to_string(), table.to_string()));
    for fmt in EXPORT_FORMATS {
        let reference = render_export_content(app, grid, *fmt, table_ref.as_ref());
        let mut buf: Vec<u8> = Vec::new();
        write_export(&mut buf, Some(cfg), schema, table, &types, grid, *fmt).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            reference,
            "streaming {:?} diverged from the string builder",
            fmt
        );
    }
}

#[test]
pub(crate) fn export_stream_matches_string_builders() {
    let cols = [
        ("id", "int"),
        ("name", "varchar(64)"),
        ("note", "text"),
        ("amount", "numeric(10,2)"),
        ("flag", "boolean"),
        ("empty", "text"),
        ("blob", "longblob"),
        ("tags", "text[]"),
    ];
    let app = export_meta_app(test_conn("mysql"), "shop", "orders", &cols);
    let grid = Grid {
        columns: cols.iter().map(|(n, _)| (*n).to_string()).collect(),
        rows: vec![
            vec![
                Val::Text("1".into()),
                Val::Text("a,b".into()),
                Val::Text("he said \"hi\"".into()),
                Val::Text("1.50".into()),
                Val::Text("true".into()),
                Val::Text(String::new()),
                Val::Text("0x00FF10".into()),
                Val::Text("[1, 2, 3]".into()),
            ],
            vec![
                Val::Null,
                Val::Text("行\n新".into()),
                Val::Text("back\\slash|pipe".into()),
                Val::Text("007".into()),
                Val::Text("false".into()),
                Val::Null,
                Val::Null,
                Val::Null,
            ],
            vec![
                Val::Text("3".into()),
                Val::Text("用户\t名".into()),
                Val::Text("cr\rlf".into()),
                Val::Text("1e5".into()),
                Val::Text("0.5".into()),
                Val::Text("  ".into()),
                Val::Text("raw".into()),
                Val::Text("[]".into()),
            ],
        ],
        note: String::new(),
    };
    assert_stream_matches(&app, &grid, "shop", "orders");

    // The PostgreSQL dialect exercises `"schema"."table"` quoting, bytea
    // and array literals through the same comparison.
    let pg = export_meta_app(test_conn("postgres"), "public", "items", &cols);
    assert_stream_matches(&pg, &grid, "public", "items");
}

/// Edge shapes: no rows, no columns, and a row count that straddles the
/// batch-INSERT chunk boundary (the streaming writer emits one statement per
/// chunk, so an off-by-one there would corrupt the output).
#[test]
pub(crate) fn export_stream_matches_on_edge_grids() {
    let app = export_meta_app(
        test_conn("mysql"),
        "shop",
        "t",
        &[("id", "int"), ("name", "varchar(16)")],
    );
    let empty = Grid {
        columns: vec!["id".into(), "name".into()],
        rows: Vec::new(),
        note: String::new(),
    };
    assert_stream_matches(&app, &empty, "shop", "t");

    let no_cols = Grid {
        columns: Vec::new(),
        rows: vec![Vec::new()],
        note: String::new(),
    };
    assert_stream_matches(&app, &no_cols, "shop", "t");

    for rows in [
        EXPORT_INSERT_BATCH - 1,
        EXPORT_INSERT_BATCH,
        EXPORT_INSERT_BATCH + 1,
    ] {
        let grid = Grid {
            columns: vec!["id".into(), "name".into()],
            rows: (0..rows)
                .map(|i| vec![Val::Text(i.to_string()), Val::Text(format!("n{i}"))])
                .collect(),
            note: String::new(),
        };
        assert_stream_matches(&app, &grid, "shop", "t");
    }
}

/// The background export result lands on the status line, and a failure is
/// surfaced instead of silently swallowing the write error.
#[test]
pub(crate) fn export_done_updates_the_status() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    apply_op_result(
        &mut app,
        OpResult::ExportDone {
            format: ExportFormat::Csv,
            path: PathBuf::from("/tmp/out.csv"),
            rows: 20_000,
            bytes: 4_178_962,
            elapsed_ms: 0,
            error: None,
        },
        &tx,
    );
    assert!(app.status.contains("CSV"), "{}", app.status);
    assert!(app.status.contains("20000"), "{}", app.status);
    assert!(app.status.contains("/tmp/out.csv"), "{}", app.status);
    apply_op_result(
        &mut app,
        OpResult::ExportDone {
            format: ExportFormat::JsonArray,
            path: PathBuf::from("/nope/out.json"),
            rows: 3,
            bytes: 0,
            elapsed_ms: 0,
            error: Some("permission denied".into()),
        },
        &tx,
    );
    assert!(app.status.contains("permission denied"), "{}", app.status);
}

/// The cached widths must equal the per-column reference for every column,
/// including the clamp at both ends of `[MIN_CELL_WIDTH, max_cell]`.
#[test]
pub(crate) fn natural_widths_matches_the_per_column_reference() {
    let grid = Grid {
        columns: vec!["id".into(), "description".into(), "n".into()],
        rows: vec![
            vec![
                Val::Text("1".into()),
                Val::Text("a very wide cell indeed".into()),
                Val::Null,
            ],
            vec![
                Val::Text("22".into()),
                Val::Text("短".into()),
                Val::Text("1234567890".into()),
            ],
        ],
        note: String::new(),
    };
    for max_cell in [MIN_CELL_WIDTH, 12, 44] {
        let all = natural_widths(&grid, max_cell);
        for (ci, w) in all.iter().enumerate() {
            assert_eq!(*w, natural_width(&grid, ci, max_cell), "col {ci}");
        }
    }
}

/// An import writes rows, so the session COUNT(*) cache must be dropped —
/// otherwise a table browsed before the import (imported into from the
/// sidebar) would show a stale total on its next open.
#[test]
pub(crate) fn import_done_invalidates_the_count_cache() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.count_cache
        .insert("d\u{1}\u{1}t\u{1}".into(), (5, false));
    app.import_progress = Some((1, 1));
    let rep = ImportReport {
        table: "t".into(),
        schema: String::new(),
        mode: ImportMode::Append,
        total: 1,
        inserted: 1,
        skipped: Vec::new(),
        aborted: None,
        elapsed_ms: 1,
    };
    apply_op_result(&mut app, OpResult::ImportDone(Box::new(rep)), &tx);
    assert!(
        app.count_cache.is_empty(),
        "stale COUNT(*) survived an import"
    );
    assert!(app.import_progress.is_none());
    assert!(app.import_report.is_some());
}

/// A transfer that (re)creates the target table leaves it freshly empty even
/// when no row moved (create-only, or an overwrite of an empty source), so
/// the session COUNT(*) cache must drop too — not only when `moved > 0`.
#[test]
pub(crate) fn transfer_done_invalidates_the_count_cache_when_the_table_is_created() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let report = |moved: u64, created: bool| {
        let mut rep = transfer_report_fixture();
        rep.moved = moved;
        rep.created = created;
        rep.cancelled = false;
        rep.aborted = None;
        Box::new(rep)
    };
    let mut app = test_app();
    app.count_cache
        .insert("d\u{1}\u{1}t\u{1}".into(), (5, false));
    apply_op_result(
        &mut app,
        OpResult::TransferDone {
            gen: 0,
            report: report(0, true),
        },
        &tx,
    );
    assert!(
        app.count_cache.is_empty(),
        "stale COUNT(*) survived a create/overwrite"
    );

    // An append that moved nothing changed no rows, so the cache stays.
    app.count_cache
        .insert("d\u{1}\u{1}t\u{1}".into(), (5, false));
    apply_op_result(
        &mut app,
        OpResult::TransferDone {
            gen: 0,
            report: report(0, false),
        },
        &tx,
    );
    assert!(!app.count_cache.is_empty());
}

#[test]
pub(crate) fn grid_at_zero_height_does_not_panic() {
    // The results pane can be squeezed to zero rows on a tiny terminal; the
    // horizontal scrollbar is drawn on the bottom border and must not do
    // `area.height - 1` arithmetic on a zero-height area.
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = test_app();
    let grid = sample_grid();
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| {
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 0,
        };
        render_grid(f, area, &mut app, &grid, GridKind::TableData, " t ", false);
    })
    .unwrap();
}

#[test]
pub(crate) fn short_message_is_untouched() {
    assert_eq!(fit_status("ok", 10), "ok");
}

#[test]
pub(crate) fn warning_keeps_head() {
    // A ⚠ heads-up (e.g. the large-result hint) must stay readable on a
    // narrow status line; its tail is only the connection / row count.
    let msg = "⚠ 大结果集 · LIMIT 20000 · 可能较慢 · rw-sqlite · 3 行 · 1ms";
    let out = fit_status(msg, 24);
    assert!(out.starts_with('⚠'));
    assert!(out.contains("大结果集"));
    assert_eq!(disp_width(&out), 24);
}

#[test]
pub(crate) fn error_keeps_head() {
    let msg = "✗ query: Server error: `ERROR 1146 (42S02): Table 'mysql.users' doesn't exist` SQL text omitted from user-facing error; enable debug SQL diagnostics to inspect the original statement.";
    let out = fit_status(msg, 30);
    assert!(out.starts_with("✗ query: Server error:"));
    assert!(out.ends_with('…'));
    assert_eq!(disp_width(&out), 30);
}

#[test]
pub(crate) fn normal_message_keeps_tail() {
    let msg = "loading tables for database mydb ... done";
    let out = fit_status(msg, 12);
    assert!(out.starts_with('…'));
    assert!(out.ends_with("done"));
    assert_eq!(disp_width(&out), 12);
}

#[test]
pub(crate) fn normal_cjk_message_keeps_a_visible_tail() {
    // Display width != char count here: the old skip-by-chars logic dropped
    // the whole message (just `…`) on a narrow screen.
    let msg = "脚本列表不支持搜索（先 Enter 进入某条语句的结果）";
    let out = fit_status(msg, 21);
    assert!(out.starts_with('…'));
    assert!(disp_width(&out) <= 21);
    assert!(out.ends_with('）'));
    // The tail must actually carry content, not be an empty `…`.
    assert!(disp_width(&out) > 1);
}

#[test]
pub(crate) fn multibyte_is_char_safe() {
    let msg = "✗ 错误：表不存在，这是一段很长的中文诊断信息";
    let out = fit_status(msg, 8);
    assert!(out.starts_with('✗'));
    assert!(disp_width(&out) <= 8);
}

#[test]
pub(crate) fn truncate_is_display_width_aware() {
    assert_eq!(truncate_disp("abcdef", 4), "abc…");
    assert_eq!(truncate_disp("abc", 4), "abc");
    // CJK characters are two columns wide
    assert_eq!(truncate_disp("中文字符", 5), "中文…");
}

#[test]
pub(crate) fn danger_detection_flags_unbounded_dml() {
    assert!(detect_danger("UPDATE users SET a = 1").is_some());
    assert!(detect_danger("DELETE FROM users").is_some());
    assert!(detect_danger("DROP TABLE users").is_some());
    assert!(detect_danger("TRUNCATE TABLE users").is_some());
}

#[test]
pub(crate) fn danger_detection_allows_bounded_and_reads() {
    assert!(detect_danger("UPDATE users SET a = 1 WHERE id = 2").is_none());
    assert!(detect_danger("DELETE FROM users WHERE id = 2").is_none());
    assert!(detect_danger("SELECT * FROM users").is_none());
    assert!(detect_danger("INSERT INTO users VALUES (1)").is_none());
}

#[test]
pub(crate) fn danger_detection_ignores_literals_and_comments() {
    assert!(detect_danger("DELETE FROM t WHERE name = 'where'").is_none());
    // A commented-out WHERE must not count as a real clause.
    assert!(detect_danger("DELETE FROM t -- WHERE id = 1\n").is_some());
    assert!(detect_danger("UPDATE t SET a = 'DROP TABLE x'").is_some());
}

#[test]
pub(crate) fn danger_detection_sees_cte_delete() {
    assert!(detect_danger("WITH x AS (SELECT id FROM t) DELETE FROM t").is_some());
    assert!(detect_danger(
        "WITH x AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM x)"
    )
    .is_none());
}

#[test]
pub(crate) fn danger_detection_flags_alter_drop_column() {
    assert!(detect_danger("ALTER TABLE users DROP COLUMN email").is_some());
    assert!(detect_danger("ALTER TABLE users DROP CONSTRAINT ck").is_some());
    // A non-destructive ALTER stays clear.
    assert!(detect_danger("ALTER TABLE users ADD COLUMN email text").is_none());
    assert!(detect_danger("ALTER TABLE users RENAME TO people").is_none());
}

#[test]
pub(crate) fn read_only_classifies_reads_and_writes() {
    for read in [
        "SELECT * FROM t",
        "SHOW TABLES",
        "EXPLAIN SELECT * FROM t",
        "EXPLAIN ANALYZE SELECT * FROM t",
        "EXPLAIN (ANALYZE, FORMAT JSON) SELECT * FROM t",
        "(SELECT 1)",
        "VALUES (1), (2)",
        "TABLE t",
        "DESCRIBE t",
    ] {
        assert!(statement_is_read_only(read), "should be read-only: {read}");
    }
    // Everything ambiguous or destructive fails closed.
    for write in [
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1 WHERE id = 1",
        "DELETE FROM t WHERE id = 1",
        "CREATE TABLE t (id int)",
        "ALTER TABLE t ADD COLUMN a int",
        "DROP TABLE t",
        "TRUNCATE TABLE t",
        "EXPLAIN ANALYZE DELETE FROM t",
        "EXPLAIN (ANALYZE) DELETE FROM t",
        "BEGIN",
        "COMMIT",
        "SET search_path = x",
        "CALL p()",
        "SELEC 1",
    ] {
        assert!(!statement_is_read_only(write), "should be blocked: {write}");
    }
}

#[test]
pub(crate) fn read_only_handles_cte_and_multi_statement_boundaries() {
    // A CTE whose body reads but whose main verb writes must block.
    assert!(statement_is_read_only(
        "WITH x AS (SELECT id FROM t) SELECT * FROM x"
    ));
    assert!(!statement_is_read_only(
        "WITH x AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM x)"
    ));
    assert!(!statement_is_read_only(
        "WITH x AS (SELECT id FROM t) INSERT INTO t SELECT * FROM x"
    ));
    // An undetermined WITH has no recognised verb → block (fail closed).
    assert!(!statement_is_read_only("WITH x AS (SELECT 1)"));
    // A read-only connection blocks a mixed batch as a whole.
    let mut cfg = test_conn("mysql");
    cfg.read_only = true;
    assert!(readonly_violation(&cfg, "SELECT 1; SELECT 2").is_none());
    assert_eq!(
        readonly_violation(&cfg, "SELECT 1; INSERT INTO t VALUES (1);").as_deref(),
        Some("INSERT")
    );
    // The guard only applies to read-only connections.
    cfg.read_only = false;
    assert!(readonly_violation(&cfg, "DROP TABLE t").is_none());
}

#[test]
pub(crate) fn readonly_block_refuses_and_reports_a_red_status() {
    let mut app = test_app();
    let mut cfg = test_conn("mysql");
    cfg.read_only = true;
    app.selected = Some(cfg);
    assert!(readonly_block(&mut app, "INSERT INTO t VALUES (1)"));
    assert!(app.status.starts_with('✗'), "red status: {}", app.status);
    assert!(app.status.contains("只读连接"));
    assert!(!readonly_block(&mut app, "SELECT * FROM t"));
    // The row-gesture guard (edit / insert / delete / CSV import) refuses
    // without needing the SQL text yet.
    assert!(readonly_conn_block(&mut app));
    assert!(app.status.starts_with('✗'));
    app.selected.as_mut().unwrap().read_only = false;
    assert!(!readonly_conn_block(&mut app));
}

/// R54: with several connections open, a read-only refusal names the
/// connection it happened on, and the editor title + status-bar badge both
/// carry the 🔒 so the write policy is visible while typing.
#[test]
pub(crate) fn read_only_refusal_names_the_connection_and_shows_a_lock() {
    let mut app = test_app();
    app.picker_open = false;
    app.backend_kind = Backend::Sql;
    let mut cfg = test_conn("mysql");
    cfg.name = "prod-ro".into();
    cfg.read_only = true;
    app.selected = Some(cfg);
    app.focus = Focus::Editor;

    assert!(readonly_block(&mut app, "DELETE FROM t"));
    assert!(app.status.contains("prod-ro"), "{}", app.status);
    assert!(app.status.contains("DELETE"), "{}", app.status);
    assert!(readonly_conn_block(&mut app));
    assert!(app.status.contains("prod-ro"), "{}", app.status);

    let screen = draw(&mut app, 100, 30).join("\n");
    assert!(
        screen.contains("SQL 🔒"),
        "editor title lock missing:\n{screen}"
    );
    assert!(
        screen.contains("● 🔒"),
        "status badge lock missing:\n{screen}"
    );
    assert!(
        screen.contains("prod-ro"),
        "status badge connection name missing:\n{screen}"
    );
}

/// R68: `!` on the tree flips the connection's read-only policy behind the
/// red confirmation layer, persists it in memory (so the 🔒 and every write
/// guard update at once) and flips it back on a second press.
#[test]
pub(crate) fn readonly_toggle_flips_marker_and_persists() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        app.focus = Focus::Sidebar;
        app.side_sel = 0;
        assert!(!app.selected.as_ref().unwrap().read_only);

        let bang = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        // `!` opens the red layer carrying the *new* value (true).
        key(&mut app, &tx, bang);
        let cc = app
            .confirm
            .as_ref()
            .and_then(|c| c.conn.clone())
            .expect("a read-only confirm layer");
        assert_eq!(cc.readonly, Some(true));
        assert!(!cc.disconnect, "a toggle is not a disconnect");
        assert!(app.status.contains("设为只读"), "{}", app.status);

        // Enter applies it to the live config and the stored copy.
        key(&mut app, &tx, enter);
        assert!(app.confirm.is_none(), "the layer closed");
        assert!(app.selected.as_ref().unwrap().read_only, "selected flipped");
        assert!(app.connections[0].read_only, "stored copy flipped");
        assert!(app.status.contains("只读"), "{}", app.status);

        // The tree now carries the 🔒 on the connection root.
        let screen = draw(&mut app, 100, 30).join("\n");
        assert!(screen.contains('🔒'), "tree lock missing:\n{screen}");

        // A second `!` offers the reverse and flips it back to writable.
        key(&mut app, &tx, bang);
        let cc = app
            .confirm
            .as_ref()
            .and_then(|c| c.conn.clone())
            .expect("a second confirm layer");
        assert_eq!(cc.readonly, Some(false));
        key(&mut app, &tx, enter);
        assert!(!app.selected.as_ref().unwrap().read_only);
        assert!(!app.connections[0].read_only);
        assert!(app.status.contains("可写"), "{}", app.status);
    });
}

/// R68: Esc on the read-only confirmation changes nothing (the policy is
/// only written on an explicit Enter / y).
#[test]
pub(crate) fn readonly_toggle_confirmation_cancels() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        app.focus = Focus::Sidebar;
        app.side_sel = 0;
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert!(app.confirm.is_some());
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(app.confirm.is_none());
        assert!(!app.selected.as_ref().unwrap().read_only, "unchanged");
        assert!(!app.connections[0].read_only, "stored copy unchanged");
    });
}

/// R68: a read-only connection refuses every named write gesture before it
/// can build SQL / a Redis command, and each refusal names the connection.
#[test]
pub(crate) fn readonly_blocks_row_redis_and_rename_gestures() {
    let mut app = test_app();
    let mut cfg = test_conn("mysql");
    cfg.name = "prod-ro".into();
    cfg.read_only = true;
    app.selected = Some(cfg);

    // Row delete in a table-data grid.
    app.grid_kind = GridKind::TableData;
    app.page_state = Some(page_of("orders"));
    app.set_grid(sample_grid());
    delete_row(&mut app);
    assert!(app.status.contains("prod-ro"), "{}", app.status);
    assert!(
        app.confirm.is_none(),
        "no delete confirm on a read-only conn"
    );

    // Redis single-key delete and the batch rename gesture.
    app.backend_kind = Backend::Redis;
    app.redis_value = Some(redis_sample_view());
    redis_confirm_delete(&mut app);
    assert!(app.confirm.is_none(), "no redis delete confirm");
    assert!(app.status.contains("prod-ro"), "{}", app.status);
    open_redis_batch_rename_prompt(&mut app);
    assert!(app.redis_prompt.is_none(), "no rename prompt");
    assert!(app.status.contains("prod-ro"), "{}", app.status);
}

/// R68 self-check: the row popup, column picker and favourites panel each
/// advance exactly one row per `j`/`k` event, matching the main results
/// grid, so a held key scrolls at the same rate everywhere.
#[test]
pub(crate) fn overlay_scroll_step_matches_the_main_grid() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);

    // Main results grid.
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.sel = 0;
    key(&mut app, &tx, j);
    assert_eq!(app.sel, 1, "main grid advances one row per j");

    // Row popup.
    app.sel = 0;
    open_row_popup(&mut app);
    key(&mut app, &tx, j);
    assert_eq!(
        app.row_popup.as_ref().unwrap().cursor,
        1,
        "row popup advances one entry per j"
    );

    // Column picker.
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    open_col_picker(&mut app);
    app.col_picker_list.select(Some(0));
    key(&mut app, &tx, j);
    assert_eq!(
        app.col_picker_list.selected(),
        Some(1),
        "column picker advances one column per j"
    );

    // SQL favourites panel.
    let mut app = test_app();
    app.snippets = (0..3)
        .map(|i| SnippetRow {
            id: format!("s{i}"),
            label: format!("fav{i}"),
            sql: "SELECT 1".into(),
        })
        .collect();
    app.snippet_view = (0..3).collect();
    app.snippet_open = true;
    app.snippet_list.select(Some(0));
    key(&mut app, &tx, j);
    assert_eq!(
        app.snippet_list.selected(),
        Some(1),
        "favourites panel advances one row per j"
    );
}

#[test]
pub(crate) fn where_predicates_are_extracted_and_truncated() {
    assert_eq!(
        where_predicates("UPDATE t SET a = 1 WHERE id = 7"),
        vec!["id = 7".to_string()]
    );
    // One predicate per UPDATE / DELETE; literals with commas are kept whole.
    assert_eq!(
        where_predicates(
            "DELETE FROM t WHERE a = 1 AND b = 'x, y';\nUPDATE t SET a = 2 WHERE id IN (1,2,3);"
        ),
        vec![
            "a = 1 AND b = 'x, y'".to_string(),
            "id IN (1,2,3)".to_string()
        ]
    );
    // A SELECT's WHERE is not an impact line.
    assert!(where_predicates("SELECT * FROM t WHERE id = 1").is_empty());
    // The top-level WHERE wins, not a subquery's.
    assert_eq!(
        where_predicates("DELETE FROM t WHERE id IN (SELECT id FROM u WHERE x = 1)"),
        vec!["id IN (SELECT id FROM u WHERE x = 1)".to_string()]
    );
    // Truncated to 80 display columns with an ellipsis.
    let long = format!("UPDATE t SET a = 1 WHERE {}", "x".repeat(200));
    let got = where_predicates(&long);
    assert_eq!(got.len(), 1);
    assert_eq!(disp_width(&got[0]), 80);
    assert!(got[0].ends_with('…'));
}

#[test]
pub(crate) fn large_limit_hint_only_fires_over_ten_thousand() {
    assert_eq!(large_limit_hint("SELECT * FROM t LIMIT 20000"), Some(20000));
    assert_eq!(large_limit_hint("SELECT * FROM t LIMIT 500"), None);
    // Not a read statement, so no heads-up.
    assert_eq!(
        large_limit_hint("UPDATE t SET a = 1 WHERE id = 1 LIMIT 20000"),
        None
    );
    // MySQL `LIMIT offset, count` counts the second number.
    assert_eq!(
        large_limit_hint("SELECT * FROM t LIMIT 10, 20000"),
        Some(20000)
    );
    assert_eq!(large_limit_hint("SELECT * FROM t"), None);
}

#[test]
pub(crate) fn quit_is_two_stage_when_the_editor_is_dirty() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.set_editor_text("SELECT 1;");
    // The first q only arms the quit.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    assert!(app.quit_armed);
    assert!(!app.quit);
    // Esc leaves without quitting and disarms.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.quit_armed);
    assert!(!app.quit);
    // Two presses quit.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    assert!(app.quit_armed);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    assert!(app.quit);
}

#[test]
pub(crate) fn quit_is_immediate_when_the_editor_matches_the_last_run() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.set_editor_text("SELECT 1;");
    app.last_executed = Some("SELECT 1;".into());
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    assert!(app.quit, "a clean editor quits on the first q");
    // Ctrl-C shares the same guard: an unrun statement arms instead of quitting.
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.set_editor_text("SELECT 1;");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert!(app.quit_armed);
    assert!(!app.quit);
}

#[test]
pub(crate) fn natural_width_is_content_sized() {
    let grid = Grid {
        columns: vec!["id".into(), "description".into()],
        rows: vec![
            vec![Val::Text("1".into()), Val::Text("a longer value".into())],
            vec![Val::Text("22".into()), Val::Null],
        ],
        note: String::new(),
    };
    assert_eq!(natural_width(&grid, 0, 44), MIN_CELL_WIDTH);
    assert_eq!(natural_width(&grid, 1, 44), 14);
    assert_eq!(natural_width(&grid, 1, 10), 10);
}

#[test]
pub(crate) fn visible_cols_fits_content_widths() {
    let grid = Grid {
        columns: vec!["a".into(), "b".into(), "c".into()],
        rows: vec![vec![
            Val::Text("1234567890".into()),
            Val::Text("1234567890".into()),
            Val::Text("1234567890".into()),
        ]],
        note: String::new(),
    };
    // 10-wide columns + 1 space each: two fit in 21, three need 32
    assert_eq!(visible_cols(&grid, 0, 21, 44), 2);
    assert_eq!(visible_cols(&grid, 0, 32, 44), 3);
    assert_eq!(visible_cols(&grid, 2, 32, 44), 1);
}

#[test]
pub(crate) fn wrap_text_splits_on_width() {
    let lines = wrap_text("abcdefghij", 4);
    assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
}

#[test]
pub(crate) fn strip_sql_noise_removes_literals_and_comments() {
    let cleaned = strip_sql_noise("SELECT 'a''b', \"c\", `d` -- trailing\n/* block */ FROM t");
    assert!(!cleaned.contains('a'));
    assert!(cleaned.contains("FROM t"));
    assert!(!cleaned.contains("trailing"));
    assert!(!cleaned.contains("block"));
}

#[test]
pub(crate) fn keyword_matching_is_word_based() {
    assert!(has_keyword("delete from t where x=1", "where"));
    assert!(!has_keyword("select * from somewhere", "where"));
    assert!(!has_keyword("update t set a='nowhere'", "where"));
}

// ── swipe / drag → column pan ──

pub(crate) fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
pub(crate) fn pan_steps_carries_the_remainder_and_caps_a_jump() {
    let mut a = 0;
    // half a step is kept, not rounded away
    assert_eq!(pan_steps(&mut a, 1), 0);
    assert_eq!(pan_steps(&mut a, 1), 1);
    assert_eq!(a, 0);
    assert_eq!(pan_steps(&mut a, -1), 0);
    assert_eq!(pan_steps(&mut a, -1), -1);
    // a coalesced jump (one event carrying the whole swipe) is capped
    let mut b = 0;
    assert_eq!(pan_steps(&mut b, 100), DRAG_MAX_STEPS);
    assert_eq!(b, 0);
    assert_eq!(pan_steps(&mut b, -100), -DRAG_MAX_STEPS);
}

#[test]
pub(crate) fn drag_left_pans_and_becomes_a_swipe() {
    let mut g = PanGesture::default();
    // the press itself is still handled as a potential tap
    assert_eq!(
        g.feed(
            MouseEventKind::Down(MouseButton::Left),
            10,
            5,
            DragPan::Button
        ),
        None
    );
    assert!(!g.is_swipe());
    // one column of travel: not enough for a step, and not yet a swipe
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            11,
            5,
            DragPan::Button
        ),
        Some(0)
    );
    assert!(!g.is_swipe());
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            13,
            5,
            DragPan::Button
        ),
        Some(1)
    );
    assert!(g.is_swipe(), "2 columns of travel is a swipe, not a tap");
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            15,
            5,
            DragPan::Button
        ),
        Some(1)
    );
    // releasing swallows nothing else and re-arms the tap logic
    assert_eq!(
        g.feed(
            MouseEventKind::Up(MouseButton::Left),
            15,
            5,
            DragPan::Button
        ),
        None
    );
    assert!(!g.is_swipe());
}

#[test]
pub(crate) fn a_drag_leftward_pans_the_other_way() {
    let mut g = PanGesture::default();
    g.feed(
        MouseEventKind::Down(MouseButton::Left),
        20,
        5,
        DragPan::Button,
    );
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            16,
            5,
            DragPan::Button
        ),
        Some(-2)
    );
}

#[test]
pub(crate) fn vertical_travel_is_neither_a_pan_nor_a_tap() {
    let mut g = PanGesture::default();
    g.feed(
        MouseEventKind::Down(MouseButton::Left),
        10,
        5,
        DragPan::Button,
    );
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            9,
            DragPan::Button
        ),
        Some(0)
    );
    assert!(
        g.is_swipe(),
        "a vertical drag must not fire the deferred tap"
    );
}

#[test]
pub(crate) fn bare_motion_is_a_swipe_only_when_opted_in() {
    // A desktop mouse sends `Moved` all the time, so it must be inert by
    // default (otherwise moving the mouse would scroll the table).
    let mut g = PanGesture::default();
    assert_eq!(g.feed(MouseEventKind::Moved, 10, 5, DragPan::Button), None);
    assert_eq!(g.feed(MouseEventKind::Moved, 14, 5, DragPan::Button), None);
    assert!(!g.is_swipe());
    // ... unless the user asked for it (touch terminals that never send a press)
    let mut h = PanGesture::default();
    assert_eq!(h.feed(MouseEventKind::Moved, 10, 5, DragPan::Any), None);
    assert_eq!(h.feed(MouseEventKind::Moved, 12, 5, DragPan::Any), Some(1));
    // but a held left button also qualifies in the default mode
    let mut i = PanGesture::default();
    i.feed(
        MouseEventKind::Down(MouseButton::Left),
        10,
        5,
        DragPan::Button,
    );
    assert_eq!(
        i.feed(MouseEventKind::Moved, 12, 5, DragPan::Button),
        Some(1)
    );
}

#[test]
pub(crate) fn other_buttons_and_off_mode_pass_through() {
    let mut g = PanGesture::default();
    assert_eq!(
        g.feed(MouseEventKind::Drag(MouseButton::Left), 10, 5, DragPan::Off),
        None
    );
    assert_eq!(g.feed(MouseEventKind::Moved, 14, 5, DragPan::Off), None);
    // a right-button drag (text selection on a desktop) is not a swipe
    let mut h = PanGesture::default();
    assert_eq!(
        h.feed(
            MouseEventKind::Down(MouseButton::Right),
            10,
            5,
            DragPan::Button
        ),
        None
    );
    assert_eq!(
        h.feed(
            MouseEventKind::Drag(MouseButton::Right),
            14,
            5,
            DragPan::Button
        ),
        None
    );
    assert_eq!(
        h.feed(
            MouseEventKind::Drag(MouseButton::Left),
            18,
            5,
            DragPan::Button
        ),
        None
    );
}

#[test]
pub(crate) fn a_drag_without_a_press_still_starts_a_gesture() {
    // Some terminals (and tmux forwarding) report the drag but drop the press.
    // The first event only establishes the reference position, so it is not
    // swallowed (it cannot be turned into travel yet) ...
    let mut g = PanGesture::default();
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            5,
            DragPan::Button
        ),
        None
    );
    // ... and every later drag pans from it.
    assert_eq!(
        g.feed(
            MouseEventKind::Drag(MouseButton::Left),
            14,
            5,
            DragPan::Button
        ),
        Some(2)
    );
}

#[test]
pub(crate) fn taps_are_deferred_only_once_a_release_was_seen() {
    let mut g = PanGesture::default();
    assert!(!g.can_defer_tap(), "press-to-click until an Up is proven");
    g.feed(
        MouseEventKind::Up(MouseButton::Left),
        10,
        5,
        DragPan::Button,
    );
    assert!(g.can_defer_tap());
}

// ── R46 double tap / click ──

#[test]
pub(crate) fn double_tap_fires_on_the_second_press_at_the_same_spot() {
    let mut d = DoubleTap::default();
    assert!(!d.feed(0, 10, 5), "the first press only arms the pair");
    assert!(
        d.feed(DOUBLE_TAP_MS - 1, 10, 5),
        "a second press inside the window is a double"
    );
    // A third press starts a fresh pair instead of firing again straight away.
    assert!(!d.feed(DOUBLE_TAP_MS, 10, 5));
    assert!(d.feed(2 * DOUBLE_TAP_MS - 1, 10, 5));
}

#[test]
pub(crate) fn double_tap_needs_the_same_spot_and_the_window() {
    // Two deliberate single taps (past the window) never merge into one.
    let mut slow = DoubleTap::default();
    assert!(!slow.feed(0, 10, 5));
    assert!(!slow.feed(DOUBLE_TAP_MS, 10, 5));
    // A finger is not pixel-accurate: one cell of drift still counts...
    let mut drift = DoubleTap::default();
    assert!(!drift.feed(0, 10, 5));
    assert!(drift.feed(DOUBLE_TAP_MS - 1, 11, 6));
    // ... two cells do not, on either axis.
    let mut far = DoubleTap::default();
    assert!(!far.feed(0, 10, 5));
    assert!(!far.feed(10, 12, 5));
    assert!(!far.feed(20, 10, 8));
}

#[test]
pub(crate) fn editor_viewport_follows_the_cursor_and_maps_a_click_back() {
    let mut vp = EditorViewport::default();
    vp.resize(50, 3);
    // A cursor at column 80 on a 50-wide viewport scrolls the origin to 31.
    vp.follow((0, 80));
    assert_eq!((vp.row, vp.col), (0, 31));
    // Clicking the first visible column maps to the scrolled column, not 0.
    assert_eq!(vp.text_pos(0, 0), (0, 31));
    assert_eq!(vp.text_pos(4, 1), (1, 35));
    // A page scroll moves the origin by the viewport height (the widget
    // scrolls first, then pulls the cursor back into view).
    vp.page(true);
    assert_eq!(vp.row, 3);
    assert_eq!(vp.text_pos(0, 2).0, 5);
    // PageUp walks it back and stops at the top.
    vp.page(false);
    vp.page(false);
    assert_eq!(vp.row, 0);
    // The mirror also replays the page-scroll keys' viewport delta.
    vp.note_key(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(vp.row, 3);
    vp.note_key(&KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(vp.row, 6);
    vp.note_key(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT));
    assert_eq!(vp.row, 3);
}

#[test]
pub(crate) fn confirm_buttons_report_their_hit_rectangles() {
    let inner = Rect {
        x: 10,
        y: 5,
        width: 40,
        height: 4,
    };
    let (_line, ok, cancel) = confirm_buttons(inner, inner.y + 2, "Enter/y 执行", "Esc/n 取消");
    assert_eq!((ok.x, ok.y, ok.height), (10, 7, 1));
    assert_eq!(ok.width as usize, disp_width("[ Enter/y 执行 ]"));
    assert_eq!(cancel.x, ok.x + ok.width + 2);
    assert!(cancel.x + cancel.width <= inner.x + inner.width);
    assert_eq!(cancel.height, 1);
    // A button row clipped out of the content area has no hit target.
    let (_l, ok2, cancel2) =
        confirm_buttons(inner, inner.y + inner.height, "Enter/y 执行", "Esc/n 取消");
    assert_eq!((ok2.width, cancel2.width), (0, 0));
    // A narrow box clamps the second button instead of letting a click land
    // outside the box.
    let narrow = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 2,
    };
    let (_l, ok3, cancel3) = confirm_buttons(narrow, 0, "Enter/y 执行", "Esc/n 取消");
    assert_eq!(ok3.width, 8);
    assert_eq!(cancel3.width, 0);
}

#[test]
pub(crate) fn tree_expander_columns_follow_the_depth() {
    assert_eq!(
        side_tri_cols(&SideRow::Conn { idx: 0, depth: 0 }),
        Some(0..2)
    );
    assert_eq!(
        side_tri_cols(&SideRow::Db {
            idx: 0,
            db: "shop".into(),
            depth: 1,
        }),
        Some(2..4)
    );
    // A table is a leaf: there is no expander to click.
    assert_eq!(
        side_tri_cols(&SideRow::Table {
            idx: 0,
            table: 0,
            depth: 2
        }),
        None
    );
    assert!(!side_row_has_tri(&SideRow::ConnLoading {
        idx: 0,
        depth: 1
    }));
}

/// A drawn query grid, with `rects` populated, ready for a mouse event.
pub(crate) fn drawn_preview_app(w: u16, h: u16) -> (App, Tx) {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.grid_kind = GridKind::Query;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    draw(&mut app, w, h);
    (app, tx)
}

pub(crate) fn press(app: &mut App, tx: &Tx, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse(app, tx, mouse(kind, x, y));
}

/// The `Up` path (a desktop terminal): press and release twice at the same
/// cell. The first click selects, the second opens the row detail.
#[test]
pub(crate) fn double_click_on_a_result_row_opens_the_row_popup() {
    let (mut app, tx) = drawn_preview_app(100, 30);
    let r = app.rects.results;
    let (x, y) = (r.x + 4, r.y + 2);
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(&mut app, &tx, MouseEventKind::Up(MouseButton::Left), x, y);
    assert!(app.row_popup.is_none(), "a single click only selects");
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(&mut app, &tx, MouseEventKind::Up(MouseButton::Left), x, y);
    assert!(
        app.row_popup.is_some(),
        "a double click opens the row detail"
    );
    assert_eq!(app.row_popup.as_ref().unwrap().lines.len(), 8);
}

/// The touch path: a terminal that never sends `Up` acts on the press, so the
/// two presses of a tap-tap must still open the row detail.
#[test]
pub(crate) fn tap_tap_without_release_events_opens_the_row_popup() {
    let (mut app, tx) = drawn_preview_app(100, 30);
    let r = app.rects.results;
    let (x, y) = (r.x + 4, r.y + 2);
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(app.row_popup.is_none(), "the first tap only selects");
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(app.row_popup.is_some(), "tap-tap opens the row detail");
}

/// A swipe that happens to end where it started must not be read as a
/// double click: the deferred tap is dropped, so no row detail opens.
#[test]
pub(crate) fn a_swipe_is_never_a_double_click() {
    let (mut app, tx) = drawn_preview_app(100, 30);
    // Swipe recognition has to be on for the drag to be seen at all.
    app.drag_pan = DragPan::Button;
    let r = app.rects.results;
    let (x, y) = (r.x + 4, r.y + 2);
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(&mut app, &tx, MouseEventKind::Up(MouseButton::Left), x, y);
    // Second press, then a horizontal drag far enough to be a swipe.
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(
        &mut app,
        &tx,
        MouseEventKind::Drag(MouseButton::Left),
        x + 6,
        y,
    );
    press(
        &mut app,
        &tx,
        MouseEventKind::Up(MouseButton::Left),
        x + 6,
        y,
    );
    assert!(app.row_popup.is_none());
}

/// The Redis value grid shares the results click path, so a double click
/// behaves exactly like the SQL side (select, then open).
#[test]
pub(crate) fn redis_value_grid_double_click_opens_the_row_popup() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.backend_kind = Backend::Redis;
    app.selected = Some(test_conn("redis"));
    let view = redis_sample_view();
    app.grid_kind = GridKind::RedisValue;
    app.set_grid(view.grid.clone());
    app.redis_value = Some(view);
    app.focus = Focus::Preview;
    draw(&mut app, 100, 30);
    let r = app.rects.results;
    let (x, y) = (r.x + 4, r.y + 2);
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(&mut app, &tx, MouseEventKind::Up(MouseButton::Left), x, y);
    assert!(app.row_popup.is_none());
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    press(&mut app, &tx, MouseEventKind::Up(MouseButton::Left), x, y);
    assert!(app.row_popup.is_some());
}

/// A click inside the row popup moves its cursor; the second click at the
/// same line drills into the full cell popup (the mouse `Enter`).
#[test]
pub(crate) fn row_popup_click_selects_then_double_click_drills() {
    let (mut app, tx) = drawn_preview_app(100, 30);
    open_row_popup(&mut app);
    draw(&mut app, 100, 30);
    assert!(app.rects.row_popup_visible);
    let inner = app.rects.row_popup_inner;
    let (x, y) = (inner.x + 2, inner.y + 2);
    assert_eq!(app.row_popup_hit.get(2), Some(&2));
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(app.row_popup.as_ref().unwrap().cursor, 2);
    assert!(app.cell_popup.is_none(), "one click only moves the cursor");
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(
        app.cell_popup.is_some(),
        "a double click drills into the cell"
    );
    // A press on the cell popup closes it (like Enter) and reveals the row.
    draw(&mut app, 100, 30);
    let cb = app.rects.cell_popup;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        cb.x + 1,
        cb.y + 1,
    );
    assert!(app.cell_popup.is_none());
    assert!(app.row_popup.is_some());
}

/// The confirm layer's two buttons act as the Enter / Esc branches.
#[test]
pub(crate) fn confirm_layer_buttons_click_through() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.confirm = Some(Confirm {
        sql: "DELETE FROM users".into(),
        reasons: vec!["DELETE 没有 WHERE 子句，会作用于整张表".into()],
        refresh: false,
        clear_batch: false,
        redis: None,
        mongo: None,
        conn: None,
    });
    draw(&mut app, 100, 30);
    let cancel = app.rects.confirm_cancel;
    assert!(cancel.width > 0, "the buttons were laid out");
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        cancel.x + 1,
        cancel.y,
    );
    assert!(
        app.confirm.is_none(),
        "the cancel button takes the Esc branch"
    );
    assert!(app.history.is_empty());
    // Now the execute button: it takes the Enter branch (the statement is
    // pushed to history before it runs).
    app.confirm = Some(Confirm {
        sql: "DELETE FROM users".into(),
        reasons: vec!["DELETE 没有 WHERE 子句，会作用于整张表".into()],
        refresh: false,
        clear_batch: false,
        redis: None,
        mongo: None,
        conn: None,
    });
    draw(&mut app, 100, 30);
    let ok = app.rects.confirm_ok;
    assert!(ok.width > 0);
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ok.x + 1,
        ok.y,
    );
    assert!(app.confirm.is_none());
    assert_eq!(app.history.len(), 1, "the Enter branch ran");
    // A press on the layer's body (neither button) is ignored.
    app.confirm = Some(Confirm {
        sql: "DELETE FROM users".into(),
        reasons: vec!["DELETE 没有 WHERE 子句，会作用于整张表".into()],
        refresh: false,
        clear_batch: false,
        redis: None,
        mongo: None,
        conn: None,
    });
    draw(&mut app, 100, 30);
    let ok = app.rects.confirm_ok;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ok.x,
        ok.y + 3,
    );
    assert!(
        app.confirm.is_some(),
        "a stray tap does not dismiss the layer"
    );
}

/// The error box: a click widens a compact box (Enter), pages a long one, and
/// closes once the end is on screen.
#[test]
pub(crate) fn error_box_click_widens_pages_then_closes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    open_error_popup(&mut app, "boom: relation does not exist");
    draw(&mut app, 100, 30);
    let ebox = app.rects.error_box;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ebox.x + 2,
        ebox.y + 1,
    );
    assert!(
        app.error_popup.as_ref().unwrap().expanded,
        "a click widens it"
    );
    draw(&mut app, 100, 30);
    assert_eq!(app.rects.error_max_scroll, 0, "the text fits");
    let ebox = app.rects.error_box;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ebox.x + 2,
        ebox.y + 1,
    );
    assert!(app.error_popup.is_none(), "a click at the end closes it");
    // A long error pages instead of closing.
    let long = (0..60)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    open_error_popup(&mut app, &long);
    draw(&mut app, 100, 30);
    let ebox = app.rects.error_box;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ebox.x + 2,
        ebox.y + 1,
    );
    draw(&mut app, 100, 30);
    assert!(app.rects.error_max_scroll > 0);
    let ebox = app.rects.error_box;
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        ebox.x + 2,
        ebox.y + 1,
    );
    assert!(
        app.error_popup.is_some(),
        "a long error pages, it does not close"
    );
    assert!(app.error_popup.as_ref().unwrap().scroll > 0);
}

/// The tree expander is its own hit target: clicking it folds the node even
/// when the tree cursor sits somewhere else.
#[test]
pub(crate) fn tree_expander_click_folds_without_selecting_first() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    app.picker_open = false;
    draw(&mut app, 110, 32);
    let r = app.rects.sidebar;
    assert!(r.width > 8, "the sidebar is expanded");
    // Park the cursor on a table row, far from the database node we click.
    app.side_sel = 2;
    // The filter row is first, so tree row 1 (the current database) sits on
    // the third content line; its expander is indented one level.
    let (x, y) = (r.x + 1 + 2, r.y + 1 + 1 + 1);
    assert!(side_row_open(&app, &app.side_rows[1].clone()));
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(
        app.tree_db_closed
            .contains(&db_node_key("id-mysql", "shop")),
        "the database folded"
    );
    assert_eq!(app.side_sel, 1, "the cursor followed the clicked node");
    assert!(
        !app.side_rows
            .iter()
            .any(|r| matches!(r, SideRow::Table { .. })),
        "the tables are hidden"
    );
}

/// Clicking the editor focuses it and places the caret where the click
/// landed — including through a horizontal scroll, and clamped to the end
/// when the click is past the text.
#[test]
pub(crate) fn editor_click_places_the_caret_through_the_viewport() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.set_editor_text("aaa\nbbb\nccc");
    draw(&mut app, 100, 30);
    let ed = app.rects.editor;
    let inner = Rect {
        x: ed.x + 1,
        y: ed.y + 1,
        width: ed.width - 2,
        height: ed.height - 2,
    };
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 1,
        inner.y + 1,
    );
    assert!(app.focus == Focus::Editor);
    assert_eq!(app.editor.cursor(), (1, 1), "the caret followed the click");
    // A click past the last line jumps to the end of the text.
    app.set_editor_text("abcdef");
    draw(&mut app, 100, 30);
    let ed = app.rects.editor;
    let inner = Rect {
        x: ed.x + 1,
        y: ed.y + 1,
        width: ed.width - 2,
        height: ed.height - 2,
    };
    press(
        &mut app,
        &tx,
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 2,
        inner.y + inner.height - 1,
    );
    assert_eq!(app.editor.cursor(), (0, 6));
}

/// The click-to-caret mapping honours the editor's horizontal scroll: with a
/// line wider than the pane, the first visible column is not column 0.
#[test]
pub(crate) fn editor_click_maps_through_a_horizontal_scroll() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    let long: String = (0..120)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    app.set_editor_text(&long);
    draw(&mut app, 100, 30);
    assert!(app.editor_vp.col > 0, "the editor scrolled horizontally");
    let ed = app.rects.editor;
    let (x, y) = (ed.x + 1, ed.y + 1);
    press(&mut app, &tx, MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(app.editor.cursor(), (0, app.editor_vp.col as usize));
}

#[test]
pub(crate) fn drag_pan_mode_parses_its_env_values() {
    assert_eq!(DragPan::parse(""), DragPan::Button);
    assert_eq!(DragPan::parse("1"), DragPan::Button);
    assert_eq!(DragPan::parse(" Button "), DragPan::Button);
    assert_eq!(DragPan::parse("OFF"), DragPan::Off);
    assert_eq!(DragPan::parse("none"), DragPan::Off);
    assert_eq!(DragPan::parse("any"), DragPan::Any);
    assert_eq!(DragPan::parse("Moved"), DragPan::Any);
}

#[test]
pub(crate) fn pan_window_moves_the_window_and_keeps_the_cursor_inside() {
    // window 4..6 (2 columns wide), cursor parked on the right edge
    let (off, cursor) = pan_window(10, 0, 4, 5, 2, 1);
    assert_eq!((off, cursor), (5, 5));
    // panning left past the cursor pulls it back to the new window's right edge
    let (off, cursor) = pan_window(10, 0, 5, 5, 2, -3);
    assert_eq!((off, cursor), (2, 3));
    // the window never scrolls into the pinned prefix
    let (off, _) = pan_window(10, 1, 1, 1, 2, -5);
    assert_eq!(off, 1);
    // a cursor inside the pinned prefix stays there
    let (off, cursor) = pan_window(10, 1, 1, 0, 2, 3);
    assert_eq!((off, cursor), (4, 0));
    // the right edge stops at the last column
    let (off, cursor) = pan_window(10, 0, 7, 8, 2, 9);
    assert_eq!((off, cursor), (9, 9));
    // an empty grid is a no-op
    assert_eq!(pan_window(0, 0, 0, 0, 2, 1), (0, 0));
}

/// The bug that made a swipe look dead: the cursor was clamped with a stale
/// visible-column count, so `window_for_cursor` on the next render pulled the
/// window straight back to where it started.
#[test]
pub(crate) fn pan_places_the_cursor_where_window_for_cursor_keeps_the_window() {
    let grid = ten_col_grid();
    let (avail, max_cell, frozen) = (21, 44, 1);
    // walk the window right one column at a time with the cursor parked on the
    // right edge (exactly what a swipe leaves behind)
    let mut off = 1;
    let mut cursor = 1;
    for _ in 0..8 {
        let target = (off + 1).min(grid.columns.len() - 1);
        let vis = visible_cols(&grid, target, avail, max_cell).max(1);
        let (next_off, next_cursor) = pan_window(grid.columns.len(), frozen, off, cursor, vis, 1);
        off = next_off;
        cursor = next_cursor;
        let vis = visible_cols(&grid, off, avail, max_cell).max(1);
        assert_eq!(
            window_for_cursor(&grid, cursor, off, avail, max_cell, frozen),
            (off, vis),
            "render must keep the window the pan chose (off={off}, cursor={cursor})"
        );
    }
    assert_eq!(off, 9);
}

#[test]
pub(crate) fn wire_hint_shows_the_exact_encoding() {
    let left = MouseEvent {
        kind: MouseEventKind::ScrollLeft,
        column: 11,
        row: 4,
        modifiers: KeyModifiers::SHIFT,
    };
    let hint = mouse_wire_hint(&left);
    assert!(hint.contains("<70;12;5M"), "{hint}"); // 66 + 4 for shift
    assert!(hint.contains("X10"), "{hint}");
    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0);
    assert!(mouse_wire_hint(&drag).contains("<32;1;1M"));
    let up = mouse(MouseEventKind::Up(MouseButton::Left), 2, 3);
    assert!(
        mouse_wire_hint(&up).contains("<3;3;4m"),
        "SGR release uses a lowercase m"
    );
    assert!(describe_mouse(&drag).contains("Drag(Left)"));
}

#[test]
pub(crate) fn values_keep_null_and_empty_distinct() {
    assert!(value_to_val(&serde_json::Value::Null).is_null());
    assert_eq!(value_to_val(&serde_json::json!("")).text(), "");
    assert_eq!(value_to_val(&serde_json::json!(42)).text(), "42");
}

#[test]
pub(crate) fn null_and_empty_string_render_distinctly() {
    let (null_text, null) = value_display(&Val::Null);
    assert_eq!(null_text, "NULL");
    assert_eq!(null.fg, Some(Color::DarkGray));
    assert_eq!(
        null.add_modifier.contains(Modifier::ITALIC),
        italic_supported(),
        "NULL is italic only when the terminal supports it"
    );

    let (empty_text, empty) = value_display(&Val::Text(String::new()));
    assert_eq!(empty_text, "''");
    assert_eq!(empty.fg, Some(Color::DarkGray));
    assert!(!empty.add_modifier.contains(Modifier::ITALIC));

    // A literal string "NULL" stays plain — that is what tells it apart
    // from the real thing, which is grey (and italic when possible).
    let (literal_text, literal) = value_display(&Val::Text("NULL".into()));
    assert_eq!(literal_text, "NULL");
    assert_eq!(literal.fg, None);
    assert_ne!(literal, null);
}

/// Render one row of grid cells exactly like the results pane does, into a
/// headless buffer, so the NULL / `''` contract can be checked without a
/// real terminal.
pub(crate) fn capture_grid_cells(
    rows: &[Vec<Val>],
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    let ncols = rows.first().map(Vec::len).unwrap_or(0);
    let rendered: Vec<Row> = rows
        .iter()
        .map(|row| {
            Row::new(
                row.iter()
                    .map(|v| cell_widget_hl(v, 8, false, None, None, false))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let table = Table::new(
        rendered,
        (0..ncols)
            .map(|_| Constraint::Length(8))
            .collect::<Vec<_>>(),
    )
    .column_spacing(1);
    term.draw(|f| f.render_widget(table, f.area())).unwrap();
    term.backend().buffer().clone()
}

#[test]
pub(crate) fn null_empty_and_literal_null_stay_apart_at_both_capture_sizes() {
    // Three columns, each a different case: a NULL-only column, an
    // empty-string-only column, and a mixed column that holds both a real
    // NULL and the literal text "NULL".
    let rows = vec![
        vec![
            Val::Null,
            Val::Text(String::new()),
            Val::Text("NULL".into()),
        ],
        vec![Val::Null, Val::Text(String::new()), Val::Null],
        vec![Val::Null, Val::Text(String::new()), Val::Text("x".into())],
    ];
    // The two capture sizes the R12 acceptance pass uses: a phone-ish 42×22
    // and a desktop 110×30.
    for (w, h) in [(42u16, 22u16), (110, 30)] {
        let buf = capture_grid_cells(&rows, w, h);
        let text_at = |x: u16, y: u16, n: u16| -> String {
            (0..n)
                .map(|i| buf.cell((x + i, y)).unwrap().symbol())
                .collect()
        };
        // 8-wide cells with a 1-column gutter: x = 0, 9, 18.
        for (r, row) in rows.iter().enumerate() {
            let y = r as u16;
            let null_cell = buf.cell((0u16, y)).unwrap();
            assert_eq!(text_at(0, y, 4), "NULL", "{w}x{h} r{r}");
            assert_eq!(null_cell.fg, Color::DarkGray, "{w}x{h} r{r}");
            assert_eq!(
                null_cell.modifier.contains(Modifier::ITALIC),
                italic_supported(),
                "{w}x{h} r{r}"
            );

            let empty_cell = buf.cell((9u16, y)).unwrap();
            assert_eq!(text_at(9, y, 2), "''", "{w}x{h} r{r}");
            assert_eq!(empty_cell.fg, Color::DarkGray, "{w}x{h} r{r}");
            assert!(
                !empty_cell.modifier.contains(Modifier::ITALIC),
                "{w}x{h} r{r}"
            );

            // The mixed column: the literal "NULL" is plain, the real
            // NULL is grey (and italic when supported).
            let expected = match &row[2] {
                Val::Text(s) => s.clone(),
                Val::Null => "NULL".to_string(),
            };
            let mixed = buf.cell((18u16, y)).unwrap();
            assert_eq!(
                text_at(18, y, expected.len() as u16),
                expected,
                "{w}x{h} r{r}"
            );
            if row[2] == Val::Null {
                assert_eq!(mixed.fg, Color::DarkGray, "{w}x{h} r{r}");
                assert_eq!(
                    mixed.modifier.contains(Modifier::ITALIC),
                    italic_supported(),
                    "{w}x{h} r{r}"
                );
            } else {
                assert_ne!(mixed.fg, Color::DarkGray, "{w}x{h} r{r}");
            }
        }
    }
}

#[test]
pub(crate) fn csv_export_keeps_null_and_empty_as_empty_fields() {
    let grid = Grid {
        columns: vec!["a".into(), "b".into(), "c".into()],
        rows: vec![vec![
            Val::Null,
            Val::Text(String::new()),
            Val::Text("NULL".into()),
        ]],
        note: String::new(),
    };
    // NULL and '' are both empty fields (RFC 4180), a literal "NULL" is not.
    assert_eq!(grid_to_csv(&grid), "a,b,c\n,,NULL\n");
}

pub(crate) fn ten_col_grid() -> Grid {
    let columns: Vec<String> = (0..10).map(|i| format!("c{i}")).collect();
    Grid {
        columns,
        rows: vec![vec![Val::Text("1234567890".into()); 10]],
        note: String::new(),
    }
}

#[test]
pub(crate) fn abs_row_counts_across_pages() {
    // page 2 (0-based), 50/page, 5th row of the page → row 106
    assert_eq!(abs_row(2, 50, 5), 106);
    assert_eq!(abs_row(0, 50, 0), 1);
}

#[test]
pub(crate) fn page_count_rounds_up_and_never_zero() {
    assert_eq!(page_count(400, 50), 8);
    assert_eq!(page_count(401, 50), 9);
    assert_eq!(page_count(0, 50), 1);
}

#[test]
pub(crate) fn frozen_first_column_needs_room() {
    let grid = ten_col_grid();
    // 10-wide columns, gutter 2: 2+1+10+1+6 = 20 fits in 40, not in 15.
    assert_eq!(effective_frozen(true, &grid, 2, 40, 44), 1);
    assert_eq!(effective_frozen(true, &grid, 2, 15, 44), 0);
    assert_eq!(effective_frozen(false, &grid, 2, 40, 44), 0);
    // a two-column grid has nothing worth pinning
    let narrow = Grid {
        columns: vec!["a".into(), "b".into()],
        rows: vec![vec![Val::Text("1".into()), Val::Text("2".into())]],
        note: String::new(),
    };
    assert_eq!(effective_frozen(true, &narrow, 2, 40, 44), 0);
}

#[test]
pub(crate) fn window_follows_cursor_to_the_right() {
    let grid = ten_col_grid();
    // 10-wide columns, 2 fit in 21; cursor on col 5 scrolls the window to 4..6
    assert_eq!(window_for_cursor(&grid, 0, 0, 21, 44, 0), (0, 2));
    assert_eq!(window_for_cursor(&grid, 5, 0, 21, 44, 0), (4, 2));
    // cursor to the left of the window pulls it back
    assert_eq!(window_for_cursor(&grid, 1, 4, 21, 44, 0), (1, 2));
}

#[test]
pub(crate) fn window_never_scrolls_into_frozen_prefix() {
    let grid = ten_col_grid();
    // frozen=1: the scrollable window starts at index 1 even with cursor at 0
    assert_eq!(window_for_cursor(&grid, 0, 0, 21, 44, 1), (1, 2));
    // cursor past the frozen column scrolls the window while staying >= 1
    let (off, vis) = window_for_cursor(&grid, 7, 1, 21, 44, 1);
    assert!(off >= 1);
    assert!(off <= 7 && 7 < off + vis);
}

#[test]
pub(crate) fn window_clamps_when_columns_are_narrow() {
    let grid = Grid {
        columns: vec!["a".into(), "b".into(), "c".into()],
        rows: vec![vec![Val::Text("x".into()); 3]],
        note: String::new(),
    };
    // avail 0 still shows one column
    assert_eq!(window_for_cursor(&grid, 0, 0, 0, 44, 0), (0, 1));
}

#[test]
pub(crate) fn sql_literal_escapes_quotes_and_backslashes() {
    assert_eq!(sql_literal("O'Brien"), "'O''Brien'");
    assert_eq!(sql_literal("a\\b"), "'a\\\\b'");
    assert_eq!(sql_literal("plain"), "'plain'");
}

#[test]
pub(crate) fn numeric_type_detection_ignores_length_params() {
    assert!(is_numeric_type("int"));
    assert!(is_numeric_type("BIGINT(20) UNSIGNED"));
    assert!(is_numeric_type("decimal(10, 2)"));
    assert!(is_numeric_type("double precision"));
    assert!(!is_numeric_type("varchar(50)"));
    assert!(!is_numeric_type("text"));
    assert!(!is_numeric_type("date"));
    assert!(!is_numeric_type("json"));
}

#[test]
pub(crate) fn value_literal_types_numbers_but_quotes_text() {
    // NULL and empty string stay distinct
    assert_eq!(val_literal(&Val::Null, Some("int")), "NULL");
    assert_eq!(val_literal(&Val::Text(String::new()), Some("int")), "''");
    // numeric column + numeric value → unquoted
    assert_eq!(val_literal(&Val::Text("42".into()), Some("int")), "42");
    // same value in a text column → quoted
    assert_eq!(
        val_literal(&Val::Text("42".into()), Some("varchar(10)")),
        "'42'"
    );
    // a non-numeric value in a numeric column must still be quoted
    assert_eq!(val_literal(&Val::Text("n/a".into()), Some("int")), "'n/a'");
    assert_eq!(val_literal(&Val::Text("true".into()), Some("bool")), "TRUE");
}

#[test]
pub(crate) fn count_cache_key_depends_on_filter_not_sort() {
    assert_eq!(
        count_cache_key("db", "", "t", ""),
        count_cache_key("db", "", "t", "")
    );
    assert_ne!(
        count_cache_key("db", "", "t", "a = 1"),
        count_cache_key("db", "", "t", "a = 2")
    );
    assert_ne!(
        count_cache_key("db1", "", "t", ""),
        count_cache_key("db2", "", "t", "")
    );
    // The schema is part of the key: `public.orders` ≠ `inv.orders`.
    assert_ne!(
        count_cache_key("db", "public", "orders", ""),
        count_cache_key("db", "inv", "orders", "")
    );
}

// ── R34: keyset pagination + bounded row count ──

pub(crate) fn pk_col(name: &str, ty: &str) -> ColumnInfo {
    ColumnInfo {
        is_primary_key: true,
        ..col_info(name, ty)
    }
}

pub(crate) fn orders_meta(pks: &[(&str, &str)], others: &[(&str, &str)]) -> TableMeta {
    let mut columns: Vec<ColumnInfo> = pks.iter().map(|(n, t)| pk_col(n, t)).collect();
    columns.extend(others.iter().map(|(n, t)| col_info(n, t)));
    TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns,
        indexes: Vec::new(),
    }
}

pub(crate) fn orders_page(order_by: Option<&str>) -> PageState {
    PageState {
        table: "orders".into(),
        schema: String::new(),
        table_type: Some("BASE TABLE".into()),
        page: 0,
        page_size: 50,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: order_by.map(str::to_string),
        keyset: None,
    }
}

#[test]
pub(crate) fn keyset_predicate_builds_single_and_composite_seeks() {
    let n = |i: i64| serde_json::json!(i);
    let s = |v: &str| serde_json::json!(v);
    // Single key, both dialects and both directions.
    let pk = vec!["id".to_string()];
    assert_eq!(
        table_data_keyset_predicate(Some(DatabaseType::Mysql), &pk, &[n(10)], ">").unwrap(),
        "`id` > 10"
    );
    assert_eq!(
        table_data_keyset_predicate(Some(DatabaseType::Postgres), &pk, &[n(10)], "<").unwrap(),
        "\"id\" < 10"
    );
    // String keys are quoted and escaped.
    assert_eq!(
        table_data_keyset_predicate(Some(DatabaseType::Mysql), &pk, &[s("O'Brien")], ">").unwrap(),
        "`id` > 'O''Brien'"
    );
    // Composite keys use a row-value comparison.
    let cpk = vec!["a".to_string(), "b".to_string()];
    assert_eq!(
        table_data_keyset_predicate(Some(DatabaseType::Postgres), &cpk, &[n(1), s("x")], ">")
            .unwrap(),
        "(\"a\", \"b\") > (1, 'x')"
    );
    // Booleans render as SQL literals.
    assert_eq!(
        table_data_keyset_predicate(
            Some(DatabaseType::Mysql),
            &pk,
            &[serde_json::Value::Bool(true)],
            ">"
        )
        .unwrap(),
        "`id` > TRUE"
    );
    // An incomplete tuple or a NULL key aborts the seek.
    assert!(table_data_keyset_predicate(Some(DatabaseType::Mysql), &cpk, &[n(1)], ">").is_none());
    assert!(table_data_keyset_predicate(
        Some(DatabaseType::Mysql),
        &pk,
        &[serde_json::Value::Null],
        ">"
    )
    .is_none());
    assert!(table_data_keyset_predicate(Some(DatabaseType::Mysql), &[], &[], ">").is_none());
}

#[test]
pub(crate) fn table_page_query_uses_keyset_and_reverses_a_backward_read() {
    let cfg = mysql_cfg();
    let pk = vec!["id".to_string()];
    let after = PageSeek::After(vec![serde_json::json!(50)]);
    let before = PageSeek::Before(vec![serde_json::json!(51)]);

    // First page under a keyset plan: primary-key order, no OFFSET at all.
    let (sql, rev) = build_table_page_query(
        &cfg,
        None,
        "orders",
        None,
        0,
        50,
        "",
        None,
        &pk,
        true,
        &PageSeek::Offset,
    );
    assert!(!rev);
    assert!(sql.contains("ORDER BY `id` ASC"), "{sql}");
    assert!(sql.contains("LIMIT 51"), "{sql}");
    assert!(!sql.contains("OFFSET"), "{sql}");

    // Next page: seek past the last key, still no OFFSET.
    let (sql, rev) = build_table_page_query(
        &cfg, None, "orders", None, 1, 50, "", None, &pk, true, &after,
    );
    assert!(!rev);
    assert!(sql.contains("`id` > 50"), "{sql}");
    assert!(!sql.contains("OFFSET"), "{sql}");

    // Previous page: seek before the first key, order and rows reversed.
    let (sql, rev) = build_table_page_query(
        &cfg, None, "orders", None, 0, 50, "", None, &pk, true, &before,
    );
    assert!(rev);
    assert!(sql.contains("`id` < 51"), "{sql}");
    assert!(sql.contains("ORDER BY `id` DESC"), "{sql}");

    // A descending view flips the comparison.
    let (sql, rev) = build_table_page_query(
        &cfg, None, "orders", None, 1, 50, "", None, &pk, false, &after,
    );
    assert!(!rev);
    assert!(sql.contains("`id` < 50"), "{sql}");
    assert!(sql.contains("ORDER BY `id` DESC"), "{sql}");

    // A user filter is ANDed with the seek predicate.
    let (sql, _) = build_table_page_query(
        &cfg, None, "orders", None, 1, 50, "grp = 1", None, &pk, true, &after,
    );
    assert!(sql.contains("(grp = 1) AND (`id` > 50)"), "{sql}");

    // Without a keyset plan a custom sort keeps the classic OFFSET page.
    let (sql, rev) = build_table_page_query(
        &cfg,
        None,
        "orders",
        None,
        3,
        50,
        "",
        Some("`name` ASC"),
        &[],
        true,
        &PageSeek::Offset,
    );
    assert!(!rev);
    assert!(sql.contains("ORDER BY `name` ASC"), "{sql}");
    assert!(sql.contains("OFFSET 150"), "{sql}");
}

#[test]
pub(crate) fn keyset_plan_only_accepts_the_primary_key_order() {
    let single = orders_meta(&[("id", "int")], &[("name", "varchar(64)")]);
    // No explicit sort → the implicit primary-key order (ascending).
    assert_eq!(
        keyset_plan(Some(&single), &orders_page(None)),
        Some((vec!["id".to_string()], true))
    );
    // Explicit sort on the key, either direction.
    assert_eq!(
        keyset_plan(Some(&single), &orders_page(Some("`id` ASC"))),
        Some((vec!["id".to_string()], true))
    );
    assert_eq!(
        keyset_plan(Some(&single), &orders_page(Some("`id` DESC"))),
        Some((vec!["id".to_string()], false))
    );
    // A custom sort falls back to OFFSET.
    assert_eq!(
        keyset_plan(Some(&single), &orders_page(Some("`name` ASC"))),
        None
    );

    // Composite key: column order must match and the direction must agree.
    let composite = orders_meta(&[("a", "int"), ("b", "int")], &[]);
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(None)),
        Some((vec!["a".to_string(), "b".to_string()], true))
    );
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(Some("`a` ASC, `b` ASC"))),
        Some((vec!["a".to_string(), "b".to_string()], true))
    );
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(Some("`a` DESC, `b` DESC"))),
        Some((vec!["a".to_string(), "b".to_string()], false))
    );
    // Mixed directions, swapped order, or a partial key cannot seek.
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(Some("`a` ASC, `b` DESC"))),
        None
    );
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(Some("`b` ASC, `a` ASC"))),
        None
    );
    assert_eq!(
        keyset_plan(Some(&composite), &orders_page(Some("`a` ASC"))),
        None
    );

    // No primary key, a binary key, or metadata for another table → OFFSET.
    let keyless = orders_meta(&[], &[("x", "int")]);
    assert_eq!(keyset_plan(Some(&keyless), &orders_page(None)), None);
    let binary = orders_meta(&[("k", "blob")], &[]);
    assert_eq!(keyset_plan(Some(&binary), &orders_page(None)), None);
    assert_eq!(keyset_plan(None, &orders_page(None)), None);
    let mut other = orders_page(None);
    other.table = "other".into();
    assert_eq!(keyset_plan(Some(&single), &other), None);
}

#[test]
pub(crate) fn keyset_seek_only_follows_adjacent_pages() {
    let plan = (vec!["id".to_string()], true);
    let cur = KeysetCursor {
        pk: vec!["id".to_string()],
        ascending: true,
        first: vec![serde_json::json!(10)],
        last: vec![serde_json::json!(60)],
    };
    assert_eq!(
        keyset_seek_for(Some(&plan), Some(&cur), 0, 1),
        PageSeek::After(vec![serde_json::json!(60)])
    );
    assert_eq!(
        keyset_seek_for(Some(&plan), Some(&cur), 1, 0),
        PageSeek::Before(vec![serde_json::json!(10)])
    );
    // A jump that is not exactly one page keeps OFFSET.
    assert_eq!(
        keyset_seek_for(Some(&plan), Some(&cur), 0, 5),
        PageSeek::Offset
    );
    assert_eq!(
        keyset_seek_for(Some(&plan), Some(&cur), 5, 0),
        PageSeek::Offset
    );
    // A direction or key mismatch keeps OFFSET.
    let desc = (vec!["id".to_string()], false);
    assert_eq!(
        keyset_seek_for(Some(&desc), Some(&cur), 0, 1),
        PageSeek::Offset
    );
    assert_eq!(keyset_seek_for(None, Some(&cur), 0, 1), PageSeek::Offset);
    assert_eq!(keyset_seek_for(Some(&plan), None, 0, 1), PageSeek::Offset);
}

#[test]
pub(crate) fn keyset_cursor_maps_result_columns_to_the_key() {
    let pk = vec!["id".to_string()];
    let columns = vec!["name".to_string(), "id".to_string()];
    let rows = vec![
        vec![serde_json::json!("a"), serde_json::json!(1)],
        vec![serde_json::json!("b"), serde_json::json!(7)],
    ];
    let c = keyset_cursor(&pk, true, &columns, &rows).unwrap();
    assert_eq!(c.pk, vec!["id".to_string()]);
    assert!(c.ascending);
    assert_eq!(c.first, vec![serde_json::json!(1)]);
    assert_eq!(c.last, vec![serde_json::json!(7)]);
    // Missing key column / NULL key / empty page → no cursor.
    assert!(keyset_cursor(&pk, true, &["name".to_string()], &rows).is_none());
    let with_null = vec![vec![serde_json::json!("a"), serde_json::Value::Null]];
    assert!(keyset_cursor(&pk, true, &columns, &with_null).is_none());
    assert!(keyset_cursor(&pk, true, &columns, &[]).is_none());
}

#[test]
pub(crate) fn bounded_count_sql_uses_an_index_only_scan_capped_at_the_sample() {
    let cfg = mysql_cfg();
    let sql = bounded_count_sql(&cfg, None, "big", "", 500_000).unwrap();
    assert_eq!(
        sql,
        "SELECT COUNT(*) AS row_count FROM (SELECT 1 FROM `big` LIMIT 500001) dbxt_count"
    );
    assert!(!sql.contains(';'), "{sql}");
    // `SELECT 1` keeps the capped scan index-only rather than materialising
    // half a million full rows.
    assert!(sql.contains("SELECT 1 FROM"), "{sql}");
    // A filter is carried into the sampled scan.
    let sql = bounded_count_sql(&cfg, None, "big", "grp = 1", 1_000).unwrap();
    assert!(sql.contains("WHERE (grp = 1)"), "{sql}");
    assert!(sql.contains("LIMIT 1001"), "{sql}");
    // A dialect without a plain LIMIT pager declines the sample so the caller
    // falls back to the exact count.
    let mut oracle = cfg.clone();
    oracle.db_type = DatabaseType::Oracle;
    assert!(bounded_count_sql(&oracle, None, "big", "", 500_000).is_none());
}

#[test]
pub(crate) fn sampled_count_reports_a_lower_bound_past_the_cap() {
    assert_eq!(classify_sample(0, 500_000), (0, false));
    assert_eq!(classify_sample(10, 500_000), (10, false));
    assert_eq!(classify_sample(500_000, 500_000), (500_000, false));
    assert_eq!(classify_sample(500_001, 500_000), (500_000, true));
}

#[test]
pub(crate) fn count_cache_remembers_value_and_lower_bound_per_filter() {
    let mut app = test_app();
    assert!(app.cached_count("db", "public", "big", "").is_none());
    app.remember_count("db", "public", "big", "", 500_000, true);
    assert_eq!(
        app.cached_count("db", "public", "big", ""),
        Some((500_000, true))
    );
    // The filter is part of the key, so a filtered view has its own total.
    assert!(app.cached_count("db", "public", "big", "grp = 1").is_none());
    app.remember_count("db", "public", "big", "grp = 1", 42, false);
    assert_eq!(
        app.cached_count("db", "public", "big", "grp = 1"),
        Some((42, false))
    );
    // ...and the exact value is untouched by the filtered one.
    assert_eq!(
        app.cached_count("db", "public", "big", ""),
        Some((500_000, true))
    );
}

#[test]
pub(crate) fn total_label_marks_a_lower_bound_with_a_greater_than() {
    let mut ps = orders_page(None);
    ps.total = Some(500_000);
    ps.total_lower_bound = true;
    assert!(total_label(&ps).contains(">500000"), "{}", total_label(&ps));
    ps.total_lower_bound = false;
    assert!(
        total_label(&ps).contains("共 500000 行"),
        "{}",
        total_label(&ps)
    );
    ps.total = None;
    assert_eq!(total_label(&ps), "总数未知");
}

#[test]
pub(crate) fn page_state_extra_reports_filter_and_sort() {
    let ps = PageState {
        table: "t".into(),
        schema: String::new(),
        table_type: None,
        page: 0,
        page_size: 50,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: "city = 'Beijing'".into(),
        order_by: Some("`id` DESC".into()),
        keyset: None,
    };
    let extra = page_state_extra(&ps);
    assert!(extra.contains("过滤: city = 'Beijing'"));
    assert!(extra.contains("排序: `id` DESC"));
}

#[test]
pub(crate) fn page_state_extra_is_empty_without_filter_or_sort() {
    let ps = PageState {
        table: "t".into(),
        schema: String::new(),
        table_type: None,
        page: 0,
        page_size: 50,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: None,
        keyset: None,
    };
    assert!(page_state_extra(&ps).is_empty());
}

#[test]
pub(crate) fn double_encoding_is_reversed_for_display() {
    // "保留表" written through a CP1252 connection: the stored string is the
    // mojibake below (U+009D for byte 0x9D).
    let mojibake = "\u{e4}\u{bf}\u{9d}\u{e7}\u{2022}\u{2122}\u{e8}\u{a1}\u{a8}";
    assert_eq!(fix_double_encoding(mojibake), "保留表");
}

#[test]
pub(crate) fn double_encoding_is_reversed_through_multiple_layers() {
    // The same name written through a latin1 connection *twice*: the server
    // stores the CP1252 form of the already-mojibake string. A single pass
    // stops at the single-mojibake form (it still contains `•`/`™`, i.e.
    // chars > U+00FF, so the one-pass heuristic thinks it succeeded) — which
    // is exactly the `ä¿…ç•™è¡¨` that was still reported in the sidebar.
    let single = "\u{e4}\u{bf}\u{9d}\u{e7}\u{2022}\u{2122}\u{e8}\u{a1}\u{a8}";
    let double = "\u{c3}\u{a4}\u{c2}\u{bf}\u{c2}\u{9d}\u{c3}\u{a7}\u{e2}\u{20ac}\u{a2}\u{e2}\u{201e}\u{a2}\u{c3}\u{a8}\u{c2}\u{a1}\u{c2}\u{a8}";
    assert_eq!(reverse_double_encoding_once(double), single);
    assert_eq!(fix_double_encoding(double), "保留表");
    // Peeling a layer off already-decoded text must not change it again.
    assert_eq!(fix_double_encoding(&fix_double_encoding(double)), "保留表");
}

#[test]
pub(crate) fn double_encoding_leaves_clean_names_alone() {
    // Correctly stored CJK contains chars > U+00FF and must pass through.
    assert_eq!(fix_double_encoding("保留表"), "保留表");
    assert_eq!(fix_double_encoding("users"), "users");
    // A Latin-1 name whose bytes are not valid UTF-8 is left untouched.
    assert_eq!(fix_double_encoding("café"), "café");
    // A lone CP1252 punctuation char maps to an invalid UTF-8 byte and is
    // therefore not treated as mojibake.
    assert_eq!(fix_double_encoding("™"), "™");
}

#[test]
pub(crate) fn scrollbar_geometry_covers_full_track() {
    // Everything visible → the whole track is the thumb.
    assert_eq!(scrollbar_geom(10, 0, 10, 40), (0, 40));
    // Half the content visible: thumb is half, at the start / end.
    assert_eq!(scrollbar_geom(10, 0, 5, 40), (0, 20));
    assert_eq!(scrollbar_geom(10, 5, 5, 40), (20, 20));
    // Clamps beyond the ends.
    assert_eq!(scrollbar_geom(10, 99, 5, 40), (20, 20));
    assert_eq!(scrollbar_geom(0, 0, 1, 40), (0, 0));
    assert_eq!(scrollbar_geom(10, 0, 5, 0), (0, 0));
}

#[test]
pub(crate) fn order_by_round_trips_through_parser() {
    let keys = parse_order_by(Some("`id` DESC, `name` ASC"));
    assert_eq!(
        keys,
        vec![("id".to_string(), true), ("name".to_string(), false)]
    );
    assert_eq!(
        parse_order_by(Some("\"a b\" DESC")),
        vec![("a b".into(), true)]
    );
    assert!(parse_order_by(None).is_empty());
}

#[test]
pub(crate) fn identifier_unquoting_strips_dialect_quotes() {
    assert_eq!(unquote_ident("`id`"), "id");
    assert_eq!(unquote_ident("\"name\""), "name");
    assert_eq!(unquote_ident("[col]"), "col");
    assert_eq!(unquote_ident("plain"), "plain");
}

#[test]
pub(crate) fn filter_mentions_matches_whole_identifiers() {
    assert!(filter_mentions("city = 'X' AND id > 3", "city"));
    assert!(filter_mentions("`city` = 'X'", "city"));
    assert!(filter_mentions("\"City\" = 'X'", "city"));
    // Substrings inside a longer identifier must not match.
    assert!(!filter_mentions("user_id = 3", "id"));
    assert!(!filter_mentions("id_card = '3'", "id"));
    assert!(!filter_mentions("", "id"));
}

#[test]
pub(crate) fn new_value_literal_handles_null_empty_and_typing() {
    // A blank box (and an explicit NULL) both mean SQL NULL now.
    assert_eq!(new_value_literal("", Some("varchar(10)")), "NULL");
    assert_eq!(new_value_literal("   ", Some("varchar(10)")), "NULL");
    assert_eq!(new_value_literal("NULL", Some("int")), "NULL");
    assert_eq!(new_value_literal("null", Some("varchar(10)")), "NULL");
    // `''` is the empty string; quoted text is taken verbatim.
    assert_eq!(new_value_literal("''", Some("varchar(10)")), "''");
    assert_eq!(new_value_literal("'text'", Some("varchar(10)")), "'text'");
    assert_eq!(new_value_literal("'O''Brien'", Some("text")), "'O''Brien'");
    // Unquoted values are coerced by column type, as before.
    assert_eq!(new_value_literal("42", Some("int")), "42");
    assert_eq!(new_value_literal("42", Some("varchar(10)")), "'42'");
    assert_eq!(new_value_literal("O'Brien", Some("text")), "'O''Brien'");
}

#[test]
pub(crate) fn edit_prefill_round_trips_through_new_value_literal() {
    let round = |v: Val, t: Option<&str>| new_value_literal(&edit_prefill(&v), t);
    // NULL opens blank and submits back as NULL.
    assert_eq!(edit_prefill(&Val::Null), "");
    assert_eq!(round(Val::Null, Some("varchar(10)")), "NULL");
    // The empty string opens as `''` and stays an empty string.
    assert_eq!(edit_prefill(&Val::Text(String::new())), "''");
    assert_eq!(round(Val::Text(String::new()), Some("varchar(10)")), "''");
    // A literal "NULL" is quoted so an unchanged submit cannot turn it into NULL.
    assert_eq!(edit_prefill(&Val::Text("NULL".into())), "'NULL'");
    assert_eq!(
        round(Val::Text("NULL".into()), Some("varchar(10)")),
        "'NULL'"
    );
    // Ordinary values open verbatim.
    assert_eq!(edit_prefill(&Val::Text("42".into())), "42");
    assert_eq!(round(Val::Text("42".into()), Some("int")), "42");
    assert_eq!(round(Val::Text("42".into()), Some("varchar(10)")), "'42'");
    assert_eq!(
        round(Val::Text("O'Brien".into()), Some("text")),
        "'O''Brien'"
    );
}

#[test]
pub(crate) fn auto_collapse_switch_controls_unfocused_panes() {
    // Off → everything stays expanded, at any width.
    assert!(!resolve_collapse(false, Focus::Preview, PANE_SIDEBAR, None));
    assert!(!resolve_collapse(false, Focus::Editor, PANE_SIDEBAR, None));
    // On → unfocused aux panes collapse at every width.
    assert!(resolve_collapse(true, Focus::Preview, PANE_SIDEBAR, None));
    assert!(resolve_collapse(true, Focus::Sidebar, PANE_EDITOR, None));
    // The focused pane never collapses; the results pane is never auto-collapsed.
    assert!(!resolve_collapse(true, Focus::Sidebar, PANE_SIDEBAR, None));
    assert!(!resolve_collapse(true, Focus::Editor, PANE_EDITOR, None));
    assert!(!resolve_collapse(true, Focus::Sidebar, PANE_RESULTS, None));
    // A manual per-pane override always wins, even against the master switch.
    assert!(resolve_collapse(
        false,
        Focus::Sidebar,
        PANE_SIDEBAR,
        Some(true)
    ));
    assert!(!resolve_collapse(
        true,
        Focus::Preview,
        PANE_SIDEBAR,
        Some(false)
    ));
}

#[test]
pub(crate) fn wrap_sql_lines_keeps_statement_shape() {
    let lines = wrap_sql_lines("UPDATE t\nSET a = 1\nWHERE id = 2;", 40);
    assert_eq!(lines, vec!["UPDATE t", "SET a = 1", "WHERE id = 2;"]);
    // blank lines are dropped so the preview stays compact
    assert_eq!(wrap_sql_lines("A\n\nB", 10), vec!["A", "B"]);
}

// ── SQL formatter (Alt-F) ──

#[test]
pub(crate) fn format_sql_splits_clauses_and_uppercases_keywords() {
    let out = format_sql(
        "select id, name from users where age > 30 and city = 'NY' order by name limit 10",
    );
    assert_eq!(
        out,
        "SELECT id, name\nFROM users\nWHERE age > 30\n  AND city = 'NY'\nORDER BY name\nLIMIT 10"
    );
    // JOIN gets its own line, ON is indented under it.
    let joined = format_sql("select a.x from a left join b on a.id = b.id where a.ok = 1");
    assert_eq!(
        joined,
        "SELECT a.x\nFROM a\nLEFT JOIN b\n  ON a.id = b.id\nWHERE a.ok = 1"
    );
}

#[test]
pub(crate) fn format_sql_protects_literals_and_quoted_identifiers() {
    // A `FROM` / keyword inside a literal or quoted identifier is untouched.
    let out = format_sql("select 'from where select' as `from`, \"where\" from t where x = 'a''b'");
    assert!(out.contains("'from where select'"), "{out}");
    assert!(out.contains("`from`"), "{out}");
    assert!(out.contains("\"where\""), "{out}");
    assert!(out.contains("'a''b'"), "{out}");
    // The literal must not have introduced a clause break.
    assert_eq!(
        out.lines().filter(|l| l.contains("FROM")).count(),
        1,
        "{out}"
    );
}

#[test]
pub(crate) fn format_sql_protects_comments() {
    let out = format_sql("select 1 -- from x where y\nfrom t /* where z */ where a = 1");
    assert_eq!(
        out,
        "SELECT 1 -- from x where y\nFROM t /* where z */\nWHERE a = 1"
    );
}

#[test]
pub(crate) fn format_sql_keeps_function_names() {
    let out = format_sql("select count(*), max(price) from t");
    // `count(` / `max(` are function calls: case is preserved, no break.
    assert_eq!(out, "SELECT count(*), max(price)\nFROM t");
    let mixed = format_sql("SELECT COUNT(*) FROM t");
    assert_eq!(mixed, "SELECT COUNT(*)\nFROM t");
}

#[test]
pub(crate) fn format_sql_uppercases_keywords_before_parens() {
    // `in(` / `values(` are keywords, not function names, so they are still
    // upper-cased even without a separating space.
    assert_eq!(
        format_sql("select 1 where id in(1, 2)"),
        "SELECT 1\nWHERE id IN(1, 2)"
    );
    assert_eq!(
        format_sql("insert into t values(1)"),
        "INSERT INTO t\nVALUES(1)"
    );
    // A keyword used as a function (`cast(`) follows the keyword rule too…
    assert_eq!(
        format_sql("select cast(x as int) from t"),
        "SELECT CAST(x AS int)\nFROM t"
    );
    // …while a plain function name keeps the user's case.
    assert_eq!(
        format_sql("select count(*) from t"),
        "SELECT count(*)\nFROM t"
    );
}

#[test]
pub(crate) fn format_sql_is_idempotent_and_toggle_round_trips() {
    let src = "select a,b from t where x=1 and y=2";
    let once = format_sql(src);
    assert_eq!(format_sql(&once), once, "format must be a fixed point");
    assert!(is_sql_formatted(&once));
    assert!(!is_sql_formatted(src));
    // Compressing the formatted form and formatting again returns to it.
    let flat = compress_sql(&once);
    assert_eq!(flat, "SELECT a, b FROM t WHERE x = 1 AND y = 2");
    assert_eq!(format_sql(&flat), once);
}

#[test]
pub(crate) fn compress_sql_keeps_line_comment_from_eating_the_rest() {
    // compress only collapses whitespace; it does not change keyword case.
    let out = compress_sql("select 1 -- note\nfrom t");
    assert_eq!(out, "select 1 -- note\nfrom t");
}

#[test]
pub(crate) fn format_sql_handles_empty_and_punctuation_only() {
    assert_eq!(format_sql(""), "");
    assert_eq!(format_sql("   \n  "), "");
    assert_eq!(compress_sql(""), "");
    assert!(!is_sql_formatted(""));
}

#[test]
pub(crate) fn editor_format_toggle_and_undo_snapshot() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.set_editor_text("select a from t where x=1");
    toggle_format_editor(&mut app);
    let formatted = app.editor_sql();
    assert_eq!(formatted, "SELECT a\nFROM t\nWHERE x = 1");
    assert!(app.editor_undo.is_some());
    // A second Alt-F compresses the (now canonical) statement to one line.
    toggle_format_editor(&mut app);
    assert_eq!(app.editor_sql(), "SELECT a FROM t WHERE x = 1");
    // Ctrl-U undoes the last reformat in one step.
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.editor_sql(), formatted);
    assert!(app.editor_undo.is_none());
}

/// `%` jumps to the matching bracket only when the cursor is on, or right
/// after, a bracket — anywhere else it is typed literally.
#[test]
pub(crate) fn percent_jumps_between_brackets_and_otherwise_types() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("SELECT f(a, (b)) FROM t");
    // On the inner `(` at col 12 -> the inner `)` at col 14.
    app.editor.move_cursor(CursorMove::Jump(0, 12));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('%'), KeyModifiers::NONE),
    );
    assert_eq!(app.editor.cursor(), (0, 14));
    // On the closing bracket it jumps back (round trip).
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('%'), KeyModifiers::NONE),
    );
    assert_eq!(app.editor.cursor(), (0, 12));
    // Just after a bracket (`b` at 13) it still jumps from the bracket.
    app.editor.move_cursor(CursorMove::Jump(0, 13));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('%'), KeyModifiers::NONE),
    );
    assert_eq!(app.editor.cursor(), (0, 14));
    // Away from any bracket `%` is a normal character (LIKE patterns / modulo).
    app.editor.move_cursor(CursorMove::Jump(0, 17));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('%'), KeyModifiers::NONE),
    );
    assert!(app.editor_sql().contains('%'), "{} ", app.editor_sql());
}

/// Brackets inside string literals / comments are invisible to `%`, so a `)`
/// in a literal never pairs with a `(` in code.
#[test]
pub(crate) fn percent_ignores_brackets_in_literals_and_comments() {
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("f(')')");
    app.editor.move_cursor(CursorMove::Jump(0, 1));
    assert!(jump_matching_bracket(&mut app));
    assert_eq!(app.editor.cursor(), (0, 5));
    // An unbalanced bracket reports failure but is still consumed.
    app.set_editor_text("(a");
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    assert!(jump_matching_bracket(&mut app));
    assert!(app.status.contains("配对"));
    // No bracket under or beside the cursor: the key is not consumed.
    app.set_editor_text("abc");
    app.editor.move_cursor(CursorMove::Jump(0, 1));
    assert!(!jump_matching_bracket(&mut app));
}

#[test]
pub(crate) fn text_offset_and_cursor_round_trip() {
    let text = "SELECT a\nFROM t\nWHERE x = 1";
    assert_eq!(text_offset(text, 0, 0), Some(0));
    assert_eq!(text_offset(text, 0, 6), Some(6));
    assert_eq!(text_offset(text, 1, 0), Some(9));
    assert_eq!(text_offset(text, 2, 4), Some(20));
    assert_eq!(text_offset(text, 9, 0), None);
    for off in 0..text.chars().count() {
        let (r, c) = offset_to_cursor(text, off);
        assert_eq!(text_offset(text, r, c), Some(off));
    }
}

#[test]
pub(crate) fn matching_bracket_pairs_nesting_and_kinds() {
    assert_eq!(matching_bracket("SELECT (a + b)", 7), Some(13));
    assert_eq!(matching_bracket("SELECT (a + b)", 13), Some(7));
    assert_eq!(matching_bracket("f(a, (b))", 1), Some(8));
    assert_eq!(matching_bracket("f(a, (b))", 5), Some(7));
    assert_eq!(matching_bracket("a[b{c}d]", 1), Some(7));
    assert_eq!(matching_bracket("(a", 0), None);
    assert_eq!(matching_bracket("a)", 1), None);
}

/// R53: the passive bracket highlight resolves the caret's bracket (or the
/// one just before it, the `%` rule) inside a ±span-line window, reusing the
/// same lexer as `%` so brackets inside strings / comments never pair.
#[test]
pub(crate) fn bracket_pair_near_is_lexer_aware_and_windowed() {
    let lines = |s: &str| s.split('\n').map(str::to_string).collect::<Vec<_>>();

    let sql = lines("SELECT f(a, (b)) FROM t");
    assert_eq!(
        bracket_pair_near(&sql, 0, 12, BRACKET_SCAN_LINES),
        Some(((0, 12), (0, 14)))
    );
    // The closing bracket pairs back the other way.
    assert_eq!(
        bracket_pair_near(&sql, 0, 14, BRACKET_SCAN_LINES),
        Some(((0, 14), (0, 12)))
    );
    // Just after the bracket still highlights it (same rule as `%`).
    assert_eq!(
        bracket_pair_near(&sql, 0, 13, BRACKET_SCAN_LINES),
        Some(((0, 12), (0, 14)))
    );
    // Away from any bracket there is nothing to highlight.
    assert_eq!(bracket_pair_near(&sql, 0, 17, BRACKET_SCAN_LINES), None);

    // A `)` inside a string literal is invisible to the lexer, so the code
    // `(` pairs with the code `)`.
    let lit = lines("f(')')");
    assert_eq!(
        bracket_pair_near(&lit, 0, 1, BRACKET_SCAN_LINES),
        Some(((0, 1), (0, 5)))
    );
    assert_eq!(
        bracket_pair_near(&lit, 0, 3, BRACKET_SCAN_LINES),
        None,
        "a bracket inside a literal never highlights"
    );
    // ...and neither is a bracket inside a comment.
    assert_eq!(
        bracket_pair_near(&lines("-- (x)"), 0, 3, BRACKET_SCAN_LINES),
        None
    );
    assert_eq!(
        bracket_pair_near(&lines("/* [a] */"), 0, 3, BRACKET_SCAN_LINES),
        None
    );

    // Unbalanced brackets and stale cursors report `None` instead of panicking.
    assert_eq!(
        bracket_pair_near(&lines("(a"), 0, 0, BRACKET_SCAN_LINES),
        None
    );
    assert_eq!(
        bracket_pair_near(&lines("abc"), 9, 0, BRACKET_SCAN_LINES),
        None
    );
    assert_eq!(
        bracket_pair_near(&lines("abc"), 0, 5, BRACKET_SCAN_LINES),
        None
    );

    // The window is real: a pair three rows away is out of reach at ±1 line,
    // in reach at ±3, and the session window resolves it too.
    let wide = lines("f(\n1,\n2\n)");
    assert_eq!(bracket_pair_near(&wide, 0, 1, 1), None);
    assert_eq!(bracket_pair_near(&wide, 0, 1, 3), Some(((0, 1), (3, 0))));
    assert_eq!(
        bracket_pair_near(&wide, 0, 1, BRACKET_SCAN_LINES),
        Some(((0, 1), (3, 0)))
    );
    // A window that spans >200 lines stays bounded: the default constant is
    // the ceiling, and a 400-line pair is simply not highlighted.
    let mut deep: Vec<String> = vec!["f(".to_string()];
    deep.extend((0..400).map(|i| format!("{i},")));
    deep.push(")".to_string());
    assert_eq!(bracket_pair_near(&deep, 0, 1, BRACKET_SCAN_LINES), None);
    // A window wider than the byte cap is skipped as well (a single giant
    // line has no useful pairing to show).
    let huge = lines(&format!("f({})", "x".repeat(BRACKET_SCAN_BYTES)));
    assert_eq!(bracket_pair_near(&huge, 0, 1, BRACKET_SCAN_LINES), None);
}

/// R61: the find hit list is a case-insensitive substring scan per line, with
/// char columns (so a hit maps straight onto the caret), overlapping matches
/// included. Pure — no backend needed.
#[test]
pub(crate) fn editor_find_hits_counts_and_ignores_case() {
    let lines = |s: &str| s.split('\n').map(str::to_string).collect::<Vec<_>>();
    let two = lines("SELECT a FROM t;\nselect A from T;");
    // `select` matches once per line, whatever the case.
    assert_eq!(
        editor_find_hits(&two, "select"),
        vec![
            FindHit {
                row: 0,
                col: 0,
                len: 6
            },
            FindHit {
                row: 1,
                col: 0,
                len: 6
            },
        ]
    );
    // A one-char needle counts every occurrence in both cases.
    assert_eq!(editor_find_hits(&two, "a").len(), 2);
    assert_eq!(editor_find_hits(&two, "t").len(), 4);
    // Overlapping matches are all reported.
    assert_eq!(editor_find_hits(&lines("aaa"), "aa").len(), 2);
    // A match never crosses a line boundary, and an empty needle is inert.
    assert!(editor_find_hits(&lines("fo\no"), "foo").is_empty());
    assert!(editor_find_hits(&two, "").is_empty());
    // Char columns, not bytes: a CJK prefix does not skew the column.
    assert_eq!(
        editor_find_hits(&lines("中中x"), "x"),
        vec![FindHit {
            row: 0,
            col: 2,
            len: 1
        }]
    );
    // Non-ASCII case folding: `Ä` matches `ä`.
    assert_eq!(editor_find_hits(&lines("ÄBC"), "äb").len(), 1);
}

/// R61: the find cursor walks the hits forward and back with wrap-around,
/// anchoring on the caret when it is not itself a hit.
#[test]
pub(crate) fn editor_find_cycles_hits_and_wraps() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    app.set_editor_text("foo bar foo\nbaz foo");
    app.editor_find_needle = "foo".into();
    app.editor_find_snapshot = app.editor.lines().to_vec();

    // The caret sits on the first hit, so a forward step goes to the second.
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    assert!(editor_find_step(&mut app, 1));
    assert_eq!(app.editor.cursor(), (0, 8));
    assert!(editor_find_step(&mut app, 1));
    assert_eq!(app.editor.cursor(), (1, 4));
    // ...and wraps to the first.
    assert!(editor_find_step(&mut app, 1));
    assert_eq!(app.editor.cursor(), (0, 0));
    // Backward wraps to the last.
    assert!(editor_find_step(&mut app, -1));
    assert_eq!(app.editor.cursor(), (1, 4));

    // Off a hit, forward finds the next and backward the previous.
    app.editor.move_cursor(CursorMove::Jump(0, 1));
    assert!(editor_find_step(&mut app, 1));
    assert_eq!(app.editor.cursor(), (0, 8));
    app.editor.move_cursor(CursorMove::Jump(0, 1));
    assert!(editor_find_step(&mut app, -1));
    assert_eq!(app.editor.cursor(), (0, 0), "the nearest earlier hit");

    // A needle with no hit reports it and moves nowhere.
    app.editor_find_needle = "zzz".into();
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    assert!(!editor_find_step(&mut app, 1));
    assert_eq!(app.editor.cursor(), (0, 0));
    assert!(app.status.contains("无命中"));
}

/// R61: `Ctrl-F` opens the bottom-bar input, `Enter` jumps to the next hit,
/// `Esc` closes the input but keeps the needle + highlight, and the next edit
/// drops the highlight. All client-side.
#[test]
pub(crate) fn editor_find_esc_keeps_highlight_until_the_next_edit() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    app.set_editor_text("alpha beta alpha");
    app.editor.move_cursor(CursorMove::Jump(0, 0));

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
    );
    assert!(app.editor_find.is_some(), "Ctrl-F opens the find input");
    assert_eq!(footer_ctx(&app).view, FooterView::EditorFind);
    for c in "alpha".chars() {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.editor_find_needle, "alpha");
    assert_eq!(editor_find_hits(app.editor.lines(), "alpha").len(), 2);
    assert!(app.status.contains("/2"), "the live count is shown");

    // Enter steps to the next hit and the count tracks it.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(app.editor.cursor(), (0, 11));
    assert!(
        app.status.contains("2/2"),
        "status shows 2/2: {}",
        app.status
    );

    // Esc closes the input but the needle (and highlight) stay.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.editor_find.is_none());
    assert_eq!(app.editor_find_needle, "alpha");

    // The matches are painted: the current one is the accent colour.
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    let cell = |dx: u16| buf.cell((ed.x + 1 + dx, ed.y + 1)).unwrap();
    assert_eq!(cell(0).bg, Color::Yellow, "a non-current hit is marked");
    assert_eq!(
        cell(11).bg,
        Color::LightGreen,
        "the current hit gets the accent"
    );
    assert_ne!(cell(5).bg, Color::Yellow, "a non-match stays plain");

    // F3 keeps cycling after Esc (the highlight is still alive).
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
    );
    assert_eq!(app.editor.cursor(), (0, 0));

    // The next edit clears the whole find state.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
    );
    assert!(
        app.editor_find_needle.is_empty(),
        "an edit drops the needle"
    );
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    assert_ne!(buf.cell((ed.x + 1, ed.y + 1)).unwrap().bg, Color::Yellow);
}

/// R61: a bare cursor move keeps the find highlight (only an edit clears it),
/// and a backend switch drops it with the other overlays.
#[test]
pub(crate) fn editor_find_survives_navigation_and_drops_on_backend_switch() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    app.set_editor_text("one two one");
    app.editor_find_needle = "one".into();
    app.editor_find_snapshot = app.editor.lines().to_vec();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    assert_eq!(app.editor_find_needle, "one", "navigation keeps the find");
    reset_overlays_for_backend_switch(&mut app);
    assert!(app.editor_find_needle.is_empty());
    assert!(app.editor_find.is_none());
}

/// The display-column mapper keeps the highlight on the exact cell the glyph
/// occupies: tabs expand to the next stop of four, CJK chars take two cells.
#[test]
pub(crate) fn editor_display_col_expands_tabs_and_wide_chars() {
    assert_eq!(editor_display_col("abc", 2), 2);
    assert_eq!(editor_display_col("\tab", 1), 4);
    assert_eq!(editor_display_col("a\tb", 2), 4);
    assert_eq!(editor_display_col("中x", 1), 2);
    assert_eq!(editor_display_col("中x", 2), 3);
}

/// R53: the highlight reaches the rendered frame — both brackets carry the
/// bold accent marker and unrelated cells do not. (`UNDERLINED` cannot be the
/// marker here: tui-textarea already underlines the caret's whole line by
/// default, so the pair is marked with `BOLD` plus the accent colour.)
#[test]
pub(crate) fn editor_renders_the_matching_bracket_highlight() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    let text = "SELECT f(a) FROM t";
    let open = text.find('(').unwrap();
    let close = text.find(')').unwrap();
    app.set_editor_text(text);
    app.editor.move_cursor(CursorMove::Jump(0, open as u16));
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    let y = ed.y + 1;
    let marked = |dx: u16| {
        let cell = buf.cell((ed.x + 1 + dx, y)).expect("editor cell");
        cell.modifier.contains(Modifier::BOLD) && cell.fg == Color::LightCyan
    };
    assert!(marked(open as u16), "the caret's `(` is marked");
    assert!(marked(close as u16), "the matching `)` is marked");
    assert!(!marked(3), "an unrelated cell is untouched");

    // Moving the caret off the bracket restores the plain editor.
    app.editor.move_cursor(CursorMove::Jump(0, 3));
    let buf = draw_buffer(&mut app, 80, 30);
    let cell = buf.cell((ed.x + 1 + open as u16, y)).unwrap();
    assert!(
        !cell.modifier.contains(Modifier::BOLD) && cell.fg != Color::LightCyan,
        "leaving the bracket clears the highlight"
    );

    // Degenerate panes must not panic: the highlight clips to the inner rect.
    app.editor.move_cursor(CursorMove::Jump(0, open as u16));
    for (w, h) in [(20u16, 6u16), (40, 2), (18, 1), (1, 1)] {
        draw(&mut app, w, h);
    }
}

/// R53: the highlight follows the editor's horizontal scroll — a pair that is
/// only visible after scrolling is still marked on the right cells.
#[test]
pub(crate) fn bracket_highlight_follows_horizontal_scroll() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    let text = format!("{}f(a)", "x".repeat(120));
    let open = text.find('(').unwrap();
    let close = text.find(')').unwrap();
    app.set_editor_text(&text);
    // Park the caret on the closing bracket so both glyphs stay in view once
    // the widget scrolls to it.
    app.editor.move_cursor(CursorMove::Jump(0, close as u16));
    let buf = draw_buffer(&mut app, 80, 30);
    assert!(app.editor_vp.col > 0, "the editor scrolled horizontally");
    let ed = app.rects.editor;
    let y = ed.y + 1;
    let top = app.editor_vp.col;
    let marked = |col: usize| {
        let cell = buf
            .cell((ed.x + 1 + (col as u16 - top), y))
            .expect("editor cell");
        cell.modifier.contains(Modifier::BOLD) && cell.fg == Color::LightCyan
    };
    assert!(marked(open), "the scrolled-to `(` is marked");
    assert!(marked(close), "the scrolled-to `)` is marked");
}

/// R53: a cell's clipboard text matches what the row popup shows, so a paste
/// reads the same everywhere.
#[test]
pub(crate) fn cell_copy_text_shapes_match_the_row_popup() {
    assert_eq!(cell_copy_text(&Val::Null), "NULL");
    assert_eq!(cell_copy_text(&Val::Text(String::new())), "''");
    assert_eq!(cell_copy_text(&Val::Text("plain".into())), "plain");
}

/// R56: `alt-↓/↑` steps by statement; the editor renders the caret's
/// statement at full brightness and dims the rest. Pure-render: the span is
/// resolved by `active_statement_rows` and painted only on the visible rows.
#[test]
pub(crate) fn editor_dims_non_active_statements() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    app.set_editor_text("SELECT 1;\nSELECT 2;\nSELECT 3;");
    app.editor.move_cursor(CursorMove::Jump(1, 3));
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    let row_fg =
        |buf: &ratatui::buffer::Buffer, r: u16| buf.cell((ed.x + 1, ed.y + 1 + r)).unwrap().fg;
    assert_ne!(
        row_fg(&buf, 1),
        Color::DarkGray,
        "the caret statement stays lit"
    );
    assert_eq!(
        row_fg(&buf, 0),
        Color::DarkGray,
        "the statement above is dimmed"
    );
    assert_eq!(
        row_fg(&buf, 2),
        Color::DarkGray,
        "the statement below is dimmed"
    );

    // Moving the caret to the last statement dims the first two.
    app.editor.move_cursor(CursorMove::Jump(2, 0));
    let buf = draw_buffer(&mut app, 80, 30);
    assert_eq!(row_fg(&buf, 0), Color::DarkGray);
    assert_eq!(row_fg(&buf, 1), Color::DarkGray);
    assert_ne!(row_fg(&buf, 2), Color::DarkGray);

    // A single-statement buffer is never dimmed.
    app.set_editor_text("SELECT 1");
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    let buf = draw_buffer(&mut app, 80, 30);
    assert_ne!(row_fg(&buf, 0), Color::DarkGray);

    // Degenerate panes must not panic.
    app.set_editor_text("SELECT 1;\nSELECT 2;");
    for (w, h) in [(20u16, 6u16), (40, 2), (18, 1), (1, 1)] {
        draw(&mut app, w, h);
    }
}

/// R56: the active-statement row span comes from the same `;` splitter as the
/// statement jumper (literals / comments never split), and a single-statement
/// buffer (or a stale caret) has nothing to dim.
#[test]
pub(crate) fn active_statement_rows_tracks_the_caret_statement() {
    let lines = |s: &str| s.split('\n').map(str::to_string).collect::<Vec<_>>();
    let three = lines("SELECT 1;\nSELECT 2;\nSELECT 3;");
    assert_eq!(active_statement_rows(&three, 0, 0), Some((0, 0)));
    assert_eq!(active_statement_rows(&three, 1, 3), Some((1, 1)));
    assert_eq!(active_statement_rows(&three, 2, 5), Some((2, 2)));

    // A statement that spans several lines dims none of its own rows.
    let multi = lines("SELECT a,\n b\nFROM t;\nSELECT 1;");
    assert_eq!(active_statement_rows(&multi, 2, 0), Some((0, 2)));
    assert_eq!(active_statement_rows(&multi, 3, 2), Some((3, 3)));

    // One statement (or none) is nothing to dim.
    assert_eq!(active_statement_rows(&lines("SELECT 1"), 0, 0), None);
    assert_eq!(active_statement_rows(&lines("SELECT 1;"), 0, 0), None);

    // A `;` inside a string literal never splits.
    let lit = lines("SELECT ';';\nSELECT 2;");
    assert_eq!(active_statement_rows(&lit, 0, 3), Some((0, 0)));
    assert_eq!(active_statement_rows(&lit, 1, 0), Some((1, 1)));

    // Empty buffer / stale caret.
    assert_eq!(active_statement_rows(&[], 0, 0), None);
    assert_eq!(active_statement_rows(&three, 9, 0), None);

    // Over the byte cap only a ±`STMT_DIM_SCAN_LINES` window is split, so two
    // statements close together still resolve while the far one does not.
    let mut big: Vec<String> = vec!["SELECT 1;".to_string(), "SELECT 2;".to_string()];
    big.extend((0..60_000).map(|i| format!("-- p{i}")));
    let bytes: usize = big.iter().map(|l| l.len() + 1).sum();
    assert!(
        bytes > STMT_DIM_MAX_BYTES,
        "test buffer must exceed the cap"
    );
    assert_eq!(active_statement_rows(&big, 0, 3), Some((0, 0)));
    assert_eq!(active_statement_rows(&big, 1, 3), Some((1, 1)));
}

/// R53: `Y` copies the focused cell and flashes the column + char count in
/// the status bar; a grid without a cell (structure view / script list)
/// reports instead of copying.
#[test]
pub(crate) fn shift_y_copies_the_focused_cell() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Val::Text("7".into()), Val::Text("seven".into())]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    app.col_cursor = 1;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
    );
    assert!(app.status.contains("已复制"), "{}", app.status);
    assert!(app.status.contains("name"), "{}", app.status);
    assert!(app.status.contains("5 字符"), "{}", app.status);

    // The structure (DDL) view has no cell on screen to copy.
    app.struct_view = StructView::Ddl;
    app.ddl = Some("CREATE TABLE t (id int)".into());
    app.status.clear();
    copy_cell_value(&mut app);
    assert!(app.status.contains("没有可复制的单元格"), "{}", app.status);
    app.struct_view = StructView::Fields;
    app.ddl = None;

    // The script list has no cell cursor either.
    app.script = Some(sample_script(2));
    app.status.clear();
    copy_cell_value(&mut app);
    assert!(app.status.contains("没有可复制的单元格"), "{}", app.status);
}

/// R53 audit: `PageUp` / `PageDown` in the results grid are a whole-screen
/// jump (R42), clamped at the first / last row, counted, and crossing no page
/// boundary here (a plain 40-row result).
#[test]
pub(crate) fn page_keys_move_a_screen_and_clamp_at_the_edges() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["c".into()],
        rows: (0..40).map(|i| vec![Val::Text(i.to_string())]).collect(),
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    // `viewport_rows` reads the laid-out results rect, so draw one frame.
    draw(&mut app, 110, 30);
    let screen = viewport_rows(&app);
    assert!(screen > 1, "a screenful should be more than one row");

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, screen);
    // A count prefix jumps several screens at once, clamped to the last row.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 39);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 39 - screen);
    // Repeated PageUp walks up to the first row and clamps there.
    for _ in 0..4 {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        );
    }
    assert_eq!(app.sel, 0, "PageUp clamps at the first row");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 0, "PageUp at the top is a no-op");
}

/// R62: the half-page keys yield where the pane already owns them — the
/// editor keeps `Ctrl-U` for undo, the results pane keeps `Ctrl-D` for
/// delete-row — and the free key carries the half-page motion. The decision
/// is a pure function so the yield rule is pinned here.
#[test]
pub(crate) fn half_page_key_yields_to_documented_bindings() {
    // Editor: Ctrl-D is free → half-page down; Ctrl-U is undo → yields.
    assert_eq!(
        half_page_key(Focus::Editor, true, KeyCode::Char('d')),
        Some(HalfPage::Down)
    );
    assert_eq!(half_page_key(Focus::Editor, true, KeyCode::Char('u')), None);
    // Results: Ctrl-D is delete-row → yields; Ctrl-U is free → half-page up.
    assert_eq!(
        half_page_key(Focus::Preview, true, KeyCode::Char('d')),
        None
    );
    assert_eq!(
        half_page_key(Focus::Preview, true, KeyCode::Char('u')),
        Some(HalfPage::Up)
    );
    // Without Ctrl the letters keep their literal meaning, and the sidebar /
    // command line never half-page.
    assert_eq!(
        half_page_key(Focus::Editor, false, KeyCode::Char('d')),
        None
    );
    assert_eq!(
        half_page_key(Focus::Preview, false, KeyCode::Char('u')),
        None
    );
    assert_eq!(
        half_page_key(Focus::Sidebar, true, KeyCode::Char('d')),
        None
    );
    assert_eq!(
        half_page_key(Focus::CmdInput, true, KeyCode::Char('u')),
        None
    );
}

/// R62: the half-page step is half the viewport and never zero, even when the
/// results pane is squeezed to nothing (`viewport_rows` floors at one).
#[test]
pub(crate) fn half_page_step_is_never_zero() {
    let app = test_app();
    assert_eq!(viewport_rows(&app), 1);
    assert_eq!(half_rows(&app), 1, "a degenerate pane still steps one row");
}

/// R62: `EditorViewport::half_page` mirrors tui-textarea's `height / 2` step
/// (truncating an odd height) and clamps at the top of the buffer.
#[test]
pub(crate) fn editor_viewport_half_page_steps_and_clamps() {
    let mut vp = EditorViewport {
        w: 40,
        h: 10,
        ..Default::default()
    };
    vp.half_page(true);
    assert_eq!(vp.row, 5);
    vp.half_page(true);
    assert_eq!(vp.row, 10);
    vp.half_page(false);
    assert_eq!(vp.row, 5);
    vp.half_page(false);
    vp.half_page(false);
    assert_eq!(vp.row, 0, "clamps at the first line");
    // An odd height truncates, matching `(height as i16) / 2`.
    let mut vp = EditorViewport {
        w: 40,
        h: 7,
        ..Default::default()
    };
    vp.half_page(true);
    assert_eq!(vp.row, 3);
}

/// R62: in the editor `Ctrl-D` scrolls half a screen and the cursor follows
/// the new viewport; `Ctrl-U` keeps its R51 undo role and does not scroll.
#[test]
pub(crate) fn editor_ctrl_d_half_pages_and_ctrl_u_still_undoes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    let long: String = (0..200).map(|i| format!("SELECT {i};\n")).collect();
    app.set_editor_text(&long);
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    // Draw a frame so the widget's viewport (and the mirror) have a size.
    draw(&mut app, 110, 30);
    let h = app.editor_vp.h as usize;
    assert!(h >= 2, "editor viewport should be a few rows, got {h}");
    assert_eq!(app.editor.cursor(), (0, 0));
    assert_eq!(app.editor_vp.row, 0);

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.editor_vp.row,
        (h / 2) as u16,
        "mirror steps half a page"
    );
    assert_eq!(
        app.editor.cursor().0,
        h / 2,
        "the cursor follows the scrolled viewport"
    );

    // Ctrl-U is the documented undo, so it must NOT scroll: the viewport
    // stays put (nothing to undo still reports a status).
    let before = app.editor_vp.row;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.editor_vp.row, before,
        "Ctrl-U does not half-page in the editor"
    );
}

/// R62: in the results grid `Ctrl-U` scrolls up half a screen (reusing the
/// full-page arithmetic) and clamps at the first row, while `Ctrl-D` yields to
/// delete-row and never scrolls.
#[test]
pub(crate) fn results_ctrl_u_half_pages_up_and_ctrl_d_still_deletes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["c".into()],
        rows: (0..40).map(|i| vec![Val::Text(i.to_string())]).collect(),
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 20;
    draw(&mut app, 110, 30);
    let half = half_rows(&app);
    assert!(half >= 1 && half < viewport_rows(&app));

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.sel, 20 - half, "Ctrl-U moves up half a screen");

    // Repeated Ctrl-U walks up to the first row and clamps there.
    for _ in 0..6 {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
    }
    assert_eq!(app.sel, 0, "Ctrl-U clamps at the first row");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.sel, 0, "Ctrl-U at the top is a no-op");

    // Ctrl-D keeps its delete-row meaning (here a plain query grid, so the
    // delete handler reports that only table browsing supports it — proving
    // the key reached the delete path instead of scrolling).
    app.status.clear();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.sel, 0, "Ctrl-D does not scroll");
    assert!(
        app.status.contains("仅表格浏览支持删除行"),
        "Ctrl-D reached delete-row, got status {:?}",
        app.status
    );
}

/// R62 audit: the results grid header is drawn through ratatui's
/// `Table::header`, so it stays pinned at the top of the pane while the rows
/// scroll underneath (the vertical scroll slices the body rows).
#[test]
pub(crate) fn results_grid_header_stays_pinned_while_scrolling() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["alpha".into(), "beta".into()],
        rows: (0..60)
            .map(|i| vec![Val::Text(format!("row{i}")), Val::Text(format!("v{i}"))])
            .collect(),
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    let top = draw(&mut app, 110, 30);
    assert!(
        top.iter().any(|l| l.contains("alpha")),
        "header visible at the top:\n{}",
        top.join("\n")
    );

    // Scroll well past the first screen: the header stays, the first row goes.
    app.sel = 55;
    let scrolled = draw(&mut app, 110, 30);
    assert!(
        scrolled
            .iter()
            .any(|l| l.contains("alpha") && l.contains("beta")),
        "header stays pinned after scrolling:\n{}",
        scrolled.join("\n")
    );
    assert!(
        !scrolled.iter().any(|l| l.contains("row0")),
        "the first data row scrolled out:\n{}",
        scrolled.join("\n")
    );
}

/// tui-textarea's readline bindings must survive `browse_key`'s routing:
/// Ctrl-A / Ctrl-E move to the line ends, Ctrl-W deletes the previous word
/// and Ctrl-K (and Ctrl-Shift-K) kill to the end of the line.
#[test]
pub(crate) fn readline_line_editing_keys_reach_the_editor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("aaa bbb ccc");
    let k = |c: char, m: KeyModifiers| KeyEvent::new(KeyCode::Char(c), m);
    key(&mut app, &tx, k('a', KeyModifiers::CONTROL));
    assert_eq!(app.editor.cursor(), (0, 0));
    key(&mut app, &tx, k('e', KeyModifiers::CONTROL));
    assert_eq!(app.editor.cursor(), (0, 11));
    key(&mut app, &tx, k('w', KeyModifiers::CONTROL));
    assert_eq!(app.editor_sql(), "aaa bbb ");
    app.set_editor_text("SELECT 1 -- tail");
    app.editor.move_cursor(CursorMove::Jump(0, 8));
    key(&mut app, &tx, k('k', KeyModifiers::CONTROL));
    assert_eq!(app.editor_sql(), "SELECT 1");
    app.set_editor_text("abc def");
    app.editor.move_cursor(CursorMove::Jump(0, 4));
    key(
        &mut app,
        &tx,
        k('K', KeyModifiers::CONTROL | KeyModifiers::SHIFT),
    );
    assert_eq!(app.editor_sql(), "abc ");
}

/// Shift+arrow selection already works in the editor (tui-textarea's built-in
/// binding), and the pan gesture stays outside the text panes.
#[test]
pub(crate) fn shift_arrows_select_inside_the_editor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("abc");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
    );
    assert!(
        app.editor.selection_range().is_some(),
        "Shift+← starts a selection in the editor"
    );
}

/// The back/forward history is browser-shaped: re-opening the entry the
/// cursor points at is a no-op, a fresh table truncates the forward branch,
/// and the depth is capped.
/// The back/forward history is browser-shaped: re-opening the entry the
/// cursor points at is a no-op, a fresh node truncates the forward branch,
/// and the depth is capped.
#[test]
pub(crate) fn nav_history_is_browser_style() {
    let mut app = test_app();
    let e = |t: &str| NavEntry::Table {
        db: "db".to_string(),
        schema: "public".to_string(),
        table: t.to_string(),
    };
    record_nav(&mut app, e("a"));
    record_nav(&mut app, e("b"));
    record_nav(&mut app, e("c"));
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 2);
    // Re-opening the current entry does not add a step.
    record_nav(&mut app, e("c"));
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 2);
    // A back step moves the cursor; the forward branch is kept.
    app.nav_pos -= 1;
    record_nav(&mut app, e("b"));
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 1);
    // Opening a new table drops the forward branch (browser semantics).
    record_nav(&mut app, e("d"));
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 2);
    let names: Vec<&str> = app
        .nav_history
        .iter()
        .map(|e| match e {
            NavEntry::Table { table, .. } => table.as_str(),
            NavEntry::RedisKey { .. } => "?",
        })
        .collect();
    assert_eq!(names, vec!["a", "b", "d"]);
    // The stack is bounded; the oldest steps fall off the front.
    for i in 0..NAV_DEPTH * 2 {
        record_nav(
            &mut app,
            NavEntry::Table {
                db: "db".into(),
                schema: "public".into(),
                table: format!("t{i}"),
            },
        );
    }
    assert_eq!(app.nav_history.len(), NAV_DEPTH);
    assert_eq!(app.nav_pos, NAV_DEPTH - 1);
}

/// R42: a Redis key is a first-class round-trip node next to a table, so a
/// table → key → table walk is replayable, while the value detail itself is
/// not a second node.
#[test]
pub(crate) fn nav_history_records_redis_keys_beside_tables() {
    let mut app = test_app();
    remember_recent_table(&mut app, "shop", "public", "orders");
    remember_redis_key(&mut app, 3, "raw:app:1", "app:1");
    // Re-recording the key the cursor already points at is a no-op.
    remember_redis_key(&mut app, 3, "raw:app:1", "app:1");
    assert_eq!(app.nav_history.len(), 2);
    remember_recent_table(&mut app, "shop", "public", "items");
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 2);
    assert!(matches!(
        &app.nav_history[1],
        NavEntry::RedisKey { db: 3, key_display, .. } if key_display == "app:1"
    ));
}

#[test]
pub(crate) fn remember_recent_table_dedups_and_records_nav() {
    let mut app = test_app();
    remember_recent_table(&mut app, "shop", "public", "orders");
    remember_recent_table(&mut app, "shop", "public", "items");
    remember_recent_table(&mut app, "shop", "public", "orders");
    assert_eq!(app.recent_tables.len(), 2);
    assert_eq!(app.recent_tables[0].2, "orders");
    // The history keeps the visit order, the repeat included.
    assert_eq!(app.nav_history.len(), 3);
    assert_eq!(app.nav_pos, 2);
}

/// R63: the recency panel can re-sort by name, purely client-side — the
/// canonical most-recent-first list never changes, only the view does.
#[test]
pub(crate) fn recent_panel_sorts_by_recency_or_name() {
    let mut app = test_app();
    remember_recent_table(&mut app, "shop", "public", "orders");
    remember_recent_table(&mut app, "shop", "public", "accounts");
    remember_recent_table(&mut app, "shop", "public", "items");
    // Default: newest first.
    assert_eq!(app.recent_sort, RecentSort::Recent);
    assert_eq!(recent_order(&app), vec![0, 1, 2]);
    assert_eq!(app.recent_tables[0].2, "items");
    // Name order is a permutation of the same rows: accounts, items, orders.
    app.recent_sort = RecentSort::Name;
    assert_eq!(recent_order(&app), vec![1, 0, 2]);
    assert_eq!(app.recent_tables[0].2, "items");
}

/// R63: `s` toggles the order and parks the cursor at the top; `k` stays
/// vim-up in this panel (the ironclad key rule), never the sort key.
#[test]
pub(crate) fn recent_panel_s_toggles_sort_and_k_still_moves() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    remember_recent_table(&mut app, "shop", "public", "orders");
    remember_recent_table(&mut app, "shop", "public", "items");
    remember_recent_table(&mut app, "shop", "public", "accounts");
    app.recent_open = true;
    app.recent_list.select(Some(2));
    recent_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(app.recent_sort, RecentSort::Name);
    assert_eq!(app.recent_list.selected(), Some(0));
    assert!(app.status.contains("按表名"), "{}", app.status);
    // `k` still walks up the list and leaves the order alone.
    app.recent_list.select(Some(2));
    recent_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.recent_sort, RecentSort::Name);
    assert_eq!(app.recent_list.selected(), Some(1));
    // `s` flips back to recency order.
    recent_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(app.recent_sort, RecentSort::Recent);
    assert!(app.status.contains("按最近"), "{}", app.status);
}

/// R63: the connect-time latency is formatted compactly, and a failed probe
/// stays silent — nothing is cached, so nothing is shown.
#[test]
pub(crate) fn connect_latency_formats_and_fails_silently() {
    assert_eq!(format_rtt(Duration::from_millis(0)), "0ms");
    assert_eq!(format_rtt(Duration::from_millis(12)), "12ms");
    assert_eq!(format_rtt(Duration::from_millis(999)), "999ms");
    assert_eq!(format_rtt(Duration::from_millis(1000)), "1.0s");
    assert_eq!(format_rtt(Duration::from_millis(1234)), "1.2s");

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    apply_op_result(
        &mut app,
        OpResult::ServerVersion {
            id: "id-postgres".into(),
            version: Some("16.2".into()),
            rtt: Some(Duration::from_millis(12)),
        },
        &tx,
    );
    assert_eq!(
        app.server_versions.get("id-postgres").map(String::as_str),
        Some("16.2")
    );
    assert_eq!(
        app.server_rtts.get("id-postgres"),
        Some(&Duration::from_millis(12))
    );
    // A failed read caches neither the version nor a latency.
    apply_op_result(
        &mut app,
        OpResult::ServerVersion {
            id: "id-postgres".into(),
            version: None,
            rtt: None,
        },
        &tx,
    );
    assert_eq!(
        app.server_versions.get("id-postgres").map(String::as_str),
        Some("16.2")
    );
}

/// Alt-← / Alt-→ are the history keys outside the text panes; the editor
/// keeps them local so editing SQL is never interrupted.
#[test]
pub(crate) fn alt_arrows_navigate_outside_the_editor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Sidebar;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    );
    assert!(app.status.contains("还没有浏览过表"), "{}", app.status);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
    );
    assert!(app.status.contains("还没有浏览过表"), "{}", app.status);
    // In the editor the arrow is left alone (it does not navigate).
    app.focus = Focus::Editor;
    app.status.clear();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    );
    assert!(
        app.status.is_empty(),
        "editor Alt-← stayed local: {}",
        app.status
    );
    // The landing hint is both the status and the context-block entry, and
    // any other key clears the latter.
    set_nav_status(&mut app, "←", "public.orders");
    assert_eq!(app.status, "← public.orders");
    assert_eq!(app.nav_landing.as_deref(), Some("← public.orders"));
    assert!(context_info(&app).starts_with("← public.orders"));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert!(app.nav_landing.is_none());
}

#[test]
pub(crate) fn sidebar_width_shrinks_toward_the_longest_name() {
    // Wide screens keep the historical 28 columns.
    assert_eq!(sidebar_width(120, LayoutMode::Wide, 40), 28);
    assert_eq!(sidebar_width(110, LayoutMode::Wide, 8), 28);
    // Mid screens shrink to the longest name, clamped to the readable floor.
    assert_eq!(sidebar_width(60, LayoutMode::Mid, 6), SIDEBAR_MIN_W);
    assert_eq!(sidebar_width(60, LayoutMode::Mid, 15), 19);
    assert_eq!(sidebar_width(60, LayoutMode::Mid, 40), SIDEBAR_MID_W);
    // The narrow layout stacks the sidebar, so it spans the terminal.
    assert_eq!(sidebar_width(42, LayoutMode::Narrow, 15), 42);
}

#[test]
pub(crate) fn truncate_table_name_marks_the_cut() {
    assert_eq!(truncate_table_name("orders", 10), "orders");
    assert_eq!(truncate_table_name("public.accounts", 10), "public.ac~");
    assert_eq!(
        truncate_table_name("public.accounts", 15),
        "public.accounts"
    );
    assert_eq!(truncate_table_name("x", 0), "");
}

/// The sidebar renders the `~` marker on a name it had to cut, so a clipped
/// identifier is obvious on a narrow screen.
#[test]
pub(crate) fn narrow_sidebar_marks_truncated_table_names() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.schema = String::new();
    app.tables = vec![
        table_info("a_very_long_table_name_that_cannot_fit", "TABLE"),
        table_info("short", "TABLE"),
    ];
    app.tables_all = app.tables.clone();
    app.table_list.select(Some(0));
    app.term_w = 60;
    let rows = draw(&mut app, 60, 22);
    let joined = rows.join("\n");
    assert!(
        joined.contains("a_very_long_table~"),
        "cut name is marked with ~: {joined}"
    );
    assert!(
        joined.contains("short"),
        "short name is untouched: {joined}"
    );
}

/// A narrow grid that clips columns shows the focused column's name early in
/// the status line, where the width cap cannot truncate it away.
#[test]
pub(crate) fn narrow_status_bar_names_the_focused_column() {
    let mut app = test_app();
    app.term_w = 42;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.grid_frozen = 1;
    app.vis_cols = 2;
    app.col_cursor = 3;
    app.focus = Focus::Preview;
    let info = context_info(&app);
    assert!(info.contains("column_3"), "{info}");
    assert!(info.contains("4/8"), "{info}");
    // The pin prefix survives the merge, and there is exactly one column
    // readout (R43 removed the duplicate scroll-window line).
    assert!(info.contains("1|"), "{info}");
    assert_eq!(info.matches("列 ").count(), 1, "{info}");
    // A wide terminal shows the same single readout instead of a bare
    // `列 1|1-2/8` scroll window.
    app.term_w = 120;
    let wide = context_info(&app);
    assert!(wide.contains("column_3"), "{wide}");
    assert_eq!(wide.matches("列 ").count(), 1, "{wide}");
    assert!(!wide.contains("1-2/"), "no scroll window: {wide}");
}
