use super::*;

#[test]
pub(crate) fn fit_title_swaps_in_the_short_variant_only_when_needed() {
    let full = " a very very long overlay title with hints ";
    let short = " short ";
    assert_eq!(fit_title(full, short, 80), full);
    assert_eq!(fit_title(full, short, 20), short);
    // The two border corners are reserved, so exactly-fitting is OK.
    let exact = "x".repeat(18);
    assert_eq!(fit_title(&exact, short, 20), exact);
    assert_eq!(fit_title(&"x".repeat(19), short, 20), short);
}

/// R36 seam: Ctrl-L switches the backend line, so every modal opened on the
/// old backend must be closed and its background task cancelled / invalidated
/// — otherwise a SQL diff or transfer wizard kept owning the keyboard over
/// the Redis / Mongo view.
#[test]
pub(crate) fn backend_switch_closes_every_overlay_and_cancels_its_task() {
    let mut app = test_app();
    app.search_open = true;
    app.search_running = true;
    app.data_where = Some(TextArea::default());
    app.data_diff = Some(Box::new(DataDiffState {
        result: data_cmp_fixture(),
        tab: DataTab::Summary,
        list: ListState::default(),
        scroll: 0,
        sync_sql: String::new(),
    }));
    let mut w = transfer_wizard_fixture();
    w.submitted = true;
    app.transfer = Some(Box::new(w));
    app.help_open = true;
    app.file_load_prompt = Some(TextArea::default());
    app.recent_open = true;
    app.table_jump_open = true;
    app.result_filter = Some(TextArea::default());
    let search_gen = app.search_gen;
    let diff_gen = app.data_diff_gen;
    let transfer_gen = app.transfer_gen;

    reset_overlays_for_backend_switch(&mut app);

    assert!(app.search_cancel.load(Ordering::Relaxed));
    assert!(app.data_cancel.load(Ordering::Relaxed));
    assert!(app.transfer_cancel.load(Ordering::Relaxed));
    assert_ne!(app.search_gen, search_gen);
    assert_ne!(app.data_diff_gen, diff_gen);
    assert_ne!(app.transfer_gen, transfer_gen);
    assert!(!app.search_open && !app.search_running);
    assert!(app.data_diff.is_none() && app.data_where.is_none());
    assert!(app.transfer.is_none() && app.transfer_report.is_none());
    assert!(!app.help_open && !app.recent_open);
    assert!(!app.table_jump_open);
    assert!(app.file_load_prompt.is_none() && app.result_filter.is_none());
}

#[test]
pub(crate) fn default_schema_prefers_public() {
    assert_eq!(default_schema(&["inv".into(), "public".into()]), "public");
    assert_eq!(default_schema(&["inv".into(), "other".into()]), "inv");
    assert_eq!(default_schema(&[]), "");
}

#[test]
pub(crate) fn config_persists_per_schema_table_keys() {
    let path = std::env::temp_dir().join(format!("dbxt-schema-{}.json", Uuid::new_v4()));
    let mut cfg = TuiConfig::default();
    cfg.entry("shop", "public", "orders").order_by = Some("\"id\" DESC".into());
    cfg.entry("shop", "inv", "orders").hidden = ["secret".to_string()].into_iter().collect();
    cfg.save(&path);
    let back = TuiConfig::load(&path);
    // Same table name, two schemas → two independent entries.
    assert_eq!(
        back.table("shop", "public", "orders")
            .unwrap()
            .order_by
            .as_deref(),
        Some("\"id\" DESC")
    );
    assert!(back
        .table("shop", "inv", "orders")
        .unwrap()
        .hidden
        .contains("secret"));
    // The public entry kept no hidden set, and vice versa.
    assert!(back
        .table("shop", "public", "orders")
        .unwrap()
        .hidden
        .is_empty());
    assert!(back
        .table("shop", "inv", "orders")
        .unwrap()
        .order_by
        .is_none());
    let _ = std::fs::remove_file(&path);
}

// ── R13: context-aware completion ──

#[test]
pub(crate) fn completion_context_follows_the_cursor() {
    let end = |s: &str| {
        let mut ta = TextArea::from([s]);
        ta.move_cursor(CursorMove::End);
        ta
    };
    assert_eq!(
        completion_context(&end("select * from us")).0,
        CompCtx::TableList
    );
    assert_eq!(completion_context(&end("select * from us")).1, "us");
    assert_eq!(
        completion_context(&end("select * from t left join ")).0,
        CompCtx::TableList
    );
    assert_eq!(
        completion_context(&end("select * from t where ")).0,
        CompCtx::Column
    );
    assert_eq!(
        completion_context(&end("select * from t on ")).0,
        CompCtx::Column
    );
    assert_eq!(completion_context(&end("select * ")).0, CompCtx::Any);
    let (ctx, partial) = completion_context(&end("select * from users.na"));
    assert_eq!(ctx, CompCtx::Qualified("users".into()));
    assert_eq!(partial, "na");
    // `db.table.` → the qualifier is the last segment only.
    assert_eq!(
        completion_context(&end("select * from shop.orders.")).0,
        CompCtx::Qualified("orders".into())
    );
    // Quoted / bracketed qualifiers are unquoted before matching.
    assert_eq!(
        completion_context(&end("select * from `users`.")).0,
        CompCtx::Qualified("users".into())
    );
    assert_eq!(
        completion_context(&end("select * from [users].")).0,
        CompCtx::Qualified("users".into())
    );
}

/// R48: the candidate list is scoped by context — tables only after FROM,
/// columns only after WHERE / ON — and a narrow terminal caps it at five.
#[test]
pub(crate) fn completion_candidates_are_context_scoped() {
    let mut app = test_app();
    app.tables_all = vec![table_info("users", "TABLE"), table_info("orders", "TABLE")];
    app.table_meta = Some(TableMeta {
        table: "users".into(),
        schema: String::new(),
        columns: vec![col_info("id", "int"), col_info("name", "text")],
        indexes: Vec::new(),
    });
    // After FROM: only tables (no columns, no keywords).
    let from = completion_candidates(&app, &CompCtx::TableList, "");
    assert!(from.iter().all(|i| i.kind == 'T'), "{from:?}");
    assert!(from.iter().any(|i| i.text == "users"));
    assert!(!from.iter().any(|i| i.text == "SELECT"), "{from:?}");
    // After WHERE / ON: only columns.
    let col = completion_candidates(&app, &CompCtx::Column, "");
    assert!(col.iter().all(|i| i.kind == 'C'), "{col:?}");
    assert!(col.iter().any(|i| i.text == "id"));
    // The catch-all context still mixes columns / tables / keywords.
    let any = completion_candidates(&app, &CompCtx::Any, "");
    assert!(any.iter().any(|i| i.kind == 'K'), "{any:?}");
    // A narrow terminal keeps at most five candidates.
    app.tables_all = (0..40)
        .map(|i| table_info(&format!("t{i:02}"), "TABLE"))
        .collect();
    app.term_w = 36;
    assert_eq!(
        completion_candidates(&app, &CompCtx::TableList, "").len(),
        5
    );
    app.term_w = 120;
    assert_eq!(
        completion_candidates(&app, &CompCtx::TableList, "").len(),
        8
    );
}

// ── R15: wheel modifier encodings, footer layout, bilingual UI ──

#[test]
pub(crate) fn every_wheel_modifier_encoding_pans_columns() {
    // The SGR button codes a terminal may send for a wheel event: 64/65 carry
    // no modifier bit; +4 shift, +8 alt, +16 ctrl. SHIFT is the one terminals
    // most often omit, so ALT and CTRL must pan as well.
    for (mods, pans) in [
        (KeyModifiers::NONE, false),
        (KeyModifiers::SHIFT, true),
        (KeyModifiers::ALT, true),
        (KeyModifiers::CONTROL, true),
    ] {
        assert_eq!(
            wheel_wants_pan(Focus::Preview, mods, false, true),
            pans,
            "modifiers {mods:?}"
        );
        // No horizontal overflow → never pan, modifier or not.
        assert!(!wheel_wants_pan(Focus::Preview, mods, false, false));
    }
    // Ctrl-G pan mode pans with an unmodified wheel; other panes never pan.
    assert!(wheel_wants_pan(
        Focus::Preview,
        KeyModifiers::NONE,
        true,
        true
    ));
    assert!(!wheel_wants_pan(
        Focus::Editor,
        KeyModifiers::CONTROL,
        false,
        true
    ));
}

#[test]
pub(crate) fn wire_hint_covers_all_wheel_encodings() {
    let mk = |code: u8| {
        let mut m = KeyModifiers::NONE;
        if code & 4 != 0 {
            m |= KeyModifiers::SHIFT;
        }
        if code & 8 != 0 {
            m |= KeyModifiers::ALT;
        }
        if code & 16 != 0 {
            m |= KeyModifiers::CONTROL;
        }
        MouseEvent {
            kind: if code & 1 == 0 {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: 0,
            row: 0,
            modifiers: m,
        }
    };
    for code in [64u8, 65, 68, 69, 72, 73, 80, 81] {
        let hint = mouse_wire_hint(&mk(code));
        assert!(
            hint.contains(&format!("<{code};1;1M")),
            "code {code}: {hint}"
        );
    }
}

#[test]
pub(crate) fn footer_keeps_help_visible_and_fits() {
    let hints: Vec<Hint> = vec![
        ("↑↓", "row"),
        ("←→", "column"),
        ("Enter", "details"),
        ("e", "edit"),
        ("i", "insert"),
        ("Del", "delete row"),
        ("y", "copy INSERT"),
        ("f", "filter"),
        ("/", "search"),
        ("?", "help"),
    ];
    // `?` is pinned: always present and always last in the source list.
    assert_eq!(*hints.last().unwrap(), ("?", "help"));
    for width in [42usize, 60, 80, 110] {
        let (chosen, more) = footer_select(&hints, width);
        let line_w = footer_line_width(&chosen, more);
        assert!(
            line_w <= width,
            "width {width}: line {line_w} chosen {chosen:?}"
        );
        assert!(!chosen.is_empty(), "width {width}: kept nothing");
    }
    // Very narrow: nothing but the pinned help hint survives.
    let (chosen, more) = footer_select(&hints, 10);
    assert!(chosen.is_empty());
    assert!(more);
    assert!(footer_line_width(&chosen, more) <= 10);
}

/// The width tiers cap the hint count so a small screen only shows the
/// highest-frequency keys and says `? 更多` instead of truncating a hint.
#[test]
pub(crate) fn footer_tiers_cap_hints_by_width() {
    assert_eq!(footer_tier(40), FooterTier::Mini);
    assert_eq!(footer_tier(59), FooterTier::Mini);
    assert_eq!(footer_tier(60), FooterTier::Compact);
    assert_eq!(footer_tier(99), FooterTier::Compact);
    assert_eq!(footer_tier(100), FooterTier::Full);
    assert_eq!(footer_tier_cap(FooterTier::Mini), Some(4));
    assert_eq!(footer_tier_cap(FooterTier::Compact), Some(6));
    assert_eq!(footer_tier_cap(FooterTier::Full), None);

    // Short hints so each tier's width is reached: a narrow footer shows at
    // most 4 hints, a mid one at most 6, a wide one all of them.
    let hints: Vec<Hint> = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
        .into_iter()
        .map(|key| (key, "d"))
        .chain(std::iter::once(("?", "help")))
        .collect();
    let (mini, more) = footer_select(&hints, 42);
    assert!(mini.len() <= 4, "mini chose {} hints", mini.len());
    assert!(more);
    let (mid, more) = footer_select(&hints, 80);
    assert!(mid.len() <= 6, "mid chose {} hints", mid.len());
    assert!(more);
    let (full, more) = footer_select(&hints, 120);
    assert_eq!(full.len(), 12);
    assert!(!more);
    // The pinned hint's label flips to `更多` when anything is hidden.
    assert_eq!(footer_help_hint(true).1, t("更多"));
    assert_eq!(footer_help_hint(false).1, t("帮助"));
}

/// `?` opens a context mini sheet first; a second `?` promotes to the full,
/// scrollable help; Esc closes whichever layer is on top.
#[test]
pub(crate) fn mini_help_is_progressive_and_context_aware() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Preview;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    let q = || KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);

    key(&mut app, &tx, q());
    assert!(
        app.help_mini && !app.help_open,
        "first ? opens the mini sheet"
    );
    assert_eq!(footer_ctx(&app).view, FooterView::HelpMini);
    // The mini rows come from the current context, capped at ten and never
    // including the pinned `?` hint itself.
    let mini_rows: Vec<Hint> = footer_hints_ctx(footer_ctx_inner(&app, false))
        .into_iter()
        .filter(|h| h.0 != "?")
        .take(10)
        .collect();
    assert!(!mini_rows.is_empty());
    assert!(mini_rows.len() <= 10);
    assert!(mini_rows.iter().any(|h| h.0 == "e"), "results-pane group");

    key(&mut app, &tx, q());
    assert!(app.help_open && !app.help_mini, "second ? opens full help");
    key(&mut app, &tx, q());
    assert!(!app.help_open && !app.help_mini, "third ? closes");

    // Esc on the mini layer closes it without ever showing the full list.
    key(&mut app, &tx, q());
    assert!(app.help_mini);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.help_mini && !app.help_open);
}

/// R70: `?` stays a literal character inside the text inputs, so `F1` opens the
/// same cheat-sheet there — and `F1` again widens it to the full reference.
#[test]
pub(crate) fn f1_opens_help_where_question_mark_is_a_character() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Editor;

    // `?` must reach the buffer, never be hijacked for help.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );
    assert!(
        !app.help_mini && !app.help_open,
        "? must stay a character in the editor"
    );
    assert_eq!(app.editor.lines().join(""), "?");

    // F1 opens the mini sheet from the editor...
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    );
    assert!(app.help_mini && !app.help_open, "F1 opens the mini sheet");
    assert_eq!(footer_ctx(&app).view, FooterView::HelpMini);
    // ...and a second F1 widens it to the full list (the editor cannot type `?`
    // into the mini sheet's promote path any other way).
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    );
    assert!(app.help_open && !app.help_mini, "second F1 opens full help");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.help_open && !app.help_mini);

    // The command input behaves the same way.
    app.focus = Focus::CmdInput;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    );
    assert!(app.help_mini, "F1 opens help from the command input too");
}

/// An overlay title drops whole key hints (never half a hint) as the box
/// narrows.
#[test]
pub(crate) fn overlay_hint_title_drops_whole_hints() {
    let hints: Vec<Hint> = vec![("a", "x"), ("b", "y"), ("c", "z")];
    assert_eq!(
        overlay_hint_title(60, " T ", &hints, " "),
        " T  · a x · b y · c z "
    );
    let narrow = overlay_hint_title(14, " T ", &hints, " ");
    assert!(narrow.starts_with(" T  · a x"), "{narrow}");
    assert!(!narrow.contains("b y"), "split hint: {narrow}");
    // Even when nothing fits, the prefix and suffix survive.
    assert_eq!(overlay_hint_title(6, " T ", &hints, " "), " T  ");
}

#[test]
pub(crate) fn count_prefix_parses_and_jumps() {
    assert_eq!(parse_count(""), None);
    assert_eq!(parse_count("0"), None);
    assert_eq!(parse_count("5"), Some(5));
    assert_eq!(parse_count("12"), Some(12));
    assert_eq!(count_jump_index(1, 10), Some(0));
    assert_eq!(count_jump_index(3, 10), Some(2));
    assert_eq!(count_jump_index(3, 2), Some(1), "clamped to the last item");
    assert_eq!(count_jump_index(3, 0), None);
    assert!(count_motion(KeyCode::Char('j')));
    assert!(count_motion(KeyCode::Down));
    assert!(!count_motion(KeyCode::Char('n')), "paging is per-context");
}

/// `3j` moves three rows, a bare digit flushes to a direct jump, and Esc
/// cancels a pending count. Digits in a text input are never hijacked.
#[test]
pub(crate) fn count_prefix_drives_lists_without_hijacking_inputs() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Sidebar;
    app.tables = (0..10)
        .map(|i| TableInfo {
            name: format!("t{i}"),
            table_type: "TABLE".into(),
            valid: None,
            comment: None,
            parent_schema: None,
            parent_name: None,
        })
        .collect();
    app.table_list.select(Some(0));
    let digit = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    key(&mut app, &tx, digit('3'));
    assert_eq!(app.count_buf, "3");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(app.table_list.selected(), Some(3));
    assert!(
        app.count_buf.is_empty(),
        "the count is consumed by the motion"
    );

    // A bare digit flushes as a jump to the Nth item.
    key(&mut app, &tx, digit('6'));
    flush_count(&mut app, &tx);
    assert_eq!(app.table_list.selected(), Some(5));

    // Esc cancels a pending count without moving.
    key(&mut app, &tx, digit('9'));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.count_buf.is_empty());
    assert_eq!(app.table_list.selected(), Some(5));

    // The editor keeps digits as text: the count buffer stays empty.
    app.focus = Focus::Editor;
    key(&mut app, &tx, digit('7'));
    assert!(app.count_buf.is_empty());
    assert!(app.editor.lines().join("").contains('7'));
}

/// `gd` shows the structure view, `gt` returns to the data grid.
#[tokio::test(flavor = "multi_thread")]
async fn gd_gt_switch_between_structure_and_data() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Preview;
    app.tables = vec![TableInfo {
        name: "t1".into(),
        table_type: "TABLE".into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    }];
    app.table_list.select(Some(0));
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert!(app.pending_g);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert!(!app.pending_g);
    assert!(app.status.contains("结构"), "status: {}", app.status);

    // `gv` opens the value-locate prompt. It must survive the browse-level
    // g-chord interceptor, which forwards only the known second keys.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert!(app.locate_prompt.is_some(), "gv opens the locate prompt");
    assert!(!app.pending_g);
    // Close it without an Esc (Esc from the results pane also refocuses the
    // sidebar, which would change how the next `g` is routed).
    app.locate_prompt = None;
    app.locate_needle.clear();

    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
    );
    assert!(app.page_state.is_some(), "gt loads the table data page");

    // A pending `g` must not survive an unrelated key: `?` opens help and
    // clears the chord instead of being swallowed.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert!(app.pending_g);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
    );
    assert!(app.help_mini && !app.pending_g, "? wins over a stale g");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
}

/// A narrow results pane keeps only the table name, page number and the
/// filter / search marks in its title.
#[test]
pub(crate) fn narrow_grid_title_keeps_the_three_essentials() {
    let mut app = test_app();
    app.backend_kind = Backend::Sql;
    app.selected = Some(test_conn("mysql"));
    app.grid_kind = GridKind::TableData;
    app.term_w = 42;
    app.page_state = Some(PageState {
        table: "items".into(),
        schema: "public".into(),
        table_type: Some("TABLE".into()),
        page: 2,
        page_size: PAGE_SIZE,
        total: Some(1234),
        total_lower_bound: false,
        has_next: true,
        filter: "id > 5".into(),
        order_by: None,
        keyset: None,
    });
    let title = grid_title(&app);
    assert!(title.contains("items"), "table name kept: {title}");
    assert!(title.contains("p3"), "page number kept: {title}");
    assert!(title.contains('⚑'), "filter mark kept: {title}");
    assert!(!title.contains("第"), "verbose page label dropped: {title}");
    // A wide terminal keeps the full descriptive title.
    app.term_w = 120;
    let wide = grid_title(&app);
    assert!(wide.contains("items") && wide.contains("第 3 页"), "{wide}");
}

/// Esc semantics audit: every modal overlay closes on Esc and every text
/// input abandons its edit (the field goes back to `None`). The list mirrors
/// the `browse_key` router order so a new overlay that forgets Esc shows up
/// here as a failure rather than a silent regression.
#[test]
pub(crate) fn esc_closes_or_abandons_every_overlay() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let ta = || Some(TextArea::default());
    let line = || PopupLine {
        text: "x".into(),
        style: Style::default(),
    };
    let diff_picker = || DiffPicker {
        mode: DiffPickMode::Table,
        kind: DiffKind::Schema,
        stage: DiffPickStage::Lists,
        list: ListState::default(),
        src_entries: vec!["a".into()],
        entries: vec!["a".into()],
        target_conn: None,
        target_db: String::new(),
        target_schema: String::new(),
        loading: false,
        comparing: false,
        where_input: String::new(),
        gen: 0,
    };
    type Opener = Box<dyn Fn(&mut App)>;
    type Check = Box<dyn Fn(&App) -> bool>;
    let cases: Vec<(&str, Opener, Check)> = vec![
        (
            "help-mini",
            Box::new(|a| a.help_mini = true),
            Box::new(|a| !a.help_mini && !a.help_open),
        ),
        (
            "help-full",
            Box::new(|a| a.help_open = true),
            Box::new(|a| !a.help_open),
        ),
        (
            "export-picker",
            Box::new(|a| a.export_open = true),
            Box::new(|a| !a.export_open),
        ),
        (
            "export-path (input)",
            Box::new(move |a| a.export_path = ta()),
            Box::new(|a| a.export_path.is_none()),
        ),
        (
            "filter-prompt (input)",
            Box::new(move |a| a.filter_prompt = ta()),
            Box::new(|a| a.filter_prompt.is_none()),
        ),
        (
            "result-filter (input)",
            Box::new(move |a| a.result_filter = ta()),
            Box::new(|a| a.result_filter.is_none()),
        ),
        (
            "table-filter (input)",
            Box::new(move |a| a.table_prompt = ta()),
            Box::new(|a| a.table_prompt.is_none()),
        ),
        (
            "history-filter (input)",
            Box::new(move |a| a.history_filter = ta()),
            Box::new(|a| a.history_filter.is_none()),
        ),
        (
            "search-input (input)",
            Box::new(move |a| a.search_input = ta()),
            Box::new(|a| a.search_input.is_none()),
        ),
        (
            "file-load-prompt (input)",
            Box::new(move |a| a.file_load_prompt = ta()),
            Box::new(|a| a.file_load_prompt.is_none()),
        ),
        (
            "data-where (input)",
            Box::new(move |a| a.data_where = ta()),
            Box::new(|a| a.data_where.is_none()),
        ),
        (
            "snippet-name (input)",
            Box::new(move |a| a.snippet_name = ta()),
            Box::new(|a| a.snippet_name.is_none()),
        ),
        (
            "conn-import-path (input)",
            Box::new(move |a| a.conn_import_path = ta()),
            Box::new(|a| a.conn_import_path.is_none()),
        ),
        (
            "recent",
            Box::new(|a| a.recent_open = true),
            Box::new(|a| !a.recent_open),
        ),
        (
            "table-jump",
            Box::new(|a| a.table_jump_open = true),
            Box::new(|a| !a.table_jump_open),
        ),
        (
            "col-picker",
            Box::new(|a| a.col_picker_open = true),
            Box::new(|a| !a.col_picker_open),
        ),
        (
            "db-picker",
            Box::new(|a| a.db_picker_open = true),
            Box::new(|a| !a.db_picker_open),
        ),
        (
            "snippets",
            Box::new(|a| a.snippet_open = true),
            Box::new(|a| !a.snippet_open),
        ),
        (
            "history",
            Box::new(|a| a.history_open = true),
            Box::new(|a| !a.history_open),
        ),
        (
            "search",
            Box::new(|a| a.search_open = true),
            Box::new(|a| !a.search_open),
        ),
        (
            "cell-popup",
            Box::new(move |a| a.cell_popup = Some(cell_popup_from_lines("c".into(), vec![line()]))),
            Box::new(|a| a.cell_popup.is_none()),
        ),
        (
            "row-popup",
            Box::new(move |a| a.row_popup = Some(row_popup_from_lines("r".into(), vec![line()]))),
            Box::new(|a| a.row_popup.is_none()),
        ),
        (
            "confirm",
            Box::new(|a| {
                a.confirm = Some(Confirm {
                    sql: "delete from t".into(),
                    reasons: Vec::new(),
                    refresh: false,
                    clear_batch: false,
                    conn: None,
                    redis: None,
                    mongo: None,
                })
            }),
            Box::new(|a| a.confirm.is_none()),
        ),
        (
            "history-confirm",
            Box::new(|a| {
                a.history_confirm = Some(HistoryConfirm {
                    id: "1".into(),
                    sql: "select 1".into(),
                })
            }),
            Box::new(|a| a.history_confirm.is_none()),
        ),
        (
            "diff-picker",
            Box::new(move |a| a.diff_picker = Some(diff_picker())),
            Box::new(|a| a.diff_picker.is_none()),
        ),
        (
            "import-prompt (input)",
            Box::new(|a| {
                a.import_prompt = Some(ImportPrompt {
                    input: TextArea::default(),
                    table: "t".into(),
                    schema: String::new(),
                    db: "d".into(),
                    error: None,
                })
            }),
            Box::new(|a| a.import_prompt.is_none()),
        ),
        (
            "transfer-wizard",
            Box::new(|a| a.transfer = Some(Box::new(transfer_wizard_fixture()))),
            Box::new(|a| a.transfer.is_none()),
        ),
    ];

    for (name, open, closed) in cases {
        let mut app = test_app();
        app.picker_open = false;
        app.selected = Some(test_conn("mysql"));
        app.backend_kind = Backend::Sql;
        app.focus = Focus::Preview;
        open(&mut app);
        key(&mut app, &tx, esc);
        assert!(closed(&app), "{name}: Esc did not close / abandon");
    }
}

#[test]
pub(crate) fn footer_groups_follow_focus_and_overlays() {
    let keys = |view, focus, has_conn| -> Vec<&'static str> {
        footer_hints_ctx(FooterCtx {
            view,
            focus,
            has_connection: has_conn,
        })
        .iter()
        .map(|h| h.0)
        .collect()
    };
    // Overlays get their own group instead of the page's.
    assert_eq!(
        keys(FooterView::Confirm, Focus::Preview, true),
        vec!["Enter/y", "Esc/n", "?"]
    );
    assert_eq!(
        keys(FooterView::EditDialog, Focus::Preview, true),
        vec!["Enter", "Esc", "Ctrl-V", "Ctrl-T", "?"]
    );
    assert_eq!(
        keys(FooterView::Help, Focus::Preview, true),
        vec!["/", "↑↓", "Esc", "?"]
    );
    // Each browse pane gets its own, most-relevant keys.
    let sidebar = keys(FooterView::Browse, Focus::Sidebar, true);
    assert!(
        sidebar.contains(&"a-z")
            && sidebar.contains(&"s")
            && sidebar.contains(&"Alt+a-z")
            && sidebar.contains(&"r")
            && sidebar.contains(&"Tab")
    );
    let editor = keys(FooterView::Browse, Focus::Editor, true);
    assert!(editor.contains(&"Ctrl-J") && editor.contains(&"Alt-/"));
    let preview = keys(FooterView::Browse, Focus::Preview, true);
    assert!(preview.contains(&"↑↓") && preview.contains(&"e") && preview.contains(&"y"));
    assert!(preview.contains(&"gv") && preview.contains(&"|"));
    // No connection yet → the picker group, never the table group.
    let no_conn = keys(FooterView::Browse, Focus::Sidebar, false);
    assert!(no_conn.contains(&"c") && !no_conn.contains(&"/"));
    // R20–R22 overlays have their own groups (the footer used to leak the
    // page group while these owned the keyboard).
    assert_eq!(
        keys(FooterView::ImportPrompt, Focus::Preview, true),
        vec!["Enter", "Esc", "?"]
    );
    assert_eq!(
        keys(FooterView::ImportPlan, Focus::Preview, true),
        vec!["Enter", "m", "s", "↑↓", "Esc", "?"]
    );
    assert_eq!(
        keys(FooterView::ImportReport, Focus::Preview, true),
        vec!["Enter/Esc", "?"]
    );
    assert_eq!(
        keys(FooterView::ExportPicker, Focus::Preview, true),
        vec!["↑↓", "Enter", "1-6", "Esc", "?"]
    );
    assert_eq!(
        keys(FooterView::ExportPath, Focus::Preview, true),
        vec!["Enter", "Esc", "?"]
    );
    assert_eq!(
        keys(FooterView::RedisPrompt, Focus::Preview, true),
        vec!["Enter", "Esc", "?"]
    );
    // Every group ends with the pinned help key.
    for view in [
        FooterView::Help,
        FooterView::Confirm,
        FooterView::EditDialog,
        FooterView::Browse,
        FooterView::NewConn,
        FooterView::ColPicker,
        FooterView::Snippets,
        FooterView::ImportPrompt,
        FooterView::ImportPlan,
        FooterView::ImportReport,
        FooterView::ExportPicker,
        FooterView::ExportPath,
        FooterView::RedisPrompt,
        FooterView::RedisKeys,
        FooterView::RedisValue,
        FooterView::MongoDocs,
        FooterView::MongoDoc,
        FooterView::Popup,
        FooterView::RowPopup,
        FooterView::FilterPrompt,
        FooterView::DbPicker,
        FooterView::Recent,
        FooterView::TableJump,
        FooterView::Completion,
        FooterView::SnippetName,
        FooterView::ConnPicker,
        FooterView::TablePrompt,
        FooterView::ResultFilter,
        FooterView::ColFilter,
        FooterView::Search,
        FooterView::SearchInput,
    ] {
        let h = footer_hints_ctx(FooterCtx {
            view,
            focus: Focus::Preview,
            has_connection: true,
        });
        assert_eq!(h.last().unwrap().0, "?", "{view:?}");
    }
}

/// The footer group must follow the *keyboard* owner. Each overlay is set on
/// a real `App` and `footer_ctx` must name it, mirroring the key router.
#[test]
pub(crate) fn footer_view_tracks_every_overlay() {
    let mut app = test_app();
    // Fresh app: no connection, picker open.
    assert_eq!(footer_ctx(&app).view, FooterView::ConnPicker);

    app.help_open = true;
    assert_eq!(footer_ctx(&app).view, FooterView::Help);
    app.help_open = false;

    app.help_mini = true;
    assert_eq!(footer_ctx(&app).view, FooterView::HelpMini);
    // The mini sheet looks *through* itself to the surface below.
    assert_eq!(footer_ctx_inner(&app, false).view, FooterView::ConnPicker);
    app.help_mini = false;

    // R65: the in-data-view table switcher owns the footer like the recent
    // overlay does.
    app.table_jump_open = true;
    assert_eq!(footer_ctx(&app).view, FooterView::TableJump);
    app.table_jump_open = false;

    app.export_open = true;
    assert_eq!(footer_ctx(&app).view, FooterView::ExportPicker);
    app.export_open = false;

    app.export_path = Some(TextArea::default());
    assert_eq!(footer_ctx(&app).view, FooterView::ExportPath);
    app.export_path = None;

    app.import_prompt = Some(ImportPrompt {
        input: TextArea::default(),
        table: "t".into(),
        schema: String::new(),
        db: "d".into(),
        error: None,
    });
    assert_eq!(footer_ctx(&app).view, FooterView::ImportPrompt);
    app.import_prompt = None;

    app.import_report = Some(Box::new(ImportReport {
        table: "t".into(),
        schema: String::new(),
        mode: ImportMode::Append,
        total: 1,
        inserted: 1,
        skipped: Vec::new(),
        aborted: None,
        elapsed_ms: 1,
    }));
    assert_eq!(footer_ctx(&app).view, FooterView::ImportReport);
    app.import_report = None;

    app.redis_prompt = Some(RedisPrompt {
        kind: RedisPromptKind::Pattern,
        title: "t".into(),
        key_display: String::new(),
        key_raw: String::new(),
        field: String::new(),
        batch: Vec::new(),
        input: TextArea::default(),
    });
    assert_eq!(footer_ctx(&app).view, FooterView::RedisPrompt);
    app.redis_prompt = None;

    app.search_input = Some(TextArea::default());
    assert_eq!(footer_ctx(&app).view, FooterView::SearchInput);
    app.search_input = None;

    app.col_filter_prompt = Some(TextArea::default());
    assert_eq!(footer_ctx(&app).view, FooterView::ColFilter);
    app.col_filter_prompt = None;
    app.search_open = true;
    assert_eq!(footer_ctx(&app).view, FooterView::Search);
    app.search_open = false;

    // A drilled cell popup sits over the row popup, so the footer follows
    // the cell; the row popup alone gets its own group.
    app.row_popup = Some(row_popup_from_lines("r".into(), vec![]));
    assert_eq!(footer_ctx(&app).view, FooterView::RowPopup);
    app.cell_popup = Some(cell_popup_from_lines("c".into(), vec![]));
    assert_eq!(footer_ctx(&app).view, FooterView::Popup);
    app.cell_popup = None;
    app.row_popup = None;

    // `confirm` is checked first in `key`, so it must win over a browse
    // overlay that happens to be open underneath it.
    app.export_open = true;
    app.confirm = Some(Confirm {
        sql: "DELETE FROM t".into(),
        reasons: Vec::new(),
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: None,
        mongo: None,
    });
    assert_eq!(footer_ctx(&app).view, FooterView::Confirm);
    app.confirm = None;
    app.export_open = false;
}

#[test]
pub(crate) fn ui_switches_between_chinese_and_english() {
    use ui_text::{t_lang, tf_lang, Lang};
    // Confirm dialog, footer help, and a templated status message.
    assert_eq!(t_lang(" ⚠ 危险操作确认 ", Lang::Zh), " ⚠ 危险操作确认 ");
    assert_eq!(
        t_lang(" ⚠ 危险操作确认 ", Lang::En),
        " ⚠ Confirm dangerous operation "
    );
    assert_eq!(t_lang("帮助", Lang::Zh), "帮助");
    assert_eq!(t_lang("帮助", Lang::En), "Help");
    assert_eq!(t_lang("执行", Lang::En), "execute");
    assert_eq!(t_lang("取消", Lang::En), "cancel");
    assert_eq!(
        tf_lang("搜索「{}」· {} 行命中", &[&"x", &3], Lang::En),
        "Search \"x\" · 3 rows matched"
    );
    assert_eq!(
        tf_lang("搜索「{}」· {} 行命中", &[&"x", &3], Lang::Zh),
        "搜索「x」· 3 行命中"
    );
    // Escaped braces survive template substitution.
    assert_eq!(
        tf_lang(
            "mongo parse: {} (例: db.col.find({{}}))",
            &[&"boom"],
            Lang::En
        ),
        "mongo parse: boom (e.g. db.col.find({}))"
    );
}

#[test]
pub(crate) fn language_detection_reads_the_environment() {
    use ui_text::{detect_lang_from, Lang};
    assert_eq!(detect_lang_from(None, Some("zh_CN.UTF-8")), Lang::Zh);
    assert_eq!(detect_lang_from(None, Some("en_US.UTF-8")), Lang::En);
    // DBXT_LANG wins over the locale.
    assert_eq!(detect_lang_from(Some("en"), Some("zh_CN.UTF-8")), Lang::En);
    assert_eq!(detect_lang_from(Some("zh"), Some("en_US.UTF-8")), Lang::Zh);
    // Nothing set → built-in default is Chinese.
    assert_eq!(detect_lang_from(None, None), Lang::Zh);
}

/// R44: the kernel's v0.6.27 secret-store codes must become an actionable
/// hint, while every other error passes through untouched.
#[test]
pub(crate) fn secret_store_errors_get_actionable_hints() {
    let migration = humanize_backend_error(
        "DATA_MIGRATION_REQUIRED: open DBX Desktop or Web to complete the data security upgrade",
    );
    assert!(migration.contains("桌面端"), "{migration}");
    assert!(migration.contains("DBX_SECRET_KEY_FILE"), "{migration}");

    let unavailable = humanize_backend_error(
        "SECRET_KEY_UNAVAILABLE: this process cannot read the DBX data encryption key",
    );
    assert!(unavailable.contains("DBX_SECRET_KEY_FILE"), "{unavailable}");

    let write = humanize_backend_error("KEY_PROVIDER_UNAVAILABLE");
    assert!(write.contains("钥匙串"), "{write}");

    // A non-secret error is returned verbatim (prefix and all).
    assert_eq!(
        humanize_backend_error("query: syntax error near FROM"),
        "query: syntax error near FROM"
    );
}

#[test]
pub(crate) fn text_table_covers_every_key() {
    for k in ui_text::ALL_KEYS {
        let en = ui_text::t_lang(k, ui_text::Lang::En);
        assert_ne!(en, *k, "missing English translation for {k:?}");
        assert!(!en.is_empty(), "empty English for {k:?}");
        assert!(
            k.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "key without CJK: {k:?}"
        );
    }
    assert!(ui_text::ALL_KEYS.len() > 300);
}

/// Scan the real source for `t("…")` / `tf("…")` call sites and require an
/// English translation for each Chinese literal. The `ALL_KEYS` list is only
/// as good as its manual upkeep; this reads the call sites themselves, so a
/// new overlay added without a table entry fails the build instead of
/// silently falling back to Chinese under `DBXT_LANG=en`.
#[test]
pub(crate) fn every_call_site_has_english() {
    // R69: the crate is no longer a single file, so scan every source
    // file that used to make up `main.rs`.
    let src = [
        include_str!("../main.rs"),
        include_str!("../prelude.rs"),
        include_str!("../state.rs"),
        include_str!("../transfer.rs"),
        include_str!("../textutil.rs"),
        include_str!("../csv_io.rs"),
        include_str!("../redis.rs"),
        include_str!("../tui_config.rs"),
        include_str!("../sqlfmt.rs"),
        include_str!("../search.rs"),
        include_str!("../input.rs"),
        include_str!("../jsonview.rs"),
        include_str!("../sidebar.rs"),
        include_str!("../editor.rs"),
        include_str!("../mongo.rs"),
        include_str!("../results.rs"),
        include_str!("../nav.rs"),
        include_str!("../diffui.rs"),
        include_str!("../filter.rs"),
        include_str!("../rowops.rs"),
        include_str!("../runner.rs"),
        include_str!("../parity.rs"),
        include_str!("../render.rs"),
        include_str!("../render_overlay.rs"),
        include_str!("../render_help.rs"),
        include_str!("../tests/mod.rs"),
        include_str!("../tests/part1.rs"),
        include_str!("../tests/part2.rs"),
        include_str!("../tests/part3.rs"),
    ]
    .join("\n");
    let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let chars: Vec<char> = src.chars().collect();
    let mut missing: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        // A `t(` or `tf(` call: `t` not part of a longer identifier.
        let prev_ok = i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
        let after = if chars[i] == 't' && chars.get(i + 1) == Some(&'(') {
            Some(i + 2)
        } else if chars[i] == 't'
            && chars.get(i + 1) == Some(&'f')
            && chars.get(i + 2) == Some(&'(')
        {
            Some(i + 3)
        } else {
            None
        };
        if let Some(mut j) = after {
            if prev_ok {
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'"') {
                    let mut lit = String::new();
                    let mut k = j + 1;
                    while k < chars.len() {
                        match chars[k] {
                            '\\' if k + 1 < chars.len() => {
                                let e = chars[k + 1];
                                lit.push(match e {
                                    'n' => '\n',
                                    't' => '\t',
                                    'r' => '\r',
                                    other => other,
                                });
                                k += 2;
                            }
                            '"' => break,
                            c => {
                                lit.push(c);
                                k += 1;
                            }
                        }
                    }
                    checked += 1;
                    let leaked: &'static str = Box::leak(lit.clone().into_boxed_str());
                    if cjk(&lit) && ui_text::t_lang(leaked, ui_text::Lang::En) == lit.as_str() {
                        missing.push(lit);
                    }
                    i = k;
                }
            }
        }
        i += 1;
    }
    // Guard against a broken scanner silently checking nothing.
    assert!(checked > 400, "scanner only saw {checked} call sites");
    assert!(
        missing.is_empty(),
        "{} t()/tf() literals have no English translation: {missing:#?}",
        missing.len()
    );
}

/// The `?` help cheat-sheet is data, not literal `t()` calls, so it needs its
/// own guard: every description must translate, and every keycap must stay
/// language-neutral (the key column is printed verbatim in both languages).
#[test]
pub(crate) fn help_rows_are_translated_and_keycaps_are_neutral() {
    let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    for (key, desc) in HELP_ROWS {
        // The key column is rendered through `t` as well, so a CJK label
        // (a section header or a descriptive row) must have an English form.
        if cjk(key) {
            assert_ne!(
                ui_text::t_lang(key, ui_text::Lang::En),
                *key,
                "help key {key:?} has no English translation"
            );
        }
        if !desc.is_empty() {
            assert_ne!(
                ui_text::t_lang(desc, ui_text::Lang::En),
                *desc,
                "help description {desc:?} has no English translation"
            );
        }
    }
}

#[test]
pub(crate) fn version_picks_injected_then_cargo() {
    // An injected release tag wins over Cargo.toml...
    assert_eq!(pick_version(Some("0.0.1"), "0.1.0"), "0.0.1");
    // ...a missing or empty injection falls back to Cargo.toml.
    assert_eq!(pick_version(None, "0.1.0"), "0.1.0");
    assert_eq!(pick_version(Some(""), "0.1.0"), "0.1.0");
}

#[test]
pub(crate) fn version_output_is_a_parseable_semver_line() {
    // cmd/install.sh reads this line, so the format is a contract: exactly
    // one `dbxt ` prefix followed by `x.y.z` (an optional pre-release
    // suffix such as `0.2.0-rc1` is allowed).
    let line = version_line();
    let ver = line
        .strip_prefix("dbxt ")
        .unwrap_or_else(|| panic!("unexpected --version format: {line:?}"));
    let core = ver.split(['-', '+']).next().unwrap();
    let parts: Vec<&str> = core.split('.').collect();
    assert_eq!(parts.len(), 3, "not x.y.z: {ver:?}");
    for p in parts {
        assert!(
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
            "non-numeric component in {ver:?}"
        );
    }
    // The reported value is the one actually compiled in.
    assert_eq!(ver, dbxt_version());
    assert!(!dbxt_version().is_empty());
}

#[test]
pub(crate) fn help_text_matches_the_real_cli() {
    // The non-interactive `--help` is a contract too: it must name the
    // store argument, both options and the docs URL, and end in a newline
    // (it is written verbatim to stdout).
    let help = help_text();
    assert!(help.starts_with(&format!("dbxt {} — ", dbxt_version())));
    assert!(help.contains("dbxt [DBX_STORE]"));
    assert!(help.contains("DBX_STORE"));
    assert!(help.contains("-h, --help"));
    assert!(help.contains("-V, --version"));
    assert!(help.contains("https://github.com/vst93/dbxt"));
    assert!(help.ends_with('\n'));
    // Exactly the two long options the parser actually accepts.
    assert_eq!(
        help.matches("--").count(),
        2,
        "unexpected --help drift: {help:?}"
    );
}

#[test]
pub(crate) fn unknown_option_and_no_tty_have_translations() {
    use ui_text::{t_lang, tf_lang, Lang};
    assert_eq!(
        tf_lang("未知选项: {}", &[&"--foo"], Lang::En),
        "Unknown option: --foo"
    );
    assert_eq!(t_lang("用法", Lang::En), "Usage");
    assert_eq!(
        t_lang(
            "stdout 不是终端，无法启动 TUI（--help / --version 可在管道中使用）",
            Lang::En
        ),
        "stdout is not a terminal, cannot start the TUI (--help / --version work over a pipe)"
    );
}

// ─── Redis / Mongo helpers ───────────────────────────────────────────────

pub(crate) fn blob(s: &str) -> RedisBlob {
    RedisBlob {
        raw_base64: base64_encode(s.as_bytes()),
        encoding: RedisBlobEncoding::Utf8,
    }
}

#[test]
pub(crate) fn b64_decode_roundtrips_and_rejects_garbage() {
    assert_eq!(b64_decode("YXBwOnVzZXI=").unwrap(), b"app:user");
    assert_eq!(b64_decode("QWRh").unwrap(), b"Ada");
    // Whitespace is tolerated, padding may be omitted.
    assert_eq!(b64_decode(" QW Rh ").unwrap(), b"Ada");
    // A stray character is not silently dropped.
    assert!(b64_decode("!!!!").is_none());
    assert!(b64_decode("").unwrap().is_empty());
}

#[test]
pub(crate) fn redis_blob_text_decodes_utf8_and_hexes_binary() {
    assert_eq!(redis_blob_text(&blob("hello world")), "hello world");
    let binary = RedisBlob {
        raw_base64: base64_encode(&[0x00, 0xff, 0x10]),
        encoding: RedisBlobEncoding::Binary,
    };
    assert_eq!(redis_blob_text(&binary), "0x00ff10");
    // A UTF-8 blob that is not valid UTF-8 must not panic.
    let broken = RedisBlob {
        raw_base64: base64_encode(&[0xff, 0xfe]),
        encoding: RedisBlobEncoding::Utf8,
    };
    assert!(redis_blob_text(&broken).starts_with("<binary"));
}

#[test]
pub(crate) fn redis_value_view_renders_each_type() {
    let hash = RedisValue {
        key_display: "app:user".into(),
        key_raw: "YXBwOnVzZXI=".into(),
        ttl: -1,
        redis_type: "hash".into(),
        data: RedisValueData::Hash {
            items: vec![
                dbx_core::db::redis_driver::RedisHashItem {
                    field: blob("name"),
                    value: blob("Ada"),
                    field_ttl: Some(-1),
                },
                dbx_core::db::redis_driver::RedisHashItem {
                    field: blob("role"),
                    value: blob("admin"),
                    field_ttl: Some(60),
                },
            ],
            total: 2,
            scan_cursor: None,
        },
    };
    let view = redis_value_view(hash);
    assert_eq!(view.grid.columns, vec!["field", "value", "TTL"]);
    assert_eq!(view.grid.rows.len(), 2);
    assert_eq!(view.grid.rows[0][0].text(), "name");
    assert_eq!(view.grid.rows[0][1].text(), "Ada");
    assert!(view.grid.rows[0][2].is_null());
    assert_eq!(view.grid.rows[1][2].text(), "60s");
    assert_eq!(view.row_keys, vec!["name", "role"]);

    let string = RedisValue {
        key_display: "k".into(),
        key_raw: "aw==".into(),
        ttl: 120,
        redis_type: "string".into(),
        data: RedisValueData::String {
            content: blob("hello world"),
            total_bytes: Some(11),
            truncated: false,
        },
    };
    let view = redis_value_view(string);
    assert_eq!(view.grid.columns, vec!["value"]);
    assert_eq!(view.grid.rows[0][0].text(), "hello world");
    assert!(view.grid.note.contains("11"));

    let zset = RedisValue {
        key_display: "z".into(),
        key_raw: "eg==".into(),
        ttl: -1,
        redis_type: "zset".into(),
        data: RedisValueData::Zset {
            items: vec![dbx_core::db::redis_driver::RedisZsetItem {
                score: "1.5".into(),
                member: blob("alice"),
            }],
            total: 1,
            scan_cursor: None,
        },
    };
    let view = redis_value_view(zset);
    assert_eq!(view.grid.columns, vec!["score", "member"]);
    assert_eq!(view.grid.rows[0][0].text(), "1.5");
    assert_eq!(view.grid.rows[0][1].text(), "alice");
}

#[test]
pub(crate) fn redis_prompt_commands_quote_and_shape() {
    assert_eq!(
        redis_prompt_command(RedisPromptKind::Ttl, "app:user", "", "120"),
        "EXPIRE \"app:user\" 120"
    );
    assert_eq!(
        redis_prompt_command(RedisPromptKind::Rename, "a", "", "b"),
        "RENAME \"a\" \"b\""
    );
    assert_eq!(
        redis_prompt_command(RedisPromptKind::StringValue, "k", "", "hi there"),
        "SET \"k\" \"hi there\""
    );
    assert_eq!(
        redis_prompt_command(RedisPromptKind::HashField, "k", "name", "Ada"),
        "HSET \"k\" \"name\" \"Ada\""
    );
    // Quotes and backslashes are escaped so the tokenizer sees one argument.
    assert_eq!(
        redis_prompt_command(RedisPromptKind::StringValue, "k", "", "a\"b\\c"),
        "SET \"k\" \"a\\\"b\\\\c\""
    );
    assert!(redis_prompt_command(RedisPromptKind::Pattern, "", "", "*").is_empty());
}

#[test]
pub(crate) fn mongo_docs_grid_unions_keys_with_id_first() {
    let docs = vec![
        serde_json::json!({"name": "Ada", "age": 36}),
        serde_json::json!({"_id": "x", "name": "Bob", "city": "Paris"}),
    ];
    let grid = mongo_docs_grid(&docs);
    assert_eq!(grid.columns[0], "_id");
    assert!(grid.columns.contains(&"name".to_string()));
    assert!(grid.columns.contains(&"city".to_string()));
    // A missing field is NULL, not the empty string.
    let row0 = &grid.rows[0];
    let id_idx = grid.columns.iter().position(|c| c == "_id").unwrap();
    assert!(row0[id_idx].is_null());
    assert_eq!(grid.rows.len(), 2);
}

#[test]
pub(crate) fn backend_kind_is_inferred_from_the_connection_type() {
    let mk = |t: &str| {
        new_connection_config(
            "id".to_string(),
            t.to_string(),
            parse_database_type(t).unwrap(),
            "h".to_string(),
            1,
            "u".to_string(),
            String::new(),
            None,
            false,
            None,
        )
        .unwrap()
    };
    assert_eq!(backend_for_connection(&mk("redis")), Backend::Redis);
    assert_eq!(backend_for_connection(&mk("mongodb")), Backend::Mongo);
    assert_eq!(backend_for_connection(&mk("mysql")), Backend::Sql);
    assert_eq!(backend_for_connection(&mk("postgres")), Backend::Sql);
}

#[test]
pub(crate) fn redis_ttl_and_type_badges() {
    assert_eq!(redis_ttl_label(-1), "永不过期");
    assert_eq!(redis_ttl_label(-2), "不存在");
    assert_eq!(redis_ttl_label(90), "90s");
    assert_eq!(redis_type_badge("string").0, "S");
    assert_eq!(redis_type_badge("hash").0, "H");
    assert_eq!(redis_type_badge("list").0, "L");
    assert_eq!(redis_type_badge("zset").0, "Z");
    assert_eq!(redis_type_badge("stream").0, "X");
    assert_eq!(redis_type_badge("weird").0, "?");
}

// ── R21: Redis batch key operations ──

pub(crate) fn rk(raw: &str, display: &str) -> RedisKeyInfo {
    RedisKeyInfo {
        key_display: display.to_string(),
        key_raw: raw.to_string(),
        key_type: "string".to_string(),
        ttl: -1,
        size: 0,
        value_preview: String::new(),
    }
}

#[test]
pub(crate) fn redis_multi_select_toggles_and_ranges() {
    let keys = vec![
        rk("a", "app:1"),
        rk("b", "app:2"),
        rk("c", "app:3"),
        rk("d", "app:4"),
    ];
    let mut sel: HashSet<String> = HashSet::new();
    let mut anchor: Option<usize> = None;
    redis_selection_toggle(&mut sel, &mut anchor, &keys, 0);
    assert!(sel.contains("a"));
    assert_eq!(anchor, Some(0));
    redis_selection_toggle(&mut sel, &mut anchor, &keys, 0);
    assert!(sel.is_empty(), "space toggles off");
    // A range extends from the anchor, additively.
    redis_selection_toggle(&mut sel, &mut anchor, &keys, 1);
    redis_selection_range(&mut sel, &mut anchor, &keys, 3);
    assert_eq!(sel.len(), 3);
    assert!(sel.contains("b") && sel.contains("c") && sel.contains("d"));
    assert_eq!(anchor, Some(1));
    // A reverse range keeps everything and adds the earlier keys.
    redis_selection_range(&mut sel, &mut anchor, &keys, 0);
    assert!(sel.contains("a"));
    // Targets preserve list order, not set order.
    let targets = redis_selection_targets(&sel, &keys, None);
    assert_eq!(
        targets.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
}

#[test]
pub(crate) fn redis_select_all_and_target_fallback() {
    let keys = vec![rk("a", "app:1"), rk("b", "app:2")];
    let mut sel: HashSet<String> = HashSet::new();
    let mut anchor: Option<usize> = None;
    assert!(!redis_selection_is_all(&sel, &keys));
    redis_selection_all(&mut sel, &mut anchor, &keys);
    assert!(redis_selection_is_all(&sel, &keys));
    assert_eq!(anchor, Some(0));
    // An empty selection falls back to the focused key.
    let empty: HashSet<String> = HashSet::new();
    assert_eq!(
        redis_selection_targets(&empty, &keys, Some(1)),
        vec![("b".to_string(), "app:2".to_string())]
    );
    assert!(redis_selection_targets(&empty, &keys, None).is_empty());
}

#[test]
pub(crate) fn redis_batch_del_chunks_and_quotes() {
    let displays: Vec<String> = (0..250).map(|i| format!("app:{i}")).collect();
    let cmds = redis_batch_del_commands(&displays);
    assert_eq!(cmds.len(), 3, "100 + 100 + 50");
    assert!(cmds[0].starts_with("DEL \"app:0\" \"app:1\""));
    assert!(cmds[2].ends_with("\"app:249\""));
    // Quotes / backslashes survive the redis-cli tokenizer.
    assert_eq!(
        redis_batch_del_commands(&["a\"b\\c".to_string()]),
        vec!["DEL \"a\\\"b\\\\c\"".to_string()]
    );
    assert!(redis_batch_del_commands(&[]).is_empty());
}

#[test]
pub(crate) fn redis_prefix_rename_plan_filters_and_rewrites() {
    let displays = vec![
        "app:1".to_string(),
        "other:2".to_string(),
        "app:3".to_string(),
    ];
    assert_eq!(
        redis_prefix_rename_plan(&displays, "app:", "new:"),
        vec![
            ("app:1".to_string(), "new:1".to_string()),
            ("app:3".to_string(), "new:3".to_string()),
        ]
    );
    // A same-prefix replacement is a no-op and is dropped.
    assert!(redis_prefix_rename_plan(&displays, "app:", "app:").is_empty());
    // An empty old prefix prepends to every key.
    assert_eq!(redis_prefix_rename_plan(&displays, "", "x").len(), 3);
    let cmds = redis_batch_rename_commands(&displays, "app:", "new:");
    assert_eq!(
        cmds,
        vec!["RENAME \"app:1\" \"new:1\"", "RENAME \"app:3\" \"new:3\""]
    );
}

#[test]
pub(crate) fn redis_batch_ttl_validates_and_generates() {
    let displays = vec!["a".to_string(), "b".to_string()];
    assert_eq!(
        redis_batch_ttl_commands(&displays, "60"),
        vec!["EXPIRE \"a\" 60", "EXPIRE \"b\" 60"]
    );
    assert!(redis_batch_ttl_commands(&displays, "nope").is_empty());
    assert!(redis_batch_ttl_commands(&displays, "1.5").is_empty());
}

#[test]
pub(crate) fn redis_batch_confirm_flow_requires_typed_count_only_for_select_all_delete() {
    let targets = vec![("a".into(), "app:1".into()), ("b".into(), "app:2".into())];
    let plan = redis_plan_batch(RedisBatchKind::Delete, &targets, false, "").unwrap();
    assert_eq!(plan.typed_confirm, None);
    assert_eq!(plan.commands.len(), 1);
    // Selecting every loaded key escalates to a typed re-confirmation.
    let plan = redis_plan_batch(RedisBatchKind::Delete, &targets, true, "").unwrap();
    assert_eq!(plan.typed_confirm, Some(2));
    // TTL / rename never demand the typed confirm, even on a select-all.
    assert_eq!(
        redis_plan_batch(RedisBatchKind::Ttl, &targets, true, "30")
            .unwrap()
            .typed_confirm,
        None
    );
    assert_eq!(
        redis_plan_batch(RedisBatchKind::RenamePrefix, &targets, true, "app:=new:")
            .unwrap()
            .typed_confirm,
        None
    );
    // Invalid arguments surface an error instead of a broken command.
    assert!(redis_plan_batch(RedisBatchKind::Ttl, &targets, false, "abc").is_err());
    assert!(redis_plan_batch(RedisBatchKind::RenamePrefix, &targets, false, "no-equals").is_err());
}

// ── R21: MongoDB document CRUD ──

#[test]
pub(crate) fn mongo_id_arg_preserves_string_object_id_shape() {
    // A real ObjectId arrives as {"$oid": ...} and is passed as its hex form.
    assert_eq!(
        mongo_id_arg(&serde_json::json!({"$oid": "507f1f77bcf86cd799439011"})),
        "507f1f77bcf86cd799439011"
    );
    // A genuine 24-hex string _id must be marked so it is not reinterpreted.
    assert_eq!(
        mongo_id_arg(&serde_json::json!("507f1f77bcf86cd799439011")),
        "__dbx_mongo_string_id__\"507f1f77bcf86cd799439011\""
    );
    assert_eq!(
        mongo_id_arg(&serde_json::json!("customer-42")),
        "customer-42"
    );
    assert_eq!(mongo_id_arg(&serde_json::json!(42)), "42");
    assert_eq!(
        mongo_id_arg(&serde_json::json!({"$numberLong": "2048938405781032962"})),
        "{\"$numberLong\":\"2048938405781032962\"}"
    );
    assert_eq!(mongo_id_label(&serde_json::json!({"$oid": "abc"})), "abc");
    assert_eq!(mongo_id_label(&serde_json::json!("plain")), "plain");
}

#[test]
pub(crate) fn mongo_doc_diff_reports_top_level_changes() {
    let old = serde_json::json!({"_id": {"$oid": "x"}, "name": "Ada", "age": 30, "gone": true});
    let new = serde_json::json!({"_id": {"$oid": "x"}, "name": "Grace", "age": 30, "added": 1});
    let diff = mongo_doc_diff(&old, &new, 10);
    assert!(diff
        .iter()
        .any(|l| l.contains("name") && l.contains("Ada") && l.contains("Grace")));
    assert!(diff.iter().any(|l| l.starts_with("+ added")));
    assert!(diff.iter().any(|l| l.starts_with("- gone")));
    assert!(
        !diff.iter().any(|l| l.contains("age")),
        "unchanged fields are omitted"
    );
    assert!(
        !diff.iter().any(|l| l.contains("_id")),
        "_id is never part of the diff"
    );
    assert!(mongo_doc_diff(&old, &old, 10).is_empty());
}

// ── R27: SSH tunnel ──

pub(crate) fn password_ssh_form() -> ConnForm {
    ConnForm {
        name: "prod".into(),
        host: "10.0.0.5".into(),
        port: "3306".into(),
        ssh_enabled: true,
        ssh_host: "jump.example.com".into(),
        ssh_port: "2222".into(),
        ssh_user: "ops".into(),
        ssh_auth: SshAuth::Password,
        ssh_password: "s3cret".into(),
        ..ConnForm::default()
    }
}

/// The serialized layer must match the kernel's `TransportLayerConfig`
/// shape exactly (`{"type":"ssh", …}`) so the desktop can read it back.
#[test]
pub(crate) fn ssh_layer_serializes_to_the_kernel_shape() {
    let layer = build_ssh_layer(&password_ssh_form()).unwrap().unwrap();
    let v = serde_json::to_value(TransportLayerConfig::Ssh(layer)).unwrap();
    assert_eq!(v["type"], "ssh");
    assert_eq!(v["enabled"], true);
    assert_eq!(v["host"], "jump.example.com");
    assert_eq!(v["port"], 2222);
    assert_eq!(v["user"], "ops");
    assert_eq!(v["password"], "s3cret");
    assert_eq!(v["auth_method"], "password");
    assert_eq!(v["key_path"], "");
    assert_eq!(v["use_ssh_agent"], false);
    // Kernel defaults: 5 s connect timeout, no LAN exposure.
    assert_eq!(v["connect_timeout_secs"], 5);
    assert_eq!(v["expose_lan"], false);
    // Empty / false optional fields are skipped, matching the desktop writer.
    assert!(v.get("profile_id").is_none());
    assert!(v.get("allow_exec_channel_proxy").is_none());
}

#[test]
pub(crate) fn ssh_auth_method_maps_key_and_agent() {
    let mut f = password_ssh_form();
    f.ssh_auth = SshAuth::Key;
    f.ssh_key_path = "~/.ssh/id_ed25519".into();
    f.ssh_key_passphrase = "pp".into();
    let layer = build_ssh_layer(&f).unwrap().unwrap();
    assert_eq!(layer.auth_method, "key");
    assert_eq!(layer.key_path, "~/.ssh/id_ed25519");
    assert_eq!(layer.key_passphrase, "pp");
    assert!(
        layer.password.is_empty(),
        "key auth must not carry a password"
    );
    assert!(!layer.use_ssh_agent);

    f.ssh_auth = SshAuth::Agent;
    f.ssh_agent_sock = "~/.ssh/agent.sock".into();
    let layer = build_ssh_layer(&f).unwrap().unwrap();
    assert_eq!(layer.auth_method, "agent");
    assert!(layer.use_ssh_agent);
    assert_eq!(layer.ssh_agent_sock_path, "~/.ssh/agent.sock");
    assert!(layer.key_path.is_empty() && layer.password.is_empty());
}

#[test]
pub(crate) fn ssh_form_validation_requires_host_user_and_credential() {
    let mut f = ConnForm {
        ssh_enabled: true,
        ..ConnForm::default()
    };
    assert!(build_ssh_layer(&f).is_err(), "host is required");
    f.ssh_host = "jump".into();
    assert!(build_ssh_layer(&f).is_err(), "user is required");
    f.ssh_user = "ops".into();
    assert!(
        build_ssh_layer(&f).is_err(),
        "password auth needs a password"
    );
    f.ssh_password = "pw".into();
    assert!(build_ssh_layer(&f).unwrap().is_some());
    // Port defaults to 22 when blank; a disabled tunnel yields no layer.
    f.ssh_port = String::new();
    assert_eq!(build_ssh_layer(&f).unwrap().unwrap().port, 22);
    f.ssh_enabled = false;
    assert!(build_ssh_layer(&f).unwrap().is_none());
}

/// A `~/.ssh/config` alias is stored verbatim — the kernel resolves it
/// (including `ProxyJump`) at connect time, not the TUI.
#[test]
pub(crate) fn ssh_alias_host_is_stored_verbatim() {
    let mut f = password_ssh_form();
    f.ssh_host = "prod-bastion".into();
    assert_eq!(build_ssh_layer(&f).unwrap().unwrap().host, "prod-bastion");
}

/// A saved tunnel must survive a JSON round-trip through the connection
/// config (the desktop's storage format) and prefill the edit form.
#[test]
pub(crate) fn ssh_tunnel_round_trips_and_prefills_the_form() {
    let mut cfg = test_conn("mysql");
    cfg.host = "db.internal".into();
    cfg.port = 3306;
    cfg.transport_layers = vec![TransportLayerConfig::Ssh(SshTunnelConfig {
        id: "layer-1".into(),
        name: "bastion".into(),
        enabled: true,
        host: "jump".into(),
        port: 22,
        user: "ops".into(),
        password: "pw".into(),
        key_path: String::new(),
        key_passphrase: String::new(),
        connect_timeout_secs: 5,
        expose_lan: false,
        use_ssh_agent: false,
        ssh_agent_sock_path: String::new(),
        auth_method: "password".into(),
        allow_exec_channel_proxy: false,
        profile_id: String::new(),
    })];
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ConnectionConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.transport_layers, cfg.transport_layers);

    let form = form_from_connection(&back, back.name.clone(), Some(back.id.clone()));
    assert!(form.ssh_enabled);
    assert_eq!(form.ssh_host, "jump");
    assert_eq!(form.ssh_port, "22");
    assert_eq!(form.ssh_user, "ops");
    assert_eq!(form.ssh_auth, SshAuth::Password);
    assert_eq!(form.ssh_password, "pw");
    assert_eq!(form.edit_id.as_deref(), Some("id-mysql"));
    // Rebuilding from the prefilled form keeps the tunnel usable.
    let rebuilt = build_ssh_layer(&form).unwrap().unwrap();
    assert_eq!(rebuilt.host, "jump");
    assert_eq!(rebuilt.auth_method, "password");
}

/// A legacy layer with an empty `auth_method` is inferred from which
/// credential field is populated.
#[test]
pub(crate) fn ssh_auth_infers_from_legacy_layer_fields() {
    let mut layer = SshTunnelConfig {
        id: String::new(),
        name: String::new(),
        enabled: true,
        host: "jump".into(),
        port: 22,
        user: "ops".into(),
        password: String::new(),
        key_path: "~/.ssh/id_rsa".into(),
        key_passphrase: String::new(),
        connect_timeout_secs: 5,
        expose_lan: false,
        use_ssh_agent: false,
        ssh_agent_sock_path: String::new(),
        auth_method: String::new(),
        allow_exec_channel_proxy: false,
        profile_id: String::new(),
    };
    assert_eq!(SshAuth::from_layer(&layer), SshAuth::Key);
    layer.key_path.clear();
    layer.use_ssh_agent = true;
    assert_eq!(SshAuth::from_layer(&layer), SshAuth::Agent);
    layer.use_ssh_agent = false;
    layer.password = "pw".into();
    assert_eq!(SshAuth::from_layer(&layer), SshAuth::Password);
}

#[test]
pub(crate) fn ssh_error_classification_distinguishes_categories() {
    assert!(classify_ssh_auth_error(
        "SSH password auth failed: rejected (remaining_methods=...)"
    ));
    assert!(classify_ssh_auth_error(
        "SSH authentication failed: both key and password were rejected"
    ));
    assert!(classify_ssh_auth_error(
        "No SSH password or key provided, and ssh-agent has no identities"
    ));
    assert!(!classify_ssh_auth_error(
        "SSH connection failed: Connection refused (os error 111)"
    ));
    assert!(classify_ssh_host_error(
        "SSH connection failed: Connection refused (os error 111)"
    ));
    assert!(classify_ssh_host_error("SSH connection timed out (5s)"));
    assert!(!classify_ssh_host_error(
        "SSH password auth failed: rejected (remaining_methods=...)"
    ));
    // A post-handshake driver teardown means the far-side port is closed…
    assert!(classify_ssh_remote_error(
        "MySQL connection failed: Input/output error: connection closed"
    ));
    assert!(classify_ssh_remote_error("unexpected EOF"));
    // …but an SSH handshake failure is never a remote-database problem.
    assert!(!classify_ssh_remote_error(
        "SSH connection failed: Connection reset by peer (os error 104)"
    ));
    assert!(!classify_ssh_remote_error(
        "SSH authentication failed: both key and password were rejected"
    ));
}

#[test]
pub(crate) fn form_rows_expand_with_ssh_and_auth_method() {
    let mut f = ConnForm::default();
    let base = form_rows(&f);
    assert!(base.iter().any(|(r, _)| *r == FormRow::SshEnabled));
    assert!(!base.iter().any(|(r, _)| *r == FormRow::SshHost));
    assert_eq!(base.last().map(|(r, _)| *r), Some(FormRow::Save));

    f.ssh_enabled = true;
    let password = form_rows(&f);
    assert!(password.iter().any(|(r, _)| *r == FormRow::SshHost));
    assert!(password.iter().any(|(r, _)| *r == FormRow::SshPassword));
    assert!(!password.iter().any(|(r, _)| *r == FormRow::SshKeyPath));

    f.ssh_auth = SshAuth::Key;
    let key = form_rows(&f);
    assert!(key.iter().any(|(r, _)| *r == FormRow::SshKeyPath));
    assert!(key.iter().any(|(r, _)| *r == FormRow::SshKeyPassphrase));
    assert!(!key.iter().any(|(r, _)| *r == FormRow::SshPassword));

    f.ssh_auth = SshAuth::Agent;
    let agent = form_rows(&f);
    assert!(agent.iter().any(|(r, _)| *r == FormRow::SshAgentSock));
    assert_eq!(agent.last().map(|(r, _)| *r), Some(FormRow::Save));
}

/// R51: the form fields follow the real creation flow — type first (so its
/// default port lands before the cursor reaches `port`), then name, host,
/// port, credentials, database.
#[test]
pub(crate) fn form_field_order_matches_the_creation_flow() {
    let f = ConnForm::default();
    let order: Vec<FormRow> = form_rows(&f).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        &order[..7],
        &[
            FormRow::DbType,
            FormRow::Name,
            FormRow::Host,
            FormRow::Port,
            FormRow::Username,
            FormRow::Password,
            FormRow::Database,
        ]
    );
    // The SSH rows sit after the base fields, before the save row.
    assert!(
        order.iter().position(|r| *r == FormRow::SshEnabled)
            > order.iter().position(|r| *r == FormRow::Color)
    );
    // R58: the query-timeout row sits right after `database`, before `ssl`.
    assert_eq!(order[7], FormRow::QueryTimeout);
    assert_eq!(order[8], FormRow::Ssl);
}

/// R51: the port field is derived from the selected database type, and a
/// hand-entered port is never overwritten by a later type change.
#[test]
pub(crate) fn default_port_tracks_type_until_the_user_edits_it() {
    let mut f = ConnForm::default();
    // A fresh form opens on mysql with its default port already shown.
    assert_eq!(f.port, "3306");
    for (ty, port) in [
        ("postgres", "5432"),
        ("redis", "6379"),
        ("mongodb", "27017"),
        ("mysql", "3306"),
    ] {
        f.db_type = ty.into();
        apply_default_port(&mut f);
        assert_eq!(f.port, port, "default port for {ty}");
    }
    // Local-file drivers have no port: the field stays blank.
    f.db_type = "sqlite".into();
    apply_default_port(&mut f);
    assert_eq!(f.port, "");
    // Once the user types a port, it is pinned across type changes.
    f.port = "15432".into();
    f.port_touched = true;
    f.db_type = "mysql".into();
    apply_default_port(&mut f);
    assert_eq!(f.port, "15432");
}

/// R51: driving the real form keys, choosing a type refills the default
/// port while the field is untouched, and typing a port marks it pinned.
#[test]
pub(crate) fn form_keys_fill_default_port_and_pin_a_typed_one() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.page = Page::NewConn;
    app.form = ConnForm::default();
    let idx = |app: &App, r: FormRow| {
        form_rows(&app.form)
            .iter()
            .position(|(x, _)| *x == r)
            .unwrap()
    };
    let press = |app: &mut App, code: KeyCode| {
        form_key(app, &tx, KeyEvent::new(code, KeyModifiers::NONE));
    };
    // Retype `db_type` to postgres: its port follows.
    let dbtype_row = idx(&app, FormRow::DbType);
    app.form.field = dbtype_row;
    press(&mut app, KeyCode::Enter);
    for _ in 0..5 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "postgres".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.form.db_type, "postgres");
    assert_eq!(app.form.port, "5432");
    // Edit the port: it becomes the user's own value.
    let port_row = idx(&app, FormRow::Port);
    app.form.field = port_row;
    press(&mut app, KeyCode::Enter);
    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "15432".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.form.port, "15432");
    assert!(app.form.port_touched);
    // Changing the type now must not clobber the typed port.
    let dbtype_row = idx(&app, FormRow::DbType);
    app.form.field = dbtype_row;
    press(&mut app, KeyCode::Enter);
    for _ in 0..8 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "mysql".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.form.db_type, "mysql");
    assert_eq!(app.form.port, "15432");
}

/// R51: a blank connection name is generated as `host-db_type`.
#[test]
pub(crate) fn blank_name_generates_host_and_type() {
    assert_eq!(
        auto_conn_name("localhost", "postgres"),
        "localhost-postgres"
    );
    assert_eq!(auto_conn_name(" 10.0.0.1 ", "mysql"), "10.0.0.1-mysql");
    assert_eq!(
        auto_conn_name("db.internal", "mongodb"),
        "db.internal-mongodb"
    );
}

#[test]
pub(crate) fn ssh_host_key_prompt_answers_and_resumes_the_handshake() {
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.ssh_prompt = Some(SshPromptState {
        request: ssh_prompt::host_key_verify_request(
            "jump",
            22,
            Some("ssh-ed25519".into()),
            Some("SHA256:abc".into()),
        ),
        responder: Some(tx),
        input: String::new(),
    });
    ssh_prompt_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(app.ssh_prompt.is_none());
    assert!(matches!(
        rx.try_recv(),
        Ok(SshPromptAnswer::Accept { remember: true })
    ));

    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.ssh_prompt = Some(SshPromptState {
        request: ssh_prompt::host_key_verify_request("jump", 22, None, None),
        responder: Some(tx),
        input: String::new(),
    });
    ssh_prompt_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(rx.try_recv(), Ok(SshPromptAnswer::Reject)));
}

#[test]
pub(crate) fn ssh_secret_prompt_collects_typed_input() {
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.ssh_prompt = Some(SshPromptState {
        request: ssh_prompt::secret_input_request("jump", 22, "code".into(), false),
        responder: Some(tx),
        input: String::new(),
    });
    for c in "123456".chars() {
        ssh_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    ssh_prompt_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(rx.try_recv(), Ok(SshPromptAnswer::Secret(s)) if s == "123456"));
}

/// The SSH prompt is modal and must swallow keys before the page handler.
#[test]
pub(crate) fn ssh_prompt_is_modal() {
    let mut app = test_app();
    app.page = Page::Browse;
    let (tx, _rx) = tokio::sync::oneshot::channel();
    app.ssh_prompt = Some(SshPromptState {
        request: ssh_prompt::host_key_verify_request("jump", 22, None, None),
        responder: Some(tx),
        input: String::new(),
    });
    let (otx, _orx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    // `q` would normally toggle the connection list; while the prompt is up
    // it must be ignored (the prompt only answers y/s/n/Esc).
    key(
        &mut app,
        &otx,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    assert!(app.ssh_prompt.is_some());
}

/// Real end-to-end tunnel: dbxt → local sshd (jump host) → MySQL on
/// 127.0.0.1. Gated on `DBXT_SSH_TEST=1` because it needs a reachable sshd
/// and a MySQL server; the two auth methods are both exercised.
///
/// Env: `DBXT_SSH_TEST_USER` (dbxtjump), `DBXT_SSH_TEST_PASSWORD`,
/// `DBXT_SSH_TEST_KEY` (key file), `DBXT_SSH_TEST_MYSQL_PORT` (13306),
/// `DBXT_SSH_TEST_MYSQL_USER` (root), `DBXT_SSH_TEST_MYSQL_PASSWORD`.
#[tokio::test(flavor = "multi_thread")]
async fn ssh_tunnel_end_to_end_through_local_jump_host() {
    if std::env::var("DBXT_SSH_TEST").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping ssh_tunnel_end_to_end: set DBXT_SSH_TEST=1 (needs a local sshd + MySQL)"
        );
        return;
    }
    let ssh_user = std::env::var("DBXT_SSH_TEST_USER").unwrap_or_else(|_| "dbxtjump".into());
    let ssh_password =
        std::env::var("DBXT_SSH_TEST_PASSWORD").unwrap_or_else(|_| "dbxt-jump-Pw1".into());
    let ssh_key = std::env::var("DBXT_SSH_TEST_KEY")
        .unwrap_or_else(|_| "/tmp/dbxt-ssh-test/id_ed25519".into());
    let mysql_port: u16 = std::env::var("DBXT_SSH_TEST_MYSQL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(13306);
    let mysql_user = std::env::var("DBXT_SSH_TEST_MYSQL_USER").unwrap_or_else(|_| "root".into());
    let mysql_password =
        std::env::var("DBXT_SSH_TEST_MYSQL_PASSWORD").unwrap_or_else(|_| "dbxt-test".into());

    // Auto-accept the host key so the test also exercises the gateway path
    // (the kernel fails closed when no gateway is installed).
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<SshPromptEnvelope>(4);
    ssh_prompt::install_ssh_prompt_gateway(prompt_tx);
    tokio::spawn(async move {
        while let Some(env) = prompt_rx.recv().await {
            let answer = match env.request.kind {
                SshPromptKind::HostKeyVerify | SshPromptKind::HostKeyChanged => {
                    SshPromptAnswer::Accept { remember: true }
                }
                _ => SshPromptAnswer::Reject,
            };
            let _ = env.responder.send(answer);
        }
    });

    let dir = std::env::temp_dir().join(format!("dbxt-ssh-e2e-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let backend = LocalBackend::open(&dir.join("dbx.db")).await.unwrap();

    for auth in [SshAuth::Key, SshAuth::Password] {
        let mut cfg = new_connection_config(
            format!("ssh-e2e-{}", auth.as_str()),
            format!("ssh-e2e-{}", auth.as_str()),
            parse_database_type("mysql").unwrap(),
            "127.0.0.1".into(),
            mysql_port,
            mysql_user.clone(),
            mysql_password.clone(),
            Some("shop".into()),
            false,
            None,
        )
        .unwrap();
        cfg.transport_layers = vec![TransportLayerConfig::Ssh(SshTunnelConfig {
            id: format!("layer-{}", auth.as_str()),
            name: "local-jump".into(),
            enabled: true,
            host: "127.0.0.1".into(),
            port: 22,
            user: ssh_user.clone(),
            password: if auth == SshAuth::Password {
                ssh_password.clone()
            } else {
                String::new()
            },
            key_path: if auth == SshAuth::Key {
                ssh_key.clone()
            } else {
                String::new()
            },
            key_passphrase: String::new(),
            connect_timeout_secs: 5,
            expose_lan: false,
            use_ssh_agent: false,
            ssh_agent_sock_path: String::new(),
            auth_method: auth.as_str().to_string(),
            allow_exec_channel_proxy: false,
            profile_id: String::new(),
        })];
        // The kernel resolves the tunnel through the connection's cached
        // config, so persist it first (as the TUI's save flow does).
        backend.add_connection_for_mcp(cfg.clone()).await.unwrap();
        let dbs = backend.list_databases(&cfg).await;
        assert!(
            dbs.is_ok(),
            "{} auth failed through the tunnel: {:?}",
            auth.as_str(),
            dbs
        );
        let dbs = dbs.unwrap();
        assert!(
            dbs.iter().any(|d| d == "shop"),
            "{} auth: expected the `shop` database, got {dbs:?}",
            auth.as_str()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The three SSH failure categories must be told apart from the raw error:
/// authentication, jump host unreachable, and remote database unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn ssh_tunnel_error_paths_are_classified() {
    if std::env::var("DBXT_SSH_TEST").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping ssh_tunnel_error_paths: set DBXT_SSH_TEST=1 (needs a local sshd + MySQL)"
        );
        return;
    }
    let ssh_user = std::env::var("DBXT_SSH_TEST_USER").unwrap_or_else(|_| "dbxtjump".into());
    let ssh_password =
        std::env::var("DBXT_SSH_TEST_PASSWORD").unwrap_or_else(|_| "dbxt-jump-Pw1".into());
    let mysql_port: u16 = std::env::var("DBXT_SSH_TEST_MYSQL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(13306);

    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<SshPromptEnvelope>(4);
    ssh_prompt::install_ssh_prompt_gateway(prompt_tx);
    tokio::spawn(async move {
        while let Some(env) = prompt_rx.recv().await {
            let answer = match env.request.kind {
                SshPromptKind::HostKeyVerify | SshPromptKind::HostKeyChanged => {
                    SshPromptAnswer::Accept { remember: true }
                }
                _ => SshPromptAnswer::Reject,
            };
            let _ = env.responder.send(answer);
        }
    });

    let dir = std::env::temp_dir().join(format!("dbxt-ssh-err-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let backend = LocalBackend::open(&dir.join("dbx.db")).await.unwrap();

    let mk = |id: &str, ssh_port: u16, db_port: u16, ssh_pw: &str| -> ConnectionConfig {
        let mut cfg = new_connection_config(
            id.into(),
            id.into(),
            parse_database_type("mysql").unwrap(),
            "127.0.0.1".into(),
            db_port,
            "root".into(),
            "dbxt-test".into(),
            None,
            false,
            None,
        )
        .unwrap();
        cfg.transport_layers = vec![TransportLayerConfig::Ssh(SshTunnelConfig {
            id: format!("layer-{id}"),
            name: "local-jump".into(),
            enabled: true,
            host: "127.0.0.1".into(),
            port: ssh_port,
            user: ssh_user.clone(),
            password: ssh_pw.into(),
            key_path: String::new(),
            key_passphrase: String::new(),
            connect_timeout_secs: 5,
            expose_lan: false,
            use_ssh_agent: false,
            ssh_agent_sock_path: String::new(),
            auth_method: "password".into(),
            allow_exec_channel_proxy: false,
            profile_id: String::new(),
        })];
        cfg
    };

    // 1. Wrong SSH password → authentication failure.
    let bad_pw = mk("e2e-bad-pw", 22, mysql_port, "definitely-wrong");
    backend
        .add_connection_for_mcp(bad_pw.clone())
        .await
        .unwrap();
    let err = backend.list_databases(&bad_pw).await.unwrap_err();
    assert!(classify_ssh_auth_error(&err), "raw: {err}");
    let msg = ssh_connect_error_message(&bad_pw, &err);
    assert!(msg.contains("SSH 认证失败"), "{msg}");

    // 2. Wrong SSH port → jump host unreachable.
    let bad_host = mk("e2e-bad-host", 2223, mysql_port, &ssh_password);
    backend
        .add_connection_for_mcp(bad_host.clone())
        .await
        .unwrap();
    let err = backend.list_databases(&bad_host).await.unwrap_err();
    assert!(classify_ssh_host_error(&err), "raw: {err}");
    let msg = ssh_connect_error_message(&bad_host, &err);
    assert!(msg.contains("SSH 主机不可达"), "{msg}");

    // 3. Tunnel up, wrong DB port → remote database unreachable.
    let bad_db = mk("e2e-bad-db", 22, 13399, &ssh_password);
    backend
        .add_connection_for_mcp(bad_db.clone())
        .await
        .unwrap();
    let err = backend.list_databases(&bad_db).await.unwrap_err();
    let msg = ssh_connect_error_message(&bad_db, &err);
    assert!(
        msg.contains("远端数据库不可达"),
        "raw err: {err}; msg: {msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
// ── R30 temporary baseline benchmark ────────────────────────────────────
pub(crate) fn r30_big_grid(rows: usize) -> Grid {
    let cols: Vec<String> = (0..12).map(|i| format!("col_{i}")).collect();
    let data: Vec<Vec<Val>> = (0..rows)
        .map(|r| {
            (0..12)
                .map(|c| match (r + c) % 9 {
                    0 => Val::Null,
                    1 => Val::Text(format!("用户_{r}_{c}")),
                    2 => Val::Text(format!("note {} 中文备注内容示例文本，用于撑宽列宽。", r)),
                    3 => Val::Text(format!("{}", r * 137 + c)),
                    _ => Val::Text(format!("value-{r}-{c}")),
                })
                .collect()
        })
        .collect();
    Grid {
        columns: cols,
        rows: data,
        note: String::new(),
    }
}

pub(crate) fn r30_hwm() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Scratch dir for the perf probes, under `target/` (never the system temp
/// dir, which may be a small tmpfs).
pub(crate) fn r30_bench_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("r30-bench")
        .join(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Legacy-only memory probe, the counterpart to `r30_stream_bench`: builds
/// the whole document in memory per format so the peak is comparable.
#[test]
#[ignore]
pub(crate) fn r30_legacy_bench() {
    let rows = 20_000usize;
    let grid = r30_big_grid(rows);
    let app = {
        let mut a = test_app();
        a.selected = Some(test_conn("mysql"));
        a
    };
    let table = Some(("shop".to_string(), "r30_big".to_string()));
    let dir = r30_bench_dir("legacy");
    let _ = std::fs::create_dir_all(&dir);
    eprintln!("BENCH legacy-only baseline peak={}", r30_hwm());
    for (name, fmt) in [
        ("csv", ExportFormat::Csv),
        ("json", ExportFormat::JsonArray),
        ("ndjson", ExportFormat::JsonNdjson),
        ("markdown", ExportFormat::Markdown),
        ("insert", ExportFormat::Insert),
        ("insertbatch", ExportFormat::InsertBatch),
    ] {
        let content = render_export_content(&app, &grid, fmt, table.as_ref());
        let path = dir.join(format!("l.{name}"));
        std::fs::write(&path, content.as_bytes()).unwrap();
        eprintln!(
            "BENCH legacy-only {name}: bytes={} peak={}",
            content.len(),
            r30_hwm()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Streaming-only memory probe: no legacy export runs first, so VmHWM
/// reflects the streaming path alone.
#[test]
#[ignore]
pub(crate) fn r30_stream_bench() {
    let rows = 20_000usize;
    let grid = r30_big_grid(rows);
    let app = {
        let mut a = test_app();
        a.selected = Some(test_conn("mysql"));
        a
    };
    let types = grid_column_types(&app, "shop", "r30_big", &grid);
    let cfg = app.selected.clone().unwrap();
    let dir = r30_bench_dir("stream");
    let _ = std::fs::create_dir_all(&dir);
    eprintln!("BENCH stream-only baseline peak={}", r30_hwm());
    for (name, fmt) in [
        ("csv", ExportFormat::Csv),
        ("json", ExportFormat::JsonArray),
        ("ndjson", ExportFormat::JsonNdjson),
        ("markdown", ExportFormat::Markdown),
        ("insert", ExportFormat::Insert),
        ("insertbatch", ExportFormat::InsertBatch),
    ] {
        let path = dir.join(format!("s.{name}"));
        let f = std::fs::File::create(&path).unwrap();
        let mut w = BufWriter::with_capacity(EXPORT_BUF_BYTES, f);
        write_export(&mut w, Some(&cfg), "shop", "r30_big", &types, &grid, fmt).unwrap();
        w.flush().unwrap();
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "BENCH stream-only {name}: bytes={} peak={}",
            bytes,
            r30_hwm()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore]
pub(crate) fn r30_baseline_bench() {
    let rows = 20_000usize;
    let grid = r30_big_grid(rows);
    let t = Instant::now();
    let g2 = grid.clone();
    eprintln!("BENCH clone grid ({}x12): {:?}", rows, t.elapsed());
    std::hint::black_box(&g2);

    let t = Instant::now();
    let widths: Vec<usize> = (0..12).map(|ci| natural_width(&grid, ci, 44)).collect();
    eprintln!("BENCH natural_width x12: {:?} -> {:?}", t.elapsed(), widths);
    let t = Instant::now();
    let _ = visible_cols(&grid, 0, 120, 44);
    eprintln!("BENCH visible_cols: {:?}", t.elapsed());

    {
        let mut app2 = test_app();
        app2.set_grid(r30_big_grid(rows));
        app2.grid_kind = GridKind::Query;
        let n = 20u32;
        let t = Instant::now();
        for i in 0..n {
            app2.sel = (i as usize) * 7;
            std::hint::black_box(draw(&mut app2, 120, 40));
        }
        eprintln!("BENCH draw query grid 120x40: {:?}/frame", t.elapsed() / n);
        let t = Instant::now();
        for i in 0..n {
            app2.sel = (i as usize) * 7;
            app2.col_cursor = (i as usize) % 12;
            std::hint::black_box(draw(&mut app2, 80, 24));
        }
        eprintln!("BENCH draw query grid 80x24: {:?}/frame", t.elapsed() / n);
    }

    eprintln!("BENCH peak before exports: {}", r30_hwm());
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    let table = Some(("shop".to_string(), "r30_big".to_string()));
    let dir = r30_bench_dir("all");
    let _ = std::fs::create_dir_all(&dir);
    for (name, fmt) in [
        ("csv", ExportFormat::Csv),
        ("json", ExportFormat::JsonArray),
        ("ndjson", ExportFormat::JsonNdjson),
        ("markdown", ExportFormat::Markdown),
        ("insert", ExportFormat::Insert),
        ("insertbatch", ExportFormat::InsertBatch),
    ] {
        let t = Instant::now();
        let content = render_export_content(&app, &grid, fmt, table.as_ref());
        let gen = t.elapsed();
        let path = dir.join(format!("bench.{name}"));
        let t2 = Instant::now();
        std::fs::write(&path, content.as_bytes()).unwrap();
        let wr = t2.elapsed();
        eprintln!(
            "BENCH export {name}: gen={:?} write={:?} bytes={} peak={}",
            gen,
            wr,
            content.len(),
            r30_hwm()
        );
    }
    // Streaming path (after): bytes go straight to a BufWriter, one row at
    // a time, so peak memory stays flat regardless of format.
    let types = grid_column_types(&app, "shop", "r30_big", &grid);
    let cfg = app.selected.clone().unwrap();
    for (name, fmt) in [
        ("csv", ExportFormat::Csv),
        ("json", ExportFormat::JsonArray),
        ("ndjson", ExportFormat::JsonNdjson),
        ("markdown", ExportFormat::Markdown),
        ("insert", ExportFormat::Insert),
        ("insertbatch", ExportFormat::InsertBatch),
    ] {
        let path = dir.join(format!("stream.{name}"));
        let t = Instant::now();
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut w = BufWriter::with_capacity(EXPORT_BUF_BYTES, f);
            write_export(&mut w, Some(&cfg), "shop", "r30_big", &types, &grid, fmt).unwrap();
            w.flush().unwrap();
        }
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "BENCH stream {name}: total={:?} bytes={} peak={}",
            t.elapsed(),
            bytes,
            r30_hwm()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ── schema diff (R32) ───────────────────────────────────────────────────

pub(crate) fn diff_side(
    db_type: &str,
    db: &str,
    schema: &str,
    table: &str,
    cols: Vec<ColumnInfo>,
) -> DiffSide {
    DiffSide {
        db: db.into(),
        schema: schema.into(),
        table: table.into(),
        db_type: parse_database_type(db_type).unwrap(),
        columns: cols,
        indexes: Vec::new(),
    }
}

pub(crate) fn idx_info(name: &str, cols: &[&str], unique: bool, primary: bool) -> IndexInfo {
    IndexInfo {
        name: name.into(),
        columns: cols.iter().map(|s| s.to_string()).collect(),
        is_unique: unique,
        is_primary: primary,
        filter: None,
        index_type: None,
        included_columns: None,
        comment: None,
        key_is_expression: Vec::new(),
        column_opclasses: Vec::new(),
        key_options: Vec::new(),
        constraint_backed: false,
    }
}

pub(crate) fn col_full(
    name: &str,
    ty: &str,
    nullable: bool,
    default: Option<&str>,
    comment: Option<&str>,
    pk: bool,
) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        is_nullable: nullable,
        column_default: default.map(str::to_string),
        comment: comment.map(str::to_string),
        is_primary_key: pk,
        ..Default::default()
    }
}

#[test]
pub(crate) fn canonical_type_maps_the_common_ten() {
    assert_eq!(
        canonical_type("varchar(255)").as_deref(),
        Some("varchar(255)")
    );
    assert_eq!(
        canonical_type("character varying(255)").as_deref(),
        Some("varchar(255)")
    );
    assert_eq!(canonical_type("int").as_deref(), Some("int"));
    assert_eq!(canonical_type("integer").as_deref(), Some("int"));
    assert_eq!(canonical_type("int4").as_deref(), Some("int"));
    assert_eq!(canonical_type("bigint").as_deref(), Some("bigint"));
    assert_eq!(canonical_type("bool").as_deref(), Some("boolean"));
    assert_eq!(canonical_type("boolean").as_deref(), Some("boolean"));
    assert_eq!(
        canonical_type("numeric(10, 2)").as_deref(),
        Some("decimal(10,2)")
    );
    assert_eq!(
        canonical_type("timestamp with time zone").as_deref(),
        Some("timestamp tz")
    );
    assert_eq!(
        canonical_type("timestamptz").as_deref(),
        Some("timestamp tz")
    );
    assert_eq!(canonical_type("jsonb").as_deref(), Some("json"));
    assert_eq!(canonical_type("bytea").as_deref(), Some("blob"));
    assert_eq!(
        canonical_type("int unsigned").as_deref(),
        Some("int unsigned")
    );
    assert!(canonical_type("geometry").is_none());
}

#[test]
pub(crate) fn type_comparison_is_dialect_aware() {
    // Same dialect: cosmetic display width is ignored, real differences kept.
    assert_eq!(compare_types("int(11)", "int", false), TypeVerdict::Same);
    assert_eq!(
        compare_types("varchar(255)", "varchar(200)", false),
        TypeVerdict::Diff
    );
    assert_eq!(
        compare_types("int unsigned", "int", false),
        TypeVerdict::Diff
    );
    // Cross dialect: the common map bridges the spellings.
    assert_eq!(
        compare_types("varchar(255)", "character varying(255)", true),
        TypeVerdict::Same
    );
    assert_eq!(compare_types("int", "integer", true), TypeVerdict::Same);
    assert_eq!(compare_types("int", "bigint", true), TypeVerdict::Diff);
    // Unmapped: `?` when the spellings differ, equal when they do not.
    assert_eq!(
        compare_types("geometry", "integer", true),
        TypeVerdict::Unknown
    );
    assert_eq!(
        compare_types("geometry", "geometry", true),
        TypeVerdict::Same
    );
}

#[test]
pub(crate) fn column_diff_marks_add_drop_and_modify() {
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(200)", false, Some("''"), None, false),
            col_full("email", "varchar(255)", true, None, None, false),
        ],
    );
    let tgt = diff_side(
        "mysql",
        "shop",
        "",
        "b",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(100)", false, Some("''"), None, false),
            col_full("fax", "varchar(20)", true, None, None, false),
        ],
    );
    let diff = build_table_diff(src, tgt);
    assert!(!diff.equal());
    let marks: Vec<(&str, DiffMark)> = diff
        .cols
        .iter()
        .map(|r| (r.name.as_str(), r.mark))
        .collect();
    assert_eq!(
        marks,
        vec![
            ("id", DiffMark::Same),
            ("name", DiffMark::Modify),
            ("email", DiffMark::Add),
            ("fax", DiffMark::Drop),
        ]
    );
    let name = diff.cols.iter().find(|r| r.name == "name").unwrap();
    assert!(name.detail.contains("type varchar(200)→varchar(100)"));
    assert_eq!(diff.changed(), 3);
}

#[test]
pub(crate) fn index_diff_matches_by_name_and_marks_changes() {
    let mut src = diff_side("mysql", "shop", "", "a", vec![col_info("id", "int")]);
    src.indexes = vec![
        idx_info("PRIMARY", &["id"], true, true),
        idx_info("idx_name", &["name"], false, false),
    ];
    let mut tgt = diff_side("mysql", "shop", "", "b", vec![col_info("id", "int")]);
    tgt.indexes = vec![
        idx_info("PRIMARY", &["id"], true, true),
        idx_info("idx_name", &["name", "id"], false, false),
        idx_info("idx_old", &["old"], false, false),
    ];
    let diff = build_table_diff(src, tgt);
    let count = |m: DiffMark| diff.idx.iter().filter(|r| r.mark == m).count();
    assert_eq!(count(DiffMark::Same), 1);
    assert_eq!(count(DiffMark::Modify), 1);
    assert_eq!(count(DiffMark::Drop), 1);
    let changed = diff
        .idx
        .iter()
        .find(|r| r.mark == DiffMark::Modify)
        .unwrap();
    assert!(changed.detail.contains("(name)"));
    assert!(changed.detail.contains("(name, id)"));
}

#[test]
pub(crate) fn generate_alter_mysql_rewrites_the_target() {
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(200)", false, None, None, false),
            col_full("email", "varchar(255)", true, None, None, false),
        ],
    );
    let tgt = diff_side(
        "mysql",
        "shop",
        "",
        "b",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(100)", false, None, None, false),
            col_full("fax", "varchar(20)", true, None, None, false),
        ],
    );
    let sql = generate_alter(&build_table_diff(src, tgt));
    assert!(
        sql.contains("ALTER TABLE `b` ADD COLUMN `email` varchar(255);"),
        "{sql}"
    );
    assert!(
        sql.contains("ALTER TABLE `b` MODIFY COLUMN `name` varchar(200) NOT NULL;"),
        "{sql}"
    );
    assert!(sql.contains("ALTER TABLE `b` DROP COLUMN `fax`;"), "{sql}");
}

#[test]
pub(crate) fn generate_alter_postgres_uses_alter_column() {
    let src = diff_side(
        "postgres",
        "shop",
        "public",
        "a",
        vec![col_full(
            "name",
            "character varying(200)",
            false,
            None,
            None,
            false,
        )],
    );
    let tgt = diff_side(
        "postgres",
        "shop",
        "public",
        "b",
        vec![col_full(
            "name",
            "character varying(100)",
            true,
            None,
            None,
            false,
        )],
    );
    let sql = generate_alter(&build_table_diff(src, tgt));
    assert!(
        sql.contains(
            "ALTER TABLE \"public\".\"b\" ALTER COLUMN \"name\" TYPE character varying(200);"
        ),
        "{sql}"
    );
    assert!(
        sql.contains("ALTER TABLE \"public\".\"b\" ALTER COLUMN \"name\" SET NOT NULL;"),
        "{sql}"
    );
}

#[test]
pub(crate) fn generate_alter_maps_types_across_dialects() {
    // MySQL source → PostgreSQL target.
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("email", "varchar(255)", false, None, None, false),
            col_full("flag", "tinyint(1)", false, None, None, false),
        ],
    );
    let tgt = diff_side("postgres", "shop", "public", "b", vec![]);
    let diff = build_table_diff(src, tgt);
    assert!(diff.cross);
    let sql = generate_alter(&diff);
    assert!(sql.contains("ADD COLUMN \"id\" integer NOT NULL;"), "{sql}");
    assert!(
        sql.contains("ADD COLUMN \"email\" character varying(255) NOT NULL;"),
        "{sql}"
    );
    assert!(
        sql.contains("ADD COLUMN \"flag\" smallint NOT NULL;"),
        "{sql}"
    );

    // PostgreSQL source → MySQL target.
    let src = diff_side(
        "postgres",
        "shop",
        "public",
        "a",
        vec![
            col_full(
                "email",
                "character varying(255)",
                true,
                Some("''::character varying"),
                None,
                false,
            ),
            col_full("flag", "boolean", true, None, None, false),
            col_full("data", "jsonb", true, None, None, false),
        ],
    );
    let tgt = diff_side("mysql", "shop", "", "b", vec![]);
    let diff = build_table_diff(src, tgt);
    assert!(diff.cross);
    let sql = generate_alter(&diff);
    // The PostgreSQL `::type` default cast is dropped for MySQL.
    assert!(
        sql.contains("ADD COLUMN `email` varchar(255) DEFAULT '';"),
        "{sql}"
    );
    assert!(sql.contains("ADD COLUMN `flag` tinyint(1);"), "{sql}");
    assert!(sql.contains("ADD COLUMN `data` json;"), "{sql}");
}

#[test]
pub(crate) fn identical_tables_report_equal_and_no_alter() {
    let cols = vec![col_full("id", "int", false, None, None, true)];
    let src = diff_side("mysql", "shop", "", "a", cols.clone());
    let tgt = diff_side("mysql", "shop", "", "b", cols);
    let diff = build_table_diff(src, tgt);
    assert!(diff.equal());
    assert_eq!(diff.changed(), 0);
    assert!(generate_alter(&diff).contains("无需同步"));
    assert!(diff_summary_text(&diff).contains("结构一致"));
}

#[test]
pub(crate) fn diff_summary_lists_changed_columns() {
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![
            col_full("name", "varchar(200)", false, None, None, false),
            col_full("email", "varchar(255)", true, None, None, false),
        ],
    );
    let tgt = diff_side(
        "mysql",
        "shop",
        "",
        "b",
        vec![
            col_full("name", "varchar(100)", false, None, None, false),
            col_full("fax", "varchar(20)", true, None, None, false),
        ],
    );
    let text = diff_summary_text(&build_table_diff(src, tgt));
    assert!(text.contains("+ 列 email"), "{text}");
    assert!(text.contains("- 列 fax"), "{text}");
    assert!(text.contains("~ 列 name"), "{text}");
}

#[test]
pub(crate) fn unmapped_cross_dialect_type_is_flagged() {
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![col_full("g", "geometry", true, None, None, false)],
    );
    let tgt = diff_side(
        "postgres",
        "shop",
        "public",
        "b",
        vec![col_full("g", "point", true, None, None, false)],
    );
    let diff = build_table_diff(src, tgt);
    let row = diff.cols.iter().find(|r| r.name == "g").unwrap();
    assert_eq!(row.mark, DiffMark::Modify);
    assert!(row.detail.contains("type ?"), "{}", row.detail);
    assert!(diff_summary_text(&diff).contains("type ?"));
}

#[test]
pub(crate) fn diff_overlays_render_at_extreme_sizes() {
    let src = diff_side(
        "mysql",
        "shop",
        "",
        "a",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(200)", false, Some("''"), None, false),
            col_full(
                "email",
                "varchar(255)",
                true,
                None,
                Some("email addr"),
                false,
            ),
        ],
    );
    let tgt = diff_side(
        "mysql",
        "shop",
        "",
        "b",
        vec![
            col_full("id", "int", false, None, None, true),
            col_full("name", "varchar(100)", false, Some("''"), None, false),
            col_full("fax", "varchar(20)", true, None, None, false),
        ],
    );
    let diff = build_table_diff(src, tgt);
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    let mut list = ListState::default();
    list.select(Some(0));
    app.diff = Some(Box::new(SchemaDiffState {
        diff,
        tab: DiffTab::Columns,
        list,
        scroll: 0,
        alter: String::new(),
    }));
    let sizes = [
        (40u16, 12u16),
        (42, 22),
        (120, 40),
        (250, 70),
        (20, 6),
        (1, 1),
    ];
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    for tab in [DiffTab::Indexes, DiffTab::Alter] {
        if let Some(s) = app.diff.as_mut() {
            s.tab = tab;
            if tab == DiffTab::Alter {
                let sql = generate_alter(&s.diff);
                s.alter = sql;
            }
        }
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
    }
    // Picker overlay (table and database modes).
    app.diff = None;
    let mut list = ListState::default();
    list.select(Some(0));
    app.diff_picker = Some(DiffPicker {
        mode: DiffPickMode::Database,
        kind: DiffKind::Schema,
        stage: DiffPickStage::Lists,
        list,
        src_entries: vec!["shop2".into(), "shop3".into()],
        entries: vec!["shop2".into(), "shop3".into()],
        target_conn: None,
        target_db: String::new(),
        target_schema: String::new(),
        loading: false,
        comparing: false,
        where_input: String::new(),
        gen: 0,
    });
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // Connection step of the picker (cross-connection target).
    if let Some(p) = app.diff_picker.as_mut() {
        p.mode = DiffPickMode::Table;
        p.stage = DiffPickStage::Connections;
        p.entries = vec!["r31-postgres (postgres)".into()];
    }
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // Loading placeholder (no entries yet).
    if let Some(p) = app.diff_picker.as_mut() {
        p.stage = DiffPickStage::Lists;
        p.loading = true;
        p.entries.clear();
    }
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // Database-list diff overlay.
    app.diff_picker = None;
    let mut list = ListState::default();
    list.select(Some(0));
    app.db_diff = Some(Box::new(DbDiffState {
        diff: DbDiff {
            src_label: "shop".into(),
            tgt_label: "shop2".into(),
            entries: vec![
                DbDiffEntry {
                    table: "orders".into(),
                    mark: DbTableMark::Both,
                },
                DbDiffEntry {
                    table: "only_src".into(),
                    mark: DbTableMark::OnlySrc,
                },
                DbDiffEntry {
                    table: "only_tgt".into(),
                    mark: DbTableMark::OnlyTgt,
                },
            ],
            src_db: "shop".into(),
            tgt_db: "shop2".into(),
            src_schema: String::new(),
            tgt_schema: String::new(),
        },
        list,
    }));
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
}

#[test]
pub(crate) fn diff_strings_have_english_translations() {
    use ui_text::Lang;
    assert_eq!(ui_text::t_lang("结构一致", Lang::En), "identical");
    assert_eq!(ui_text::t_lang("索引", Lang::En), "index");
    assert_eq!(ui_text::t_lang("源", Lang::En), "source");
    assert_eq!(ui_text::t_lang("目标", Lang::En), "target");
    assert_ne!(
        ui_text::t_lang("+ 新增  - 多余  ~ 差异", Lang::En),
        "+ 新增  - 多余  ~ 差异"
    );
}

// ── data compare (R33) ──────────────────────────────────────────────────

pub(crate) fn data_align(
    src: Vec<ColumnInfo>,
    tgt: Vec<ColumnInfo>,
    src_pk: &[&str],
    tgt_pk: &[&str],
    cross: bool,
) -> Result<DataAlign, String> {
    let src_pk: Vec<String> = src_pk.iter().map(|s| s.to_string()).collect();
    let tgt_pk: Vec<String> = tgt_pk.iter().map(|s| s.to_string()).collect();
    build_data_align(&src, &tgt, &src_pk, &tgt_pk, cross)
}

/// A small `mysql` result with one `<`, one `>`, one `≠` and one identical
/// row, plus a changed-column popup target.
pub(crate) fn data_cmp_fixture() -> DataCompare {
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("name", "varchar(20)", true, None, None, false),
    ];
    let tgt = src.clone();
    let align = data_align(src, tgt, &["id"], &["id"], false).unwrap();
    let rows = vec![
        only_data_row(
            &align,
            &[Val::Text("1".into()), Val::Text("a".into())],
            RowMark::OnlySrc,
        ),
        only_data_row(
            &align,
            &[Val::Text("2".into()), Val::Text("b".into())],
            RowMark::OnlyTgt,
        ),
        compare_data_row(
            &align,
            &[Val::Text("3".into()), Val::Text("new".into())],
            &[Val::Text("3".into()), Val::Text("old".into())],
        )
        .unwrap(),
    ];
    let dt = parse_database_type("mysql").unwrap();
    DataCompare {
        src_label: "shop.a".into(),
        tgt_label: "shop.b".into(),
        src_db_type: dt,
        tgt_schema: String::new(),
        tgt_table: "b".into(),
        tgt_db_type: dt,
        src_count: Some(3),
        tgt_count: Some(3),
        filter: String::new(),
        align,
        rows,
        only_src: 1,
        only_tgt: 1,
        differing: 1,
        truncated: false,
        cancelled: false,
    }
}

#[test]
pub(crate) fn data_align_uses_column_name_intersection() {
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("name", "varchar(50)", true, None, None, false),
        col_full("only_src", "text", true, None, None, false),
    ];
    // Target columns are reordered and use a different case for `name`.
    let tgt = vec![
        col_full("NAME", "varchar(50)", true, None, None, false),
        col_full("id", "int", false, None, None, true),
        col_full("only_tgt", "text", true, None, None, false),
    ];
    let align = data_align(src, tgt, &["id"], &["id"], false).unwrap();
    assert_eq!(align.pk_len, 1);
    assert_eq!(align.src_pk_names(), vec!["id".to_string()]);
    assert_eq!(align.tgt_pk_names(), vec!["id".to_string()]);
    // PK first, then the source-ordered intersection; nothing extra.
    let names: Vec<String> = align.cols.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, vec!["id", "name"]);
    assert_eq!(align.cols[1].tgt_name, "NAME");
    assert_eq!(
        align.src_select(),
        vec!["id".to_string(), "name".to_string()]
    );
    assert_eq!(
        align.tgt_select(),
        vec!["id".to_string(), "NAME".to_string()]
    );
}

/// R36 seam: when the source PK is `(a, b)` and the target's primary index
/// lists `(b, a)`, the aligned order follows the *source* key order, and the
/// SELECT / ORDER BY / keyset seek stay consistent on each side.
#[test]
pub(crate) fn composite_pk_order_mismatch_still_aligns_by_name() {
    let src = vec![
        col_full("a", "int", false, None, None, true),
        col_full("b", "int", false, None, None, true),
        col_full("v", "text", true, None, None, false),
    ];
    let tgt = src.clone();
    let align = data_align(src, tgt, &["a", "b"], &["b", "a"], false).unwrap();
    assert_eq!(align.pk_len, 2);
    assert_eq!(align.src_pk_names(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(align.tgt_pk_names(), vec!["a".to_string(), "b".to_string()]);
    // The composite seek predicate uses the aligned order, matching ORDER BY.
    let dt = parse_database_type("mysql").unwrap();
    let sql = build_data_select(
        dt,
        "",
        "t",
        &align.src_select(),
        &align.src_pk_names(),
        &align.src_types(),
        "",
        Some(&[Val::Text("1".into()), Val::Text("2".into())]),
        DATA_CHUNK,
    );
    assert!(sql.contains("ORDER BY `a`, `b`"), "{sql}");
    assert!(sql.contains("(`a` > 1) OR (`a` = 1 AND `b` > 2)"), "{sql}");
    // Row values line up positionally: the pk tuple is (a, b) on both sides.
    let s = [
        Val::Text("1".into()),
        Val::Text("2".into()),
        Val::Text("x".into()),
    ];
    assert_eq!(
        &s[..align.pk_len],
        &[Val::Text("1".into()), Val::Text("2".into())]
    );
}

#[test]
pub(crate) fn data_align_rejects_tables_without_pk() {
    let no_pk = vec![col_full("a", "int", false, None, None, false)];
    let with_pk = vec![col_full("id", "int", false, None, None, true)];
    let err = data_align(no_pk.clone(), no_pk.clone(), &[], &[], false).unwrap_err();
    assert!(err.contains("主键"), "{err}");
    let err = data_align(no_pk.clone(), with_pk.clone(), &[], &["id"], false).unwrap_err();
    assert!(err.contains("源表没有主键"), "{err}");
    let err = data_align(with_pk.clone(), no_pk.clone(), &["id"], &[], false).unwrap_err();
    assert!(err.contains("目标表没有主键"), "{err}");
    // Source has a PK the target lacks entirely.
    let err = data_align(with_pk.clone(), no_pk, &["id"], &["id"], false).unwrap_err();
    assert!(err.contains("目标表缺少主键列"), "{err}");
    // Different PK arity cannot be aligned.
    let two = vec![
        col_full("id", "int", false, None, None, true),
        col_full("seq", "int", false, None, None, true),
    ];
    let err = data_align(two, with_pk, &["id", "seq"], &["id"], false).unwrap_err();
    assert!(err.contains("主键列数不一致"), "{err}");
    // A PK present on both sides still needs an intersection to compare.
    let src = vec![col_full("id", "int", false, None, None, true)];
    let tgt = vec![col_full("ID", "int", false, None, None, true)];
    let align = data_align(src, tgt, &["id"], &["ID"], false).unwrap();
    assert_eq!(align.tgt_pk_names(), vec!["ID".to_string()]);
}

#[test]
pub(crate) fn data_row_classification_marks_only_and_diff() {
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("name", "varchar(20)", true, None, None, false),
        col_full("qty", "int", true, None, None, false),
    ];
    let tgt = vec![
        col_full("id", "integer", false, None, None, true),
        col_full("name", "character varying(20)", true, None, None, false),
        col_full("qty", "int", true, None, None, false),
    ];
    let align = data_align(src, tgt, &["id"], &["id"], true).unwrap();
    let row = |id: &str, name: &str, qty: &str| {
        vec![
            Val::Text(id.into()),
            Val::Text(name.into()),
            Val::Text(qty.into()),
        ]
    };
    // Identical rows produce no diff.
    assert!(compare_data_row(&align, &row("1", "a", "10"), &row("1", "a", "10")).is_none());
    // Cross-dialect numeric normalisation: 10 == 10.0.
    assert!(compare_data_row(&align, &row("1", "a", "10"), &row("1", "a", "10.0")).is_none());
    // One changed column is recorded by name, keyed by the primary key.
    let d = compare_data_row(&align, &row("2", "b", "5"), &row("2", "c", "5")).unwrap();
    assert_eq!(d.mark, RowMark::Diff);
    assert_eq!(d.key, "2");
    assert_eq!(d.cells.len(), 1);
    assert_eq!(d.cells[0].col, "name");
    // NULL is distinct from the empty string.
    let d = compare_data_row(
        &align,
        &[Val::Text("3".into()), Val::Null, Val::Text("1".into())],
        &[
            Val::Text("3".into()),
            Val::Text(String::new()),
            Val::Text("1".into()),
        ],
    )
    .unwrap();
    assert_eq!(d.cells[0].col, "name");
    // Only-source / only-target rows carry the whole row.
    let o = only_data_row(&align, &row("9", "z", "7"), RowMark::OnlySrc);
    assert_eq!(o.mark, RowMark::OnlySrc);
    assert_eq!(o.key, "9");
    assert_eq!(o.vals.len(), 3);
}

#[test]
pub(crate) fn data_merge_state_machine_classifies_chunks() {
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("v", "varchar(10)", true, None, None, false),
    ];
    let tgt = src.clone();
    let align = data_align(src, tgt, &["id"], &["id"], false).unwrap();
    let modes: Vec<PkCmp> = align.pk().iter().map(pk_cmp_mode).collect();
    let row = |id: i64, v: &str| vec![Val::Text(id.to_string()), Val::Text(v.into())];
    // Source: 1, 3, 5, 7 · Target: 2, 3, 5(changed), 7, 8 (a mixed merge).
    let src_rows = [row(1, "a"), row(3, "b"), row(5, "c"), row(7, "d")];
    let tgt_rows = [
        row(2, "x"),
        row(3, "b"),
        row(5, "z"),
        row(7, "d"),
        row(8, "e"),
    ];
    let mut si = 0usize;
    let mut ti = 0usize;
    let (mut only_src, mut only_tgt, mut diff) = (0usize, 0usize, 0usize);
    let mut steps = Vec::new();
    loop {
        let s = src_rows.get(si).map(Vec::as_slice);
        let t = tgt_rows.get(ti).map(Vec::as_slice);
        match merge_next(&align, &modes, s, t) {
            MergeStep::Done => break,
            MergeStep::SrcOnly => {
                only_src += 1;
                steps.push(format!("<{}", pk_display(&align, &src_rows[si])));
                si += 1;
            }
            MergeStep::TgtOnly => {
                only_tgt += 1;
                steps.push(format!(">{}", pk_display(&align, &tgt_rows[ti])));
                ti += 1;
            }
            MergeStep::Both(d) => {
                si += 1;
                ti += 1;
                if let Some(r) = d {
                    diff += 1;
                    steps.push(format!("≠{}", r.key));
                } else {
                    steps.push("=".to_string());
                }
            }
        }
    }
    assert_eq!((only_src, only_tgt, diff), (1, 2, 1));
    assert_eq!(steps, vec!["<1", ">2", "=", "≠5", "=", ">8"]);
    assert_eq!(si, src_rows.len());
    assert_eq!(ti, tgt_rows.len());
}

#[test]
pub(crate) fn data_sync_sql_generates_insert_update_delete() {
    let cmp = data_cmp_fixture();
    let sql = generate_data_sync(&cmp);
    assert!(
        sql.contains("INSERT INTO `b` (`id`, `name`) VALUES (1, 'a');"),
        "{sql}"
    );
    assert!(sql.contains("DELETE FROM `b` WHERE `id` = 2;"), "{sql}");
    assert!(
        sql.contains("UPDATE `b` SET `name` = 'new' WHERE `id` = 3;"),
        "{sql}"
    );
    // The header names the direction and the target dialect.
    assert!(sql.contains("源 → 目标"), "{sql}");
    // The summary is ticket-friendly and marks each row.
    let text = data_diff_summary_text(&cmp);
    assert!(text.contains("仅源"), "{text}");
    assert!(text.contains("仅目标"), "{text}");
    assert!(text.contains('≠'), "{text}");
    assert!(text.contains("[name]"), "{text}");
}

/// R36 seam: the generated sync SQL must survive a value with a quote, a
/// backslash, a newline and an emoji without breaking the literal.
#[test]
pub(crate) fn data_sync_sql_escapes_hostile_values() {
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("name", "text", true, None, None, false),
    ];
    let tgt = src.clone();
    let align = data_align(src, tgt, &["id"], &["id"], false).unwrap();
    let hostile = "O'Brien\nback\\slash 😀";
    let row = only_data_row(
        &align,
        &[Val::Text("7".into()), Val::Text(hostile.into())],
        RowMark::OnlySrc,
    );
    let dt = parse_database_type("mysql").unwrap();
    let cmp = DataCompare {
        src_label: "a".into(),
        tgt_label: "b".into(),
        src_db_type: dt,
        tgt_schema: String::new(),
        tgt_table: "b".into(),
        tgt_db_type: dt,
        src_count: Some(1),
        tgt_count: Some(0),
        filter: String::new(),
        align,
        rows: vec![row],
        only_src: 1,
        only_tgt: 0,
        differing: 0,
        truncated: false,
        cancelled: false,
    };
    let sql = generate_data_sync(&cmp);
    // The single quote is doubled, the backslash doubled, and the newline /
    // emoji stay inside the single literal without escaping.
    assert!(sql.contains("'O''Brien"), "{sql}");
    assert!(sql.contains("back\\\\slash"), "{sql}");
    assert!(sql.contains("😀"), "{sql}");
}

#[test]
pub(crate) fn data_cross_dialect_value_comparison() {
    // Boolean family: true == 1, false == 0.
    assert!(canon_cell_equal("true", "1", "boolean"));
    assert!(canon_cell_equal("FALSE", "0", "boolean"));
    assert!(!canon_cell_equal("true", "0", "boolean"));
    // Numeric families normalise the text form.
    assert!(canon_cell_equal("1.50", "1.5", "decimal(10,2)"));
    assert!(canon_cell_equal("1", "1.0", "int"));
    assert!(!canon_cell_equal("1", "2", "int"));
    // Temporal: the ISO `T` separator does not matter.
    assert!(canon_cell_equal(
        "2026-01-01T00:00:00",
        "2026-01-01 00:00:00",
        "timestamp"
    ));
    // Unmapped types compare verbatim (and stay case-sensitive).
    assert!(canon_cell_equal("abc", "abc", "geometry"));
    assert!(!canon_cell_equal("abc", "ABC", "geometry"));
    // NULL is never equal to the empty string.
    assert!(!values_equal(&Val::Null, &Val::Text(String::new()), None));
    assert!(values_equal(&Val::Null, &Val::Null, None));
    // An unmapped cross-dialect column is flagged `?`.
    let src = vec![col_full("g", "geometry", true, None, None, true)];
    let tgt = vec![col_full("g", "point", true, None, None, true)];
    let align = data_align(src, tgt, &["g"], &["g"], true).unwrap();
    assert!(align.cols[0].unknown_type);
    assert!(align.cols[0].canon.is_none());
}

/// R36 seam: a `bigint` key beyond `f64`'s exact range (2^53) must not
/// collapse two adjacent ids to “equal”. Snowflake ids live here, so the
/// old `f64`-only comparison could silently misalign the whole merge.
#[test]
pub(crate) fn numeric_comparison_keeps_bigint_precision() {
    use std::cmp::Ordering;
    assert!(!canon_cell_equal(
        "9007199254740993",
        "9007199254740992",
        "bigint"
    ));
    assert!(canon_cell_equal(
        "9007199254740992",
        "9007199254740992",
        "bigint"
    ));
    assert!(!canon_cell_equal(
        "9223372036854775807",
        "9223372036854775806",
        "bigint"
    ));
    assert_eq!(
        cmp_numeric_text("9007199254740993", "9007199254740992"),
        Ordering::Greater
    );
    assert_eq!(
        cmp_numeric_text("9223372036854775807", "9223372036854775806"),
        Ordering::Greater
    );
    // Decimals still normalise through the float fallback.
    assert_eq!(cmp_numeric_text("1.0", "1"), Ordering::Equal);
    assert!(canon_cell_equal("1.50", "1.5", "decimal(10,2)"));
    // The merge ordering keeps the exact distinction too.
    let modes = vec![PkCmp::Numeric];
    let a = vec![Val::Text("9007199254740993".into())];
    let b = vec![Val::Text("9007199254740992".into())];
    assert_eq!(cmp_pk_row(&a, &b, &modes), Ordering::Greater);
}

/// R36 seam: a NULL in a keyset tuple makes `k > NULL` never true, so the
/// next chunk read would come back empty and look like end-of-table. The
/// compare / transfer guards must detect it before issuing that query.
#[test]
pub(crate) fn null_primary_key_is_detected_before_a_keyset_seek() {
    assert!(pk_tuple_has_null(&[Val::Text("1".into()), Val::Null]));
    assert!(!pk_tuple_has_null(&[
        Val::Text("1".into()),
        Val::Text(String::new())
    ]));
    assert!(!pk_tuple_has_null(&[]));
    // The data-compare predicate renders a NULL key as SQL NULL (never
    // true), which is exactly why the guard exists.
    let pred = keyset_predicate(
        &["a".to_string()],
        &["int".to_string()],
        &[Val::Null],
        DatabaseType::Mysql,
    );
    assert!(pred.to_ascii_uppercase().contains("NULL"), "{pred}");
}

#[test]
pub(crate) fn data_select_and_keyset_are_dialect_aware() {
    let mysql = parse_database_type("mysql").unwrap();
    let pg = parse_database_type("postgres").unwrap();
    let cols = vec!["id".to_string(), "name".to_string()];
    let pk = vec!["id".to_string()];
    let types = vec!["int".to_string(), "varchar(20)".to_string()];
    let sql = build_data_select(mysql, "", "t", &cols, &pk, &types, "", None, DATA_CHUNK);
    assert_eq!(sql, "SELECT `id`, `name` FROM `t` ORDER BY `id` LIMIT 1000");
    let sql = build_data_select(
        mysql,
        "",
        "t",
        &cols,
        &pk,
        &types,
        "status = 'a'",
        Some(&[Val::Text("5".into())]),
        DATA_CHUNK,
    );
    assert!(
        sql.contains("WHERE (status = 'a') AND ((`id` > 5))"),
        "{sql}"
    );
    // Composite keyset: (a > 1) OR (a = 1 AND b > 2).
    let pk2 = vec!["a".to_string(), "b".to_string()];
    let types2 = vec!["int".to_string(), "int".to_string()];
    let pred = keyset_predicate(
        &pk2,
        &types2,
        &[Val::Text("1".into()), Val::Text("2".into())],
        mysql,
    );
    assert_eq!(pred, "((`a` > 1) OR (`a` = 1 AND `b` > 2))");
    // PostgreSQL quotes the schema separately.
    let sql = build_data_select(pg, "public", "t", &cols, &pk, &types, "", None, DATA_CHUNK);
    assert!(sql.contains("\"public\".\"t\""), "{sql}");
    assert!(sql.contains("ORDER BY \"id\""), "{sql}");
    // Chunk forecast.
    assert_eq!(chunk_total(Some(1000), Some(1)), 2);
    assert_eq!(chunk_total(None, Some(2500)), 3);
    assert_eq!(chunk_total(None, None), 0);
    assert_eq!(chunk_ceil(Some(0)), 0);
}

#[test]
pub(crate) fn data_feed_chunk_advances_the_keyset_cursor() {
    // A full chunk keeps the side open and advances the cursor to its last
    // row (the bug this guards: a missing cursor update re-reads page one).
    let full: Vec<Vec<Val>> = (1..=DATA_CHUNK)
        .map(|i| vec![Val::Text(i.to_string())])
        .collect();
    let mut s = DataSideStream::default();
    assert!(feed_chunk(&mut s, full, 1));
    assert!(!s.exhausted);
    assert_eq!(s.buf.len(), DATA_CHUNK);
    assert_eq!(s.last, Some(vec![Val::Text(DATA_CHUNK.to_string())]));
    // A short chunk advances further and marks the side exhausted.
    let short: Vec<Vec<Val>> = (1..=3)
        .map(|i| vec![Val::Text((DATA_CHUNK + i).to_string())])
        .collect();
    assert!(feed_chunk(&mut s, short, 1));
    assert!(s.exhausted);
    assert_eq!(s.buf.len(), DATA_CHUNK + 3);
    assert_eq!(s.last, Some(vec![Val::Text((DATA_CHUNK + 3).to_string())]));
    // An empty chunk yields nothing and leaves the side exhausted.
    assert!(!feed_chunk(&mut s, Vec::new(), 1));
    assert!(s.exhausted);
}

#[test]
pub(crate) fn data_diff_key_tabs_and_generates_sync() {
    let mut app = test_app();
    let mut list = ListState::default();
    list.select(Some(0));
    app.data_diff = Some(Box::new(DataDiffState {
        result: data_cmp_fixture(),
        tab: DataTab::Summary,
        list,
        scroll: 0,
        sync_sql: String::new(),
    }));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    data_diff_key(&mut app, &tx, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.data_diff.as_ref().unwrap().tab, DataTab::OnlySrc);
    data_diff_key(&mut app, &tx, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.data_diff.as_ref().unwrap().tab, DataTab::OnlyTgt);
    data_diff_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('g')));
    assert_eq!(app.data_diff.as_ref().unwrap().tab, DataTab::Sync);
    assert!(!app.data_diff.as_ref().unwrap().sync_sql.is_empty());
    // Tab from Sync returns to the summary.
    data_diff_key(&mut app, &tx, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.data_diff.as_ref().unwrap().tab, DataTab::Summary);
    data_diff_key(&mut app, &tx, KeyEvent::from(KeyCode::Esc));
    assert!(app.data_diff.is_none());
}

#[test]
pub(crate) fn data_diff_overlays_render_at_extreme_sizes() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    let mut list = ListState::default();
    list.select(Some(0));
    app.data_diff = Some(Box::new(DataDiffState {
        result: data_cmp_fixture(),
        tab: DataTab::Summary,
        list,
        scroll: 0,
        sync_sql: String::new(),
    }));
    let sizes = [
        (40u16, 12u16),
        (42, 22),
        (120, 40),
        (250, 70),
        (20, 6),
        (1, 1),
    ];
    for tab in [
        DataTab::Summary,
        DataTab::OnlySrc,
        DataTab::OnlyTgt,
        DataTab::Diff,
        DataTab::Sync,
    ] {
        if let Some(s) = app.data_diff.as_mut() {
            s.tab = tab;
            if tab == DataTab::Sync {
                s.sync_sql = generate_data_sync(&s.result);
            }
        }
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
    }
    // Picker in data mode, then mid-run progress.
    app.data_diff = None;
    let mut list = ListState::default();
    list.select(Some(0));
    app.diff_picker = Some(DiffPicker {
        mode: DiffPickMode::Table,
        kind: DiffKind::Data,
        stage: DiffPickStage::Lists,
        list,
        src_entries: vec!["b".into()],
        entries: vec!["b".into()],
        target_conn: None,
        target_db: String::new(),
        target_schema: String::new(),
        loading: false,
        comparing: false,
        where_input: "id > 0".into(),
        gen: 0,
    });
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    if let Some(p) = app.diff_picker.as_mut() {
        p.comparing = true;
    }
    app.data_progress = Some((2, 5));
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // WHERE prompt on its own.
    app.diff_picker = None;
    app.data_progress = None;
    app.data_where = Some(TextArea::default());
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
}

#[test]
pub(crate) fn data_diff_strings_have_english_translations() {
    use ui_text::Lang;
    assert_ne!(ui_text::t_lang("仅源", Lang::En), "仅源");
    assert_ne!(ui_text::t_lang("仅目标", Lang::En), "仅目标");
    assert_ne!(ui_text::t_lang("差异", Lang::En), "差异");
    assert_ne!(ui_text::t_lang("数据一致", Lang::En), "数据一致");
    assert_ne!(ui_text::t_lang("同步 SQL", Lang::En), "同步 SQL");
}

// ── data transfer (Alt-T) ──

pub(crate) fn transfer_wizard_fixture() -> TransferWizard {
    let conn = test_conn("mysql");
    let mut name_input = TextArea::from(vec!["orders".to_string()]);
    name_input.move_cursor(CursorMove::End);
    let mut conn_list = ListState::default();
    conn_list.select(Some(0));
    let mut opt_list = ListState::default();
    opt_list.select(Some(0));
    TransferWizard {
        step: TransferStep::Options,
        conn_list,
        conns: vec![conn.clone()],
        target_conn: conn.clone(),
        name_focus: NameFocus::Table,
        name_values: ["shop".into(), String::new(), "orders".into()],
        name_input,
        opt_list,
        mode: TransferMode::CreateAndCopy,
        conflict: TransferConflict::Stop,
        on_error: TransferOnError::Stop,
        where_input: String::new(),
        limit_input: String::new(),
        with_indexes: true,
        with_auto_increment: true,
        large_warn: None,
        error: None,
        prompt: None,
        submitted: false,
        src_conn: conn,
        src_db: "shop".into(),
        src_schema: String::new(),
        src_table: "orders".into(),
    }
}

pub(crate) fn transfer_src_cols() -> Vec<ColumnInfo> {
    vec![
        col_full("id", "int", false, None, None, true),
        col_full("name", "varchar(50)", false, None, Some("名称"), false),
        col_full("price", "decimal(10,2)", true, None, None, false),
        ColumnInfo {
            extra: Some("auto_increment".into()),
            ..col_full("seq", "bigint", false, None, None, false)
        },
        col_full("payload", "blob", true, None, None, false),
        col_full("created_at", "timestamp", true, None, None, false),
    ]
}

pub(crate) fn transfer_report_fixture() -> TransferReport {
    let mysql = parse_database_type("mysql").unwrap();
    TransferReport {
        src_label: "shop.orders".into(),
        tgt_label: "warehouse.orders".into(),
        src_db_type: mysql,
        tgt_db_type: mysql,
        mode: TransferMode::CreateAndCopy,
        conflict: TransferConflict::Stop,
        on_error: TransferOnError::Stop,
        tgt_db: "warehouse".into(),
        tgt_schema: String::new(),
        tgt_table: "orders".into(),
        tgt_conn_id: "id-mysql".into(),
        src_rows: 5000,
        moved: 5000,
        skipped: Vec::new(),
        aborted: None,
        cancelled: true,
        estimated: Some(5000),
        created: true,
        breakpoint: Some("4999".into()),
        warnings: Vec::new(),
        elapsed_ms: 2500,
        chunks_done: 5,
    }
}

#[test]
pub(crate) fn transfer_create_table_is_dialect_correct() {
    let cols = transfer_src_cols();
    let idx = vec![
        idx_info("PRIMARY", &["id"], true, true),
        idx_info("idx_name", &["name"], false, false),
    ];
    let pk = vec!["id".to_string()];
    let mysql = parse_database_type("mysql").unwrap();
    let pg = parse_database_type("postgres").unwrap();
    // MySQL → MySQL keeps the dialect verbatim (inline COMMENT + AUTO_INCREMENT).
    let (script, warns) = generate_transfer_create(
        &cols,
        &idx,
        &pk,
        mysql,
        mysql,
        "",
        "orders_copy",
        true,
        true,
    )
    .unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    assert!(script.contains("CREATE TABLE `orders_copy` ("));
    assert!(script.contains("`seq` bigint NOT NULL AUTO_INCREMENT"));
    assert!(script.contains("PRIMARY KEY (`id`)"));
    assert!(script.contains("`name` varchar(50) NOT NULL COMMENT '名称'"));
    assert!(script.contains("CREATE INDEX `idx_name` ON `orders_copy` (`name`);"));
    // MySQL → PostgreSQL maps types, serial and COMMENT ON.
    let (script, _) =
        generate_transfer_create(&cols, &idx, &pk, mysql, pg, "", "orders_copy", true, true)
            .unwrap();
    assert!(script.contains("CREATE TABLE \"orders_copy\" ("));
    // Cross-dialect auto-increment stays a plain integer (no stale sequence).
    assert!(script.contains("\"seq\" bigint NOT NULL"));
    assert!(!script.contains("bigserial"));
    assert!(script.contains("\"payload\" bytea"));
    assert!(script.contains("\"name\" character varying(50) NOT NULL"));
    assert!(script.contains("COMMENT ON COLUMN \"orders_copy\".\"name\" IS '名称';"));
    assert!(script.contains("PRIMARY KEY (\"id\")"));
    assert!(!script.contains("AUTO_INCREMENT"));
    assert!(script.contains("CREATE INDEX \"idx_name\" ON \"orders_copy\" (\"name\");"));
}

#[test]
pub(crate) fn transfer_without_auto_increment_keeps_a_plain_type() {
    let cols = transfer_src_cols();
    let idx = vec![idx_info("PRIMARY", &["id"], true, true)];
    let pk = vec!["id".to_string()];
    let mysql = parse_database_type("mysql").unwrap();
    let pg = parse_database_type("postgres").unwrap();
    let (script, _) =
        generate_transfer_create(&cols, &idx, &pk, mysql, pg, "", "t", false, false).unwrap();
    assert!(script.contains("\"seq\" bigint"));
    assert!(!script.contains("bigserial"));
    assert!(!script.contains("AUTO_INCREMENT"));
    assert!(!script.contains("CREATE INDEX"));
}

#[test]
pub(crate) fn transfer_warns_when_the_source_has_no_primary_key() {
    let cols = vec![col_full("a", "int", true, None, None, false)];
    let mysql = parse_database_type("mysql").unwrap();
    let (script, warns) =
        generate_transfer_create(&cols, &[], &[], mysql, mysql, "", "t", true, true).unwrap();
    assert!(!script.contains("PRIMARY KEY"));
    assert_eq!(warns.len(), 1);
}

#[test]
pub(crate) fn transfer_type_mapping_and_tz_normalisation() {
    let mysql = parse_database_type("mysql").unwrap();
    let pg = parse_database_type("postgres").unwrap();
    let c = |ty: &str| col_full("c", ty, true, None, None, false);
    assert_eq!(transfer_target_type(&c("blob"), pg, true, true), "bytea");
    assert_eq!(
        transfer_target_type(&c("varchar(20)"), pg, true, true),
        "character varying(20)"
    );
    assert_eq!(
        transfer_target_type(&c("tinyint(1)"), pg, true, true),
        "smallint"
    );
    assert_eq!(transfer_target_type(&c("int"), pg, true, true), "integer");
    // Same-dialect PostgreSQL keeps the auto-increment counter as `serial`.
    let auto = ColumnInfo {
        extra: Some("auto_increment".into()),
        ..col_full("id", "int", false, None, None, true)
    };
    assert_eq!(transfer_target_type(&auto, pg, false, true), "serial");
    // Same dialect is verbatim.
    assert_eq!(
        transfer_target_type(&c("varchar(20)"), mysql, false, true),
        "varchar(20)"
    );
    // temporal literal normalisation for a MySQL target
    assert_eq!(
        strip_tz_suffix("2024-01-01 12:00:00+00").as_deref(),
        Some("2024-01-01 12:00:00")
    );
    assert_eq!(
        strip_tz_suffix("2024-01-01T12:00:00Z").as_deref(),
        Some("2024-01-01 12:00:00")
    );
    assert_eq!(strip_tz_suffix("2024-01-01 12:00:00"), None);
}

#[test]
pub(crate) fn transfer_append_aligns_by_name_and_keeps_the_pk_cursor() {
    let mysql = parse_database_type("mysql").unwrap();
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("a", "int", true, None, None, false),
        col_full("b", "int", true, None, None, false),
    ];
    let tgt = vec![
        col_full("id", "bigint", false, None, None, true),
        col_full("b", "int", true, None, None, false),
        col_full("c", "text", true, None, None, false),
    ];
    let pk = vec!["id".to_string()];
    let align = build_transfer_align(&src, &tgt, &pk, mysql, mysql, false).unwrap();
    assert_eq!(align.cols.len(), 2);
    assert_eq!(align.cols[0].src_name, "id");
    assert_eq!(align.cols[1].src_name, "b");
    assert_eq!(align.pk_idx, vec![0]);
    assert!(align.keyset());
    assert_eq!(
        align
            .pk_of(&[Val::Text("7".into()), Val::Text("x".into())])
            .unwrap(),
        vec![Val::Text("7".into())]
    );
    // A key column missing on the target disables the keyset path.
    let tgt_no_id = vec![col_full("b", "int", true, None, None, false)];
    let align = build_transfer_align(&src, &tgt_no_id, &pk, mysql, mysql, false).unwrap();
    assert!(!align.keyset());
}

#[test]
pub(crate) fn transfer_count_probe_is_bounded_and_honours_the_limit() {
    let mysql = parse_database_type("mysql").unwrap();
    let sql = build_transfer_count_sql(mysql, "", "orders", "id > 10", None);
    assert!(sql.contains("LIMIT 1000001"), "{sql}");
    assert!(sql.contains("WHERE (id > 10)"));
    let sql = build_transfer_count_sql(mysql, "", "orders", "", Some(50));
    assert!(sql.contains("LIMIT 50"), "{sql}");
    // The offset fallback orders by the first column.
    let sql = build_transfer_offset_select(
        mysql,
        "",
        "orders",
        &["id".to_string(), "name".to_string()],
        "",
        1000,
        2000,
    );
    assert!(sql.contains("ORDER BY `id`"));
    assert!(sql.contains("LIMIT 1000 OFFSET 2000"));
}

#[test]
pub(crate) fn transfer_overwrite_flows_through_a_red_confirmation() {
    assert_eq!(TransferMode::CreateAndCopy.next(), TransferMode::CreateOnly);
    assert_eq!(TransferMode::CreateOnly.next(), TransferMode::Append);
    assert_eq!(TransferMode::Append.next(), TransferMode::CreateAndCopy);
    let mut app = test_app();
    app.transfer = Some(Box::new(transfer_wizard_fixture()));
    assert_eq!(
        app.transfer.as_ref().unwrap().conflict,
        TransferConflict::Stop
    );
    transfer_cycle_conflict(&mut app);
    assert_eq!(
        app.transfer.as_ref().unwrap().conflict,
        TransferConflict::Drop
    );
    assert_eq!(app.transfer.as_ref().unwrap().step, TransferStep::Confirm);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    transfer_confirm_key(&mut app, &tx, KeyEvent::from(KeyCode::Esc));
    assert_eq!(
        app.transfer.as_ref().unwrap().conflict,
        TransferConflict::Stop
    );
    assert_eq!(app.transfer.as_ref().unwrap().step, TransferStep::Options);
}

#[test]
pub(crate) fn transfer_breakpoint_report_summarises_the_run() {
    let rep = transfer_report_fixture();
    assert_eq!(rep.rate(), 2000);
    assert!(!rep.ok());
    let text = transfer_summary_text(&rep);
    assert!(text.contains("断点主键: 4999"), "{text}");
    assert!(text.contains("已中止"), "{text}");
    assert!(text.contains("已完成块: 5"), "{text}");
}

#[test]
pub(crate) fn transfer_overlays_render_at_extreme_sizes() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.transfer = Some(Box::new(transfer_wizard_fixture()));
    let sizes = [
        (40u16, 12u16),
        (42, 22),
        (120, 40),
        (250, 70),
        (20, 6),
        (1, 1),
    ];
    for step in [
        TransferStep::Connection,
        TransferStep::Name,
        TransferStep::Options,
        TransferStep::Confirm,
    ] {
        if let Some(w) = app.transfer.as_mut() {
            w.step = step;
        }
        for (w, h) in sizes {
            draw(&mut app, w, h);
        }
    }
    // The WHERE / LIMIT prompt on top of the wizard.
    app.transfer.as_mut().unwrap().step = TransferStep::Options;
    transfer_open_prompt(&mut app, TransferField::Where);
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // The in-flight progress overlay.
    if let Some(w) = app.transfer.as_mut() {
        w.prompt = None;
        w.submitted = true;
    }
    app.transfer_progress = Some((1234, 2, 900, Some(100000)));
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
    // The completion summary.
    app.transfer = None;
    app.transfer_report = Some(Box::new(transfer_report_fixture()));
    for (w, h) in sizes {
        draw(&mut app, w, h);
    }
}

#[test]
pub(crate) fn transfer_large_warn_points_at_the_start_row() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    // The fixture is on the options step with the cursor on row 0.
    app.transfer = Some(Box::new(transfer_wizard_fixture()));
    app.transfer_gen = 0;
    apply_op_result(
        &mut app,
        OpResult::TransferNeedsConfirm {
            gen: 0,
            estimated: 2_000_000,
        },
        &tx,
    );
    let w = app.transfer.as_ref().unwrap();
    assert_eq!(w.large_warn, Some(2_000_000));
    assert!(!w.submitted);
    // The cursor sits on “start”, so the advertised “press Enter again” is
    // a single key.
    assert_eq!(w.opt_list.selected(), Some(TRANSFER_OPTION_ROWS - 1));
}

#[test]
pub(crate) fn transfer_running_esc_requests_an_abort() {
    let mut app = test_app();
    let mut w = transfer_wizard_fixture();
    w.submitted = true;
    app.transfer = Some(Box::new(w));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    transfer_key(&mut app, &tx, KeyEvent::from(KeyCode::Esc));
    assert!(app.transfer_cancel.load(Ordering::Relaxed));
    // Other keys are ignored while the job runs.
    app.transfer_cancel.store(false, Ordering::Relaxed);
    transfer_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('x')));
    assert!(!app.transfer_cancel.load(Ordering::Relaxed));
}

#[test]
pub(crate) fn transfer_strings_have_english_translations() {
    use ui_text::Lang;
    for s in [
        "数据搬运仅支持 SQL 连接",
        "开始搬运",
        "断点主键",
        "已搬运",
        "⚠ 覆盖会先 DROP 目标表（数据不可恢复）· Enter 确认 · Esc 返回",
        "DROP TABLE 会永久删除目标表的全部数据，且无法恢复。",
        // Labels rendered through `t()` indirectly (the enum `label()`s).
        "建表+搬数据",
        "插入已有表",
        "报错停下",
        "停止报行号",
        "跳过继续",
    ] {
        assert_ne!(ui_text::t_lang(s, Lang::En), s, "{s}");
    }
}

// ── connection bundle import / export (Alt-E / Alt-I) ──

/// A MySQL connection with a colour and an SSH tunnel, for the export round trip.
pub(crate) fn export_fixture_conn(name: &str) -> ConnectionConfig {
    let mut cfg = new_connection_config(
        format!("id-{name}"),
        name.into(),
        DatabaseType::Mysql,
        "db.internal".into(),
        3306,
        "root".into(),
        "s3cret".into(),
        Some("shop".into()),
        true,
        None,
    )
    .unwrap();
    cfg.color = Some("#e06c75".into());
    cfg.transport_layers = vec![TransportLayerConfig::Ssh(SshTunnelConfig {
        id: "ssh-1".into(),
        name: "ssh".into(),
        enabled: true,
        host: "jump".into(),
        port: 22,
        user: "ops".into(),
        password: "sshpw".into(),
        key_path: "/home/o/.ssh/id".into(),
        key_passphrase: "kp".into(),
        connect_timeout_secs: 5,
        expose_lan: false,
        use_ssh_agent: false,
        ssh_agent_sock_path: String::new(),
        auth_method: "password".into(),
        allow_exec_channel_proxy: false,
        profile_id: String::new(),
    })];
    cfg
}

#[test]
pub(crate) fn conn_bundle_excludes_passwords_by_default_and_roundtrips() {
    let cfg = export_fixture_conn("prod-mysql");
    let plain = conn_bundle_json(std::slice::from_ref(&cfg), false);
    // The security default: no secret anywhere, and no `password` key at all.
    assert!(!plain.contains("s3cret"), "password leaked: {plain}");
    assert!(!plain.contains("sshpw"), "ssh password leaked: {plain}");
    assert!(!plain.contains("kp"), "ssh passphrase leaked: {plain}");
    assert!(
        !plain.contains("\"password\":"),
        "password key present: {plain}"
    );

    let v: serde_json::Value = serde_json::from_str(&plain).unwrap();
    assert_eq!(v["format"], "dbxt-connections");
    assert_eq!(v["version"], CONN_BUNDLE_VERSION);

    let (source, conns) = sniff_connections(&plain).unwrap();
    assert_eq!(source, ConnSource::Dbxt);
    assert_eq!(conns.len(), 1);
    let c = &conns[0];
    assert_eq!(c.name, "prod-mysql");
    assert_eq!(c.db_type.as_deref(), Some("mysql"));
    assert_eq!(c.host, "db.internal");
    assert_eq!(c.port, Some(3306));
    assert_eq!(c.user, "root");
    assert_eq!(c.database.as_deref(), Some("shop"));
    assert!(c.ssl);
    assert_eq!(c.color.as_deref(), Some("#e06c75"));
    assert!(c.password.is_none());
    let ssh = c.ssh.as_ref().expect("ssh tunnel survived");
    assert_eq!(ssh.host, "jump");
    assert_eq!(ssh.port, 22);
    assert_eq!(ssh.user, "ops");
    assert_eq!(ssh.key_path.as_deref(), Some("/home/o/.ssh/id"));
    assert!(ssh.password.is_none());

    // Explicit password export keeps them, and they re-parse.
    let with_pw = conn_bundle_json(std::slice::from_ref(&cfg), true);
    assert!(with_pw.contains("s3cret"));
    let (_, conns2) = sniff_connections(&with_pw).unwrap();
    assert_eq!(conns2[0].password.as_deref(), Some("s3cret"));
    assert_eq!(
        conns2[0].ssh.as_ref().unwrap().password.as_deref(),
        Some("sshpw")
    );
}

#[test]
pub(crate) fn driver_alias_table_maps_both_ecosystems() {
    assert_eq!(map_driver_to_db_type("mysql8"), Some("mysql"));
    assert_eq!(map_driver_to_db_type("mariadb"), Some("mysql"));
    assert_eq!(map_driver_to_db_type("PostgreSQL"), Some("postgres"));
    assert_eq!(map_driver_to_db_type("MSSQL"), Some("sqlserver"));
    assert_eq!(map_driver_to_db_type("duckdb"), Some("duckdb"));
    assert_eq!(map_driver_to_db_type("mongodb"), Some("mongodb"));
    assert_eq!(map_driver_to_db_type("Redis"), Some("redis"));
    // A canonical dbxt name passes straight through.
    assert_eq!(
        map_driver_to_db_type("cloudflare-d1"),
        Some("cloudflare-d1")
    );
    // An unknown driver maps to nothing (and lands in the skipped list).
    assert_eq!(map_driver_to_db_type("derby"), None);
    assert_eq!(map_driver_to_db_type(""), None);
}

#[test]
pub(crate) fn dbeaver_data_sources_map_drivers_database_and_ssh() {
    let json = r#"{
          "connections": {
            "mysql8-1": {"provider":"mysql8","name":"shop","configuration":{"host":"db","port":"3306","database":"shop","user":"root"}},
            "pg-1": {"provider":"postgresql","name":"analytics","configuration":{"host":"10.0.0.5","port":5432,"database":"dw","user":"pg","sslMode":"require"},
                     "ssh-tunnel":{"host":"jump","port":"22","user":"ops","auth-type":"public-key","private-key-path":"/k"}},
            "duck-1": {"driver":"duckdb","name":"local","configuration":{"url":"jdbc:duckdb:/tmp/x.duckdb"}},
            "ora-1": {"provider":"oracle","name":"ora","configuration":{"host":"ora","port":"1521","database":"svc","user":"scott"}},
            "derby-1": {"provider":"derby","name":"legacy","configuration":{"host":"x","port":"1527"}}
          }
        }"#;
    let (source, conns) = sniff_connections(json).unwrap();
    assert_eq!(source, ConnSource::DBeaver);
    let by = |n: &str| conns.iter().find(|c| c.name == n).unwrap();
    assert_eq!(by("shop").db_type.as_deref(), Some("mysql"));
    assert_eq!(by("shop").port, Some(3306));
    assert_eq!(by("shop").user, "root");
    assert!(by("shop").needs_password);
    assert_eq!(by("analytics").db_type.as_deref(), Some("postgres"));
    assert_eq!(by("analytics").port, Some(5432));
    assert!(by("analytics").ssl, "sslMode=require maps to ssl");
    let ssh = by("analytics").ssh.as_ref().expect("ssh tunnel");
    assert_eq!(ssh.host, "jump");
    assert_eq!(ssh.port, 22);
    assert_eq!(ssh.user, "ops");
    assert_eq!(ssh.key_path.as_deref(), Some("/k"));
    assert_eq!(ssh.auth_method, "key");
    assert_eq!(by("local").db_type.as_deref(), Some("duckdb"));
    assert!(by("local").database.is_none());
    assert_eq!(by("ora").db_type.as_deref(), Some("oracle"));
    assert_eq!(by("ora").database.as_deref(), Some("svc"));
    assert!(by("legacy").db_type.is_none());

    let plan = build_conn_import_plan(source, "x".into(), conns, &[]);
    assert_eq!(plan.rows.len(), 4);
    assert_eq!(plan.skipped.len(), 1);
    assert!(plan.skipped[0].contains("derby"), "{:?}", plan.skipped);
}

#[test]
pub(crate) fn navicat_ncx_xml_parses_elements_attributes_and_ssh() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Connections>
  <Connection>
    <Name>hero</Name>
    <ConnType>MYSQL</ConnType>
    <Host>127.0.0.1</Host>
    <Port>3306</Port>
    <UserName>root</UserName>
    <Password>ENC</Password>
    <Database>hero_db</Database>
    <UseSSL>true</UseSSL>
  </Connection>
  <Connection Name="pg-prod" ConnType="POSTGRESQL" Host="10.1.2.3" Port="5432" UserName="pg" Database="dw">
    <SSH_Enabled>true</SSH_Enabled>
    <SSH_Host>jump</SSH_Host>
    <SSH_Port>22</SSH_Port>
    <SSH_UserName>ops</SSH_UserName>
  </Connection>
</Connections>"#;
    let (source, conns) = sniff_connections(xml).unwrap();
    assert_eq!(source, ConnSource::Navicat);
    assert_eq!(conns.len(), 2);
    assert_eq!(conns[0].name, "hero");
    assert_eq!(conns[0].db_type.as_deref(), Some("mysql"));
    assert_eq!(conns[0].port, Some(3306));
    assert_eq!(conns[0].user, "root");
    assert_eq!(conns[0].database.as_deref(), Some("hero_db"));
    assert!(conns[0].ssl);
    assert!(conns[0].needs_password);
    assert!(conns[0].password.is_none());
    assert_eq!(conns[1].name, "pg-prod");
    assert_eq!(conns[1].db_type.as_deref(), Some("postgres"));
    assert_eq!(conns[1].host, "10.1.2.3");
    assert_eq!(conns[1].port, Some(5432));
    assert_eq!(conns[1].database.as_deref(), Some("dw"));
    let ssh = conns[1].ssh.as_ref().expect("ssh tunnel");
    assert_eq!(ssh.host, "jump");
    assert_eq!(ssh.port, 22);
    assert_eq!(ssh.user, "ops");
}

#[test]
pub(crate) fn sniff_rejects_empty_and_unknown_files() {
    assert!(sniff_connections("   ").is_err());
    assert!(sniff_connections("not json or xml").is_err());
    assert!(sniff_connections(r#"{"foo": 1}"#).is_err());
    assert!(sniff_connections("<html><body>nope</body></html>").is_err());
}

pub(crate) fn import_row(name: &str, policy: DupPolicy) -> ConnImportRow {
    ConnImportRow {
        conn: ImportConn {
            name: name.into(),
            driver: "mysql".into(),
            db_type: Some("mysql".into()),
            host: "h".into(),
            port: Some(3306),
            ..ImportConn::default()
        },
        dup: true,
        policy,
        selected: true,
    }
}

#[test]
pub(crate) fn conn_import_duplicate_policy_resolves() {
    let existing = vec![export_fixture_conn("dup")];
    // Skip: nothing imported, one duplicate skipped.
    let t = resolve_import_targets(&[import_row("dup", DupPolicy::Skip)], &existing);
    assert!(t.items.is_empty());
    assert_eq!(t.skipped, 1);
    // Overwrite: the existing id is reused and routed through remove-then-add.
    let t = resolve_import_targets(&[import_row("dup", DupPolicy::Overwrite)], &existing);
    assert_eq!(t.items.len(), 1);
    assert_eq!(t.items[0].0.as_deref(), Some(existing[0].id.as_str()));
    assert_eq!(t.items[0].1.id, existing[0].id);
    // Both: renamed with the suffix, inserted alongside.
    let t = resolve_import_targets(&[import_row("dup", DupPolicy::Both)], &existing);
    assert_eq!(t.items.len(), 1);
    assert_eq!(t.items[0].1.name, "dup-imported");
    assert!(t.items[0].0.is_none());
    // A second “both” picks a distinct suffix.
    let used = vec!["dup".to_string(), "dup-imported".to_string()];
    assert_eq!(unique_import_name("dup", &used), "dup-imported2");
    // An unselected row is ignored entirely.
    let mut row = import_row("dup", DupPolicy::Skip);
    row.selected = false;
    let t = resolve_import_targets(&[row], &existing);
    assert!(t.items.is_empty());
    assert_eq!(t.skipped, 0);
}

#[test]
pub(crate) fn conn_import_fills_default_port_and_counts_missing_passwords() {
    let row = ConnImportRow {
        conn: ImportConn {
            name: "pg".into(),
            driver: "postgresql".into(),
            db_type: Some("postgres".into()),
            host: "h".into(),
            port: None,
            needs_password: true,
            ..ImportConn::default()
        },
        dup: false,
        policy: DupPolicy::Skip,
        selected: true,
    };
    let t = resolve_import_targets(&[row], &[]);
    assert_eq!(t.items.len(), 1);
    assert_eq!(t.items[0].1.port, 5432);
    assert_eq!(t.needs_password, 1);
    assert!(t.errors.is_empty());
}

#[test]
pub(crate) fn conn_export_password_toggle_requires_red_confirmation() {
    let mut app = test_app();
    app.connections = vec![test_conn("mysql")];
    open_conn_export(&mut app);
    assert!(app.conn_export.is_some());
    // `p` raises the red confirmation without enabling the export yet.
    conn_export_key(&mut app, KeyEvent::from(KeyCode::Char('p')));
    assert!(app.conn_export.as_ref().unwrap().confirm_pw);
    assert!(!app.conn_export.as_ref().unwrap().include_passwords);
    // Esc backs out of the confirmation, not the overlay.
    conn_export_key(&mut app, KeyEvent::from(KeyCode::Esc));
    assert!(app.conn_export.is_some());
    assert!(!app.conn_export.as_ref().unwrap().confirm_pw);
    assert!(!app.conn_export.as_ref().unwrap().include_passwords);
    // p then Enter enables it.
    conn_export_key(&mut app, KeyEvent::from(KeyCode::Char('p')));
    conn_export_key(&mut app, KeyEvent::from(KeyCode::Enter));
    assert!(app.conn_export.as_ref().unwrap().include_passwords);
    // Esc then closes the overlay.
    conn_export_key(&mut app, KeyEvent::from(KeyCode::Esc));
    assert!(app.conn_export.is_none());
    app.conn_export = None;
}

#[test]
pub(crate) fn conn_import_plan_key_cycles_duplicate_policy() {
    let plan = build_conn_import_plan(
        ConnSource::DBeaver,
        "x".into(),
        vec![ImportConn {
            name: "dup".into(),
            driver: "mysql".into(),
            db_type: Some("mysql".into()),
            host: "h".into(),
            port: Some(3306),
            ..ImportConn::default()
        }],
        &[export_fixture_conn("dup")],
    );
    let mut app = test_app();
    app.connections = vec![export_fixture_conn("dup")];
    app.conn_import_plan = Some(Box::new(plan));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    // `b` sets keep-both on every duplicate, no confirmation needed.
    conn_import_plan_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('b')));
    assert_eq!(
        app.conn_import_plan.as_ref().unwrap().rows[0].policy,
        DupPolicy::Both
    );
    // `r` is guarded by the red overwrite layer...
    conn_import_plan_key(&mut app, &tx, KeyEvent::from(KeyCode::Char('r')));
    assert!(app.conn_import_plan.as_ref().unwrap().confirm.is_some());
    // ...and Enter applies it.
    conn_import_plan_key(&mut app, &tx, KeyEvent::from(KeyCode::Enter));
    assert!(app.conn_import_plan.as_ref().unwrap().confirm.is_none());
    assert_eq!(
        app.conn_import_plan.as_ref().unwrap().rows[0].policy,
        DupPolicy::Overwrite
    );
    // Space toggles the row's inclusion.
    conn_import_plan_key(&mut app, &tx, KeyEvent::from(KeyCode::Char(' ')));
    assert!(!app.conn_import_plan.as_ref().unwrap().rows[0].selected);
    // Esc drops the preview entirely.
    conn_import_plan_key(&mut app, &tx, KeyEvent::from(KeyCode::Esc));
    assert!(app.conn_import_plan.is_none());
}

// ── R41: connection quick-switch, statement scope, error box ──

pub(crate) fn conn(id: &str, name: &str, db_type: &str) -> ConnectionConfig {
    new_connection_config(
        id.to_string(),
        name.to_string(),
        parse_database_type(db_type).unwrap(),
        "127.0.0.1".into(),
        3306,
        "u".into(),
        "p".into(),
        None,
        false,
        None,
    )
    .unwrap()
}

/// Run a closure on a current-thread Tokio runtime so code paths that call
/// `tokio::spawn` (connection switching, table loads) work in tests.
pub(crate) fn run_rt<F: FnOnce()>(f: F) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { f() });
}

pub(crate) fn page_of(table: &str) -> PageState {
    PageState {
        table: table.into(),
        schema: String::new(),
        table_type: Some("TABLE".into()),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: None,
        keyset: None,
    }
}

/// The statement splitter must ignore semicolons inside string literals,
/// quoted identifiers and comments, and honour doubled quotes / backslash
/// escapes.
#[test]
pub(crate) fn statement_ranges_ignore_literals_and_comments() {
    let text = "SELECT ';' AS a; SELECT \"b;c\"; -- trailing ; comment\nSELECT 3; /* ; */ SELECT 4";
    let ranges = statement_ranges(text);
    let chars: Vec<char> = text.chars().collect();
    let stmts: Vec<String> = ranges
        .iter()
        .map(|&(s, e)| chars[s..e].iter().collect::<String>())
        .collect();
    assert_eq!(stmts.len(), 4, "four statements, got {stmts:?}");
    assert_eq!(stmts[0], "SELECT ';' AS a");
    assert_eq!(stmts[1], "SELECT \"b;c\"");
    assert_eq!(stmts[2], "-- trailing ; comment\nSELECT 3");
    assert_eq!(stmts[3], "/* ; */ SELECT 4");
    // A doubled quote keeps the literal open: the `;` stays inside it.
    let doubled = statement_ranges("SELECT 'a'';b'");
    assert_eq!(doubled.len(), 1);
    // A trailing semicolon yields no empty statement.
    assert_eq!(statement_ranges("SELECT 1;").len(), 1);
}

/// The cursor-to-statement resolver picks the span under the cursor and
/// falls forward across a separator / whitespace gap.
#[test]
pub(crate) fn statement_range_at_follows_the_cursor() {
    let text = "SELECT 1; SELECT 2;\nSELECT 3";
    let ranges = statement_ranges(text);
    let chars: Vec<char> = text.chars().collect();
    let at = |off: usize| {
        let (s, e) = statement_range_at(&ranges, off).unwrap();
        chars[s..e].iter().collect::<String>()
    };
    assert_eq!(at(0), "SELECT 1");
    assert_eq!(at(7), "SELECT 1"); // on the statement
    assert_eq!(at(8), "SELECT 2"); // on the `;` separator -> next
    assert_eq!(at(10), "SELECT 2");
    assert_eq!(at(19), "SELECT 3"); // trailing newline -> last
    assert!(statement_range_at(&[], 0).is_none());
}

/// A selection runs only the highlighted text, trimmed of surrounding
/// whitespace.
#[test]
pub(crate) fn editor_selection_is_trimmed_to_the_highlighted_sql() {
    let mut app = test_app();
    app.set_editor_text("SELECT 1;\n   SELECT 2;\nSELECT 3");
    assert!(editor_selection_text(&app).is_none(), "no selection yet");
    // Select all of line 1 (which has leading spaces) through its `;`.
    app.editor.move_cursor(CursorMove::Jump(1, 0));
    app.editor.start_selection();
    app.editor.move_cursor(CursorMove::Jump(1, 12));
    assert_eq!(editor_selection_text(&app).as_deref(), Some("SELECT 2;"));
    // A whitespace-only selection is treated as no selection.
    app.editor.cancel_selection();
    app.editor.move_cursor(CursorMove::Jump(1, 0));
    app.editor.start_selection();
    app.editor.move_cursor(CursorMove::Jump(1, 3));
    assert!(editor_selection_text(&app).is_none());
}

/// `Alt-<n>` switches to the Nth connection and `Alt-Tab` / `Alt-`` toggles
/// back — the last-connection double buffer.
#[test]
pub(crate) fn alt_digit_switches_connections_and_alt_tab_toggles_back() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.connections = vec![
            conn("id-a", "A", "mysql"),
            conn("id-b", "B", "postgres"),
            conn("id-c", "C", "sqlite"),
        ];
        app.conn_list.select(Some(0));
        app.selected = Some(conn("id-a", "A", "mysql"));
        app.backend_kind = Backend::Sql;
        // Alt-3 jumps straight to the third connection.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT),
        );
        assert_eq!(app.selected.as_ref().unwrap().id, "id-c");
        assert_eq!(app.last_conn_id.as_deref(), Some("id-a"));
        // Alt-Tab toggles back to A, and again to C.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT),
        );
        assert_eq!(app.selected.as_ref().unwrap().id, "id-a");
        assert_eq!(app.last_conn_id.as_deref(), Some("id-c"));
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('`'), KeyModifiers::ALT),
        );
        assert_eq!(app.selected.as_ref().unwrap().id, "id-c");
        // Alt-9 out of range reports instead of switching.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('9'), KeyModifiers::ALT),
        );
        assert_eq!(app.selected.as_ref().unwrap().id, "id-c");
        assert!(app.status.contains("没有第"));
    });
}

/// A switch remembers where the user was and restores a same-named table on
/// the new connection; a missing table falls back to the first screen with a
/// landing hint.
#[test]
pub(crate) fn connection_switch_restores_a_same_named_table() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.connections = vec![conn("id-a", "A", "mysql"), conn("id-b", "B", "mysql")];
        app.conn_list.select(Some(0));
        app.selected = Some(conn("id-a", "A", "mysql"));
        app.backend_kind = Backend::Sql;
        app.databases = vec!["shop".into()];
        app.db_index = 0;
        app.tables_all = vec![table_info("orders", "TABLE"), table_info("users", "TABLE")];
        apply_table_filter(&mut app);
        app.page_state = Some(page_of("orders"));
        // Alt-2 switches to B, carrying `shop.orders`.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT),
        );
        assert_eq!(app.selected.as_ref().unwrap().id, "id-b");
        let gen = app.conn_gen;
        apply_op_result(
            &mut app,
            OpResult::Databases {
                databases: vec!["shop".into()],
                warning: None,
                gen,
            },
            &tx,
        );
        assert_eq!(app.current_db(), "shop");
        let tgen = app.tables_gen;
        apply_op_result(
            &mut app,
            OpResult::TablesFor {
                tables: vec![table_info("orders", "TABLE"), table_info("other", "TABLE")],
                gen: tgen,
            },
            &tx,
        );
        assert_eq!(
            app.page_state.as_ref().map(|p| p.table.as_str()),
            Some("orders"),
            "the same-named table is reopened"
        );
        assert!(app.nav_landing.as_deref().unwrap_or("").contains("orders"));
    });
}

/// A stale `list_databases` reply (older `conn_gen`) is dropped, so a fast
/// switch cannot be clobbered by the connection it left.
#[test]
pub(crate) fn stale_database_list_is_discarded() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.connections = vec![conn("id-a", "A", "mysql"), conn("id-b", "B", "mysql")];
        app.selected = Some(conn("id-a", "A", "mysql"));
        app.conn_gen = 5;
        apply_op_result(
            &mut app,
            OpResult::Databases {
                databases: vec!["stale".into()],
                warning: None,
                gen: 4,
            },
            &tx,
        );
        assert!(app.databases.is_empty(), "stale reply dropped");
        apply_op_result(
            &mut app,
            OpResult::Databases {
                databases: vec!["fresh".into()],
                warning: None,
                gen: 5,
            },
            &tx,
        );
        assert_eq!(app.databases, vec!["fresh".to_string()]);
    });
}

/// The execution-error box starts compact (first line + line count) and
/// `Enter` widens it; `Esc` closes it.
#[test]
pub(crate) fn error_popup_compacts_then_expands() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Preview;
    open_error_popup(&mut app, "line one\nline two\nline three");
    let p = app.error_popup.as_ref().unwrap();
    assert_eq!(p.lines.len(), 3);
    assert!(!p.expanded, "starts compact");
    // A compact box renders at 42×22 without panicking.
    let rows = draw(&mut app, 42, 22);
    let text: String = rows
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(text.contains("执行错误"), "{text}");
    key(&mut app, &tx, KeyEvent::from(KeyCode::Enter));
    assert!(app.error_popup.as_ref().unwrap().expanded, "Enter expands");
    key(&mut app, &tx, KeyEvent::from(KeyCode::Enter));
    assert!(app.error_popup.is_none(), "Enter again closes");
}

/// R41 small-screen wrap-up: the connection form and the history panel draw
/// at 42×22 without panicking, with the form labels abbreviated.
#[test]
pub(crate) fn narrow_form_and_history_render_without_panicking() {
    let mut app = test_app();
    app.page = Page::NewConn;
    app.form = ConnForm::default();
    app.form.ssh_enabled = true;
    let rows = draw(&mut app, 42, 22);
    assert!(
        rows.iter().any(|r| r.contains("ssh.host")),
        "SSH label abbreviated"
    );
    assert!(
        rows.iter().any(|r| r.contains("type")),
        "db_type abbreviated"
    );
    // History panel at 42×22 with a long statement: the preview must not
    // push the panel outside the buffer.
    app.page = Page::Browse;
    app.history_open = true;
    app.history_rows = vec![history_row(
            "1",
            "SELECT very_long_column_name, another_long_column FROM a_very_long_table_name WHERE x = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
        )];
    app.history_view = vec![0];
    app.history_list.select(Some(0));
    let rows = draw(&mut app, 42, 22);
    // The TestBackend pads each wide CJK glyph with a space cell, so strip
    // whitespace before matching a multi-character title.
    let text: String = rows
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(text.contains("查询历史"), "{text}");
}

// ── R57: results row selection + batch statements + Redis cursor-preserving delete ──

pub(crate) fn test_tx() -> Tx {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    tx
}

/// An App showing a browsable `orders` table whose first column is the
/// primary key, so `batch_target` resolves. `rows` is the row count.
pub(crate) fn orders_app(cols: &[(&str, &str)], rows: usize) -> App {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.page_state = Some(orders_page(None));
    let columns: Vec<ColumnInfo> = cols
        .iter()
        .enumerate()
        .map(|(i, (n, t))| {
            let mut c = col_info(n, t);
            c.is_primary_key = i == 0;
            c
        })
        .collect();
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns,
        indexes: Vec::new(),
    });
    let grid = Grid {
        columns: cols.iter().map(|(n, _)| n.to_string()).collect(),
        rows: (0..rows)
            .map(|r| {
                cols.iter()
                    .enumerate()
                    .map(|(c, (_, ty))| {
                        if *ty == "int" {
                            Val::Text((r + 1).to_string())
                        } else {
                            Val::Text(format!("r{r}c{c}"))
                        }
                    })
                    .collect()
            })
            .collect(),
        note: String::new(),
    };
    app.set_grid(grid);
    app.focus = Focus::Preview;
    app.sel = 0;
    app
}

#[test]
pub(crate) fn row_select_state_machine_moves_extends_and_clears() {
    let tx = test_tx();
    let mut app = orders_app(&[("id", "int"), ("name", "text")], 6);
    app.sel = 1;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(app.row_sel_anchor, Some(1));
    // Shift+Down stretches the block; the anchor stays put.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
    );
    assert_eq!((app.row_sel_anchor, app.sel), (Some(1), 2));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
    );
    assert_eq!((app.row_sel_anchor, app.sel), (Some(1), 3));
    // In-mode `v` extends down as well.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert_eq!((app.row_sel_anchor, app.sel), (Some(1), 4));
    // A plain arrow collapses the block onto one row.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );
    assert_eq!((app.row_sel_anchor, app.sel), (Some(3), 3));
    // Esc leaves the mode.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert_eq!(app.row_sel_anchor, None);
    // V is refused where there is no selectable row.
    app.clear_grid();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    assert_eq!(app.row_sel_anchor, None);
}

/// R57: a three-row block is drawn as a reverse-video band.
#[test]
pub(crate) fn row_select_draws_a_reverse_video_block() {
    let mut app = orders_app(&[("id", "int"), ("name", "text")], 8);
    app.sel = 0;
    enter_row_select(&mut app);
    app.row_sel_anchor = Some(2);
    app.sel = 4;
    let buf = draw_buffer(&mut app, 60, 20);
    let reversed = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            buf.cell((x, y))
                .is_some_and(|c| c.modifier.contains(Modifier::REVERSED))
        })
        .count();
    assert!(reversed > 0, "no reversed row band was drawn");
}

pub(crate) fn batch_target_for(db: DatabaseType, cols: &[(&str, &str, bool)]) -> BatchTarget {
    BatchTarget {
        db_type: db,
        schema: "public".into(),
        table: "orders".into(),
        columns: cols.iter().map(|(n, _, _)| n.to_string()).collect(),
        types: cols.iter().map(|(_, t, _)| Some(t.to_string())).collect(),
        pks: cols
            .iter()
            .filter(|(_, _, pk)| *pk)
            .map(|(n, _, _)| n.to_string())
            .collect(),
    }
}

#[test]
pub(crate) fn batch_delete_sql_uses_in_for_a_single_key_and_escapes_values() {
    let t = batch_target_for(
        DatabaseType::Mysql,
        &[("id", "int", true), ("name", "varchar(64)", false)],
    );
    let rows = vec![
        vec![Val::Text("1".into()), Val::Text("a".into())],
        vec![Val::Text("2".into()), Val::Text("b".into())],
    ];
    assert_eq!(
        batch_delete_sql(&t, &rows),
        "DELETE FROM `public`.`orders`\nWHERE `id` IN (1, 2);"
    );
    // A text key is quoted with doubled single quotes; NULL stays bare.
    let ts = batch_target_for(DatabaseType::Postgres, &[("code", "text", true)]);
    let rows = vec![vec![Val::Text("O'Brien".into())], vec![Val::Null]];
    assert_eq!(
        batch_delete_sql(&ts, &rows),
        "DELETE FROM \"public\".\"orders\"\nWHERE \"code\" IN ('O''Brien', NULL);"
    );
    // The pk column is picked by name even when it is not the first column.
    let swapped = batch_target_for(
        DatabaseType::Mysql,
        &[("name", "text", false), ("id", "int", true)],
    );
    let rows = vec![vec![Val::Text("a".into()), Val::Text("7".into())]];
    assert_eq!(
        batch_delete_sql(&swapped, &rows),
        "DELETE FROM `public`.`orders`\nWHERE `id` IN (7);"
    );
}

#[test]
pub(crate) fn batch_delete_sql_chains_composite_keys() {
    let t = batch_target_for(
        DatabaseType::Postgres,
        &[
            ("a", "int", true),
            ("b", "text", true),
            ("note", "text", false),
        ],
    );
    let rows = vec![
        vec![Val::Text("1".into()), Val::Text("x".into()), Val::Null],
        vec![Val::Text("2".into()), Val::Text("y".into()), Val::Null],
    ];
    assert_eq!(
            batch_delete_sql(&t, &rows),
            "DELETE FROM \"public\".\"orders\"\nWHERE (\"a\" = 1 AND \"b\" = 'x')\n   OR (\"a\" = 2 AND \"b\" = 'y');"
        );
}

#[test]
pub(crate) fn batch_update_sql_templates_every_non_key_column() {
    let t = batch_target_for(
        DatabaseType::Mysql,
        &[("id", "int", true), ("name", "varchar(64)", false)],
    );
    let rows = vec![
        vec![Val::Text("1".into()), Val::Text("a".into())],
        vec![Val::Text("2".into()), Val::Text("O'Brien".into())],
    ];
    assert_eq!(
            batch_update_sql(&t, &rows),
            "UPDATE `public`.`orders`\nSET `name` = 'a'\nWHERE `id` = 1;\n\nUPDATE `public`.`orders`\nSET `name` = 'O''Brien'\nWHERE `id` = 2;"
        );
    // A key-only table yields a comment rather than an empty SET.
    let key_only = batch_target_for(DatabaseType::Mysql, &[("id", "int", true)]);
    let sql = batch_update_sql(&key_only, &[vec![Val::Text("1".into())]]);
    assert!(sql.starts_with("--"), "{sql}");
}

#[test]
pub(crate) fn rows_to_tsv_keeps_header_and_null_shapes() {
    let cols = vec!["id".to_string(), "name".to_string()];
    let rows = vec![
        vec![Val::Text("1".into()), Val::Null],
        vec![Val::Text("2".into()), Val::Text(String::new())],
    ];
    assert_eq!(rows_to_tsv(&cols, &rows), "id\tname\n1\tNULL\n2\t''");
}

#[test]
pub(crate) fn row_select_d_and_c_land_in_the_editor_without_executing() {
    let tx = test_tx();
    let mut app = orders_app(&[("id", "int"), ("name", "text")], 5);
    app.sel = 0;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
    );
    assert_eq!(app.sel, 2);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    let sql = app.editor_sql();
    assert!(sql.starts_with("DELETE FROM"), "{sql}");
    assert!(sql.contains("IN (1, 2, 3)"), "{sql}");
    assert!(app.focus == Focus::Editor);
    assert_eq!(app.row_sel_anchor, None);
    // Zero-write red line: nothing was queued to run.
    assert!(!app.pending_write);
    // `c` produces the UPDATE template instead.
    app.focus = Focus::Preview;
    app.sel = 1;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );
    let sql = app.editor_sql();
    assert!(sql.contains("SET `name` = 'r1c1'"), "{sql}");
    assert!(sql.contains("WHERE `id` = 2;"), "{sql}");
}

#[test]
pub(crate) fn row_select_d_without_a_primary_key_warns_and_keeps_the_editor() {
    let tx = test_tx();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.grid_kind = GridKind::Query;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.editor = TextArea::from(vec!["SELECT 1".to_string()]);
    let before = app.editor_sql();
    app.sel = 1;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert_eq!(app.row_sel_anchor, None);
    assert_eq!(app.editor_sql(), before);
    assert!(!app.status.is_empty());
    // `c` behaves the same way.
    app.sel = 0;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );
    assert_eq!(app.editor_sql(), before);
}

#[test]
pub(crate) fn redis_ttl_counts_down_locally() {
    assert_eq!(redis_ttl_advance(60, 1), 59);
    assert_eq!(redis_ttl_advance(1, 5), 0);
    assert_eq!(redis_ttl_advance(0, 5), 0);
    assert_eq!(redis_ttl_advance(-1, 5), -1);
    assert_eq!(redis_ttl_advance(-2, 5), -2);
}

#[test]
pub(crate) fn redis_single_key_delete_confirms_and_removes_in_place() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("redis"));
    app.backend_kind = Backend::Redis;
    app.redis_scan.all = vec![mk_redis_key("a"), mk_redis_key("b"), mk_redis_key("c")];
    apply_redis_filter(&mut app);
    app.redis_list.select(Some(1));
    app.redis_scan.cursor = 42;
    redis_batch_delete(&mut app);
    let rc = app
        .confirm
        .as_ref()
        .and_then(|c| c.redis.as_ref())
        .expect("delete confirm opened");
    assert!(!rc.batch.is_empty(), "a DEL command is generated");
    assert_eq!(rc.remove_in_place.len(), 1);
    assert!(
        !rc.reload_list,
        "single delete must not rescan from cursor 0"
    );
    // Esc cancels the confirm: nothing changes.
    confirm_key(
        &mut app,
        &test_tx(),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.confirm.is_none());
    assert_eq!(app.redis_scan.keys.len(), 3);
    // The confirmed delete prunes in place: cursor index and SCAN cursor stay.
    let raw = app.redis_scan.keys[1].key_raw.clone();
    redis_remove_keys_in_place(&mut app, &[raw]);
    assert_eq!(app.redis_scan.keys.len(), 2);
    assert_eq!(app.redis_list.selected(), Some(1));
    assert_eq!(app.redis_scan.cursor, 42);
}

#[test]
pub(crate) fn redis_multi_key_delete_still_rescans() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("redis"));
    app.backend_kind = Backend::Redis;
    app.redis_scan.all = vec![mk_redis_key("a"), mk_redis_key("b")];
    apply_redis_filter(&mut app);
    for k in &app.redis_scan.keys {
        app.redis_selected.insert(k.key_raw.clone());
    }
    redis_batch_delete(&mut app);
    let rc = app
        .confirm
        .as_ref()
        .and_then(|c| c.redis.as_ref())
        .expect("delete confirm opened");
    assert!(rc.reload_list);
    assert!(rc.remove_in_place.is_empty());
}

// ── R58: connection query timeout ──

#[test]
pub(crate) fn query_timeout_field_parsing() {
    assert_eq!(parse_query_timeout(""), Ok(None));
    assert_eq!(parse_query_timeout("   "), Ok(None));
    assert_eq!(parse_query_timeout("0"), Ok(Some(0)));
    assert_eq!(parse_query_timeout("30"), Ok(Some(30)));
    assert_eq!(parse_query_timeout(" 45 "), Ok(Some(45)));
    assert_eq!(parse_query_timeout("86400"), Ok(Some(86_400)));
    assert_eq!(parse_query_timeout("abc"), Err(()));
    assert_eq!(parse_query_timeout("-1"), Err(()));
    assert_eq!(parse_query_timeout("86401"), Err(()));
}

/// The `options` value of a postgres url-params string, decoded and split.
pub(crate) fn pg_option_tokens(params: &str) -> Vec<String> {
    params
        .split('&')
        .find_map(|part| {
            let (k, v) = part.split_once('=')?;
            k.eq_ignore_ascii_case("options")
                .then(|| percent_decode_url_value(v))
        })
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

pub(crate) fn pg_statement_timeout(params: &str) -> Option<u64> {
    pg_option_tokens(params)
        .iter()
        .find_map(|t| t.strip_prefix("statement_timeout=")?.parse().ok())
}

#[test]
pub(crate) fn with_pg_statement_timeout_merges_and_clears() {
    // Sets statement_timeout while preserving other params and `-c` options.
    let set = with_pg_statement_timeout(
        Some("sslmode=require&options=-c%20TimeZone%3DUTC"),
        Some(30_000),
    )
    .expect("url_params built");
    assert!(set.contains("sslmode=require"));
    assert_eq!(pg_statement_timeout(&set), Some(30_000));
    assert!(pg_option_tokens(&set).contains(&"TimeZone=UTC".to_string()));

    // Re-setting replaces the value instead of duplicating the pair.
    let reset = with_pg_statement_timeout(Some(&set), Some(60_000)).unwrap();
    let pairs = pg_option_tokens(&reset)
        .iter()
        .filter(|t| t.starts_with("statement_timeout="))
        .count();
    assert_eq!(pairs, 1);
    assert_eq!(pg_statement_timeout(&reset), Some(60_000));

    // Clearing drops only statement_timeout.
    let cleared = with_pg_statement_timeout(Some(&reset), None).unwrap();
    assert_eq!(pg_statement_timeout(&cleared), None);
    assert!(pg_option_tokens(&cleared).contains(&"TimeZone=UTC".to_string()));

    // A lone statement_timeout option disappears entirely when cleared.
    assert_eq!(
        with_pg_statement_timeout(Some("options=-c%20statement_timeout%3D5000"), None),
        None
    );
    // No params and nothing to set stays `None`.
    assert_eq!(with_pg_statement_timeout(None, None), None);
}

#[test]
pub(crate) fn form_query_timeout_maps_to_config_and_pg_option() {
    // MySQL: the timeout lands on the config, with no url_params rewrite.
    let mut my = test_conn("mysql");
    apply_form_query_timeout(&mut my, "30").unwrap();
    assert_eq!(my.query_timeout_secs, 30);
    assert_eq!(my.url_params, None);

    // Blank inherits the kernel default (60 s).
    let mut blank = test_conn("mysql");
    blank.query_timeout_secs = 7;
    apply_form_query_timeout(&mut blank, "").unwrap();
    assert_eq!(blank.query_timeout_secs, 60);

    // 0 disables the limit.
    let mut unlimited = test_conn("mysql");
    apply_form_query_timeout(&mut unlimited, "0").unwrap();
    assert_eq!(unlimited.query_timeout_secs, 0);

    // PostgreSQL: mirrored into the connection's statement_timeout option.
    let mut pg = test_conn("postgres");
    apply_form_query_timeout(&mut pg, "30").unwrap();
    assert_eq!(pg.query_timeout_secs, 30);
    let params = pg.url_params.expect("pg gets url_params");
    assert_eq!(pg_statement_timeout(&params), Some(30_000));

    // 0 is unlimited, encoded as statement_timeout=0.
    let mut pg0 = test_conn("postgres");
    apply_form_query_timeout(&mut pg0, "0").unwrap();
    assert_eq!(
        pg_statement_timeout(&pg0.url_params.expect("url_params")),
        Some(0)
    );

    // An invalid value bubbles up and leaves the config untouched.
    let mut bad = test_conn("postgres");
    assert_eq!(apply_form_query_timeout(&mut bad, "x"), Err(()));
    assert_eq!(bad.query_timeout_secs, 60);
    assert_eq!(bad.url_params, None);

    // The form prefills a saved value, and an explicit 0 round-trips.
    let mut saved = test_conn("postgres");
    saved.query_timeout_secs = 25;
    let f = form_from_connection(&saved, "n".into(), None);
    assert_eq!(f.query_timeout, "25");
    saved.query_timeout_secs = 0;
    let f0 = form_from_connection(&saved, "n".into(), None);
    assert_eq!(f0.query_timeout, "0");
}

#[test]
pub(crate) fn query_errors_are_rewritten_only_when_they_are_timeouts() {
    assert_eq!(
        query_error_text("Query timed out after 30 seconds", 30),
        tf("查询超时（{}s），可调大超时或优化语句", &[&30])
    );
    assert_eq!(
        query_error_text("canceling statement due to statement timeout", 30),
        tf("查询超时（{}s），可调大超时或优化语句", &[&30])
    );
    assert_eq!(
        query_error_text(
            "Query execution was interrupted, maximum statement execution time exceeded",
            30
        ),
        tf("查询超时（{}s），可调大超时或优化语句", &[&30])
    );
    // Unlimited connection: a generic timeout hint.
    assert_eq!(
        query_error_text("Query timed out after 5 seconds", 0),
        t("查询超时，可调大超时或优化语句")
    );
    // A real SQL error passes through untouched.
    assert_eq!(
        query_error_text("syntax error near FROM", 30),
        "syntax error near FROM"
    );
}

// ── R58: editor undo/redo visibility ──

#[test]
pub(crate) fn editor_undo_redo_flash_state_machine() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("select 1");

    // A fresh buffer has nothing to undo or redo: the status bar says so
    // instead of a silent no-op.
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.status, t("没有可撤销的"));
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.status, t("没有可重做的"));
    assert_eq!(editor_undo_step(&mut app), EditorUndo::None);

    // A real edit then Ctrl-Z undoes it and Ctrl-Y redoes it.
    app.editor.insert_str(" SELECT 2");
    let edited = app.editor_sql();
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.status, t("已撤销"));
    assert_ne!(app.editor_sql(), edited);
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.status, t("已重做"));
    assert_eq!(app.editor_sql(), edited);

    // Ctrl-R is the tui-textarea redo alias and shares the flash.
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.editor_sql(), edited);

    // A pending Alt-F reformat is undone in one step with its own notice.
    app.set_editor_text("select a from t where x=1");
    toggle_format_editor(&mut app);
    assert!(app.editor_undo.is_some());
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.status, t("已撤销格式化"));
    assert_eq!(app.editor_sql(), "select a from t where x=1");
}

// ── R58: long-cell abbreviation + NULL rendering ──

#[test]
pub(crate) fn long_cells_abbreviate_and_null_stays_grey() {
    assert_eq!(abbreviate_cell_text("short"), "short");
    // The 40-column boundary: exactly 40 stays, 41 collapses to `first 38…`.
    assert_eq!(abbreviate_cell_text(&"y".repeat(40)), "y".repeat(40));
    let ab = abbreviate_cell_text(&"y".repeat(41));
    assert_eq!(ab, format!("{}…", "y".repeat(38)));
    assert_eq!(ab.chars().count(), 39);

    // The width pass agrees with what is drawn.
    assert_eq!(cell_text_width(&Val::Text("short".into())), 5);
    assert_eq!(cell_text_width(&Val::Text("z".repeat(41))), 39);

    // The R53 popup keeps the full value (`value_display` is unabridged).
    let long = Val::Text("a".repeat(60));
    assert_eq!(value_display(&long).0, "a".repeat(60));

    // NULL is already grey (R58 deliberately skips the `∅` glyph swap).
    let (text, style) = value_display(&Val::Null);
    assert_eq!(text, "NULL");
    assert_eq!(style.fg, Some(Color::DarkGray));

    // End to end: the grid draws the abbreviation plus the literal ellipsis.
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["c".into()],
        rows: vec![vec![Val::Text("b".repeat(80))]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    app.col_cursor = 0;
    let screen = draw(&mut app, 110, 30);
    let joined = screen.join("\n");
    assert!(joined.contains(&format!("{}…", "b".repeat(38))), "{joined}");
    assert!(
        !joined.contains(&"b".repeat(45)),
        "long value leaked: {joined}"
    );
}

/// R58: the query-timeout row decorates an idle value (`30 s`) but shows the
/// raw buffer (plus the caret) while the field is being edited, so the next
/// digit lands where the caret is rather than after a derived unit suffix.
#[test]
pub(crate) fn query_timeout_row_shows_raw_buffer_while_editing() {
    let mut app = test_app();
    app.page = Page::NewConn;
    app.form = ConnForm::default();
    app.form.query_timeout = "30".into();
    let idx = form_rows(&app.form)
        .iter()
        .position(|(r, _)| *r == FormRow::QueryTimeout)
        .unwrap();
    app.form.field = idx;
    app.form.scroll = 0;

    app.form.editing = false;
    let idle = draw(&mut app, 60, 24).join("\n");
    assert!(
        idle.contains("30 s"),
        "idle row should show the unit: {idle}"
    );

    app.form.editing = true;
    let editing = draw(&mut app, 60, 24).join("\n");
    assert!(
        editing.contains("30▏"),
        "editing row should show the raw buffer + caret: {editing}"
    );
    assert!(
        !editing.contains("30 s"),
        "editing row must not show the derived suffix: {editing}"
    );
}

/// R73: `Alt-W` closes the active query-result tab, keeps the tab that slides
/// into its slot on screen, and never closes the last one (the grid would have
/// nowhere to go).
#[test]
pub(crate) fn result_tab_close_keeps_current_and_protects_last() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Preview;
    for title in ["select a", "select b", "select c"] {
        push_result_tab(
            &mut app,
            title.into(),
            Some(sample_grid()),
            None,
            GridKind::Query,
        );
    }
    assert_eq!(app.result_tabs.len(), 3);
    assert_eq!(app.result_tab, 2);

    // Make the middle tab current, then close it.
    app.result_tab = 1;
    app.restore_result_tab();
    close_result_tab(&mut app);
    let titles: Vec<String> = app.result_tabs.iter().map(|t| t.title.clone()).collect();
    assert_eq!(titles, vec!["select a".to_string(), "select c".to_string()]);
    assert_eq!(app.result_tab, 1, "the tab that slid in should be shown");
    assert!(app.grid_kind == GridKind::Query);

    // Down to one: the last tab is protected.
    close_result_tab(&mut app);
    assert_eq!(app.result_tabs.len(), 1);
    close_result_tab(&mut app);
    assert_eq!(app.result_tabs.len(), 1);
    assert!(app.status.contains("最后一个"), "{}", app.status);
}

/// R73: a tab with an unconfirmed edit is not closed — the hint asks the user
/// to confirm or cancel first. Closing is otherwise pure client-side.
#[test]
pub(crate) fn result_tab_close_refuses_while_edit_pending() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Preview;
    push_result_tab(
        &mut app,
        "select a".into(),
        Some(sample_grid()),
        None,
        GridKind::Query,
    );
    push_result_tab(
        &mut app,
        "select b".into(),
        Some(sample_grid()),
        None,
        GridKind::Query,
    );
    app.confirm = Some(Confirm {
        sql: "update t set a = 1".into(),
        reasons: vec!["write".into()],
        refresh: true,
        clear_batch: false,
        redis: None,
        mongo: None,
        conn: None,
    });
    assert!(result_tab_pending_edit(&app));
    close_result_tab(&mut app);
    assert_eq!(
        app.result_tabs.len(),
        2,
        "pending edit must block the close"
    );
    assert!(app.status.contains("未确认"), "{}", app.status);
}

/// R73: the tab-strip layout shows every tab when they fit, folds the middle
/// into `…` when they do not, and always keeps the current tab inside the
/// window.
#[test]
pub(crate) fn tab_strip_folds_but_keeps_current_visible() {
    let widths = vec![6usize; 8];
    // Wide: every tab, no folds.
    assert_eq!(tab_strip_window(8, 3, &widths, 500), (0, 7, false, false));
    // Narrow around a middle tab: folds on both sides, current inside.
    let (lo, hi, lf, rf) = tab_strip_window(8, 3, &widths, 30);
    assert!(lo <= 3 && 3 <= hi, "current hidden: {lo}..{hi}");
    assert!(
        lf && rf,
        "expected folds on both sides: {lo}..{hi} {lf} {rf}"
    );
    // Current at the far end: the right fold disappears.
    let (lo, hi, lf, rf) = tab_strip_window(8, 7, &widths, 30);
    assert!(lo <= 7 && 7 <= hi, "current hidden: {lo}..{hi}");
    assert!(!rf && lf, "far-end window should only fold the left");
    // Too narrow even for one folded tab: no folds, the caller clips the label.
    assert_eq!(tab_strip_window(8, 3, &widths, 5), (3, 3, false, false));
}

/// R73: the tab label is the 1-based number plus the statement's first 12
/// display columns.
#[test]
pub(crate) fn result_tab_label_uses_number_and_twelve_chars() {
    assert_eq!(
        result_tab_label(0, "select * from orders"),
        "1:select * fr…"
    );
    assert_eq!(result_tab_label(2, "short"), "3:short");
}

/// R73: the strip actually reaches the screen for a multi-tab query result and
/// folds when the terminal is narrow (42×22, bilingual-neutral glyphs).
#[test]
pub(crate) fn result_tab_strip_renders_and_folds() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.focus = Focus::Preview;
    for i in 0..5 {
        push_result_tab(
            &mut app,
            format!("select col_{i} from t"),
            Some(sample_grid()),
            None,
            GridKind::Query,
        );
    }
    let screen = draw(&mut app, 42, 22).join("\n");
    let cur = result_tab_label(4, "select col_4 from t");
    assert!(screen.contains(&cur), "missing current tab {cur}: {screen}");
    assert!(screen.contains('…'), "expected a folded tab: {screen}");
}

/// R73: the connection picker shows each connection's connect-time latency from
/// the R63 session cache, and `-` for one never probed. Pure client-side read.
#[test]
pub(crate) fn conn_picker_shows_latency_column() {
    let mut app = test_app();
    let mk = |id: &str, name: &str| {
        new_connection_config(
            id.into(),
            name.into(),
            parse_database_type("mysql").unwrap(),
            "127.0.0.1".into(),
            1,
            "u".into(),
            "p".into(),
            None,
            false,
            None,
        )
        .unwrap()
    };
    let alpha = mk("id-alpha", "alpha");
    let beta = mk("id-beta", "beta");
    app.connections = vec![alpha.clone(), beta];
    app.selected = None;
    app.picker_open = true;
    app.backend_kind = Backend::Sql;
    app.server_rtts
        .insert(alpha.id.clone(), Duration::from_millis(12));
    let screen = draw(&mut app, 70, 20).join("\n");
    assert!(
        screen.contains("12ms"),
        "connected latency missing: {screen}"
    );
    assert!(
        screen.contains("     -"),
        "unconnected connection should show `-`: {screen}"
    );
}

/// R73: the new result-tab strings resolve in English (the tab strip itself is
/// language-neutral: `…` and the SQL-derived labels read the same in both).
#[test]
pub(crate) fn result_tab_close_strings_are_bilingual() {
    use ui_text::{t_lang, tf_lang, Lang};
    assert_eq!(
        t_lang("最后一个结果标签不可关闭", Lang::En),
        "the last result tab cannot be closed"
    );
    assert_eq!(t_lang("当前没有结果标签", Lang::En), "no result tab open");
    assert_eq!(t_lang("切标签", Lang::En), "switch tab");
    assert_eq!(t_lang("关标签", Lang::En), "close tab");
    assert_eq!(
        tf_lang("已关闭结果标签 · 剩 {}", &[&2], Lang::En),
        "result tab closed · 2 left"
    );
    assert!(t_lang(
        "关闭当前结果标签（最后一个不可关；有未确认编辑时先处理；纯客户端，不查询）",
        Lang::En
    )
    .starts_with("Close the active result tab"));
}

/// R73: while a table page (not a query result) is on screen, `Alt-W` refuses
/// so closing a stale query tab cannot yank the user into another result.
#[test]
pub(crate) fn result_tab_close_refuses_on_table_page() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Preview;
    push_result_tab(
        &mut app,
        "select a".into(),
        Some(sample_grid()),
        None,
        GridKind::Query,
    );
    push_result_tab(
        &mut app,
        "select b".into(),
        Some(sample_grid()),
        None,
        GridKind::Query,
    );
    app.grid_kind = GridKind::TableData;
    close_result_tab(&mut app);
    assert_eq!(app.result_tabs.len(), 2);
    assert!(app.status.contains("不是查询结果"), "{}", app.status);
}

/// R73: a connection root's right-aligned cell shows its connect-time latency
/// from the R63 cache, and `-` for one never probed in this session.
#[test]
pub(crate) fn sidebar_connection_row_shows_latency() {
    let mut app = tree_app();
    app.term_w = 100;
    let id = app.selected.as_ref().unwrap().id.clone();
    app.server_rtts.insert(id, Duration::from_millis(26));
    assert_eq!(
        side_row_size(&app, &SideRow::Conn { idx: 0, depth: 0 }).as_deref(),
        Some("26ms")
    );
    assert_eq!(
        side_row_size(&app, &SideRow::Conn { idx: 1, depth: 0 }).as_deref(),
        Some("-")
    );
    // A narrow sidebar hides the column entirely, exactly like the size cell.
    app.term_w = 42;
    assert_eq!(
        side_row_size(&app, &SideRow::Conn { idx: 0, depth: 0 }),
        None
    );
}

/// R74: only a JSON object/array is pretty-printed; scalars and non-JSON text
/// stay verbatim so `v` never reformats something it does not understand.
#[test]
pub(crate) fn pretty_json_only_accepts_objects_and_arrays() {
    assert!(pretty_json("{\"a\":1}").is_some());
    assert!(pretty_json("[1,2,3]").is_some());
    assert!(pretty_json("  {\"a\": [1, {\"b\": true}]}  ").is_some());
    for scalar in [
        "42", "-1.5", "\"x\"", "true", "false", "null", "not json", "{oops", "", "   ",
    ] {
        assert!(
            pretty_json(scalar).is_none(),
            "{scalar:?} should not be pretty-printed"
        );
    }
}

/// R74: the pretty-JSON token scanner colours keys / strings / numbers / literals
/// and its tokens concatenate back to the pretty text (so wrapping is lossless).
#[test]
pub(crate) fn json_pretty_spans_colour_tokens_losslessly() {
    let pretty = pretty_json("{\"n\": 42, \"s\": \"x\", \"b\": true, \"z\": null}").unwrap();
    let lines = pretty_json_spans(&pretty);
    let all: Vec<&PopupSpan> = lines.iter().flatten().collect();
    let fg = |t: &str| {
        all.iter()
            .find(|s| s.text.contains(t))
            .and_then(|s| s.style.fg)
    };
    assert_eq!(fg("\"n\""), Some(Color::Cyan), "key is cyan");
    assert_eq!(fg("42"), Some(Color::Yellow), "number is yellow");
    assert_eq!(fg("\"x\""), Some(Color::Green), "string is green");
    assert_eq!(fg("true"), Some(Color::Magenta), "bool is magenta");
    assert_eq!(fg("null"), Some(Color::DarkGray), "null is dim");
    let joined: Vec<String> = lines
        .iter()
        .map(|sp| sp.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect();
    assert_eq!(joined.join("\n"), pretty);
}

fn json_cell_app(raw: &str) -> App {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("postgres"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(Grid {
        columns: vec!["payload".into()],
        rows: vec![vec![Val::Text(raw.into())]],
        note: String::new(),
    });
    app.focus = Focus::Preview;
    app.sel = 0;
    app.col_cursor = 0;
    app
}

/// R74: a JSON object opens in the pretty view, `J` flips to the raw value and
/// back, and `y`/`Y` copy the original compact JSON (never the pretty form).
#[test]
pub(crate) fn cell_popup_pretty_json_toggle_and_raw_copy() {
    let raw = "{\"name\":\"alice\",\"tags\":[\"a\",\"b\"]}";
    let mut app = json_cell_app(raw);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    let popup = app.cell_popup.as_ref().expect("cell popup");
    assert_eq!(popup.raw, raw, "raw copy source is the original");
    assert!(!popup.raw.contains('\n'), "raw stays compact");
    assert!(popup.pretty.is_some(), "a JSON object should pretty-print");
    assert!(popup.show_pretty, "opens in the pretty view");
    assert!(
        popup.col.contains("payload"),
        "copy status names the column"
    );
    // The drawn body shows the indented, coloured form.
    let rows = draw(&mut app, 60, 20);
    assert!(
        rows.iter().any(|r| r.contains("\"name\": \"alice\"")),
        "pretty body missing: {rows:#?}"
    );
    // `J` flips back to the raw single line.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
    );
    assert!(!app.cell_popup.as_ref().unwrap().show_pretty);
    let rows = draw(&mut app, 60, 20);
    assert!(rows.iter().any(|r| r.contains(raw)), "raw body missing");
    // `J` again returns to pretty, and the status says so.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
    );
    assert!(app.cell_popup.as_ref().unwrap().show_pretty);
    assert!(app.status.contains("JSON"), "{}", app.status);
    // `y` copies the raw value; the status names the column and shows it.
    app.status.clear();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(app.status.contains("已复制"), "{}", app.status);
    assert!(app.status.contains("payload"), "{}", app.status);
    assert!(app.status.contains("alice"), "{}", app.status);
}

/// R74: `J` on a non-JSON cell explains itself and leaves the view alone.
#[test]
pub(crate) fn cell_popup_j_rejects_non_json() {
    let mut app = json_cell_app("hello world");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    let popup = app.cell_popup.as_ref().unwrap();
    assert!(popup.pretty.is_none());
    assert!(!popup.show_pretty);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
    );
    assert!(app.status.contains("不是 JSON"), "{}", app.status);
    assert!(!app.cell_popup.as_ref().unwrap().show_pretty);
    assert!(app.cell_popup.is_some(), "the popup stays open");
}

/// R74: a multi-statement batch reports `3/7` while it runs and the intermediate
/// message does not stop the spinner.
#[test]
pub(crate) fn query_progress_updates_the_status() {
    let mut app = test_app();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    app.pending_ops = 1;
    app.loading = true;
    apply_op_result(&mut app, OpResult::QueryProgress { done: 3, total: 7 }, &tx);
    assert!(app.status.contains("3/7"), "{}", app.status);
    assert!(app.loading, "progress must not stop the spinner");
    assert_eq!(app.pending_ops, 1);
}

/// R74: a finished multi-statement script names its total elapsed time.
#[test]
pub(crate) fn script_status_names_total_elapsed() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("postgres"));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let outcome = |ms: u128| StmtOutcome {
        sql: "select 1".into(),
        grid: Grid {
            columns: vec!["a".into()],
            rows: vec![vec![Val::Text("1".into())]],
            note: String::new(),
        },
        error: None,
        affected: 0,
        ms,
    };
    apply_op_result(
        &mut app,
        OpResult::Script(vec![outcome(300), outcome(450)]),
        &tx,
    );
    assert!(app.status.contains("750ms"), "{}", app.status);
}

// ── R75: Esc flash + sidebar table info card (`i`) ──

/// The `Esc` flash auto-clears once its TTL lapses, and a newer status set in
/// the meantime is never wiped by that expiry.
#[test]
pub(crate) fn esc_flash_auto_clears_and_yields_to_a_new_message() {
    let mut app = test_app();
    app.flash(t("已关闭帮助").into());
    assert_eq!(app.status, t("已关闭帮助"));
    assert!(app.flash_until.is_some());
    // Not yet due: the message stays.
    expire_flash(&mut app);
    assert_eq!(app.status, t("已关闭帮助"));
    // Deadline forced into the past: it clears and the flash resets.
    app.flash_until = Some(Instant::now() - Duration::from_millis(1));
    expire_flash(&mut app);
    assert!(app.status.is_empty(), "{}", app.status);
    assert!(app.flash_until.is_none());
    // A newer status after the flash is preserved by the expiry guard.
    app.flash(t("已清除定位").into());
    app.status = "别的消息".into();
    app.flash_until = Some(Instant::now() - Duration::from_millis(1));
    expire_flash(&mut app);
    assert_eq!(app.status, "别的消息");
    assert!(app.flash_until.is_none());
}

/// The `Esc` rule matrix: a nested overlay pops exactly one layer, the result
/// set clears its search, and the editor find keeps its highlight (R61).
#[test]
pub(crate) fn esc_closes_one_layer_clears_search_and_keeps_editor_highlight() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    // 1) Nested overlays: cell over row. Esc pops one layer per press.
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    open_row_popup(&mut app);
    open_cell_popup(&mut app);
    assert!(app.cell_popup.is_some() && app.row_popup.is_some());
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.cell_popup.is_none(), "Esc closed only the cell");
    assert!(app.row_popup.is_some(), "the row popup stays underneath");
    assert_eq!(app.status, t("已关闭单元格"));
    assert!(app.flash_until.is_some(), "closing an overlay flashes");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.row_popup.is_none());
    assert_eq!(app.status, t("已关闭行详情"));

    // 2) Result set: Esc clears the search needle and rebuilds the view.
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::Query;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.result_needle = "r0c1".into();
    app.rebuild_view();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.result_needle.is_empty());
    assert_eq!(app.status, t("已清除结果搜索"));
    assert!(app.flash_until.is_some());

    // 3) Editor find: Esc leaves the find box but keeps the needle + highlight.
    let mut app = test_app();
    app.focus = Focus::Editor;
    app.set_editor_text("alpha\nbeta\nalpha");
    open_editor_find(&mut app);
    app.editor_find_needle = "alpha".into();
    editor_find_recompute_idx(&mut app);
    assert!(app.editor_find_idx.is_some());
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.editor_find.is_none(), "the find box closed");
    assert_eq!(app.editor_find_needle, "alpha", "the needle stays");
    assert!(app.editor_find_idx.is_some(), "the highlight stays");
}

/// `i` on a table node opens an info card built purely from session-cached
/// metadata: columns / indexes from the open table's `table_meta`, row / size
/// estimates from the per-database `db_sizes` cache. No query is issued.
#[test]
pub(crate) fn table_info_card_reads_cached_metadata_only() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    app.tables[0].comment = Some("订单主表".into());
    rebuild_side_rows(&mut app);
    let pos = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Table { table: 0, .. }))
        .expect("orders node");
    app.side_sel = pos;
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![ColumnInfo {
            name: "id".into(),
            data_type: "int".into(),
            is_primary_key: true,
            ..Default::default()
        }],
        indexes: Vec::new(),
    });
    let mut info = DbSizeInfo::default();
    info.rows.insert("orders".into(), 42);
    info.sizes.insert("orders".into(), 4096);
    app.db_sizes.insert("shop".into(), info);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    );
    assert!(app.table_info_open, "i opened the card on a table node");
    let lines = table_info_lines(&app);
    let value = |label: &str| {
        lines
            .iter()
            .find(|l| l.label == label)
            .map(|l| l.value.clone())
    };
    assert_eq!(value("列数").as_deref(), Some("1"));
    assert_eq!(value("行数估算").as_deref(), Some("42"));
    assert_eq!(value("数据大小").as_deref(), Some("4.0 KB"));
    assert_eq!(value("注释").as_deref(), Some("订单主表"));
    // Esc closes it and flashes a message.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.table_info_open);
    assert_eq!(app.status, t("已关闭表信息"));
    assert!(app.flash_until.is_some());
}

/// A table never opened this session degrades gracefully: the card shows the
/// tree-level fields (comment / engine) and a "打开表后可用" hint for the
/// metadata only an open would have cached — still with no query.
#[test]
pub(crate) fn table_info_card_degrades_when_the_table_was_never_opened() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    app.tables[0].comment = Some("订单主表".into());
    rebuild_side_rows(&mut app);
    let pos = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Table { table: 0, .. }))
        .expect("orders node");
    app.side_sel = pos;
    // No table_meta / page_state / db_sizes: nothing cached beyond the tree.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    );
    assert!(app.table_info_open);
    let lines = table_info_lines(&app);
    let value = |label: &str| {
        lines
            .iter()
            .find(|l| l.label == label)
            .map(|l| l.value.clone())
    };
    assert_eq!(value("列数").as_deref(), Some(t("打开表后可用")));
    assert_eq!(value("行数估算").as_deref(), Some(t("打开表后可用")));
    assert_eq!(value("注释").as_deref(), Some("订单主表"));
    assert_eq!(value("引擎").as_deref(), Some("mysql"));
    assert!(
        lines.iter().any(|l| l.value.contains("尚未打开")),
        "the card notes the table was never opened"
    );
    // `i` on a non-table row does not open an empty card.
    app.table_info_open = false;
    let conn = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Conn { .. }))
        .expect("conn node");
    app.side_sel = conn;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
    );
    assert!(
        !app.table_info_open,
        "i on a connection row is a filter char"
    );
}

/// The info card actually paints its title and rows at phone (42×22) and
/// desktop (110×30) sizes, and stays open while it does.
#[test]
pub(crate) fn table_info_card_renders_at_phone_and_desktop_sizes() {
    let mut app = tree_app();
    app.tables[0].comment = Some("订单主表".into());
    rebuild_side_rows(&mut app);
    app.side_sel = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Table { .. }))
        .expect("orders node");
    app.table_info_open = true;
    for (w, h) in [(42u16, 22u16), (110, 30)] {
        let text = draw(&mut app, w, h).join("\n");
        // Wide CJK glyphs occupy two cells, so the second is blank in the
        // captured buffer; strip spaces before matching the title.
        assert!(
            text.replace(' ', "").contains("表信息"),
            "card title missing at {w}x{h}"
        );
        assert!(text.contains("orders"), "table name missing at {w}x{h}");
        assert!(
            app.table_info_open,
            "the card stays open while the cursor is on a table ({w}x{h})"
        );
    }
}
