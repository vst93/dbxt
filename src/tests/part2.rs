use super::*;

/// R43: a clipped grid shows exactly one column readout — the focused
/// column's name and its absolute position — instead of the old narrow name
/// line plus a separate scroll-window line.
#[test]
pub(crate) fn status_bar_column_readout_is_single_without_a_scroll_window() {
    let mut app = test_app();
    app.term_w = 42;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.grid_frozen = 0;
    app.vis_cols = 2;
    app.col_cursor = 5;
    app.focus = Focus::Preview;
    let info = context_info(&app);
    assert!(info.contains("column_5"), "{info}");
    assert!(info.contains("6/8"), "{info}");
    assert_eq!(info.matches("列 ").count(), 1, "{info}");
    assert!(!info.contains("-"), "no a-b scroll window: {info}");
}

/// R63: the connect-time latency rides next to the server version in the
/// status context block, and is omitted entirely when no probe succeeded.
#[test]
pub(crate) fn status_bar_shows_connect_latency_when_known() {
    let mut app = test_app();
    app.selected = Some(test_conn("postgres"));
    let id = app.selected.as_ref().unwrap().id.clone();
    app.server_versions.insert(id.clone(), "16.2".into());
    app.server_rtts
        .insert(id.clone(), Duration::from_millis(12));
    let info = context_info(&app);
    assert!(info.contains("服务器 16.2"), "{info}");
    assert!(info.contains("延迟 12ms"), "{info}");
    // The compact latency survives even on a phone status bar, while the
    // long version string is hidden below 56 columns (same rule as before).
    app.term_w = 42;
    let narrow = context_info(&app);
    assert!(narrow.contains("延迟 12ms"), "{narrow}");
    assert!(!narrow.contains("服务器"), "{narrow}");
    app.term_w = 0;
    // A connection whose probe failed shows the version (if any) but no
    // latency, and never an error.
    app.server_rtts.clear();
    let info = context_info(&app);
    assert!(info.contains("服务器 16.2"), "{info}");
    assert!(!info.contains("延迟"), "{info}");
}

/// Build the shared fixture for the tree tests: two connections, the first
/// active with two databases and two tables in the current one.
pub(crate) fn tree_app() -> App {
    let mut app = test_app();
    let c1 = test_conn("mysql");
    let c2 = test_conn("postgres");
    app.connections = vec![c1.clone(), c2];
    app.selected = Some(c1);
    app.backend_kind = Backend::Sql;
    app.schema = String::new();
    app.databases = vec!["shop".into(), "logs".into()];
    app.db_index = 0;
    app.tables = vec![table_info("orders", "TABLE"), table_info("users", "TABLE")];
    app.tables_all = app.tables.clone();
    app.table_list.select(Some(0));
    app
}

/// R43: the sidebar is a connection → database → table tree; the active
/// connection and its current database start expanded, siblings stay
/// collapsed.
#[test]
pub(crate) fn sidebar_tree_nests_database_and_table_levels() {
    let app = tree_app();
    let rows = compute_side_rows(&app);
    assert_eq!(rows[0], SideRow::Conn { idx: 0, depth: 0 });
    assert!(matches!(&rows[1], SideRow::Db { idx: 0, db, .. } if db == "shop"));
    assert!(matches!(
        &rows[2],
        SideRow::Table {
            table: 0,
            depth: 2,
            ..
        }
    ));
    assert!(matches!(
        &rows[3],
        SideRow::Table {
            table: 1,
            depth: 2,
            ..
        }
    ));
    assert!(matches!(&rows[4], SideRow::Db { idx: 0, db, .. } if db == "logs"));
    // The second connection is a collapsed root.
    assert_eq!(rows[5], SideRow::Conn { idx: 1, depth: 0 });
    assert_eq!(rows.len(), 6);
}

// ── R48: DBX Desktop sidebar groups ──

pub(crate) fn group(id: &str, name: &str, nodes: Vec<LayoutNode>) -> LayoutGroup {
    LayoutGroup {
        id: id.into(),
        name: name.into(),
        nodes,
    }
}

/// R48: the desktop `sidebar_layout` JSON parses nested groups, the legacy
/// flat `connectionIds`, and keeps every member in the desktop's own order.
#[test]
pub(crate) fn parse_sidebar_layout_reads_groups_and_legacy_ids() {
    let layout: serde_json::Value = serde_json::from_str(
        r#"{
                "groups": [
                    {"id":"g1","name":"生产"},
                    {"id":"g2","name":"核心"},
                    {"id":"g3","name":"归档"}
                ],
                "order": [
                    {"type":"group","id":"g1","children":[
                        {"type":"connection","id":"c1"},
                        {"type":"group","id":"g2","children":[{"type":"connection","id":"c2"}]}
                    ]},
                    {"type":"connection","id":"c9"},
                    {"type":"group","id":"g3","connectionIds":["c3"]}
                ]
            }"#,
    )
    .unwrap();
    let parsed = parse_sidebar_layout(&layout);
    // Only top-level groups; the bare `c9` stays flat.
    assert_eq!(parsed.groups.len(), 2);
    let g1 = &parsed.groups[0];
    assert_eq!((g1.id.as_str(), g1.name.as_str()), ("g1", "生产"));
    assert_eq!(g1.nodes[0], LayoutNode::Conn("c1".into()));
    match &g1.nodes[1] {
        LayoutNode::Group(g2) => {
            assert_eq!(g2.name, "核心");
            assert_eq!(g2.nodes, vec![LayoutNode::Conn("c2".into())]);
        }
        other => panic!("expected a nested group, got {other:?}"),
    }
    // The legacy `connectionIds` form is still read.
    assert_eq!(parsed.groups[1].nodes, vec![LayoutNode::Conn("c3".into())]);
}

/// R48: a broken layout — bad JSON, a missing group name, an unknown group
/// id, or an unknown child type — degrades to the flat list instead of
/// breaking the sidebar.
#[test]
pub(crate) fn parse_sidebar_layout_degrades_to_flat() {
    let bad = [
        "not json at all",
        "[1, 2, 3]",
        r#"{"groups":[{"id":"g1"}],"order":[]}"#,
        r#"{"groups":[],"order":[{"type":"group","id":"missing"}]}"#,
        r#"{"groups":[{"id":"g1","name":"x"}],"order":[{"type":"group","id":"g1","children":[{"type":"bogus"}]}]}"#,
    ];
    for raw in bad {
        let v: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
        assert!(
            parse_sidebar_layout(&v).groups.is_empty(),
            "should degrade: {raw}"
        );
    }
    assert!(parse_sidebar_layout(&serde_json::Value::Null)
        .groups
        .is_empty());
}

/// R48: desktop groups render as `▾ 组名 [n]` nodes with their members
/// nested in desktop order, and the `[n]` badge counts live connections
/// (nested groups included).
#[test]
pub(crate) fn sidebar_groups_nest_connections_in_desktop_order() {
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    let c2 = app.connections[1].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group(
            "g1",
            "生产",
            vec![
                LayoutNode::Conn(c2.clone()),
                LayoutNode::Group(group("g2", "核心", vec![LayoutNode::Conn(c1.clone())])),
            ],
        )],
    };
    let rows = compute_side_rows(&app);
    assert!(matches!(
        &rows[0],
        SideRow::Group { name, depth: 0, count: 2, open: true, .. } if name == "生产"
    ));
    // The desktop order is respected: `c2` first, then the nested group.
    assert!(matches!(&rows[1], SideRow::Conn { idx: 1, depth: 1 }));
    assert!(matches!(
        &rows[2],
        SideRow::Group { name, depth: 1, count: 1, .. } if name == "核心"
    ));
    assert!(matches!(&rows[3], SideRow::Conn { idx: 0, depth: 2 }));
    // The active connection's subtree is indented below its group.
    assert!(matches!(&rows[4], SideRow::Db { idx: 0, db, depth: 3 } if db == "shop"));
    assert!(matches!(
        &rows[5],
        SideRow::Table {
            table: 0,
            depth: 4,
            ..
        }
    ));
}

/// R48: a group with no live member (or none matching the filter) draws
/// nothing, and its connections do not reappear in the flat tail.
#[test]
pub(crate) fn sidebar_group_hides_when_no_member_matches() {
    let mut app = tree_app();
    let c2 = app.connections[1].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c2.clone())])],
    };
    // No filter: the group and its (collapsed) connection are both visible,
    // and the ungrouped active connection stays flat after them.
    let rows = compute_side_rows(&app);
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(r, SideRow::Group { .. }))
            .count(),
        1
    );
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 0, .. })));
    // A filter the grouped connection does not match hides the whole group.
    app.table_filter = "zzz".into();
    let rows = compute_side_rows(&app);
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(r, SideRow::Group { .. }))
            .count(),
        0
    );
    assert!(!rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 0, .. })));
    // A member hit keeps the group (and only that member) visible.
    app.table_filter = "postgres".into();
    let rows = compute_side_rows(&app);
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(r, SideRow::Group { .. }))
            .count(),
        1
    );
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));
}

/// R48: `h` folds a group and the fold is remembered for the session; `l`
/// unfolds it again.
#[test]
pub(crate) fn sidebar_group_fold_is_remembered() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c1)])],
    };
    rebuild_side_rows(&mut app);
    let gi = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Group { .. }))
        .unwrap();
    app.side_sel = gi;
    side_collapse(&mut app);
    assert!(app.group_closed.contains("g1"));
    assert!(
        !app.side_rows
            .iter()
            .any(|r| matches!(r, SideRow::Conn { idx: 0, .. })),
        "a folded group hides its connections"
    );
    side_expand(&mut app, &tx);
    assert!(!app.group_closed.contains("g1"));
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 0, .. })));
}

// ── R54: sidebar tree quick search (`f`) ──

/// Type each character of `text` into the open quick-search prompt.
pub(crate) fn tree_search_type(app: &mut App, text: &str) {
    for ch in text.chars() {
        tree_search_key(app, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()));
    }
}

/// `f` searches connections / databases / tables across groups, force-opens
/// a hit group, lands the cursor on the first hit, and Enter clears the
/// needle (Esc restores the starting row).
#[test]
pub(crate) fn tree_quick_search_expands_hit_groups_and_clears_input() {
    let mut app = tree_app();
    let c2 = app.connections[1].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c2.clone())])],
    };
    // The user folded the group; a search inside it must still reveal hits.
    app.group_closed.insert("g1".into());
    rebuild_side_rows(&mut app);
    assert!(!app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));

    open_tree_search(&mut app);
    assert!(app.tree_search_prompt.is_some());
    assert!(app.tree_search.is_empty());
    tree_search_type(&mut app, "postgres");
    assert_eq!(app.tree_search, "postgres");
    assert!(matches!(
        app.side_rows[0],
        SideRow::Group { open: true, .. }
    ));
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));
    // The cursor is pulled onto the first direct hit.
    assert_eq!(
        side_row_label(&app, &app.side_rows[app.side_sel]),
        "test-postgres"
    );

    // Enter jumps and clears the needle; the ancestor group stays open so
    // the landing is visible.
    tree_search_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert!(app.tree_search_prompt.is_none());
    assert!(app.tree_search.is_empty());
    assert!(app.status.contains("已清除搜索"), "{}", app.status);
    assert!(app.status.contains("test-postgres"), "{}", app.status);
    assert!(!app.group_closed.contains("g1"));
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 1, .. })));

    // Esc clears too, and restores the row the search started from.
    let before = app.side_sel;
    open_tree_search(&mut app);
    tree_search_type(&mut app, "orders");
    assert!(side_row_label(&app, &app.side_rows[app.side_sel]).contains("orders"));
    tree_search_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert!(app.tree_search_prompt.is_none());
    assert!(app.tree_search.is_empty());
    assert!(app.status.contains("已清除搜索"), "{}", app.status);
    assert_eq!(app.side_sel, before, "Esc returns to the starting row");

    // A needle with no hit reports it and Enter clears without moving.
    open_tree_search(&mut app);
    tree_search_type(&mut app, "zzz");
    assert!(app.status.contains("0 个命中"), "{}", app.status);
    tree_search_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert!(app.tree_search_prompt.is_none());
    assert!(app.status.contains("没有匹配"), "{}", app.status);
}

/// The quick search also reaches a *database* name loaded for a sibling
/// connection (cross-group, client-side cache), and filtering never spawns
/// a query — it only reads `tree_dbs`.
#[test]
pub(crate) fn tree_quick_search_reaches_sibling_databases_from_cache() {
    let mut app = tree_app();
    let c2 = app.connections[1].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c2.clone())])],
    };
    app.tree_dbs
        .insert(c2.clone(), vec!["analytics".into(), "billing".into()]);
    // The sibling root is expanded (its databases are already cached).
    app.tree_conn_open.insert(c2.clone());
    app.group_closed.insert("g1".into());
    rebuild_side_rows(&mut app);

    open_tree_search(&mut app);
    tree_search_type(&mut app, "analytics");
    // The group auto-opened and the cached database row is a hit.
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Db { idx: 1, db, .. } if db == "analytics")));
    assert_eq!(tree_search_hits(&app), 1);
    // The sibling database that does not match is hidden.
    assert!(!app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Db { db, .. } if db == "billing")));
}

/// R50: `h` / `l` answer the same way on every node type. `l` on a collapsed
/// parent opens it in place and a second `l` steps into its first child; `h`
/// on an expanded parent folds it in place and a second `h` climbs to the
/// parent. Groups, connections, databases and leaf tables all agree.
#[test]
pub(crate) fn sidebar_h_l_state_machine_is_consistent_across_row_types() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    app.conn_live.insert(c1.clone(), true);
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c1)])],
    };
    rebuild_side_rows(&mut app);
    // rows: Group(0) Conn(1) Db·shop(2) Table·orders(3) Table·users(4) Db·logs(5)

    // ── leaf table ── `l` does nothing, `h` climbs to the database.
    let table = app.side_sel;
    assert!(matches!(app.side_rows[table], SideRow::Table { .. }));
    side_expand(&mut app, &tx);
    assert_eq!(app.side_sel, table, "l on a leaf table stays put");
    side_collapse(&mut app);
    assert!(
        matches!(app.side_rows[app.side_sel], SideRow::Db { .. }),
        "h on a leaf table climbs to its database"
    );

    // ── database ── `h` folds in place, a second `h` climbs to the root.
    side_collapse(&mut app);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Db { .. }));
    assert!(
        !app.side_rows
            .iter()
            .any(|r| matches!(r, SideRow::Table { .. })),
        "a folded database hides its tables"
    );
    side_collapse(&mut app);
    assert!(
        matches!(app.side_rows[app.side_sel], SideRow::Conn { .. }),
        "h on a collapsed database climbs to the connection"
    );
    // `l` re-opens the folded database in place (it stays on the db row).
    app.side_sel = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Db { .. }))
        .unwrap();
    side_expand(&mut app, &tx);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Db { .. }));
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Table { .. })));

    // ── connection ── `h` folds in place, a second `h` climbs to the group
    // (the previously missing half of the state machine).
    app.side_sel = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Conn { .. }))
        .unwrap();
    side_collapse(&mut app);
    assert!(app.tree_conn_closed.contains(&app.connections[0].id));
    assert!(
        matches!(app.side_rows[app.side_sel], SideRow::Conn { .. }),
        "h on an open connection folds it in place"
    );
    side_collapse(&mut app);
    assert!(
        matches!(app.side_rows[app.side_sel], SideRow::Group { .. }),
        "a second h on a collapsed connection climbs to its group"
    );
    // `l` on the open group steps into its first child (the connection).
    side_expand(&mut app, &tx);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Conn { .. }));
    // `l` on the collapsed connection re-opens it in place.
    side_expand(&mut app, &tx);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Conn { .. }));
    assert!(!app.tree_conn_closed.contains(&app.connections[0].id));

    // ── group ── `h` folds in place, a second `h` has no parent (stays).
    app.side_sel = 0;
    side_collapse(&mut app);
    assert!(app.group_closed.contains("g1"));
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Group { .. }));
    side_collapse(&mut app);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Group { .. }));
}

/// R50: a nested group folds with the same rule, and the root group's fold
/// keeps the cursor on a semantically adjacent row (the group header).
#[test]
pub(crate) fn sidebar_nested_group_fold_keeps_the_cursor_semantic() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group(
            "outer",
            "外层",
            vec![LayoutNode::Group(group(
                "inner",
                "内层",
                vec![LayoutNode::Conn(c1)],
            ))],
        )],
    };
    rebuild_side_rows(&mut app);
    // rows: outer(0) inner(1) Conn(2) Db…
    assert!(matches!(&app.side_rows[0], SideRow::Group { id, depth: 0, .. } if id == "outer"));
    assert!(matches!(&app.side_rows[1], SideRow::Group { id, depth: 1, .. } if id == "inner"));

    // Fold the inner group: it stays the cursor, its grouped connection
    // (idx 0) vanishes. The ungrouped sibling root (idx 1) stays flat.
    app.side_sel = 1;
    side_collapse(&mut app);
    assert!(app.group_closed.contains("inner"));
    assert!(matches!(&app.side_rows[app.side_sel], SideRow::Group { id, .. } if id == "inner"));
    assert!(!app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Conn { idx: 0, .. })));
    // A second `h` climbs to the outer group.
    side_collapse(&mut app);
    assert!(matches!(&app.side_rows[app.side_sel], SideRow::Group { id, .. } if id == "outer"));
    // Folding the outer group leaves the cursor on its header and drops the
    // nested group entirely.
    side_collapse(&mut app);
    assert!(app.group_closed.contains("outer"));
    assert!(!app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Group { id, .. } if id == "inner")));
    assert!(matches!(&app.side_rows[app.side_sel], SideRow::Group { id, .. } if id == "outer"));
    // `l` unfolds the outer group in place; a second `l` steps into the
    // inner group.
    side_expand(&mut app, &tx);
    assert!(!app.group_closed.contains("outer"));
    assert!(matches!(&app.side_rows[app.side_sel], SideRow::Group { id, .. } if id == "outer"));
    side_expand(&mut app, &tx);
    assert!(matches!(&app.side_rows[app.side_sel], SideRow::Group { id, .. } if id == "inner"));
}

/// R50: every row type is padded with the selection background out to the
/// sidebar's right edge, so the highlight is one solid band. Widths are
/// measured with `disp_width`, so wide emoji (`📁` / `▤`) count as the two
/// cells they occupy and never leave a short pad.
#[test]
pub(crate) fn sidebar_selected_rows_pad_to_the_full_inner_width() {
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    let c2 = app.connections[1].id.clone();
    // Keep every name short enough that no row overflows the narrow 20-cell
    // check, so the assertion is an exact width, not a floor.
    app.connections[1].name = "pg".into();
    let mut c3 = test_conn("mysql");
    c3.id = "id-mysql3".into();
    c3.name = "third".into();
    let c3_id = c3.id.clone();
    app.connections.push(c3);
    app.sidebar_layout = SidebarLayout {
        groups: vec![group(
            "g1",
            "生产环境",
            vec![
                LayoutNode::Conn(c1),
                LayoutNode::Conn(c2.clone()),
                LayoutNode::Conn(c3_id.clone()),
            ],
        )],
    };
    // Give the two sibling roots an error row and a loading row so every
    // row type is exercised.
    app.tree_conn_open.insert(c2.clone());
    app.tree_db_state
        .insert(c2, TreeDbState::Error("boom".into()));
    app.tree_conn_open.insert(c3_id.clone());
    app.tree_db_state.insert(c3_id, TreeDbState::Loading);
    rebuild_side_rows(&mut app);

    let kind = |r: &SideRow| match r {
        SideRow::Group { .. } => "Group",
        SideRow::Conn { .. } => "Conn",
        SideRow::Db { .. } => "Db",
        SideRow::Table { .. } => "Table",
        SideRow::Column { .. } => "Column",
        SideRow::ConnError { .. } => "ConnError",
        SideRow::ConnLoading { .. } => "ConnLoading",
    };
    for want in ["Group", "Conn", "Db", "Table", "ConnError", "ConnLoading"] {
        assert!(
            app.side_rows.iter().any(|r| kind(r) == want),
            "fixture is missing a {want} row"
        );
    }

    for w in [110u16, 42, 20] {
        let full_w = (w as usize) - 2;
        for row in app.side_rows.clone() {
            let line = side_row_line(&app, &row, true, "", w);
            let width: usize = line.spans.iter().map(|s| disp_width(&s.content)).sum();
            assert_eq!(
                width, full_w,
                "{w} wide: {row:?} fills {width} cells, not the {full_w}-cell row"
            );
            for s in &line.spans {
                assert_eq!(
                    s.style.bg,
                    Some(Color::DarkGray),
                    "{w} wide: a gap in the selection band of {row:?}"
                );
            }
        }
    }

    // An unselected row is not padded (no background to align).
    let unselected = side_row_line(&app, &app.side_rows[0].clone(), false, "", 110);
    let width: usize = unselected
        .spans
        .iter()
        .map(|s| disp_width(&s.content))
        .sum();
    assert!(width < 108, "an unselected row is not padded: {width}");
}

/// R50: `x` on a group row is a no-op — it must not leak into the one-step
/// type-to-filter (which only ever reached a *connection* root before).
#[test]
pub(crate) fn sidebar_x_on_a_group_row_does_not_start_the_filter() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    app.picker_open = false;
    let c1 = app.connections[0].id.clone();
    app.sidebar_layout = SidebarLayout {
        groups: vec![group("g1", "生产", vec![LayoutNode::Conn(c1)])],
    };
    rebuild_side_rows(&mut app);
    app.side_sel = 0;
    assert!(matches!(app.side_rows[0], SideRow::Group { .. }));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert!(app.filter_prompt.is_none(), "x must not open the filter");
    assert!(app.table_filter.is_empty(), "x must not seed the filter");
}

// ── R55: tree rename validation / reorder / column-width memory ──

pub(crate) fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

/// A rename buffer is trimmed, rejected when empty or over the cap, and the
/// two failure messages have English translations (bilingual errors).
#[test]
pub(crate) fn rename_name_validation_and_bilingual_errors() {
    assert!(validate_rename_name("   ").is_err());
    assert!(validate_rename_name(&"x".repeat(MAX_CONN_NAME_LEN + 1)).is_err());
    assert_eq!(validate_rename_name("  prod  ").unwrap(), "prod");
    assert!(validate_rename_name(&"x".repeat(MAX_CONN_NAME_LEN)).is_ok());
    assert_eq!(
        ui_text::t_lang("✗ 名称不能为空", ui_text::Lang::En),
        "✗ name cannot be empty"
    );
    assert!(ui_text::t_lang("✗ 名称过长（最多 {} 字符）", ui_text::Lang::En).contains("{}"));
}

/// R55: a grouped connection swaps with its previous sibling inside the
/// group's `children` array, and `parse_sidebar_layout` reflects the new
/// order; a move past the top of the list reports failure.
#[test]
pub(crate) fn layout_swap_moves_grouped_connection_up_and_down() {
    let mut raw = json(
        r#"{
                "groups":[{"id":"g1","name":"prod"}],
                "order":[{"type":"group","id":"g1","children":[
                    {"type":"connection","id":"c1"},
                    {"type":"connection","id":"c2"}
                ]}]
            }"#,
    );
    let target = LayoutTarget::Conn("c2".into());
    assert!(swap_in_layout(&mut raw, &target, -1));
    let parsed = parse_sidebar_layout(&raw);
    assert_eq!(
        parsed.groups[0].nodes,
        vec![LayoutNode::Conn("c2".into()), LayoutNode::Conn("c1".into())]
    );
    // Already at the top: another up is a no-op that reports failure.
    assert!(!swap_in_layout(&mut raw, &target, -1));
    // Down brings it back to the end.
    assert!(swap_in_layout(&mut raw, &target, 1));
    assert_eq!(
        parse_sidebar_layout(&raw).groups[0].nodes,
        vec![LayoutNode::Conn("c1".into()), LayoutNode::Conn("c2".into())]
    );
}

/// R55: a nested group moves within its parent's `children`, top-level
/// groups move only among other top-level groups (never past a bare
/// connection entry), and ungrouped connections are not movable at all.
#[test]
pub(crate) fn layout_swap_handles_nested_groups_and_top_level() {
    let mut raw = json(
        r#"{
                "groups":[
                    {"id":"g1","name":"prod"},
                    {"id":"g2","name":"core"},
                    {"id":"g3","name":"archive"}
                ],
                "order":[
                    {"type":"group","id":"g1","children":[
                        {"type":"connection","id":"c1"},
                        {"type":"group","id":"g2","children":[{"type":"connection","id":"c2"}]}
                    ]},
                    {"type":"connection","id":"c9"},
                    {"type":"group","id":"g3","children":[{"type":"connection","id":"c3"}]}
                ]
            }"#,
    );
    // A nested group swaps with its previous sibling (the connection c1).
    assert!(swap_in_layout(
        &mut raw,
        &LayoutTarget::Group("g2".into()),
        -1
    ));
    let g1 = &parse_sidebar_layout(&raw).groups[0];
    assert!(matches!(g1.nodes[0], LayoutNode::Group(ref g) if g.id == "g2"));
    // Top-level g3 swaps with the nearest top-level group (g1); the bare
    // connection entry c9 is skipped over, not swapped with.
    assert!(swap_in_layout(
        &mut raw,
        &LayoutTarget::Group("g3".into()),
        -1
    ));
    let order = raw["order"].as_array().unwrap();
    assert_eq!(order[0]["id"], "g3");
    assert_eq!(order[1]["id"], "c9");
    assert_eq!(order[2]["id"], "g1");
    // g3 is now the first top-level group: no further up move.
    assert!(!swap_in_layout(
        &mut raw,
        &LayoutTarget::Group("g3".into()),
        -1
    ));
    // An ungrouped connection is not part of any group's member list.
    assert!(!swap_in_layout(
        &mut raw,
        &LayoutTarget::Conn("c9".into()),
        -1
    ));
}

/// R55: the legacy flat `connectionIds` form can be reordered too.
#[test]
pub(crate) fn layout_swap_handles_legacy_connection_ids() {
    let mut raw = json(
        r#"{
                "groups":[{"id":"g1","name":"prod"}],
                "order":[{"type":"group","id":"g1","connectionIds":["c1","c2"]}]
            }"#,
    );
    assert!(swap_in_layout(
        &mut raw,
        &LayoutTarget::Conn("c2".into()),
        -1
    ));
    let ids = raw["order"][0]["connectionIds"].as_array().unwrap();
    assert_eq!(ids[0], "c2");
    assert_eq!(ids[1], "c1");
}

/// R55: `move_side_row` re-parses the tree, keeps the cursor on the moved
/// connection, and refuses a top-level ungrouped connection with a hint.
#[tokio::test(flavor = "multi_thread")]
async fn move_side_row_updates_tree_and_cursor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    let raw = json(
        r#"{
                "groups":[{"id":"g1","name":"prod"}],
                "order":[{"type":"group","id":"g1","children":[
                    {"type":"connection","id":"id-mysql"},
                    {"type":"connection","id":"id-postgres"}
                ]}]
            }"#,
    );
    app.sidebar_layout = parse_sidebar_layout(&raw);
    app.sidebar_layout_raw = Some(raw);
    rebuild_side_rows(&mut app);
    let pos = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Conn { idx: 1, .. }))
        .unwrap();
    app.side_sel = pos;
    move_side_row(&mut app, &tx, -1);
    assert_eq!(
        app.sidebar_layout.groups[0].nodes,
        vec![
            LayoutNode::Conn("id-postgres".into()),
            LayoutNode::Conn("id-mysql".into())
        ]
    );
    assert!(matches!(
        app.side_rows[app.side_sel],
        SideRow::Conn { idx: 1, .. }
    ));
    // With no groups at all, a connection cannot be reordered.
    let mut flat = tree_app();
    rebuild_side_rows(&mut flat);
    flat.side_sel = flat
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Conn { idx: 1, .. }))
        .unwrap();
    move_side_row(&mut flat, &tx, -1);
    assert!(flat.status.contains("分组"), "{}", flat.status);
}

/// R55: a rename buffers the row in place — the rendered tree line carries
/// the edit text and a caret instead of the old name.
#[test]
pub(crate) fn rename_renders_inline_on_the_row() {
    let mut app = tree_app();
    app.rename_edit = Some(RenameEdit {
        target: RenameTarget::Conn {
            id: "id-mysql".into(),
        },
        text: "new-name".into(),
    });
    rebuild_side_rows(&mut app);
    let row = app
        .side_rows
        .iter()
        .find(|r| matches!(r, SideRow::Conn { idx: 0, .. }))
        .cloned()
        .unwrap();
    let line = side_row_line(&app, &row, true, "", 60);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("new-name▏"), "{text}");
}

/// R55: the session column-width memory widens / narrows from the current
/// width, clamps at both ends, and keeps scopes and columns independent.
#[test]
pub(crate) fn column_width_memory_clamps_and_scopes() {
    let mut mem = ColWidthMemory::default();
    let scope = "t\u{0}c1\u{0}db\u{0}\u{0}orders";
    assert_eq!(mem.get(scope, "id"), None);
    // First adjustment starts from what is on screen.
    assert_eq!(mem.adjust(scope, "id", 10, 4), 14);
    assert_eq!(mem.get(scope, "id"), Some(14));
    assert_eq!(mem.adjust(scope, "id", 14, -100), MIN_CELL_WIDTH);
    assert_eq!(mem.adjust(scope, "id", MIN_CELL_WIDTH, 1000), COL_W_MAX);
    // A different column and a different scope are untouched.
    assert_eq!(mem.get(scope, "name"), None);
    assert_eq!(mem.get("other", "id"), None);
    assert_eq!(mem.adjust(scope, "name", 8, 2), 10);
    assert_eq!(mem.get(scope, "id"), Some(COL_W_MAX));
}

/// R55: the width scope is stable across a page turn on the same table, and
/// differs for another table / a plain query result.
#[test]
pub(crate) fn col_width_scope_is_stable_across_pages() {
    let mut app = tree_app();
    app.grid_kind = GridKind::TableData;
    app.schema = String::new();
    let page_state = |page: usize, table: &str| PageState {
        table: table.into(),
        schema: String::new(),
        table_type: Some("TABLE".into()),
        page,
        page_size: PAGE_SIZE,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: None,
        keyset: None,
    };
    app.page_state = Some(page_state(0, "orders"));
    let a = col_width_scope(&app);
    app.page_state = Some(page_state(3, "orders"));
    assert_eq!(a, col_width_scope(&app), "a page turn keeps the scope");
    app.page_state = Some(page_state(0, "users"));
    assert_ne!(a, col_width_scope(&app), "another table is another scope");
    app.grid_kind = GridKind::Query;
    assert!(col_width_scope(&app).starts_with("q\u{0}"));
}

// ── R72: column-width memory persists to tui.json ──

/// A `(database, table)` page state for the width tests.
pub(crate) fn width_page(table: &str) -> PageState {
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

/// R72: widths written for two tables round-trip through `tui.json` and are
/// looked up by the full `(conn, db, schema, table, col)` identity.
#[test]
pub(crate) fn config_persists_and_restores_col_widths() {
    let path = std::env::temp_dir().join(format!("dbxt-cw-{}.json", Uuid::new_v4()));
    let mut cfg = TuiConfig::default();
    cfg.set_col_width("c1", "shop", "", "orders", "id", 14);
    cfg.set_col_width("c1", "shop", "", "orders", "name", 30);
    cfg.set_col_width("c2", "shop", "", "users", "id", 8);
    cfg.save(&path);
    let back = TuiConfig::load(&path);
    assert_eq!(back.col_width("c1", "shop", "", "orders", "id"), Some(14));
    assert_eq!(back.col_width("c1", "shop", "", "orders", "name"), Some(30));
    assert_eq!(back.col_width("c2", "shop", "", "users", "id"), Some(8));
    // A different connection / table / column is a different identity.
    assert_eq!(back.col_width("c1", "shop", "", "orders", "missing"), None);
    assert_eq!(back.col_width("c1", "shop", "", "users", "id"), None);
    assert_eq!(back.col_width("c9", "shop", "", "orders", "id"), None);
    let _ = std::fs::remove_file(&path);
}

/// R72: the persisted list is capped at `COL_WIDTH_MEM_MAX` and drops the
/// least-recently-adjusted entries first.
#[test]
pub(crate) fn col_widths_lru_caps_and_evicts_oldest() {
    let path = std::env::temp_dir().join(format!("dbxt-cw-lru-{}.json", Uuid::new_v4()));
    let mut cfg = TuiConfig::default();
    for i in 0..(COL_WIDTH_MEM_MAX + 25) {
        cfg.set_col_width("c1", "db", "", "t", &format!("col{i}"), 10 + (i % 5));
    }
    cfg.save(&path);
    let back = TuiConfig::load(&path);
    assert_eq!(back.col_widths.len(), COL_WIDTH_MEM_MAX);
    assert!(back.col_width("c1", "db", "", "t", "col0").is_none());
    assert!(
        back.col_width(
            "c1",
            "db",
            "",
            "t",
            &format!("col{}", COL_WIDTH_MEM_MAX + 24)
        )
        .is_some(),
        "the most recent entry survives the cap"
    );
    let _ = std::fs::remove_file(&path);
}

/// R72: clearing one column / a whole table removes exactly those entries and
/// leaves other tables alone; a fresh session's save merges rather than wipes.
#[test]
pub(crate) fn config_clears_col_widths_and_merges_sessions() {
    let path = std::env::temp_dir().join(format!("dbxt-cw-clear-{}.json", Uuid::new_v4()));
    let mut cfg = TuiConfig::default();
    cfg.set_col_width("c1", "db", "", "t", "a", 12);
    cfg.set_col_width("c1", "db", "", "t", "b", 13);
    cfg.set_col_width("c1", "db", "", "u", "a", 14);
    cfg.save(&path);

    // A second session only knows about one entry; its save must not wipe the
    // others, and a clear removes just the named column.
    let mut b = TuiConfig::default();
    b.set_col_width("c1", "db", "", "t", "b", 15);
    b.save(&path);
    let after = TuiConfig::load(&path);
    assert_eq!(after.col_width("c1", "db", "", "t", "a"), Some(12));
    assert_eq!(after.col_width("c1", "db", "", "t", "b"), Some(15));
    assert_eq!(after.col_width("c1", "db", "", "u", "a"), Some(14));

    let mut c = TuiConfig::load(&path);
    c.clear_col_width("c1", "db", "", "t", "a");
    c.save(&path);
    let after = TuiConfig::load(&path);
    assert_eq!(after.col_width("c1", "db", "", "t", "a"), None);
    assert_eq!(after.col_width("c1", "db", "", "t", "b"), Some(15));

    let mut d = TuiConfig::load(&path);
    assert_eq!(d.clear_table_col_widths("c1", "db", "", "t"), 1);
    assert_eq!(d.clear_table_col_widths("c1", "db", "", "t"), 0);
    d.save(&path);
    let after = TuiConfig::load(&path);
    assert_eq!(after.col_width("c1", "db", "", "t", "b"), None);
    assert_eq!(after.col_width("c1", "db", "", "u", "a"), Some(14));
    let _ = std::fs::remove_file(&path);
}

/// R72: the renderer applies the session override first, then the persisted one,
/// and leaves a column with neither at its natural width.
#[test]
pub(crate) fn col_width_overrides_apply_session_then_persisted() {
    let mut app = tree_app();
    app.grid_kind = GridKind::TableData;
    app.page_state = Some(width_page("orders"));
    // Persisted for `id`; the session remembers a manual `name`.
    app.config
        .set_col_width("id-mysql", "shop", "", "orders", "id", 20);
    let scope = col_width_scope(&app);
    app.col_width_mem.adjust(&scope, "name", 10, 2);
    let grid = Grid {
        columns: vec!["id".into(), "name".into(), "note".into()],
        rows: vec![],
        note: String::new(),
        types: Vec::new(),
    };
    let mut widths = vec![5usize, 5, 5];
    apply_col_width_overrides(&app, &grid, &mut widths);
    assert_eq!(widths, vec![20, 12, 5]);
}

/// R72: the session memory can drop one column or a whole scope.
#[test]
pub(crate) fn col_width_memory_reset_and_clear_scope() {
    let mut mem = ColWidthMemory::default();
    mem.adjust("s", "a", 10, 2);
    mem.adjust("s", "b", 10, 2);
    mem.adjust("t", "a", 10, 2);
    assert_eq!(mem.clear_scope("s"), 2);
    assert!(mem.overrides("s").is_none());
    assert_eq!(mem.get("t", "a"), Some(12));
    mem.reset("t", "a");
    assert_eq!(mem.get("t", "a"), None);
}

/// R72: `0` resets just the focused column (session + disk); `Alt-0` forgets the
/// whole table. `Alt-0` (not `Ctrl-0`) because most terminals collapse `Ctrl-0`
/// to a plain `0`, which is the single-column reset.
#[test]
pub(crate) fn reset_and_clear_col_width_keys() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    app.grid_kind = GridKind::TableData;
    app.page_state = Some(width_page("orders"));
    app.set_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Val::Text("1".into()), Val::Text("a".into())]],
        note: String::new(),
        types: Vec::new(),
    });
    app.focus = Focus::Preview;
    app.grid_widths = vec![20, 10];
    app.col_cursor = 0;
    app.config
        .set_col_width("id-mysql", "shop", "", "orders", "id", 20);
    app.config
        .set_col_width("id-mysql", "shop", "", "orders", "name", 10);
    let scope = col_width_scope(&app);
    app.col_width_mem.adjust(&scope, "id", 6, 14);
    app.col_width_mem.adjust(&scope, "name", 6, 4);

    // `0` drops the focused column only.
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.config.col_width("id-mysql", "shop", "", "orders", "id"),
        None
    );
    assert_eq!(
        app.config
            .col_width("id-mysql", "shop", "", "orders", "name"),
        Some(10)
    );
    assert_eq!(app.col_width_mem.get(&scope, "id"), None);
    assert_eq!(app.col_width_mem.get(&scope, "name"), Some(10));

    // Alt-0 clears the whole table (session + disk).
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT),
    );
    assert_eq!(
        app.config
            .col_width("id-mysql", "shop", "", "orders", "name"),
        None
    );
    assert!(app.col_width_mem.overrides(&scope).is_none());
}

// ── R48: pinned result pane ──

/// R48: `Alt-F` (results pane) toggles a pin; the pinned grid survives a
/// live-grid clear (a table switch) and is drawn with a 📌 header above the
/// live pane at 42×22 without panicking.
#[test]
pub(crate) fn pin_results_survives_a_table_switch_and_renders() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    // Alt-F in the results pane pins; it is free there (the editor keeps
    // Alt-F for SQL formatting).
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
    );
    assert!(app.pinned_result.is_some(), "Alt-F should pin the grid");
    // Switching table clears the live grid, but the pinned snapshot stays.
    app.clear_grid();
    assert!(
        app.pinned_result.is_some(),
        "a table switch must not drop the pin"
    );
    let rows = draw(&mut app, 42, 22);
    assert!(
        rows.iter().any(|r| r.contains("📌")),
        "pin marker missing: {rows:?}"
    );
    // A second Alt-F releases it.
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT),
    );
    assert!(app.pinned_result.is_none());
}

/// R48: pinning an empty pane reports why instead of pinning nothing.
#[test]
pub(crate) fn pin_results_without_a_grid_reports() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.clear_grid();
    toggle_pin_results(&mut app);
    assert!(app.pinned_result.is_none());
}

// ── R48: `gc` column-structure popup ──

/// R48: the `g c` chord opens the column-structure popup from the results
/// pane (and only the chord — a bare `c` still opens column visibility).
#[test]
pub(crate) fn gc_chord_opens_the_column_popup() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![pk_col("id", "bigint")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    app.focus = Focus::Preview;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert!(app.pending_g, "g should arm the chord");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );
    assert!(app.cols_popup_open, "g c should open the column popup");
    assert!(!app.col_picker_open, "g c must not open column visibility");
}

/// R48: `gc` opens a mini overlay from the cached `table_meta` (name / type /
/// nullable / key / comment) — no extra query — and it renders at 42×22.
#[test]
pub(crate) fn cols_popup_lists_cached_metadata() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![pk_col("id", "bigint"), col_info("note", "text")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    open_cols_popup(&mut app);
    assert!(app.cols_popup_open);
    let rows = draw(&mut app, 42, 22);
    let joined = rows.join("\n");
    assert!(
        joined.contains("id") && joined.contains("bigint"),
        "{joined}"
    );
    assert!(joined.contains("PRI"), "primary-key mark missing: {joined}");
    // A query result has no `table_meta`: the grid columns stand in.
    app.cols_popup_open = false;
    app.table_meta = None;
    open_cols_popup(&mut app);
    assert!(app.cols_popup_open);
    let rows = draw(&mut app, 42, 22);
    assert!(rows.join("\n").contains("column_0"));
    // Esc closes it.
    cols_popup_key(&mut app, &test_tx(), KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.cols_popup_open);
    // With neither metadata nor a grid there is nothing to open.
    app.cols_popup_open = false;
    app.clear_grid();
    open_cols_popup(&mut app);
    assert!(!app.cols_popup_open);
}

// ── R56: popup default value / key mark / in-place column filter ──

/// R56: the key mark follows `COLUMN_KEY` where dbx-core exposes it (`PRI` /
/// `UNI`) and derives `MUL` from the cached index metadata (first column of a
/// non-unique, non-primary index, MySQL's rule).
#[test]
pub(crate) fn column_key_mark_reads_flags_and_indexes() {
    assert_eq!(column_key_mark(&pk_col("id", "bigint"), &[]), "PRI");
    let mut uq = col_info("email", "varchar(64)");
    uq.is_unique = true;
    assert_eq!(column_key_mark(&uq, &[]), "UNI");

    let mul = col_info("user_id", "bigint");
    let ix = idx_info("idx_user", &["user_id"], false, false);
    assert_eq!(column_key_mark(&mul, std::slice::from_ref(&ix)), "MUL");
    // A second-position column of the same index is not `MUL`.
    let second = col_info("created_at", "datetime");
    assert_eq!(
        column_key_mark(
            &second,
            &[idx_info(
                "idx_two",
                &["user_id", "created_at"],
                false,
                false
            )]
        ),
        ""
    );
    // A unique / primary index never yields MUL.
    assert_eq!(
        column_key_mark(&mul, &[idx_info("uq", &["user_id"], true, false)]),
        ""
    );
    assert_eq!(
        column_key_mark(&mul, &[idx_info("pk", &["user_id"], true, true)]),
        ""
    );
    // No cached index metadata (PostgreSQL, or a backend that cannot list)
    // leaves the mark blank instead of guessing.
    assert_eq!(column_key_mark(&mul, &[]), "");
    // The column name matches case-insensitively.
    assert_eq!(
        column_key_mark(&col_info("USER_ID", "bigint"), &[ix]),
        "MUL"
    );
}

/// R56: the popup rows carry the default value and the key mark; a NULL or
/// whitespace-only default is an empty string, never the literal `NULL`.
#[test]
pub(crate) fn cols_popup_rows_carry_default_and_key_marks() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    let mut uq = col_info("email", "varchar(64)");
    uq.is_unique = true;
    uq.column_default = Some("anon@example.com".into());
    let mut created = col_info("created_at", "datetime");
    created.column_default = Some("CURRENT_TIMESTAMP".into());
    created.is_nullable = false;
    let mut blank = col_info("note", "text");
    blank.column_default = Some("   ".into());
    blank.comment = Some("free text".into());
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![pk_col("id", "bigint"), uq, created, blank],
        indexes: vec![idx_info("idx_note", &["note"], false, false)],
        foreign_keys: Vec::new(),
    });

    let rows = cols_popup_rows(&app);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].key, "PRI");
    assert_eq!(rows[1].key, "UNI");
    assert_eq!(rows[1].default, "anon@example.com");
    assert_eq!(rows[2].key, "");
    assert_eq!(rows[2].default, "CURRENT_TIMESTAMP");
    assert!(!rows[2].nullable);
    assert_eq!(rows[3].key, "MUL");
    assert_eq!(rows[3].default, "", "a blank default shows as empty");
    assert_eq!(rows[3].comment, "free text");

    // A query result (no metadata) degrades to names only, no key column.
    app.table_meta = None;
    let rows = cols_popup_rows(&app);
    assert_eq!(rows.len(), 8);
    assert!(rows
        .iter()
        .all(|r| r.key.is_empty() && r.default.is_empty()));
}

/// R56: the aligned popup line keeps the column name / type / key readable on
/// a narrow screen while the (lower-priority) default and comment clip first.
#[test]
pub(crate) fn cols_popup_line_keeps_name_and_type_on_a_narrow_screen() {
    let row = ColPopupRow {
        name: "user_id".into(),
        data_type: "bigint".into(),
        key: "MUL",
        default: "CURRENT_TIMESTAMP".into(),
        nullable: false,
        comment: "the owner".into(),
    };
    let refs = vec![&row];
    let wide = cols_popup_layout(&refs, 96);
    let line = cols_popup_line(&row, &wide, 96);
    assert!(line.starts_with("user_id  bigint  MUL"), "{line}");
    assert!(line.contains("=CURRENT_TIMESTAMP"), "{line}");
    assert!(line.contains("NOT NULL"), "{line}");
    assert!(line.contains("· the owner"), "{line}");

    // 28 columns: name / type / key survive, the comment is dropped first.
    let narrow = cols_popup_layout(&refs, 28);
    let line = cols_popup_line(&row, &narrow, 28);
    assert!(disp_width(&line) <= 28, "{line}");
    assert!(line.starts_with("user_id  bigint  MUL"), "{line}");
    assert!(!line.contains("the owner"), "comment clips first: {line}");

    // Empty columns collapse instead of leaving stray separators.
    let name_only = ColPopupRow {
        name: "column_0".into(),
        data_type: String::new(),
        key: "",
        default: String::new(),
        nullable: true,
        comment: String::new(),
    };
    let refs = vec![&name_only];
    let l = cols_popup_layout(&refs, 40);
    assert_eq!(cols_popup_line(&name_only, &l, 40), "column_0");
}

/// R56: the popup renders the new default / key columns at both a phone and
/// a desktop width (the default clips on the phone, by design).
#[test]
pub(crate) fn cols_popup_renders_default_and_key() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    let mut created = col_info("created_at", "datetime");
    created.column_default = Some("CURRENT_TIMESTAMP".into());
    created.is_nullable = false;
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![pk_col("id", "bigint"), created],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    open_cols_popup(&mut app);
    let phone = draw(&mut app, 42, 22).join("\n");
    assert!(phone.contains("PRI"), "{phone}");
    assert!(
        phone.contains("CURRENT"),
        "default clipped but visible: {phone}"
    );
    let desktop = draw(&mut app, 110, 30).join("\n");
    assert!(desktop.contains("PRI"), "{desktop}");
    assert!(desktop.contains("CURRENT_TIMESTAMP"), "{desktop}");
    assert!(desktop.contains("NOT NULL"), "{desktop}");
}

/// R56: `/` inside the popup filters the column list by name — typed needle,
/// Enter keeps it, Esc clears it, and a needle that matches nothing renders an
/// explicit empty state instead of a blank box.
#[test]
pub(crate) fn cols_popup_filter_state_machine() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![
            col_info("id", "bigint"),
            col_info("user_id", "bigint"),
            col_info("total", "numeric"),
        ],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    open_cols_popup(&mut app);
    assert!(app.cols_popup_open);
    assert_eq!(cols_popup_hits(&app), (3, 3));

    // `/` opens the prompt; typing filters as you go.
    cols_popup_key(
        &mut app,
        &test_tx(),
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert!(app.cols_popup_filter.is_some());
    for ch in "user".chars() {
        cols_popup_filter_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.cols_popup_needle, "user");
    assert_eq!(cols_popup_hits(&app), (1, 3));

    // Enter keeps the needle and closes the prompt (popup stays open).
    cols_popup_filter_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.cols_popup_filter.is_none());
    assert_eq!(app.cols_popup_needle, "user");
    assert!(app.cols_popup_open);

    // Re-opening prefills the needle; Esc clears it.
    cols_popup_key(
        &mut app,
        &test_tx(),
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert_eq!(
        app.cols_popup_filter.as_ref().unwrap().lines().join(""),
        "user"
    );
    cols_popup_filter_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.cols_popup_filter.is_none());
    assert!(app.cols_popup_needle.is_empty());
    assert_eq!(cols_popup_hits(&app), (3, 3));

    // A needle with no hit renders the empty state, and Esc closes the popup
    // and clears the needle.
    app.cols_popup_needle = "zzz".into();
    assert_eq!(cols_popup_hits(&app), (0, 3));
    let screen: String = draw(&mut app, 42, 22)
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(screen.contains("没有匹配的列"), "{screen}");
    cols_popup_key(&mut app, &test_tx(), KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.cols_popup_open);
    assert!(app.cols_popup_needle.is_empty());
}

/// R65: Enter in the `gc` popup jumps the cell cursor to the highlighted
/// column (matched by name on the visible grid) and closes the popup. `j`
/// moves the cursor; a column the view does not show reports instead.
#[test]
pub(crate) fn gc_enter_jumps_to_the_highlighted_column() {
    // Pure name mapping: exact first, then case-insensitive, else `None`.
    let cols = vec!["a".to_string(), "Column_5".to_string()];
    assert_eq!(col_index_by_name(&cols, "a"), Some(0));
    assert_eq!(col_index_by_name(&cols, "column_5"), Some(1));
    assert_eq!(col_index_by_name(&cols, "Column_5"), Some(1));
    assert_eq!(col_index_by_name(&cols, "zzz"), None);

    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![col_info("column_3", "text"), col_info("column_5", "text")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    open_cols_popup(&mut app);
    assert_eq!(app.cols_popup_sel, 0);
    // `j` moves the cursor (the scroll window follows it), not the offset.
    cols_popup_key(
        &mut app,
        &test_tx(),
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(app.cols_popup_sel, 1);
    cols_popup_key(&mut app, &test_tx(), KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.cols_popup_open, "Enter closes the popup");
    assert_eq!(app.col_cursor, 5, "cursor lands on column_5");
    assert!(app.status.contains("column_5"), "{}", app.status);

    // A column hidden from the view reports and keeps the popup open.
    app.col_hidden.insert("column_3".into());
    app.reapply_col_filter();
    open_cols_popup(&mut app);
    cols_popup_key(&mut app, &test_tx(), KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.cols_popup_open, "a missing column keeps the popup open");
    assert!(app.status.contains("不在当前视图"), "{}", app.status);
}

// ── R66: `gc` column value distribution (client-side stats) ──

/// R66: the stats count non-null / null / distinct and, when every non-null
/// value parses as a number, the min / max / average.
#[test]
pub(crate) fn col_stats_counts_nulls_distinct_and_numbers() {
    let grid = Grid {
        columns: vec!["n".into(), "s".into()],
        rows: vec![
            vec![Val::Text("1".into()), Val::Text("a".into())],
            vec![Val::Text("2".into()), Val::Text("a".into())],
            vec![Val::Text("2".into()), Val::Text("b".into())],
            vec![Val::Null, Val::Null],
            vec![Val::Text(" 3 ".into()), Val::Text(String::new())],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    let n = col_stats(&grid, 0, 100);
    assert_eq!(n.non_null, 4);
    assert_eq!(n.nulls, 1);
    assert_eq!(n.distinct, 3);
    assert!(n.numeric);
    assert_eq!(n.min, 1.0);
    assert_eq!(n.max, 3.0);
    assert_eq!(n.avg, 2.0);
    assert!(!n.truncated);

    let s = col_stats(&grid, 1, 100);
    assert_eq!(s.non_null, 4);
    assert_eq!(s.nulls, 1);
    assert_eq!(s.distinct, 3, "a, b and the empty string");
    assert!(!s.numeric, "a text column has no min/max");

    // A missing cell counts as null, like an explicit NULL.
    let short = Grid {
        columns: vec!["n".into()],
        rows: vec![vec![Val::Text("1".into())], vec![]],
        note: String::new(),
        types: Vec::new(),
    };
    assert_eq!(col_stats(&short, 0, 100).nulls, 1);
}

/// R66: a page larger than the scan limit is sampled and flagged so the
/// popup can say "over the first N rows".
#[test]
pub(crate) fn col_stats_scan_limit_truncates() {
    let grid = Grid {
        columns: vec!["n".into()],
        rows: (1..=5).map(|i| vec![Val::Text(i.to_string())]).collect(),
        note: String::new(),
        types: Vec::new(),
    };
    let s = col_stats(&grid, 0, 3);
    assert!(s.truncated);
    assert_eq!(s.scanned, 3);
    assert_eq!(s.non_null, 3);
    assert_eq!(s.min, 1.0);
    assert_eq!(s.max, 3.0);
    assert_eq!(s.avg, 2.0);

    let full = col_stats(&grid, 0, 10);
    assert!(!full.truncated);
    assert_eq!(full.scanned, 5);
    assert_eq!(full.max, 5.0);
    assert_eq!(full.avg, 3.0);
}

/// R66: numbers format as bare integers or trimmed fractions.
#[test]
pub(crate) fn fmt_stat_num_trims_and_keeps_integers_bare() {
    assert_eq!(fmt_stat_num(12.0), "12");
    assert_eq!(fmt_stat_num(4.5), "4.5");
    assert_eq!(fmt_stat_num(1.0 / 3.0), "0.3333");
    assert_eq!(fmt_stat_num(-0.25), "-0.25");
}

/// R66: the stats lines show the counts, a numeric tail, the non-numeric
/// note or the no-data hint, and the sample note when truncated.
#[test]
pub(crate) fn col_stats_lines_cover_each_state() {
    let num = ColStats {
        non_null: 4,
        nulls: 1,
        distinct: 3,
        numeric: true,
        min: 1.0,
        max: 3.0,
        avg: 2.0,
        scanned: 5,
        truncated: false,
        spark: String::new(),
    };
    let joined = col_stats_lines("total", Some(&num), 40, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("值分布 · total"), "{joined}");
    assert!(joined.contains("非空 4"), "{joined}");
    assert!(joined.contains("min 1 · max 3 · avg 2"), "{joined}");

    let text = ColStats {
        numeric: false,
        truncated: true,
        scanned: 5000,
        ..num.clone()
    };
    let joined = col_stats_lines("name", Some(&text), 40, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("（非数值列）"), "{joined}");
    assert!(joined.contains("（按前 5000 行统计）"), "{joined}");

    let none = col_stats_lines("name", None, 40, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(none.contains("打开表数据后可用"), "{none}");
}

// ── R72: value-distribution sparkline ──

/// R72: the eight-level sparkline puts a bar in the matching bucket, leaves an
/// empty bucket blank, and gives a single distinct value one centred bar.
#[test]
pub(crate) fn sparkline_buckets_numbers_and_lengths() {
    // Two values at the ends of the range: a bar at each end, blanks between.
    assert_eq!(sparkline_numeric(&[0.0, 10.0], 12), "█          █");
    // A single distinct value centres in one bucket.
    let single = sparkline_numeric(&[5.0, 5.0, 5.0], 12);
    assert_eq!(single.chars().count(), 12);
    assert_eq!(single.chars().filter(|c| *c == '█').count(), 1);
    assert_eq!(single.chars().position(|c| c == '█'), Some(6));
    // Levels scale with the bucket count.
    assert_eq!(spark_from_buckets(&[1, 2, 3]), "▃▆█");
    // An all-zero histogram is all spaces, never a misleading bar.
    assert_eq!(spark_from_buckets(&[0, 0, 0]), "   ");
    // Lengths bucket the same way (all-equal lengths land in one bucket).
    let lens = sparkline_lengths(&[2, 2, 2], 12);
    assert_eq!(lens.chars().filter(|c| *c == '█').count(), 1);
    assert_eq!(sparkline_lengths(&[1, 2, 3], 3), "███");
    // Empty inputs render nothing.
    assert!(sparkline_numeric(&[], 12).is_empty());
    assert!(sparkline_lengths(&[], 12).is_empty());
}

/// R72: `col_stats` fills a fixed-width sparkline from the sampled values —
/// numeric buckets for a numeric column, length buckets for a text one, and
/// nothing at all when there is no data.
#[test]
pub(crate) fn col_stats_builds_a_sparkline() {
    let numeric = Grid {
        columns: vec!["n".into()],
        rows: (1..=12).map(|i| vec![Val::Text(i.to_string())]).collect(),
        note: String::new(),
        types: Vec::new(),
    };
    let s = col_stats(&numeric, 0, 100);
    assert_eq!(s.spark.chars().count(), COL_SPARK_W);
    assert!(s.spark.contains('█'), "{}", s.spark);

    let text = Grid {
        columns: vec!["s".into()],
        rows: vec![
            vec![Val::Text("a".into())],
            vec![Val::Text("bb".into())],
            vec![Val::Text("bbbb".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    let s = col_stats(&text, 0, 100);
    assert_eq!(s.spark.chars().count(), COL_SPARK_W);
    assert!(s.spark.contains('█'), "{}", s.spark);

    let empty = Grid {
        columns: vec!["s".into()],
        rows: Vec::new(),
        note: String::new(),
        types: Vec::new(),
    };
    assert!(col_stats(&empty, 0, 100).spark.is_empty());
}

/// R72: the stats line appends the sparkline only when asked (the caller drops
/// it on a narrow terminal).
#[test]
pub(crate) fn col_stats_lines_show_sparkline_on_demand() {
    let stats = ColStats {
        non_null: 4,
        nulls: 1,
        distinct: 3,
        numeric: true,
        min: 1.0,
        max: 3.0,
        avg: 2.0,
        scanned: 5,
        truncated: false,
        spark: "█▁█▁█▁█▁█▁█▁".into(),
    };
    let with = col_stats_lines("total", Some(&stats), 60, true)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(with.contains('█'), "{with}");
    assert!(with.contains("去重 3"), "{with}");
    let without = col_stats_lines("total", Some(&stats), 60, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!without.contains('█'), "{without}");
}

/// R66: the popup draws the value stats — beside the list on a wide terminal,
/// stacked under it on a phone — and hints when no data is loaded.
#[test]
pub(crate) fn cols_popup_renders_value_stats() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(Grid {
        columns: vec!["total".into(), "note".into()],
        rows: vec![
            vec![Val::Text("10".into()), Val::Text("a".into())],
            vec![Val::Text("20".into()), Val::Text("b".into())],
            vec![Val::Null, Val::Text("a".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    });
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: String::new(),
        columns: vec![col_info("total", "int"), col_info("note", "text")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    open_cols_popup(&mut app);
    // CJK glyphs occupy two cells, so compare on a whitespace-stripped copy.
    let compact = |rows: Vec<String>| -> String {
        rows.join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    // Wide: the stats sit in a right pane.
    let wide = compact(draw(&mut app, 110, 30));
    assert!(wide.contains("值分布·total"), "{wide}");
    assert!(wide.contains("min10·max20"), "{wide}");
    assert!(wide.contains("avg15"), "{wide}");
    // R72: the sparkline rides beside the distinct count on a wide terminal.
    assert!(wide.contains("去重2██"), "{wide}");
    // `j` moves to the text column: the counts stay, the numeric tail turns
    // into a non-numeric note.
    cols_popup_key(
        &mut app,
        &test_tx(),
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    let wide2 = compact(draw(&mut app, 110, 30));
    assert!(wide2.contains("值分布·note"), "{wide2}");
    assert!(wide2.contains("（非数值列）"), "{wide2}");
    // Phone: the same stats stack under the list (both stay on screen).
    let phone = compact(draw(&mut app, 42, 22));
    assert!(phone.contains("值分布·note"), "{phone}");
    assert!(phone.contains("非空3"), "{phone}");
    // R72: below 56 columns the sparkline is dropped.
    assert!(!phone.contains("去重2█"), "{phone}");

    // No loaded data: the popup still opens from the cached metadata and
    // hints instead of showing an empty stats pane.
    app.cols_popup_open = false;
    app.clear_grid();
    open_cols_popup(&mut app);
    assert!(app.cols_popup_open);
    let empty = compact(draw(&mut app, 110, 30));
    assert!(empty.contains("打开表数据后可用"), "{empty}");
}

/// R65: `g b` opens a type-to-filter table switcher over the current
/// database's *cached* tables. The needle narrows the rows, Enter opens the
/// highlighted table's data view, and Esc closes it without a query.
#[tokio::test(flavor = "multi_thread")]
async fn table_jump_panel_filters_and_opens() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.databases = vec!["shop".into()];
    app.schema = "shop".into();
    app.tables_all = vec![
        table_info("orders", "TABLE"),
        table_info("order_items", "TABLE"),
        table_info("users", "TABLE"),
        table_info("v_orders", "VIEW"),
    ];
    app.tables = app.tables_all.clone();
    app.focus = Focus::Preview;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());

    // `g` arms the chord, `b` opens the switcher; the list is name-ordered.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
    );
    assert!(app.pending_g);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
    );
    assert!(app.table_jump_open, "g b opens the switcher");
    assert!(!app.pending_g);
    assert_eq!(table_jump_rows(&app).len(), 4);

    // Type to filter: `order` keeps orders / order_items / v_orders.
    for ch in "order".chars() {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    let names: Vec<String> = table_jump_rows(&app).into_iter().map(|r| r.0).collect();
    assert_eq!(names, vec!["order_items", "orders", "v_orders"]);
    assert!(app.status.contains("3/4"), "{}", app.status);

    // `j` / `k` stay vim-down/up (they move, they never filter); Enter
    // opens the selected table.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(app.table_jump_needle, "order", "j must not filter");
    assert_eq!(app.table_jump_list.selected(), Some(1));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    );
    assert_eq!(app.table_jump_list.selected(), Some(0));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(app.table_jump_list.selected(), Some(1));
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(!app.table_jump_open, "Enter closes the switcher");
    let ps = app.page_state.as_ref().expect("the table data page opened");
    assert_eq!(ps.table, "orders");
    assert_eq!(app.table_list.selected(), Some(0));

    // Esc closes it too, and the overlay renders with its needle at 42x22.
    open_table_jump(&mut app);
    app.table_jump_needle = "ord".into();
    let screen: String = draw(&mut app, 42, 22)
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(screen.contains("切换表"), "{screen}");
    assert!(screen.contains("orders"), "{screen}");
    // A needle with no hit renders an empty list without panicking.
    app.table_jump_needle = "zzz".into();
    app.table_jump_list.select(Some(0));
    let empty: String = draw(&mut app, 42, 22).join("\n");
    assert!(empty.contains("0/4"), "{empty}");
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.table_jump_open);
}

/// R65: the status bar names where the open data view lives. The pure label
/// folds the schema in only when it differs from the database (PostgreSQL)
/// and skips it when it duplicates it (MySQL); a narrow terminal keeps just
/// the table name.
#[test]
pub(crate) fn status_bar_shows_the_open_table_location() {
    assert_eq!(
        session_label("shop", "shop", "orders", 120).as_deref(),
        Some("shop.orders")
    );
    assert_eq!(
        session_label("shop", "public", "orders", 120).as_deref(),
        Some("shop.public.orders")
    );
    assert_eq!(
        session_label("shop", "shop", "orders", 42).as_deref(),
        Some("orders")
    );
    assert_eq!(
        session_label("", "", "orders", 120).as_deref(),
        Some("orders")
    );
    assert_eq!(session_label("shop", "shop", "", 120), None);
    // `term_w == 0` (tests / unknown width) counts as wide, like the server
    // version field.
    assert_eq!(
        session_label("shop", "public", "orders", 0).as_deref(),
        Some("shop.public.orders")
    );

    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("postgres"));
    app.backend_kind = Backend::Sql;
    app.databases = vec!["shop".into()];
    app.schema = "public".into();
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.page_state = Some(PageState {
        table: "orders".into(),
        schema: "public".into(),
        table_type: Some("TABLE".into()),
        page: 0,
        page_size: PAGE_SIZE,
        total: None,
        total_lower_bound: false,
        has_next: false,
        filter: String::new(),
        order_by: None,
        keyset: None,
    });
    app.term_w = 120;
    let wide = context_info(&app);
    assert!(wide.contains("shop.public.orders"), "{wide}");
    app.term_w = 42;
    let narrow = context_info(&app);
    assert!(narrow.contains("orders"), "{narrow}");
    assert!(!narrow.contains("shop.public"), "{narrow}");
}

/// R43: the tree cursor walks tables, `h` collapses the active database
/// (hiding its tables) and remembers it, and `l` expands it again.
#[test]
pub(crate) fn sidebar_tree_collapse_memory_and_cursor_walk() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = tree_app();
    rebuild_side_rows(&mut app);
    // The cursor follows the externally selected table.
    assert!(matches!(
        app.side_rows[app.side_sel],
        SideRow::Table { table: 0, .. }
    ));
    side_step(&mut app, 1, true);
    assert_eq!(app.table_list.selected(), Some(1));
    side_step(&mut app, 1, false);
    assert_eq!(app.table_list.selected(), Some(0));
    // `h` on a table steps up to its database, then collapses it.
    side_collapse(&mut app);
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Db { .. }));
    side_collapse(&mut app);
    assert!(
        app.side_rows
            .iter()
            .all(|r| !matches!(r, SideRow::Table { .. })),
        "collapsed database hides its tables"
    );
    assert!(matches!(app.side_rows[app.side_sel], SideRow::Db { .. }));
    // `l` re-expands the database; the collapse is remembered per (conn, db).
    side_expand(&mut app, &tx);
    assert!(app
        .side_rows
        .iter()
        .any(|r| matches!(r, SideRow::Table { .. })));
}

/// R43: the `/` filter keeps a node when it or a descendant matches, so the
/// database / connection ancestors of a table hit survive.
#[test]
pub(crate) fn sidebar_tree_filter_scopes_to_visible_nodes() {
    let mut app = tree_app();
    app.table_filter = "ord".into();
    apply_table_filter(&mut app);
    assert_eq!(app.tables.len(), 1);
    let rows = compute_side_rows(&app);
    assert!(rows.iter().any(|r| matches!(r, SideRow::Conn { .. })));
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Db { db, .. } if db == "shop")));
    assert!(
        !rows
            .iter()
            .any(|r| matches!(r, SideRow::Db { db, .. } if db == "logs")),
        "a non-matching sibling database is hidden"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(r, SideRow::Table { .. }))
            .count(),
        1
    );
}

/// R43: a failed lazy database fetch draws an error row under the connection
/// and never breaks the rest of the tree.
#[test]
pub(crate) fn sidebar_tree_shows_error_row_for_a_failed_lazy_load() {
    let mut app = tree_app();
    let c2 = app.connections[1].clone();
    app.tree_conn_open.insert(c2.id.clone());
    app.tree_db_state
        .insert(c2.id, TreeDbState::Error("boom".into()));
    let rows = compute_side_rows(&app);
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::ConnError { msg, .. } if msg == "boom")));
    // The active subtree is untouched.
    assert!(rows.iter().any(|r| matches!(r, SideRow::Table { .. })));
}

/// R43: expanding a non-active connection reads its cached database list
/// (no switch), and a cached list is shown as database rows.
#[test]
pub(crate) fn sidebar_tree_shows_cached_databases_for_a_sibling() {
    let mut app = tree_app();
    let c2 = app.connections[1].clone();
    app.tree_conn_open.insert(c2.id.clone());
    app.tree_dbs
        .insert(c2.id.clone(), vec!["analytics".into(), "staging".into()]);
    let rows = compute_side_rows(&app);
    let dbs: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            SideRow::Db { idx: 1, db, .. } => Some(db.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(dbs, vec!["analytics", "staging"]);
}

/// R43: a sibling whose cached databases are all excluded by the filter
/// shows nothing, not a misleading "loading" row.
#[test]
pub(crate) fn sidebar_tree_filter_does_not_fake_a_loading_row() {
    let mut app = tree_app();
    let c2 = app.connections[1].clone();
    app.tree_conn_open.insert(c2.id.clone());
    app.tree_dbs.insert(c2.id.clone(), vec!["analytics".into()]);
    app.table_filter = "ord".into();
    apply_table_filter(&mut app);
    let rows = compute_side_rows(&app);
    assert!(!rows
        .iter()
        .any(|r| matches!(r, SideRow::ConnLoading { .. })));
    assert!(rows
        .iter()
        .any(|r| matches!(r, SideRow::Db { db, .. } if db == "shop")));
}

// ── R47b: connection status dots + manual disconnect ──

/// The status dot is a pure function of the local connecting mark and the
/// cached kernel liveness; shape (not just colour) tells the states apart.
#[test]
pub(crate) fn conn_status_dot_state_machine() {
    let mut app = tree_app();
    let c1 = app.connections[0].id.clone();
    let c2 = app.connections[1].id.clone();
    // Unknown ids read as idle; the three shapes are distinct.
    assert_eq!(conn_status_for(&app, &c1), ConnStatus::Idle);
    assert_eq!(conn_status_for(&app, &c2), ConnStatus::Idle);
    assert_eq!(ConnStatus::Active.shape(), "●");
    assert_eq!(ConnStatus::Idle.shape(), "○");
    assert_eq!(ConnStatus::Connecting.shape(), "◐");
    // A pool in the kernel cache is active.
    app.conn_live.insert(c1.clone(), true);
    assert_eq!(conn_status_for(&app, &c1), ConnStatus::Active);
    // A pending open wins over a stale live flag (it is the newer fact).
    app.conn_connecting.insert(c1.clone());
    assert_eq!(conn_status_for(&app, &c1), ConnStatus::Connecting);
    // Resolving through the tree root index agrees, and an out-of-range root
    // (or a config-less synthetic root) is idle, never a panic.
    assert_eq!(side_conn_status(&app, 0), ConnStatus::Connecting);
    assert_eq!(side_conn_status(&app, 1), ConnStatus::Idle);
    assert_eq!(side_conn_status(&app, 99), ConnStatus::Idle);
}

/// The rendered tree shows the shape per connection: `●` live, `○` idle,
/// `◐` while a lazy open is in flight.
#[test]
pub(crate) fn tree_root_draws_the_status_shape_per_connection() {
    let mut app = tree_app();
    app.picker_open = false;
    let c1 = app.connections[0].id.clone();
    let c2 = app.connections[1].id.clone();
    app.conn_live.insert(c1.clone(), true);
    app.conn_live.insert(c2.clone(), false);
    let joined = draw(&mut app, 100, 30).join("\n");
    assert!(joined.contains("● "), "active dot missing: {joined}");
    assert!(joined.contains("○ "), "idle dot missing: {joined}");
    app.conn_connecting.insert(c2);
    let joined = draw(&mut app, 100, 30).join("\n");
    assert!(joined.contains("◐ "), "connecting dot missing: {joined}");
}

/// `x` on a root opens the red confirm only when the connection is live (or
/// opening); Enter then spawns a *disconnect*, not a delete.
#[test]
pub(crate) fn disconnect_confirm_opens_only_for_live_connections() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        let c1 = app.connections[0].clone();
        // Idle: nothing to disconnect, no modal.
        request_disconnect(&mut app, 0);
        assert!(app.confirm.is_none());
        assert!(app.status.contains("已断开"), "{}", app.status);
        // Live: the confirm carries the disconnect semantics.
        app.conn_live.insert(c1.id.clone(), true);
        request_disconnect(&mut app, 0);
        let cc = app
            .confirm
            .as_ref()
            .and_then(|c| c.conn.clone())
            .expect("a confirm layer");
        assert!(cc.disconnect, "it is a disconnect, not a delete");
        assert_eq!(cc.id, c1.id);
        // Enter accepts and spawns the disconnect op.
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(app.confirm.is_none(), "the layer closed");
        assert!(app.status.contains("断开连接"), "{}", app.status);
    });
}

/// Disconnecting the *active* connection clears the pool, collapses the root
/// to a grey anchor and moves the cursor to the nearest other root — never
/// an empty screen.
#[test]
pub(crate) fn disconnect_active_root_collapses_and_moves_focus() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        let c1 = app.connections[0].id.clone();
        app.conn_live.insert(c1.clone(), true);
        rebuild_side_rows(&mut app);
        app.side_sel = app
            .side_rows
            .iter()
            .position(|r| matches!(r, SideRow::Conn { idx: 0, .. }))
            .unwrap();
        apply_op_result(
            &mut app,
            OpResult::ConnDisconnected {
                id: c1.clone(),
                name: "test-mysql".into(),
                error: None,
            },
            &tx,
        );
        assert!(!conn_is_live(&app, &c1), "the pool is gone");
        assert!(app.tree_conn_closed.contains(&c1), "the root collapsed");
        assert!(
            app.databases.is_empty(),
            "the dead pool's browse state is gone"
        );
        // The cursor landed on the sibling root, not the dead one.
        assert!(matches!(
            app.side_rows[app.side_sel],
            SideRow::Conn { idx: 1, .. }
        ));
        // Re-expanding the dead root reconnects (marks it connecting).
        let idx = app
            .side_rows
            .iter()
            .position(|r| matches!(r, SideRow::Conn { idx: 0, .. }))
            .unwrap();
        app.side_sel = idx;
        side_expand(&mut app, &tx);
        assert!(app.conn_connecting.contains(&c1), "reconnect is in flight");
        assert!(!app.tree_conn_closed.contains(&c1));
    });
}

/// Disconnecting a *non-active* connection keeps its cached database list
/// visible (muted) and leaves the active subtree alone.
#[test]
pub(crate) fn disconnect_sibling_keeps_cached_databases() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        let c2 = app.connections[1].id.clone();
        app.tree_conn_open.insert(c2.clone());
        app.tree_dbs
            .insert(c2.clone(), vec!["analytics".into(), "staging".into()]);
        app.conn_live.insert(c2.clone(), true);
        apply_op_result(
            &mut app,
            OpResult::ConnDisconnected {
                id: c2.clone(),
                name: "test-postgres".into(),
                error: None,
            },
            &tx,
        );
        assert!(!conn_is_live(&app, &c2));
        let rows = compute_side_rows(&app);
        assert!(
            rows.iter()
                .any(|r| matches!(r, SideRow::Db { idx: 1, db, .. } if db == "analytics")),
            "the cached sibling list stays visible"
        );
        assert!(
            rows.iter().any(|r| matches!(r, SideRow::Table { .. })),
            "the active subtree is untouched"
        );
    });
}

/// Expanding a collapsed, disconnected sibling re-fetches (a reconnect), not
/// just re-shows the stale cache.
#[test]
pub(crate) fn expand_reconnects_a_disconnected_sibling() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        app.picker_open = false;
        let c2 = app.connections[1].id.clone();
        app.tree_dbs.insert(c2.clone(), vec!["analytics".into()]);
        app.conn_live.insert(c2.clone(), false);
        rebuild_side_rows(&mut app);
        let idx = app
            .side_rows
            .iter()
            .position(|r| matches!(r, SideRow::Conn { idx: 1, .. }))
            .unwrap();
        app.side_sel = idx;
        side_expand(&mut app, &tx);
        assert!(
            app.conn_connecting.contains(&c2),
            "a reconnect is in flight"
        );
        assert!(matches!(
            app.tree_db_state.get(&c2),
            Some(TreeDbState::Loading)
        ));
    });
}

/// The red layer spells out the transaction-rollback semantics, at every
/// terminal size.
#[test]
pub(crate) fn disconnect_confirm_renders_the_rollback_warning() {
    let mut app = tree_app();
    app.picker_open = false;
    let c1 = app.connections[0].clone();
    app.conn_live.insert(c1.id.clone(), true);
    open_disconnect_confirm(&mut app, &c1);
    assert!(app.confirm.is_some());
    let joined = draw(&mut app, 100, 30).join("\n");
    // Wide glyphs leave a blank continuation cell; strip whitespace so the
    // CJK runs are contiguous.
    let tight: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(tight.contains("断开连接"), "{joined}");
    assert!(tight.contains("回滚"), "the rollback warning: {joined}");
    // Degenerate sizes must not panic.
    for (w, h) in [(42u16, 22u16), (30, 10), (20, 6), (1, 1)] {
        draw(&mut app, w, h);
    }
}

/// A lazy fetch failure flips the root back to idle and shows the error row.
#[test]
pub(crate) fn failed_lazy_fetch_marks_the_root_idle() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = tree_app();
        let c2 = app.connections[1].id.clone();
        app.conn_connecting.insert(c2.clone());
        let gen = 1;
        app.tree_gen.insert(c2.clone(), gen);
        apply_op_result(
            &mut app,
            OpResult::TreeDatabases {
                conn_id: c2.clone(),
                databases: Vec::new(),
                error: Some("boom".into()),
                gen,
            },
            &tx,
        );
        assert!(
            !app.conn_connecting.contains(&c2),
            "the pending mark cleared"
        );
        assert!(!conn_is_live(&app, &c2), "a failed open is not live");
        assert!(matches!(
            app.tree_db_state.get(&c2),
            Some(TreeDbState::Error(_))
        ));
    });
}

// ── R47b: auto-hiding, thin horizontal scroll bar ──

/// The pure auto-hide window.
#[test]
pub(crate) fn hbar_auto_hides_after_its_window() {
    let now = Instant::now();
    assert!(!hbar_should_show(None, now), "never poked means hidden");
    assert!(hbar_should_show(Some(now + Duration::from_millis(1)), now));
    assert!(
        !hbar_should_show(Some(now), now),
        "the deadline is exclusive"
    );
    assert!(!hbar_should_show(Some(now - Duration::from_millis(1)), now));
    // `poke_hbar` sets a future deadline.
    let mut app = test_app();
    assert!(!hbar_should_show(app.hbar_until, Instant::now()));
    app.poke_hbar();
    assert!(hbar_should_show(app.hbar_until, Instant::now()));
}

/// At rest the bar is gone; a horizontal scroll brings it back, drawn with
/// the thin dashed/heavy line instead of the old half-cell band.
#[test]
pub(crate) fn hbar_shows_after_a_horizontal_scroll_with_a_thin_track() {
    let mut app = test_app();
    app.picker_open = false;
    app.grid_kind = GridKind::TableData;
    app.set_grid(ten_col_grid());
    app.focus = Focus::Preview;
    // First render lays out the grid and fills `vis_cols`.
    let rows = draw(&mut app, 46, 20);
    assert!(!app.rects.hbar_visible, "hidden at rest");
    assert!(
        rows.iter().all(|r| !r.contains('━') && !r.contains('┄')),
        "no bar glyph at rest"
    );
    // A horizontal pan summons it.
    assert!(pan_columns(&mut app, 1), "the grid overflows horizontally");
    let rows = draw(&mut app, 46, 20);
    assert!(app.rects.hbar_visible, "a horizontal scroll shows the bar");
    let border = &rows[app.rects.hbar.y as usize];
    assert!(
        border.contains('━') || border.contains('┄'),
        "thin bar drawn: {border:?}"
    );
    assert!(
        !border.contains('▁') && !border.contains('▄'),
        "the old thick band is gone: {border:?}"
    );
    // The `◀` / `▶` tap targets appear with the bar.
    assert!(app.rects.hbar_prev.width == 1 && app.rects.hbar_next.width == 1);
}

pub(crate) fn history_row(id: &str, sql: &str) -> HistoryRow {
    HistoryRow {
        id: id.into(),
        sql: sql.into(),
        executed_at: "2026-06-27T12:34:56Z".into(),
        connection_name: "prod".into(),
        success: true,
        duration_ms: 0,
        origin: String::new(),
        count: 1,
        session: false,
    }
}

#[test]
pub(crate) fn history_filter_is_case_insensitive_substring() {
    let mut app = test_app();
    app.history_rows = vec![
        history_row("1", "SELECT * FROM users"),
        history_row("2", "delete from orders where id = 1"),
    ];
    app.history_needle = "FROM".into();
    recompute_history_view(&mut app);
    assert_eq!(app.history_view, vec![0, 1]);
    app.history_needle = "orders".into();
    recompute_history_view(&mut app);
    assert_eq!(app.history_view, vec![1]);
    app.history_needle = "nomatch".into();
    recompute_history_view(&mut app);
    assert!(app.history_view.is_empty());
    assert_eq!(app.history_list.selected(), None);
    // The list *is* the filter view: clearing the needle restores all rows.
    app.history_needle.clear();
    recompute_history_view(&mut app);
    assert_eq!(app.history_view, vec![0, 1]);
}

/// R51: the `/` filter also matches the source connection and the run-origin
/// badge, not just the statement text.
#[test]
pub(crate) fn history_filter_matches_origin_badge_and_source() {
    let mut app = test_app();
    let mut a = history_row("1", "SELECT 1");
    a.origin = "editor".into();
    a.connection_name = "prod".into();
    let mut b = history_row("2", "SELECT 2");
    b.origin = "direct".into();
    b.connection_name = "analytics".into();
    app.history_rows = vec![a, b];
    app.history_needle = t("编").into();
    recompute_history_view(&mut app);
    assert_eq!(app.history_view, vec![0], "editor badge narrows to row 1");
    app.history_needle = "analytics".into();
    recompute_history_view(&mut app);
    assert_eq!(
        app.history_view,
        vec![1],
        "source connection narrows the view"
    );
}

/// R51: the `/` filter state machine — `/` opens the input, typing narrows
/// the view and pins the cursor, Esc clears the needle, Enter keeps it, and
/// a no-match state leaves the cursor unset.
#[test]
pub(crate) fn history_filter_state_machine_keeps_a_valid_cursor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.history_rows = vec![
        history_row("1", "SELECT alpha"),
        history_row("2", "SELECT beta"),
    ];
    app.history_open = true;
    let press = |app: &mut App, code: KeyCode| {
        history_key(app, &tx, KeyEvent::new(code, KeyModifiers::NONE));
    };
    press(&mut app, KeyCode::Char('/'));
    assert!(app.history_filter.is_some());
    for c in "beta".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.history_needle, "beta");
    assert_eq!(app.history_view, vec![1]);
    assert_eq!(app.history_list.selected(), Some(0));
    // Esc clears the needle and restores the full list.
    press(&mut app, KeyCode::Esc);
    assert!(app.history_filter.is_none());
    assert_eq!(app.history_needle, "");
    assert_eq!(app.history_view, vec![0, 1]);
    // A non-matching needle leaves the cursor unset (no bogus row 0).
    press(&mut app, KeyCode::Char('/'));
    for c in "zzz".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert!(app.history_view.is_empty());
    assert_eq!(app.history_list.selected(), None);
    press(&mut app, KeyCode::Esc);
    // Enter keeps the needle (the panel stays filtered).
    press(&mut app, KeyCode::Char('/'));
    for c in "alpha".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.history_filter.is_none());
    assert_eq!(app.history_needle, "alpha");
    assert_eq!(app.history_view, vec![0]);
}

/// R51: repeated statements collapse into one row that counts the repeats,
/// keeping the newest occurrence's metadata and the original order.
#[test]
pub(crate) fn history_merge_counts_repeats_keeping_the_newest() {
    let mut second = history_row("2", "SELECT 2");
    second.connection_name = "other".into();
    let rows = vec![
        history_row("1", "SELECT 1"),
        second,
        history_row("3", "SELECT 1"),
        history_row("4", "SELECT 1"),
        history_row("5", "SELECT 3"),
    ];
    let merged = merge_history_rows(rows);
    let sqls: Vec<&str> = merged.iter().map(|r| r.sql.as_str()).collect();
    assert_eq!(sqls, vec!["SELECT 1", "SELECT 2", "SELECT 3"]);
    assert_eq!(merged[0].count, 3);
    assert_eq!(merged[0].id, "1", "the newest occurrence wins the row");
    assert_eq!(merged[1].count, 1);
    assert_eq!(merged[2].count, 1);
}

#[test]
pub(crate) fn history_enter_recalls_into_editor_and_closes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.history_rows = vec![history_row("1", "SELECT 42")];
    app.history_view = vec![0];
    app.history_list.select(Some(0));
    app.history_open = true;
    app.focus = Focus::Sidebar;
    history_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(!app.history_open);
    assert_eq!(app.editor_sql(), "SELECT 42");
    assert!(matches!(app.focus, Focus::Editor));
}

#[test]
pub(crate) fn history_time_label_and_summary_are_compact() {
    assert_eq!(history_time_label("2026-06-27T12:34:56Z"), "06-27 12:34");
    assert_eq!(history_time_label("nope"), "nope");
    assert_eq!(history_summary("\n   SELECT a\nFROM t"), "SELECT a");
}

#[test]
pub(crate) fn history_delete_and_favorite_results_update_state() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.history_rows = vec![history_row("1", "SELECT 1"), history_row("2", "SELECT 2")];
    app.history_view = vec![0, 1];
    apply_op_result(
        &mut app,
        OpResult::HistoryDeleted {
            id: "1".into(),
            error: None,
        },
        &tx,
    );
    assert_eq!(app.history_rows.len(), 1);
    assert_eq!(app.history_rows[0].id, "2");
    // A failed delete keeps the row.
    apply_op_result(
        &mut app,
        OpResult::HistoryDeleted {
            id: "2".into(),
            error: Some("boom".into()),
        },
        &tx,
    );
    assert_eq!(app.history_rows.len(), 1);
    // Favorite toggle tracks the SQL text.
    apply_op_result(
        &mut app,
        OpResult::HistoryFavorite {
            sql: "SELECT 2".into(),
            favorited: true,
            error: None,
        },
        &tx,
    );
    assert!(app.history_favorites.contains("SELECT 2"));
    apply_op_result(
        &mut app,
        OpResult::HistoryFavorite {
            sql: "SELECT 2".into(),
            favorited: false,
            error: None,
        },
        &tx,
    );
    assert!(!app.history_favorites.contains("SELECT 2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn history_direct_run_bypasses_the_editor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.history_rows = vec![history_row("1", "SELECT 42")];
    app.history_view = vec![0];
    app.history_list.select(Some(0));
    app.history_open = true;
    app.editor.insert_str("SELECT untouched");
    history_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
    );
    assert!(!app.history_open, "the panel closes on a direct run");
    assert!(app.direct_run, "the run is marked as a direct run");
    assert_eq!(
        app.editor_sql(),
        "SELECT untouched",
        "a direct run must not touch the editor"
    );
}

#[test]
pub(crate) fn history_direct_run_danger_stops_at_confirm() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.history_rows = vec![history_row("1", "DELETE FROM users")];
    app.history_view = vec![0];
    app.history_list.select(Some(0));
    app.history_open = true;
    history_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    assert!(
        app.confirm.is_some(),
        "a DELETE still needs the red confirm"
    );
    assert_eq!(app.pending_run_origin, "direct");
    assert!(!app.history_open);
}

#[test]
pub(crate) fn history_favorites_sort_to_the_top_and_toggle_moves() {
    let mut app = test_app();
    app.history_rows = vec![
        history_row("1", "SELECT 1"),
        history_row("2", "SELECT 2"),
        history_row("3", "SELECT 3"),
    ];
    app.history_favorites.insert("SELECT 3".into());
    recompute_history_view(&mut app);
    assert_eq!(
        app.history_view,
        vec![2, 0, 1],
        "favorite first, then time order"
    );
    assert_eq!(history_fav_count(&app), 1);
    // Toggling SELECT 1 into favorites moves it under the favorites header,
    // and the cursor follows it.
    app.history_favorites.insert("SELECT 1".into());
    recompute_history_view_keep(&mut app, "SELECT 1");
    assert_eq!(
        app.history_view,
        vec![0, 2, 1],
        "favorites stay in time order"
    );
    assert_eq!(history_fav_count(&app), 2);
    assert_eq!(app.history_list.selected(), Some(0));
    // Un-favoriting drops it back to the chronological section.
    app.history_favorites.remove("SELECT 3");
    recompute_history_view_keep(&mut app, "SELECT 3");
    assert_eq!(app.history_view, vec![0, 1, 2]);
    assert_eq!(history_fav_count(&app), 1);
}

// ── R84: in-memory session run log (Alt-H panel, zero query) ──

fn session_run(sql: &str, ms: u64, success: bool) -> SessionRun {
    SessionRun {
        sql: sql.into(),
        executed_at: "2026-06-27T12:34:56Z".into(),
        duration_ms: ms,
        success,
        origin: "editor",
        connection_name: "prod".into(),
    }
}

/// R84: the session log is an LRU window — a re-run of an earlier statement is
/// promoted to the top (dedup), and the log caps at [`SESSION_RUN_MAX`].
#[test]
pub(crate) fn session_run_log_is_lru_with_dedup_promote() {
    let mut app = test_app();
    for i in 0..3 {
        app.push_session_run(session_run(&format!("SELECT {i}"), i as u64, true));
    }
    // Newest first.
    let sqls: Vec<&str> = app.session_runs.iter().map(|r| r.sql.as_str()).collect();
    assert_eq!(sqls, vec!["SELECT 2", "SELECT 1", "SELECT 0"]);
    // Re-running SELECT 0 floats it to the top without duplicating it.
    app.push_session_run(session_run("SELECT 0", 9, false));
    let sqls: Vec<&str> = app.session_runs.iter().map(|r| r.sql.as_str()).collect();
    assert_eq!(sqls, vec!["SELECT 0", "SELECT 2", "SELECT 1"]);
    assert_eq!(app.session_runs.len(), 3, "dedup, not append");
    assert!(!app.session_runs[0].success, "the fresh outcome wins");
    assert_eq!(app.session_runs[0].duration_ms, 9);
    // Empty statements are never logged.
    app.push_session_run(session_run("   ", 0, true));
    assert_eq!(app.session_runs.len(), 3);
    // The window is capped at SESSION_RUN_MAX, oldest evicted.
    for i in 0..SESSION_RUN_MAX + 5 {
        app.push_session_run(session_run(&format!("INSERT {i}"), 1, true));
    }
    assert_eq!(app.session_runs.len(), SESSION_RUN_MAX);
    // The oldest of the survivors is the newest of the first batch that still
    // fits; the very first statements are gone.
    assert!(!app.session_runs.iter().any(|r| r.sql == "SELECT 1"));
}

/// R84: the display rows merge the session log with the raw persisted rows in
/// one pass, so a statement that ran this session shows once (the session row
/// wins, the persisted duplicate folds into its `×n` count).
#[test]
pub(crate) fn session_runs_merge_on_top_of_persisted_history() {
    let mut app = test_app();
    // Two persisted rows, one of them a repeat of a session statement.
    app.history_persisted = vec![history_row("p1", "SELECT 1"), history_row("p2", "SELECT 2")];
    app.push_session_run(session_run("SELECT 2", 4, true));
    let rows: Vec<&str> = app.history_rows.iter().map(|r| r.sql.as_str()).collect();
    assert_eq!(rows, vec!["SELECT 2", "SELECT 1"], "session row first");
    assert!(app.history_rows[0].session, "marked as a session run");
    assert_eq!(app.history_rows[0].duration_ms, 4);
    assert_eq!(app.history_rows[0].count, 2, "persisted repeat folded in");
    assert!(!app.history_rows[1].session);
}

/// R84: a session row carries no store id, so the delete gesture declines it
/// (nothing to persist-remove); Esc/Enter/y still work. `Y` also copies, and
/// Enter recalls into the editor with the caret at the end.
#[test]
pub(crate) fn session_row_delete_declines_and_enter_recalls_to_end() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.push_session_run(session_run("SELECT 42", 3, true));
    app.history_open = true;
    recompute_history_view(&mut app);
    app.history_list.select(Some(0));
    assert!(app.history_rows[0].session);
    // Delete declines a session row instead of opening the red confirmation.
    history_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    assert!(
        app.history_confirm.is_none(),
        "no red confirm for a session row"
    );
    assert!(app.status.contains("内存"), "status: {}", app.status);
    // Enter recalls the statement and parks the caret at the end of the buffer.
    history_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(!app.history_open);
    assert_eq!(app.editor_sql(), "SELECT 42");
    let (row, col) = app.editor.cursor();
    assert_eq!(app.editor.lines().len(), 1);
    assert_eq!((row, col), (0, "SELECT 42".chars().count()));
}

/// R84: a `SessionRun` op result is the only writer of the session log, so the
/// panel refreshes live (and only the UI thread mutates the log).
#[test]
pub(crate) fn session_run_op_result_feeds_the_panel() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.history_open = true;
    apply_op_result(
        &mut app,
        OpResult::SessionRun(Box::new(session_run("SELECT 7", 2, true))),
        &tx,
    );
    assert_eq!(app.session_runs.len(), 1);
    assert_eq!(app.history_rows.len(), 1);
    assert!(app.history_rows[0].session);
    assert_eq!(app.history_view, vec![0], "the open panel sees it at once");
}

/// R84 (B): opening a table keeps updating the status in place, and now shows
/// the absolute row window so a deep page reads as the batch it is.
#[tokio::test(flavor = "multi_thread")]
async fn page_load_status_shows_the_row_window() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    let page_state = |page: usize, total: Option<u64>| PageState {
        table: "big".into(),
        schema: String::new(),
        table_type: None,
        page,
        page_size: 50,
        total,
        total_lower_bound: false,
        has_next: true,
        filter: String::new(),
        order_by: None,
        keyset: None,
    };
    app.page_state = Some(page_state(2, Some(1234)));
    spawn_table_page(&mut app, &tx, 2);
    assert!(app.status.contains("第 3 页"), "{}", app.status);
    assert!(app.status.contains("第 101-150 行"), "{}", app.status);
    // The last page clamps the window to the real total.
    app.page_state = Some(page_state(24, Some(1234)));
    spawn_table_page(&mut app, &tx, 24);
    assert!(app.status.contains("第 1201-1234 行"), "{}", app.status);
    // An empty table shows no bogus window.
    app.page_state = Some(page_state(0, Some(0)));
    spawn_table_page(&mut app, &tx, 0);
    assert!(app.status.contains("第 1 页"), "{}", app.status);
    assert!(!app.status.contains("行"), "{}", app.status);
}

// ── R59: SQL favourites (Ctrl-O list) ──

pub(crate) fn snippet_row(id: &str, label: &str, sql: &str) -> SnippetRow {
    SnippetRow {
        id: id.into(),
        label: label.into(),
        sql: sql.into(),
    }
}

/// R59: the `/` filter matches the label or the SQL text, case-insensitively,
/// and the list *is* the filter view (clearing the needle restores all rows).
#[test]
pub(crate) fn snippet_filter_is_case_insensitive_over_label_and_sql() {
    let mut app = test_app();
    app.snippets = vec![
        snippet_row("1", "users.sql", "SELECT * FROM users"),
        snippet_row("2", "Orders.sql", "delete from orders"),
        snippet_row("3", "misc.sql", "SELECT 1"),
    ];
    recompute_snippet_view(&mut app);
    assert_eq!(app.snippet_view, vec![0, 1, 2]);
    app.snippet_needle = "USERS".into();
    recompute_snippet_view(&mut app);
    assert_eq!(app.snippet_view, vec![0], "label match is case-insensitive");
    app.snippet_needle = "orders".into();
    recompute_snippet_view(&mut app);
    assert_eq!(app.snippet_view, vec![1], "sql text match");
    app.snippet_needle = "zzz".into();
    recompute_snippet_view(&mut app);
    assert!(app.snippet_view.is_empty());
    assert_eq!(app.snippet_list.selected(), None);
    app.snippet_needle.clear();
    recompute_snippet_view(&mut app);
    assert_eq!(app.snippet_view, vec![0, 1, 2]);
}

/// R59: the `/` filter state machine (open / narrow / keep / clear) and the
/// `d` delete confirmation (Esc cancels, Enter requests the delete).
#[test]
pub(crate) fn snippet_filter_state_machine_and_delete_confirm() {
    run_rt(|| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.snippets = vec![
            snippet_row("a", "alpha.sql", "SELECT alpha"),
            snippet_row("b", "beta.sql", "SELECT beta"),
        ];
        recompute_snippet_view(&mut app);
        app.snippet_open = true;
        let press = |app: &mut App, code: KeyCode| {
            snippet_key(app, &tx, KeyEvent::new(code, KeyModifiers::NONE));
        };
        press(&mut app, KeyCode::Char('/'));
        assert!(app.snippet_filter.is_some());
        for c in "beta".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.snippet_needle, "beta");
        assert_eq!(app.snippet_view, vec![1]);
        assert_eq!(app.snippet_list.selected(), Some(0));
        // Enter keeps the needle; Esc clears it and restores the full list.
        press(&mut app, KeyCode::Enter);
        assert!(app.snippet_filter.is_none());
        assert_eq!(app.snippet_needle, "beta");
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Esc);
        assert!(app.snippet_filter.is_none());
        assert_eq!(app.snippet_needle, "");
        assert_eq!(app.snippet_view, vec![0, 1]);

        // `d` opens a confirmation; Esc cancels without deleting anything.
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.snippet_confirm.as_deref(), Some("a"));
        press(&mut app, KeyCode::Esc);
        assert!(app.snippet_confirm.is_none());
        assert_eq!(app.snippets.len(), 2);

        // Enter on the confirmation hands the id to the backend op.
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Enter);
        assert!(app.snippet_confirm.is_none());
        assert!(app.status.contains("删除"), "status: {}", app.status);
    });
}

/// R59: Enter inserts the row under the cursor of the *filtered* view, not the
/// raw index 0.
#[test]
pub(crate) fn snippet_enter_inserts_the_filtered_selection() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.snippets = vec![
        snippet_row("a", "alpha.sql", "SELECT alpha"),
        snippet_row("b", "beta.sql", "SELECT beta"),
    ];
    app.snippet_needle = "beta".into();
    recompute_snippet_view(&mut app);
    app.snippet_open = true;
    snippet_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.editor_sql().contains("SELECT beta"));
    assert!(!app.snippet_open);
}

/// R59: the 100-item cap is a pure predicate so both save paths (the Ctrl-O
/// `s` prompt and Alt-S in the editor) refuse the 101st favourite.
#[test]
pub(crate) fn snippet_cap_refuses_the_101st_save() {
    assert_eq!(SNIPPET_LIMIT, 100);
    assert!(!snippet_limit_reached(0));
    assert!(!snippet_limit_reached(SNIPPET_LIMIT - 1));
    assert!(snippet_limit_reached(SNIPPET_LIMIT));
    assert!(snippet_limit_reached(SNIPPET_LIMIT + 1));
}

/// R59: `Alt-S` in the editor opens the favourite name prompt (the one-step
/// save); an empty editor refuses with a hint instead.
#[test]
pub(crate) fn editor_alt_s_opens_the_favourite_name_prompt() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.set_editor_text("SELECT * FROM users");
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT),
    );
    assert!(app.snippet_name.is_some());

    let mut empty = test_app();
    empty.selected = Some(test_conn("mysql"));
    empty.set_editor_text("   ");
    editor_key(
        &mut empty,
        &tx,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT),
    );
    assert!(empty.snippet_name.is_none());
}

// ── R71: built-in SQL template panel (`Alt-T` in the editor) ──

/// The pure scanner finds every `{{name}}` token and nothing else: an empty
/// `{{}}`, an unterminated `{{x` and a nested brace are not placeholders.
#[test]
pub(crate) fn editor_placeholders_scans_tokens_only() {
    let lines = vec![
        "SELECT {{col}} FROM {{table}}".to_string(),
        "WHERE a = '{{}}' OR b = '{{x'".to_string(),
    ];
    let phs = editor_placeholders(&lines);
    assert_eq!(phs.len(), 2);
    assert_eq!((phs[0].row, phs[0].col, phs[0].len), (0, 7, 7));
    assert_eq!((phs[1].row, phs[1].col, phs[1].len), (0, 20, 9));
}

/// `Alt-T` with the editor focused opens the template panel; with any other
/// pane focused it keeps its data-transfer role.
#[test]
pub(crate) fn editor_alt_t_opens_the_template_panel_only_in_the_editor() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.focus = Focus::Editor;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
    );
    assert!(app.template_open);
    assert!(app.transfer.is_none());

    let mut side = test_app();
    side.focus = Focus::Sidebar;
    key(
        &mut side,
        &tx,
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
    );
    assert!(!side.template_open);
}

/// Enter on a template inserts its text at the caret and selects the first
/// `{{…}}` placeholder, so the next keystroke replaces it; `template_active`
/// gates the Tab walk and the highlight.
#[test]
pub(crate) fn template_enter_inserts_and_selects_the_first_placeholder() {
    let mut app = test_app();
    open_template_panel(&mut app);
    // Move to the UPDATE template (index 2) and insert it.
    app.template_list.select(Some(2));
    template_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.editor_sql().contains("UPDATE {{table}}"));
    assert!(!app.template_open);
    assert!(app.template_active);
    // "UPDATE " is 7 chars, so `{{table}}` starts at col 7 and is 9 long.
    assert_eq!(app.template_ph_start, Some((0, 7)));
    assert_eq!(app.editor.selection_range(), Some(((0, 7), (0, 16))));
}

/// `Tab` walks the placeholders in order and wraps; once every `{{…}}` has been
/// replaced it falls back to the normal pane switch and drops the mode.
#[test]
pub(crate) fn template_tab_walks_placeholders_then_releases_the_key() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.set_editor_text("{{a}} {{b}}");
    app.template_active = true;
    app.focus = Focus::Editor;
    // Select the first, then Tab to the second, then wrap back to the first.
    select_placeholder(
        &mut app,
        Placeholder {
            row: 0,
            col: 0,
            len: 5,
        },
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    );
    assert_eq!(app.template_ph_start, Some((0, 6)));
    assert!(app.status.contains("2/2"), "status: {}", app.status);
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    );
    assert_eq!(app.template_ph_start, Some((0, 0)), "wraps to the first");

    // Fill both placeholders: Tab is a pane switch again and the mode is off.
    app.set_editor_text("x y");
    app.template_active = true;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
    );
    assert!(!app.template_active);
    assert!(app.focus == Focus::Preview);
}

/// The `/` filter narrows by label (English or Chinese) or SQL text,
/// case-insensitively, and the list *is* the filter view.
#[test]
pub(crate) fn template_filter_is_case_insensitive_over_label_and_sql() {
    let mut app = test_app();
    open_template_panel(&mut app);
    assert_eq!(app.template_view.len(), TEMPLATES.len());
    app.template_needle = "insert".into();
    recompute_template_view(&mut app);
    assert_eq!(app.template_view.len(), 1);
    assert_eq!(
        ui_text::t_lang(TEMPLATES[app.template_view[0]].label, ui_text::Lang::En),
        "INSERT"
    );
    app.template_needle = "truncate".into();
    recompute_template_view(&mut app);
    assert_eq!(app.template_view.len(), 1);
    app.template_needle = "zzz".into();
    recompute_template_view(&mut app);
    assert!(app.template_view.is_empty());
    assert_eq!(app.template_list.selected(), None);
    app.template_needle.clear();
    recompute_template_view(&mut app);
    assert_eq!(app.template_view.len(), TEMPLATES.len());
}

/// The `/` filter state machine: open, narrow, keep the needle on Enter, clear
/// it on Esc (restoring the full list).
#[test]
pub(crate) fn template_filter_state_machine() {
    let mut app = test_app();
    open_template_panel(&mut app);
    let press = |app: &mut App, code: KeyCode| {
        template_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    };
    press(&mut app, KeyCode::Char('/'));
    assert!(app.template_filter.is_some());
    for c in "drop".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.template_needle, "drop");
    assert_eq!(app.template_view.len(), 1);
    press(&mut app, KeyCode::Enter);
    assert!(app.template_filter.is_none());
    assert_eq!(app.template_needle, "drop");
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Esc);
    assert!(app.template_filter.is_none());
    assert_eq!(app.template_needle, "");
    assert_eq!(app.template_view.len(), TEMPLATES.len());
}

/// Every built-in template label has an English translation (so the panel is
/// bilingual) and its SQL is pure text with no query side effect.
#[test]
pub(crate) fn template_labels_are_translated() {
    for tpl in TEMPLATES {
        assert_ne!(
            ui_text::t_lang(tpl.label, ui_text::Lang::En),
            tpl.label,
            "missing English for {:?}",
            tpl.label
        );
        assert!(!tpl.sql.trim().is_empty());
    }
}

/// The `{{…}}` placeholders are painted into the editor cells (a distinct
/// background), and the highlight is gone once the token is replaced.
#[test]
pub(crate) fn template_placeholders_are_painted_then_cleared() {
    let mut app = test_app();
    app.picker_open = false;
    app.focus = Focus::Editor;
    open_template_panel(&mut app);
    app.template_list.select(Some(2)); // UPDATE
    template_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    let cell = |dx: u16| buf.cell((ed.x + 1 + dx, ed.y + 1)).unwrap();
    // "UPDATE " is 7 cells; the placeholder spans the next 9.
    assert_eq!(cell(7).bg, Color::LightMagenta, "placeholder start");
    assert_eq!(cell(15).bg, Color::LightMagenta, "placeholder end");
    assert_ne!(cell(0).bg, Color::LightMagenta, "plain SQL stays plain");

    // Replacing the last token retires the mode; nothing is painted after.
    app.set_editor_text("UPDATE users SET x = 1");
    app.template_active = true;
    sync_editor_template(&mut app);
    assert!(!app.template_active);
    let buf = draw_buffer(&mut app, 80, 30);
    let ed = app.rects.editor;
    assert_ne!(
        buf.cell((ed.x + 1, ed.y + 1)).unwrap().bg,
        Color::LightMagenta
    );
}

// ── R59: `}` / `{` non-blank row jump ──

pub(crate) fn blank_aware_grid() -> Grid {
    Grid {
        columns: vec!["id".into(), "note".into()],
        rows: vec![
            vec![Val::Text("1".into()), Val::Text("a".into())],
            vec![Val::Text("2".into()), Val::Null],
            vec![Val::Text("3".into()), Val::Text(String::new())],
            vec![Val::Text("4".into()), Val::Text("hi".into())],
            vec![Val::Text("5".into()), Val::Null],
            vec![Val::Text("6".into()), Val::Text("yo".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    }
}

/// R59: `}` / `{` walk to the next / previous non-blank cell in the focused
/// column, skipping NULL and empty strings, and the status line reports the
/// absolute row number.
#[test]
pub(crate) fn jump_nonblank_row_skips_null_and_empty_cells() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.set_grid(blank_aware_grid());
    app.col_cursor = 1;
    app.sel = 0;
    jump_nonblank_row(&mut app, 1);
    assert_eq!(app.sel, 3, "skips rows 1 (NULL) and 2 (empty)");
    assert!(app.status.contains("第 4 行"), "status: {}", app.status);
    jump_nonblank_row(&mut app, 1);
    assert_eq!(app.sel, 5);
    // Past the last non-blank row: the cursor holds and the status says so.
    jump_nonblank_row(&mut app, 1);
    assert_eq!(app.sel, 5);
    assert!(
        app.status.contains("下方没有非空单元格"),
        "status: {}",
        app.status
    );
    // `{` walks back through the same gaps.
    jump_nonblank_row(&mut app, -1);
    assert_eq!(app.sel, 3);
    jump_nonblank_row(&mut app, -1);
    assert_eq!(app.sel, 0);
    jump_nonblank_row(&mut app, -1);
    assert_eq!(app.sel, 0);
    assert!(
        app.status.contains("上方没有非空单元格"),
        "status: {}",
        app.status
    );
}

/// R59: a blank focused column (every cell NULL) has nowhere to jump, so the
/// motion is a no-op with a hint rather than a silent freeze.
#[test]
pub(crate) fn jump_nonblank_row_reports_an_all_blank_column() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    let mut grid = blank_aware_grid();
    for r in &mut grid.rows {
        r[1] = Val::Null;
    }
    app.set_grid(grid);
    app.col_cursor = 1;
    app.sel = 0;
    jump_nonblank_row(&mut app, 1);
    assert_eq!(app.sel, 0);
    assert!(
        app.status.contains("下方没有非空单元格"),
        "status: {}",
        app.status
    );
}

/// R59: the `}` / `{` keys are actually wired into the results-pane keymap
/// (not just the helper), and `n` / `p` keep their page-turn meaning.
#[test]
pub(crate) fn preview_key_binds_brace_jump() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.set_grid(blank_aware_grid());
    app.col_cursor = 1;
    app.sel = 0;
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('}'), KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 3);
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('{'), KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 0);
}

#[test]
pub(crate) fn size_and_row_labels_are_human_readable() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(1024), "1.0 KB");
    assert_eq!(
        human_bytes(2 * 1024 * 1024 * 1024 + 100 * 1024 * 1024),
        "2.1 GB"
    );
    assert_eq!(human_count(12), "12");
    assert_eq!(human_count(1200), "1.2k");
    assert_eq!(human_count(3_400_000), "3.4M");
}

#[test]
pub(crate) fn parse_db_size_info_sums_bytes_and_keeps_rows() {
    let rows = vec![
        vec![
            serde_json::json!("users"),
            serde_json::json!(10),
            serde_json::json!(1024),
        ],
        vec![
            serde_json::json!("orders"),
            serde_json::json!("250"),
            serde_json::json!("2048"),
        ],
    ];
    let info = parse_db_size_info(&rows);
    assert_eq!(info.total_bytes, Some(3072));
    assert_eq!(info.rows.get("users"), Some(&10));
    assert_eq!(info.rows.get("orders"), Some(&250));
    assert_eq!(info.sizes.get("users"), Some(&1024));
}

#[test]
pub(crate) fn connection_copy_name_is_unique() {
    assert_eq!(copy_connection_name("prod", &[]), "prod-copy");
    assert_eq!(
        copy_connection_name("prod", &["prod".to_string()]),
        "prod-copy"
    );
    assert_eq!(
        copy_connection_name("prod", &["prod".to_string(), "prod-copy".to_string()]),
        "prod-copy-2"
    );
    assert_eq!(
        copy_connection_name(
            "prod",
            &["prod-copy".to_string(), "prod-copy-2".to_string()]
        ),
        "prod-copy-3"
    );
}

#[test]
pub(crate) fn history_origin_and_duration_labels() {
    assert_eq!(
        history_origin_from_details(Some("{\"dbxt_origin\":\"direct\"}")),
        "direct"
    );
    assert_eq!(history_origin_from_details(Some("not json")), "");
    assert_eq!(history_origin_from_details(None), "");
    assert_eq!(history_duration_label(12), "12ms");
    assert_eq!(history_duration_label(1234), "1.2s");
}

#[test]
pub(crate) fn tree_size_column_respects_narrow_and_wide() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.databases = vec!["shop".into()];
    app.db_index = 0;
    app.tables = vec![TableInfo {
        name: "orders".into(),
        table_type: "TABLE".into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    }];
    let info = DbSizeInfo {
        total_bytes: Some(2 * 1024 * 1024 * 1024 + 100 * 1024 * 1024),
        rows: std::collections::HashMap::from([("orders".to_string(), 1200)]),
        sizes: std::collections::HashMap::new(),
    };
    app.db_sizes.insert("shop".into(), info);
    let db_row = SideRow::Db {
        idx: 0,
        db: "shop".into(),
        depth: 1,
    };
    let table_row = SideRow::Table {
        idx: 0,
        table: 0,
        depth: 2,
    };
    app.term_w = 42;
    assert_eq!(
        side_row_size(&app, &db_row),
        None,
        "hidden below 56 columns"
    );
    assert_eq!(side_row_size(&app, &table_row), None);
    app.term_w = 80;
    assert_eq!(side_row_size(&app, &db_row), Some("2.1 GB".into()));
    assert_eq!(side_row_size(&app, &table_row), Some("1.2k".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn sidebar_s_on_db_row_requests_size_else_sorts() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.connections = vec![app.selected.clone().unwrap()];
    app.databases = vec!["shop".into()];
    app.db_index = 0;
    app.focus = Focus::Sidebar;
    rebuild_side_rows(&mut app);
    app.side_sel = app
        .side_rows
        .iter()
        .position(|r| matches!(r, SideRow::Db { .. }))
        .expect("a database row is visible");
    sidebar_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    assert!(matches!(
        app.db_size_state.get("shop"),
        Some(TreeDbState::Loading)
    ));
    // On a connection row, `s` keeps its old job: cycle the table sort.
    app.side_sel = 0;
    let before = app.table_sort;
    sidebar_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    assert_ne!(app.table_sort, before);
}

#[test]
pub(crate) fn watchdog_tiers_are_bounded() {
    // Metadata / Redis / Mongo calls must never spin forever...
    assert_eq!(Op::ListConnections.watchdog(), OP_WATCHDOG_FALLBACK);
    // ...but a (possibly multi-statement) SQL script gets a wider ceiling.
    let cfg = new_connection_config(
        "t".into(),
        "t".into(),
        parse_database_type("sqlite").unwrap(),
        String::new(),
        0,
        String::new(),
        String::new(),
        None,
        false,
        None,
    )
    .unwrap();
    let q = Op::Query(
        Box::new(cfg.clone()),
        "db".into(),
        "SELECT 1".into(),
        QUERY_MAX_ROWS,
        "editor",
        0,
    );
    assert_eq!(q.watchdog(), OP_WATCHDOG_SQL);
    assert!(OP_WATCHDOG_FALLBACK < OP_WATCHDOG_SQL);
    // Every R30–R35 long-running task has its own generous tier instead of
    // silently falling back to the 60 s default.
    let cancel = Arc::new(AtomicBool::new(false));
    let export = Op::Export(Box::new(ExportJob {
        format: ExportFormat::Csv,
        path: std::env::temp_dir().join("dbxt-watchdog.csv"),
        grid: Grid::default(),
        cfg: None,
        schema: String::new(),
        table: String::new(),
        types: Vec::new(),
    }));
    assert_eq!(export.watchdog(), OP_WATCHDOG_EXPORT);
    let import = Op::Import(Box::new(ImportJob {
        cfg: Box::new(cfg.clone()),
        db: "db".into(),
        schema: String::new(),
        table: "t".into(),
        columns: Vec::new(),
        rows: Vec::new(),
        mode: ImportMode::Append,
        on_error: ImportOnError::Stop,
    }));
    assert_eq!(import.watchdog(), OP_WATCHDOG_IMPORT);
    let search = Op::GlobalSearch {
        cfg: Box::new(cfg.clone()),
        db: "db".into(),
        schema: String::new(),
        needle: "x".into(),
        scan_limit: 1000,
        max_rows: 1_000_000,
        gen: 0,
        cancel: cancel.clone(),
    };
    assert_eq!(search.watchdog(), OP_WATCHDOG_SEARCH);
    let data_diff = Op::DataDiff {
        src_cfg: Box::new(cfg.clone()),
        src_db: "db".into(),
        src_schema: String::new(),
        src_table: "a".into(),
        tgt_cfg: Box::new(cfg.clone()),
        tgt_db: "db".into(),
        tgt_schema: String::new(),
        tgt_table: "b".into(),
        where_input: String::new(),
        gen: 0,
        cancel: cancel.clone(),
    };
    assert_eq!(data_diff.watchdog(), OP_WATCHDOG_DATA_DIFF);
    let transfer = Op::DataTransfer(Box::new(TransferJob {
        src_cfg: Box::new(cfg.clone()),
        src_db: "db".into(),
        src_schema: String::new(),
        src_table: "a".into(),
        tgt_cfg: Box::new(cfg),
        tgt_db: "db".into(),
        tgt_schema: String::new(),
        tgt_table: "b".into(),
        mode: TransferMode::CreateAndCopy,
        conflict: TransferConflict::Stop,
        on_error: TransferOnError::Stop,
        where_input: String::new(),
        limit: None,
        with_indexes: true,
        with_auto_increment: true,
        allow_large: false,
        gen: 0,
        cancel,
    }));
    assert_eq!(transfer.watchdog(), OP_WATCHDOG_TRANSFER);
}

#[test]
pub(crate) fn help_overlay_widens_on_large_screens_and_falls_back_on_narrow_ones() {
    // Wide: cap raised from 64 to 96, leaving a 2-column gutter on each side.
    assert_eq!(help_overlay_width(200), 96);
    assert_eq!(help_overlay_width(100), 96);
    assert_eq!(help_overlay_width(80), 76);
    assert_eq!(help_overlay_width(64), 60);
    // Narrow: under 34 columns the old whole-area fallback still applies.
    assert_eq!(help_overlay_width(33), 33);
    assert_eq!(help_overlay_width(30), 30);
    assert_eq!(help_overlay_width(24), 24);
    assert_eq!(help_overlay_width(10), 10);
    // Just past the fallback the gutter applies again.
    assert_eq!(help_overlay_width(34), 30);
    // The key column grows only where there is room for it.
    assert_eq!(help_key_width(96), 24);
    assert_eq!(help_key_width(72), 24);
    assert_eq!(help_key_width(71), 16);
    assert_eq!(help_key_width(60), 16);
    // ...and never overflows a tiny box.
    assert_eq!(help_key_width(20), 14);
    assert_eq!(help_key_width(8), 2);
}

#[test]
pub(crate) fn help_overlay_has_a_section_spacer_for_every_group_after_the_first() {
    // Rendering inserts a blank line before each group header (empty
    // description) except the very first, so the long list stays scannable.
    let groups = HELP_ROWS.iter().filter(|(_, d)| d.is_empty()).count();
    assert!(groups > 5, "expected several help sections, found {groups}");
    let lines = help_overlay_lines(24);
    assert_eq!(
        lines.len(),
        HELP_ROWS.len() + (groups - 1),
        "help line count should be the rows plus one spacer per extra group"
    );
    // Exactly one blank spacer line per group beyond the first.
    let blank = lines.iter().filter(|l| l.spans.is_empty()).count();
    assert_eq!(blank, groups - 1);
}

#[test]
pub(crate) fn help_documents_core_bindings() {
    // Guard against the overlay drifting away from the real keymap: every
    // binding a user is likely to reach for must stay documented.
    let keys: Vec<&str> = HELP_ROWS.iter().map(|(k, _)| *k).collect();
    for needle in [
        "Home / End",
        "Ctrl-E",
        "Ctrl-O",
        "Ctrl-P",
        "Ctrl-N",
        "Ctrl-Y",
        "Ctrl-R",
        "Ctrl-K",
        "Ctrl-D",
        "Ctrl-V",
        "Ctrl-T",
        "Alt-/",
        "Alt-G",
        "Alt-H",
        "F1",
        "Alt-F",
        "Alt-L",
        "Alt-D",
        "Shift+Alt-D",
        "Alt-K",
        "Alt-T",
        "Alt-1..9",
        "Alt-Tab / Alt-`",
        "Alt-Enter",
        "Alt-P",
        "Ctrl-F",
        "F3 / Shift-F3 · Alt-N / Alt-B",
    ] {
        assert!(
            keys.iter().any(|k| k.contains(needle)),
            "help is missing {needle}"
        );
    }
    // The connection-picker and editor Esc sections are documented too.
    assert!(keys.contains(&"— 连接选择 —"));
    assert!(HELP_ROWS.iter().any(|(_, d)| *d == "回到侧栏"));
    // CSV import has a documented sidebar binding.
    assert!(
        keys.contains(&"I"),
        "help is missing the CSV import binding"
    );
}

#[test]
pub(crate) fn help_has_no_bare_uppercase_shortcuts() {
    // Regression guard for the R8 keymap: every shortcut must be lowercase,
    // a named key, or a Ctrl/Alt/Shift/F-key combination — never a lone
    // uppercase letter the user has to reach with Shift. `I` (CSV import),
    // `G` (vim's go-to-bottom, R42), `Y` (copy connection, R45, which
    // mirrors the vim-ish uppercase twin of the picker's `p`) and `V` (R57
    // linewise row-select, whose lowercase `v` is the cell popup) are the
    // deliberate exceptions. `J` (R74, the pretty-JSON toggle inside the cell
    // popup) joins them: its lowercase `j` is the popup's scroll-down. `T`
    // (R81, set the focused key's TTL from the key list) joins too: its
    // lowercase `t` cycles the client-side type filter. `L` (R83, quick-open a
    // SQLite file) joins: a rare, deliberate action like `I`, and its lowercase
    // `l` is the pane's expand / next-column key. `U` (R89, cycle the cell
    // popup's Unicode view: raw / decoded / re-escaped) joins: its lowercase
    // `u` is not bound there, but the uppercase keeps the three-way conversion
    // gesture visually distinct from the `y`/`Y` copy pair. `S` (R94, the
    // status-bar numeric-summary toggle) joins: its lowercase `s` is the result
    // grid's sort, so the uppercase carries the deliberate, opt-in summary.
    // `E` (R100, export the data dictionary from the connection tree) joins: the
    // picker's lowercase `e` edits a connection, and `Alt-E` is the unrelated
    // connection-bundle export, so the plain uppercase carries the new gesture.
    // `M` (R104, sample the loaded Redis keys' memory) joins: its lowercase `m`
    // is the key list's prefix-rename, so the uppercase carries the new sample.
    for (key, _) in HELP_ROWS {
        if key.starts_with('—') {
            continue;
        }
        for tok in key.split(['/', ' ', '+']).filter(|t| !t.is_empty()) {
            if tok == "I"
                || tok == "G"
                || tok == "Y"
                || tok == "V"
                || tok == "J"
                || tok == "T"
                || tok == "L"
                || tok == "U"
                || tok == "S"
                || tok == "E"
                || tok == "M"
                // `D` (R107, the complete-DDL export) joins: its lowercase `d`
                // is the results pane's delete-row / generate-DELETE, so the
                // uppercase carries the deliberate export gesture.
                || tok == "D"
                // `A` (R108, the all-tabs multi-sheet export) joins: it lives
                // only inside the `Ctrl-Y` picker (never a global key), and its
                // lowercase `a` is unused there, so the uppercase labels the
                // deliberate all-tabs gesture next to `S` (the SQL-zip one).
                || tok == "A"
                // `F` (R112, freeze the first column) joins: it only ever
                // appears as the `g F` chord (Ctrl-F is page-forward), and its
                // lowercase `g f` freezes the focused column.
                || tok == "F"
            {
                continue;
            }
            assert!(
                !(tok.len() == 1 && tok.chars().all(|c| c.is_ascii_uppercase())),
                "bare uppercase shortcut in help: {tok:?} ({key})"
            );
        }
    }
}

/// R60: the `/` filter matches a row's keycap or its description, in either
/// language, case-insensitively — and rejects a genuine miss.
#[test]
pub(crate) fn help_filter_matches_keys_and_features_in_both_languages() {
    // Feature-name match (Chinese).
    assert!(help_needle_matches(
        "收藏",
        "f",
        "收藏 / 取消收藏该条（同一 DBX saved_sql_files 存储）"
    ));
    // Feature-name match (English) against the same row's translation.
    assert!(help_needle_matches(
        "unfavorite",
        "f",
        "收藏 / 取消收藏该条（同一 DBX saved_sql_files 存储）"
    ));
    // Keycap match, case-insensitive.
    assert!(help_needle_matches("ctrl-y", "Ctrl-Y", "导出当前结果"));
    // Empty needle matches everything.
    assert!(help_needle_matches("", "y", "复制当前行"));
    // A miss stays a miss.
    assert!(!help_needle_matches("zzz-no-such", "y", "复制当前行"));
}

/// R60: a filter result floats the surface's own keys to the top. The sort
/// is a stable bucket, so once an irrelevant row appears none may follow.
#[test]
pub(crate) fn help_filter_floats_context_keys_first() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.picker_open = false;
    app.focus = Focus::Preview;
    app.help_needle = "复制".into();
    let rows = help_rows(&app);
    assert!(!rows.is_empty(), "expected matches for 复制");
    assert!(
        rows.iter().all(|r| matches!(r, HelpRow::Item { .. })),
        "a filtered view drops section headers"
    );
    let mut seen_irrelevant = false;
    for r in &rows {
        if let HelpRow::Item { relevant, .. } = r {
            if *relevant {
                assert!(
                    !seen_irrelevant,
                    "a relevant row followed an irrelevant one"
                );
            } else {
                seen_irrelevant = true;
            }
        }
    }
}

/// R60: typing a section name (e.g. 结果) surfaces that whole group, even for
/// rows whose key/description do not contain the word themselves.
#[test]
pub(crate) fn help_filter_can_name_a_whole_section() {
    let mut app = test_app();
    app.help_needle = "结果（表格浏览）".into();
    let rows = help_rows(&app);
    let keys: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            HelpRow::Item { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    assert!(
        keys.contains(&"大表翻页"),
        "section match should include rows without the literal word: {keys:?}"
    );
}

/// R60: an empty filter leads with a `— 当前上下文 —` quick-access section
/// built from the surface under the overlay.
#[test]
pub(crate) fn help_without_a_filter_leads_with_the_current_context() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.picker_open = false;
    app.focus = Focus::Preview;
    let rows = help_rows(&app);
    assert_eq!(rows.first(), Some(&HelpRow::Section("— 当前上下文 —")));
    // The results keys the spec calls out must appear in that first block.
    let quick: Vec<&str> = rows
        .iter()
        .take_while(|r| !matches!(r, HelpRow::Blank))
        .filter_map(|r| match r {
            HelpRow::Item { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    let quick_tokens: Vec<String> = quick.iter().flat_map(|k| help_key_tokens(k)).collect();
    for needle in ["y", "v", "*"] {
        assert!(
            quick_tokens.contains(&needle.to_string()),
            "context block is missing {needle:?}: {quick:?}"
        );
    }
}

/// R60: wide overlays pack two rows per line; narrow ones use one column.
#[test]
pub(crate) fn help_switches_between_two_columns_and_one_on_narrow_screens() {
    assert!(help_two_columns(56));
    assert!(help_two_columns(96));
    assert!(!help_two_columns(55));
    assert!(!help_two_columns(40));
    let rows = help_grouped_rows(&[]);
    let single = help_lines_single(&rows, 24);
    let double = help_lines_two_col(&rows, 16, 24);
    assert_eq!(single.len(), rows.len(), "one line per row in a column");
    assert!(
        double.len() < single.len(),
        "two columns should pack more rows per line"
    );
    // The two-column key width never eats the whole cell.
    assert!(help_two_col_key_width(24, 24) <= 24);
    assert!(help_two_col_key_width(24, 10) < 10);
}

/// R60: `/` inside the full help opens the filter; Enter keeps the needle,
/// Esc clears it, and closing the sheet resets everything.
#[test]
pub(crate) fn help_filter_state_machine() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.help_open = true;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert!(app.help_filter.is_some(), "`/` opens the filter input");
    for c in "favourite".chars() {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.help_needle, "favourite");
    // Enter keeps the needle and closes the input.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.help_filter.is_none());
    assert_eq!(app.help_needle, "favourite");
    // Esc inside the input clears the needle.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.help_filter.is_none());
    assert!(app.help_needle.is_empty());
    // Closing the sheet resets the filter state.
    app.help_needle = "y".into();
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.help_open);
    assert!(app.help_needle.is_empty());
}

/// R60: an active sidebar / result filter counts as unrun work, so `q`
/// arms the two-stage quit instead of quitting outright.
#[test]
pub(crate) fn quit_guard_counts_active_filters_as_unrun_work() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    assert!(!quit_has_unsaved(&app));
    app.result_needle = "foo".into();
    assert!(quit_has_unsaved(&app));
    app.result_needle.clear();
    app.table_filter = "bar".into();
    assert!(quit_has_unsaved(&app));
}

#[test]
pub(crate) fn explain_uses_dialect_prefix() {
    assert_eq!(
        explain_sql_for("mysql", "SELECT 1;").as_deref(),
        Some("EXPLAIN SELECT 1")
    );
    assert_eq!(
        explain_sql_for("postgres", "select * from t").as_deref(),
        Some("EXPLAIN select * from t")
    );
    // A write statement still becomes a plain EXPLAIN: PostgreSQL's
    // `EXPLAIN ANALYZE` would actually run the INSERT/UPDATE/DELETE.
    assert_eq!(
        explain_sql_for("postgres", "INSERT INTO t (a) VALUES (1);").as_deref(),
        Some("EXPLAIN INSERT INTO t (a) VALUES (1)")
    );
    assert_eq!(
        explain_sql_for("postgres", "DELETE FROM t WHERE id = 1;").as_deref(),
        Some("EXPLAIN DELETE FROM t WHERE id = 1")
    );
    assert_eq!(
        explain_sql_for("sqlite", "SELECT 1").as_deref(),
        Some("EXPLAIN QUERY PLAN SELECT 1")
    );
    // Unknown / unsupported engines return None instead of a broken statement.
    assert!(explain_sql_for("sqlserver", "SELECT 1").is_none());
    assert!(explain_sql_for("oracle", "SELECT 1").is_none());
    assert!(explain_sql_for("mysql", "   ;").is_none());
}

#[test]
pub(crate) fn csv_quoting_follows_rfc4180() {
    assert_eq!(csv_field("plain"), "plain");
    assert_eq!(csv_field("a,b"), "\"a,b\"");
    assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
    assert_eq!(csv_field("a\nb"), "\"a\nb\"");
}

#[test]
pub(crate) fn grid_to_csv_keeps_null_empty() {
    let grid = Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec![Val::Text("1".into()), Val::Null],
            vec![Val::Text("2".into()), Val::Text("a,b".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    assert_eq!(grid_to_csv(&grid), "id,name\n1,\n2,\"a,b\"\n");
}

// ── R22: CSV import ──

pub(crate) fn col_info(name: &str, ty: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        ..Default::default()
    }
}

pub(crate) fn mysql_cfg() -> ConnectionConfig {
    new_connection_config(
        "t".into(),
        "t".into(),
        parse_database_type("mysql").unwrap(),
        "127.0.0.1".into(),
        13306,
        "root".into(),
        String::new(),
        Some("shop".into()),
        false,
        None,
    )
    .unwrap()
}

#[test]
pub(crate) fn parse_csv_handles_quotes_commas_and_newlines() {
    let rows = parse_csv(
        "a,b\n\"x,1\",\"he said \"\"hi\"\"\"\n\"multi\nline\",2\n",
        ',',
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], vec!["a", "b"]);
    assert_eq!(rows[1], vec!["x,1", "he said \"hi\""]);
    assert_eq!(rows[2], vec!["multi\nline", "2"]);
}

#[test]
pub(crate) fn parse_csv_strips_bom_blank_lines_and_crlf() {
    let rows = parse_csv("\u{feff}a,b\r\n1,2\r\n\r\n", ',');
    assert_eq!(rows, vec![vec!["a", "b"], vec!["1", "2"]]);
    // A trailing delimiter keeps the empty field.
    assert_eq!(parse_csv("a,\n", ','), vec![vec!["a", ""]]);
}

#[test]
pub(crate) fn detect_delimiter_prefers_the_densest() {
    assert_eq!(detect_delimiter("a,b,c\n1,2,3"), ',');
    assert_eq!(detect_delimiter("a;b;c\n1;2;3"), ';');
    assert_eq!(detect_delimiter("a\tb\tc"), '\t');
    // Quoted separators do not count.
    assert_eq!(detect_delimiter("\"a,b\";\"c,d\";e"), ';');
    assert_eq!(detect_delimiter("single"), ',');
}

#[test]
pub(crate) fn gbk_bytes_decode_to_chinese_text() {
    let (bytes, _, _) = encoding_rs::GB18030.encode("编号,名称\n1,北京\n");
    let (text, enc) = decode_csv_bytes(&bytes);
    assert_eq!(enc, "GB18030/GBK");
    assert!(text.contains("北京"), "{text}");
    let (utf, enc2) = decode_csv_bytes("名称\n中文\n".as_bytes());
    assert_eq!(enc2, "UTF-8");
    assert!(utf.contains("中文"));
}

#[test]
pub(crate) fn type_inference_covers_common_shapes() {
    assert_eq!(infer_col_type(&["1", "2", "-3"]), ColType::Int);
    assert_eq!(infer_col_type(&["1.5", "2", "-0.25"]), ColType::Float);
    assert_eq!(infer_col_type(&["true", "FALSE"]), ColType::Bool);
    assert_eq!(infer_col_type(&["2026-06-27", "1999-01-02"]), ColType::Date);
    assert_eq!(
        infer_col_type(&["2026-06-27 10:00", "1999-01-02 23:59:59"]),
        ColType::DateTime
    );
    assert_eq!(infer_col_type(&["abc", "1"]), ColType::Text);
    // Empty values are ignored; an all-empty column is text.
    assert_eq!(infer_col_type(&["", "42", " "]), ColType::Int);
    assert_eq!(infer_col_type(&["", ""]), ColType::Text);
}

#[test]
pub(crate) fn header_alignment_matches_missing_and_extra() {
    let headers = vec!["ID".to_string(), "name".to_string(), "junk".to_string()];
    let rows = vec![vec!["1".to_string(), "Ada".to_string(), "x".to_string()]];
    let cols = vec![
        col_info("id", "int"),
        col_info("name", "varchar(20)"),
        col_info("age", "int"),
    ];
    let (mapped, extra, missing) = align_import_columns(&headers, &rows, &cols);
    assert_eq!(mapped.len(), 3);
    assert_eq!(mapped[0].src, Some(0));
    assert_eq!(mapped[0].ty, ColType::Int);
    assert_eq!(mapped[1].src, Some(1));
    assert_eq!(mapped[1].ty, ColType::Text);
    assert_eq!(mapped[2].src, None);
    assert_eq!(extra, vec!["junk"]);
    assert_eq!(missing, vec!["age"]);
}

#[test]
pub(crate) fn import_literal_nulls_bools_and_quotes() {
    assert_eq!(import_literal("", ColType::Text, "varchar(10)"), "NULL");
    assert_eq!(import_literal("42", ColType::Int, "int"), "42");
    assert_eq!(import_literal("3.5", ColType::Float, "double"), "3.5");
    assert_eq!(import_literal("true", ColType::Bool, "tinyint"), "TRUE");
    assert_eq!(import_literal("FALSE", ColType::Bool, "tinyint"), "FALSE");
    assert_eq!(
        import_literal("O'Brien", ColType::Text, "text"),
        "'O''Brien'"
    );
    // A numeric target keeps a number bare even when inference said text.
    assert_eq!(import_literal("42", ColType::Text, "int"), "42");
    assert_eq!(
        import_literal("2026-06-27", ColType::Date, "date"),
        "'2026-06-27'"
    );
}

#[test]
pub(crate) fn import_insert_sql_uses_only_present_columns() {
    let cfg = mysql_cfg();
    let cols = vec![
        ImportCol {
            name: "id".into(),
            src: Some(0),
            ty: ColType::Int,
            data_type: "int".into(),
        },
        ImportCol {
            name: "name".into(),
            src: Some(1),
            ty: ColType::Text,
            data_type: "varchar(20)".into(),
        },
        ImportCol {
            name: "age".into(),
            src: None,
            ty: ColType::Text,
            data_type: "int".into(),
        },
    ];
    let sql = import_insert_sql(&cfg, "", "users", &cols, &["7".into(), "Ada".into()]);
    assert_eq!(sql, "INSERT INTO `users` (`id`, `name`) VALUES (7, 'Ada');");
}

#[test]
pub(crate) fn import_chunks_split_at_the_chunk_size() {
    let rows: Vec<usize> = (0..(IMPORT_CHUNK * 2 + 3)).collect();
    let chunks = import_chunks(&rows);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), IMPORT_CHUNK);
    assert_eq!(chunks[1].len(), IMPORT_CHUNK);
    assert_eq!(chunks[2].len(), 3);
}

#[test]
pub(crate) fn import_error_row_reads_the_statement_index() {
    assert_eq!(
        import_row_of_error(1000, "Statement 3 failed: Duplicate entry", 500),
        1003
    );
    // Unparseable / out-of-range errors fall back to the chunk start.
    assert_eq!(import_row_of_error(1000, "boom", 500), 1001);
    assert_eq!(import_row_of_error(1000, "Statement 999 failed", 500), 1001);
}

// ── R22: export formats ──

#[test]
pub(crate) fn json_export_keeps_leading_zero_strings() {
    let grid = Grid {
        columns: vec!["zip".into(), "n".into(), "ok".into(), "nil".into()],
        rows: vec![vec![
            Val::Text("0123".into()),
            Val::Text("42".into()),
            Val::Text("true".into()),
            Val::Null,
        ]],
        note: String::new(),
        types: Vec::new(),
    };
    let out = grid_to_json_array(&grid);
    assert!(out.contains("\"zip\": \"0123\""), "{out}");
    assert!(out.contains("\"n\": 42"), "{out}");
    assert!(out.contains("\"ok\": true"), "{out}");
    assert!(out.contains("\"nil\": null"), "{out}");
    let nd = grid_to_json_ndjson(&grid);
    assert_eq!(nd.lines().count(), 1);
    assert!(nd.starts_with('{') && nd.trim_end().ends_with('}'));
}

#[test]
pub(crate) fn markdown_export_escapes_pipes_and_newlines() {
    let grid = Grid {
        columns: vec!["a".into(), "b".into()],
        rows: vec![
            vec![Val::Text("x|y".into()), Val::Null],
            vec![Val::Text("l1\nl2".into()), Val::Text("ok".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    let md = grid_to_markdown(&grid);
    assert!(md.starts_with("| a | b |\n| --- | --- |\n"), "{md}");
    assert!(md.contains("| x\\|y | NULL |"), "{md}");
    assert!(md.contains("| l1<br>l2 | ok |"), "{md}");
}

#[test]
pub(crate) fn batch_insert_groups_rows() {
    let cfg = mysql_cfg();
    let columns = vec!["id".to_string(), "name".to_string()];
    let types = vec![Some("int".to_string()), Some("varchar(20)".to_string())];
    let rows = vec![
        vec![Val::Text("1".into()), Val::Text("a".into())],
        vec![Val::Text("2".into()), Val::Null],
        vec![Val::Text("3".into()), Val::Text("c".into())],
    ];
    let out = batch_insert_sql(&cfg, "", "t", &columns, &rows, &types, 2);
    assert_eq!(
            out,
            "INSERT INTO `t` (`id`, `name`) VALUES\n(1, 'a'),\n(2, NULL);\nINSERT INTO `t` (`id`, `name`) VALUES\n(3, 'c');\n"
        );
}

#[test]
pub(crate) fn connection_colour_prefers_explicit_hex() {
    assert_eq!(parse_hex_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
    assert_eq!(parse_hex_color("00ff00"), Some(Color::Rgb(0, 255, 0)));
    assert_eq!(parse_hex_color("not-a-colour"), None);
    assert_eq!(parse_hex_color("#fff"), None);
    // Families are distinct so a picker row is identifiable at a glance.
    assert_ne!(db_type_color("mysql"), db_type_color("redis"));
    assert_ne!(db_type_color("mongodb"), db_type_color("redis"));
}

#[test]
pub(crate) fn colour_palette_round_trips_and_wraps() {
    // `color_sel` maps an unset colour to the "none" stop, an exact preset
    // (case-insensitively) to its index, and anything else to custom.
    assert_eq!(color_sel_for(""), 0);
    assert_eq!(color_sel_for("  "), 0);
    assert_eq!(color_sel_for("#e06c75"), 1);
    assert_eq!(color_sel_for("#E06C75"), 1);
    assert_eq!(color_sel_for("#123456"), CONN_COLOR_CUSTOM);
    // …and back again for the value behind each stop.
    assert_eq!(color_value_for(0, "#123456"), "");
    assert_eq!(color_value_for(1, ""), "#e06c75");
    assert_eq!(color_value_for(CONN_COLOR_CUSTOM, "#123456"), "#123456");
    // Space wraps none → presets → custom → none.
    let mut sel = 0usize;
    for _ in 0..CONN_COLOR_STOPS {
        sel = color_next_sel(sel);
    }
    assert_eq!(sel, 0);
    assert_eq!(color_next_sel(CONN_COLOR_PRESETS.len()), CONN_COLOR_CUSTOM);
    assert_eq!(color_next_sel(CONN_COLOR_CUSTOM), 0);
}

#[test]
pub(crate) fn connection_colour_normalises_to_lowercase_hex() {
    assert_eq!(normalize_conn_color(""), Ok(None));
    assert_eq!(normalize_conn_color("  "), Ok(None));
    assert_eq!(normalize_conn_color("00FF00"), Ok(Some("#00ff00".into())));
    assert_eq!(normalize_conn_color("#ABCDEF"), Ok(Some("#abcdef".into())));
    assert_eq!(normalize_conn_color("nope"), Err(()));
    assert_eq!(normalize_conn_color("#fff"), Err(()));
}

#[test]
pub(crate) fn form_from_connection_preserves_colour() {
    let mut cfg = test_conn("mysql");
    cfg.color = Some("#E06C75".into());
    // Editing keeps the id and the colour; duplicating keeps the colour too.
    let edit = form_from_connection(&cfg, cfg.name.clone(), Some(cfg.id.clone()));
    assert_eq!(edit.color, "#E06C75");
    assert_eq!(edit.color_sel, 1);
    let dup = form_from_connection(&cfg, "copy".into(), None);
    assert_eq!(dup.color, "#E06C75");
    assert_eq!(dup.edit_id, None);
    // A free-form hex lands on the custom stop and survives the round-trip.
    cfg.color = Some("#123456".into());
    let custom = form_from_connection(&cfg, cfg.name.clone(), None);
    assert_eq!(custom.color_sel, CONN_COLOR_CUSTOM);
    assert_eq!(
        normalize_conn_color(&custom.color).unwrap().as_deref(),
        Some("#123456")
    );
}

pub(crate) fn conn_for_sort(
    id: &str,
    name: &str,
    db_type: &str,
    color: Option<&str>,
) -> ConnectionConfig {
    let mut c = test_conn(db_type);
    c.id = id.into();
    c.name = name.into();
    c.color = color.map(str::to_string);
    c
}

#[test]
pub(crate) fn connection_sort_covers_name_type_and_colour() {
    let mut list = vec![
        conn_for_sort("a", "prod-red", "mysql", Some("#e06c75")),
        conn_for_sort("b", "alpha", "redis", Some("#61afef")),
        conn_for_sort("c", "beta", "mysql", None),
        conn_for_sort("d", "prod-red-2", "postgres", Some("#e06c75")),
    ];
    let names = |l: &[ConnectionConfig]| l.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    sort_connection_list(&mut list, ConnSort::Name);
    assert_eq!(names(&list), ["alpha", "beta", "prod-red", "prod-red-2"]);
    sort_connection_list(&mut list, ConnSort::Type);
    assert_eq!(names(&list), ["beta", "prod-red", "prod-red-2", "alpha"]);
    // Colour mode groups the two reds together; uncoloured ones follow by
    // family so the default badge colours still line up.
    sort_connection_list(&mut list, ConnSort::Color);
    assert_eq!(names(&list), ["alpha", "prod-red", "prod-red-2", "beta"]);
    // `s` cycles name → type → colour → name.
    assert_eq!(ConnSort::Name.next(), ConnSort::Type);
    assert_eq!(ConnSort::Type.next(), ConnSort::Color);
    assert_eq!(ConnSort::Color.next(), ConnSort::Name);
}

#[test]
pub(crate) fn sort_connections_keeps_the_highlight_on_the_same_connection() {
    let mut app = test_app();
    app.connections = vec![
        conn_for_sort("a", "zeta", "mysql", None),
        conn_for_sort("b", "alpha", "redis", None),
    ];
    app.conn_list.select(Some(0));
    app.conn_sort = ConnSort::Name;
    app.sort_connections();
    assert_eq!(app.connections[0].id, "b");
    assert_eq!(app.conn_list.selected(), Some(1));
}

#[test]
pub(crate) fn query_tab_title_is_first_line() {
    assert_eq!(query_tab_title("\n\nSELECT 1\nFROM t"), "SELECT 1");
    assert_eq!(query_tab_title("   "), "");
}

// ── mobile efficiency: compact columns ──

#[test]
pub(crate) fn compact_mode_is_automatic_only_on_a_narrow_terminal() {
    assert!(compact_active(None, LayoutMode::Narrow));
    assert!(!compact_active(None, LayoutMode::Mid));
    assert!(!compact_active(None, LayoutMode::Wide));
    // An explicit choice always wins, in both directions.
    assert!(!compact_active(Some(false), LayoutMode::Narrow));
    assert!(compact_active(Some(true), LayoutMode::Wide));
}

#[test]
pub(crate) fn compact_cap_shares_the_pane_so_all_columns_fit() {
    // 40 usable columns, 5 columns: each may be 7 wide, and 5*7+4 = 39 fits.
    let cap = compact_max_cell(42, 2, 5, 18);
    assert_eq!(cap, 7);
    assert!(5 * cap + 4 <= 40);
    // A wide pane with few columns keeps its normal content cap.
    assert_eq!(compact_max_cell(120, 2, 2, 44), 44);
    // One column on a narrow pane is capped at the base, not at 8.
    assert_eq!(compact_max_cell(42, 2, 1, 18), 18);
}

#[test]
pub(crate) fn compact_cap_stays_readable_when_columns_cannot_all_fit() {
    // 20 columns on a phone: even 6 each cannot fit, so it scrolls but the
    // columns stay at the readable minimum instead of collapsing to 1-2 cells.
    let cap = compact_max_cell(42, 2, 20, 18);
    assert_eq!(cap, COMPACT_MIN_CELL);
    assert!(cap <= COMPACT_MAX_CELL);
    // The empty grid is harmless.
    assert_eq!(compact_max_cell(42, 2, 0, 18), COMPACT_MAX_CELL);
}

#[test]
pub(crate) fn filter_grid_hides_columns_and_keeps_values_aligned() {
    let grid = Grid {
        columns: vec!["id".into(), "name".into(), "secret".into()],
        rows: vec![vec![
            Val::Text("1".into()),
            Val::Text("a".into()),
            Val::Text("s".into()),
        ]],
        note: String::new(),
        types: Vec::new(),
    };
    let hidden: HashSet<String> = ["secret".to_string()].into_iter().collect();
    let out = filter_grid(&grid, &hidden);
    assert_eq!(out.columns, vec!["id", "name"]);
    assert_eq!(out.rows[0][0].text(), "1");
    assert_eq!(out.rows[0][1].text(), "a");
}

#[test]
pub(crate) fn filter_grid_is_identity_without_hidden_columns() {
    let grid = ten_col_grid();
    let out = filter_grid(&grid, &HashSet::new());
    assert_eq!(out.columns.len(), 10);
    // Hiding every column still leaves one, so a grid never renders empty.
    let all: HashSet<String> = grid.columns.iter().cloned().collect();
    let out = filter_grid(&grid, &all);
    assert_eq!(out.columns, vec!["c0"]);
}

// ── SQL completion ──

#[test]
pub(crate) fn word_before_cursor_takes_the_identifier_tail() {
    let mut ta = TextArea::from(["select * from us"]);
    ta.move_cursor(CursorMove::End);
    let (n, w) = word_before_cursor(&ta);
    assert_eq!((n, w.as_str()), (2, "us"));
    // A dot is part of the word so `t.col` completes as one fragment.
    let mut ta = TextArea::from(["select t.co"]);
    ta.move_cursor(CursorMove::End);
    assert_eq!(word_before_cursor(&ta).1, "t.co");
    // Whitespace before the cursor yields an empty fragment (complete-all).
    let mut ta = TextArea::from(["select "]);
    ta.move_cursor(CursorMove::End);
    assert_eq!(word_before_cursor(&ta).1, "");
}

// ── DBX favourites write-back ──

#[test]
pub(crate) fn iso_timestamp_matches_the_dbx_shape() {
    let ts = now_iso8601();
    assert_eq!(ts.len(), 20, "{ts}");
    assert!(ts.ends_with('Z'), "{ts}");
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[10..11], "T");
    assert!(ts[..4].chars().all(|c| c.is_ascii_digit()));
    let year: i32 = ts[..4].parse().unwrap();
    assert!((2024..2100).contains(&year), "{ts}");
}

#[test]
pub(crate) fn accepting_a_completion_replaces_the_prefix() {
    let mut ta = TextArea::from(["select * from us"]);
    ta.move_cursor(CursorMove::End);
    let (n, _) = word_before_cursor(&ta);
    let (row, col) = ta.cursor();
    ta.move_cursor(CursorMove::Jump(row as u16, col.saturating_sub(n) as u16));
    ta.delete_str(n);
    ta.insert_str("users");
    assert_eq!(ta.lines(), ["select * from users"]);
}

/// R30c: `Alt-/` opens the completion popup and `Tab` accepts it. The old
/// `Ctrl-Space` chord stays as a compatibility alias (it clashes with input-
/// method switching), and the bare-NUL legacy path still opens completion.
#[test]
pub(crate) fn alt_slash_opens_completion_and_tab_accepts() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let table = || TableInfo {
        name: "users".into(),
        table_type: "TABLE".into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    };
    let open = |mods: KeyModifiers, code: KeyCode| {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
        let mut app = test_app();
        app.tables_all = vec![table()];
        app.set_editor_text("select * from us");
        editor_key(&mut app, &tx, KeyEvent::new(code, mods));
        app
    };
    // Primary key: Alt-/ opens, then Tab accepts the first candidate.
    let mut app = open(KeyModifiers::ALT, KeyCode::Char('/'));
    assert!(app.completion.is_some(), "Alt-/ should open completion");
    editor_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
    );
    assert!(app.completion.is_none(), "Tab should accept the completion");
    assert_eq!(app.editor.lines(), ["select * from users"]);
    // Compatibility aliases still open the popup.
    assert!(open(KeyModifiers::CONTROL, KeyCode::Char(' '))
        .completion
        .is_some());
    assert!(open(KeyModifiers::empty(), KeyCode::Null)
        .completion
        .is_some());
}

// ── R13: copy row as INSERT ──

#[test]
pub(crate) fn insert_literal_keeps_null_empty_and_quotes_apart() {
    assert_eq!(insert_literal(&Val::Null, None, None), "NULL");
    assert_eq!(insert_literal(&Val::Text(String::new()), None, None), "''");
    assert_eq!(
        insert_literal(&Val::Text("O'Brien".into()), None, None),
        "'O''Brien'"
    );
    assert_eq!(
        insert_literal(&Val::Text("a\\b".into()), None, None),
        "'a\\\\b'"
    );
    // A numeric column keeps a real number bare.
    assert_eq!(
        insert_literal(&Val::Text("42".into()), Some("int"), None),
        "42"
    );
    // A binary column becomes a portable hex literal.
    assert_eq!(
        insert_literal(
            &Val::Text("\u{0}\u{1}A".into()),
            Some("varbinary(8)"),
            Some("mysql")
        ),
        "X'000141'"
    );
    // PostgreSQL bytea needs its own form — `X'…'` is a bit string there.
    assert_eq!(
        insert_literal(&Val::Text("AB".into()), Some("bytea"), Some("postgres")),
        "'\\x4142'::bytea"
    );
}

#[test]
pub(crate) fn insert_literal_recovers_binary_bytes_from_0x_rendering() {
    // The kernel renders bytea/blob cells as `0x<hex>`; re-hexing that text
    // used to write the ASCII of `0x…` instead of the original bytes.
    assert_eq!(
        insert_literal(
            &Val::Text("0xdeadbeef".into()),
            Some("bytea"),
            Some("postgres")
        ),
        "'\\xdeadbeef'::bytea"
    );
    assert_eq!(
        insert_literal(&Val::Text("0xDEADBEEF".into()), Some("blob"), Some("mysql")),
        "X'DEADBEEF'"
    );
    assert_eq!(
        insert_literal(
            &Val::Text("\\x0a1b".into()),
            Some("bytea"),
            Some("postgres")
        ),
        "'\\x0a1b'::bytea"
    );
    // A non-hex `0x…`-looking string still falls back to raw-byte hex.
    assert_eq!(
        insert_literal(&Val::Text("0xzz".into()), Some("bytea"), Some("postgres")),
        "'\\x30787A7A'::bytea"
    );
    assert_eq!(binary_hex_digits("0xdead"), Some("dead"));
    assert_eq!(binary_hex_digits("0xde"), Some("de"));
    assert_eq!(binary_hex_digits("0xdea"), None);
    assert_eq!(binary_hex_digits("plain"), None);
}

#[test]
pub(crate) fn insert_literal_renders_postgres_arrays() {
    // PostgreSQL arrays arrive as JSON; an INSERT needs `ARRAY[…]`, not the
    // JSON text (which the server rejects as a malformed array literal).
    assert_eq!(
        insert_literal(
            &Val::Text("[\"admin\",\"beta\"]".into()),
            Some("text[]"),
            Some("postgres")
        ),
        "ARRAY['admin', 'beta']::text[]"
    );
    assert_eq!(
        insert_literal(
            &Val::Text("[1,2,3]".into()),
            Some("integer[]"),
            Some("postgres")
        ),
        "ARRAY[1, 2, 3]::integer[]"
    );
    assert_eq!(
        insert_literal(&Val::Text("[]".into()), Some("text[]"), Some("postgres")),
        "'{}'::text[]"
    );
    // An element containing a quote or comma is quoted safely.
    assert_eq!(
        insert_literal(
            &Val::Text("[\"a,b\",\"O'Brien\"]".into()),
            Some("text[]"),
            Some("postgres")
        ),
        "ARRAY['a,b', 'O''Brien']::text[]"
    );
    // A non-array column is untouched.
    assert_eq!(
        insert_literal(&Val::Text("[1,2]".into()), Some("jsonb"), Some("postgres")),
        "'[1,2]'"
    );
}

/// R36 seam: the data-compare sync SQL and the transfer share the literal
/// rules with the copy-as-INSERT / export path, so a PostgreSQL array (JSON
/// cell) or a `bytea` cell is escaped correctly there too — not as a plain
/// quoted string the server rejects.
#[test]
pub(crate) fn data_literal_handles_arrays_and_binary_like_the_insert_path() {
    let pg = parse_database_type("postgres").unwrap();
    assert_eq!(
        data_val_literal(
            &Val::Text("[\"admin\",\"beta\"]".into()),
            Some("text[]"),
            pg
        ),
        "ARRAY['admin', 'beta']::text[]"
    );
    assert_eq!(
        data_val_literal(&Val::Text("0xdeadbeef".into()), Some("bytea"), pg),
        "'\\xdeadbeef'::bytea"
    );
    // A raw (non-0x) binary value falls back to a hex of its bytes, like the
    // insert / export path, instead of a quoted control-character string.
    assert_eq!(
        data_val_literal(&Val::Text("\u{0}\u{1}A".into()), Some("bytea"), pg),
        "'\\x000141'::bytea"
    );
    // The sync script for a changed array column carries the ARRAY literal.
    let src = vec![
        col_full("id", "int", false, None, None, true),
        col_full("tags", "text[]", true, None, None, false),
    ];
    let tgt = src.clone();
    let align = data_align(src, tgt, &["id"], &["id"], false).unwrap();
    let row = compare_data_row(
        &align,
        &[Val::Text("1".into()), Val::Text("[\"a\"]".into())],
        &[Val::Text("1".into()), Val::Text("[\"b\"]".into())],
    )
    .unwrap();
    let cmp = DataCompare {
        src_label: "a".into(),
        tgt_label: "b".into(),
        src_db_type: pg,
        tgt_schema: "public".into(),
        tgt_table: "b".into(),
        tgt_db_type: pg,
        src_count: Some(1),
        tgt_count: Some(1),
        filter: String::new(),
        align,
        rows: vec![row],
        only_src: 0,
        only_tgt: 0,
        differing: 1,
        compared: 1,
        positional: false,
        truncated: false,
        cancelled: false,
    };
    let sql = generate_data_sync(&cmp);
    assert!(sql.contains("ARRAY['a']::text[]"), "{sql}");
}

#[test]
pub(crate) fn postgres_family_detection() {
    assert!(is_postgres_family("postgres"));
    assert!(is_postgres_family("PostgreSQL"));
    assert!(is_postgres_family("opengauss"));
    assert!(is_postgres_family("kingbase"));
    assert!(!is_postgres_family("mysql"));
    assert!(!is_postgres_family("sqlite"));
    assert!(!is_postgres_family("redis"));
}

#[test]
pub(crate) fn identifier_quoting_follows_the_dialect() {
    let pg = parse_database_type("postgres").unwrap();
    let my = parse_database_type("mysql").unwrap();
    // PostgreSQL double-quotes; MySQL backticks. Reserved words included.
    assert_eq!(quote_table_identifier(Some(pg), "select"), "\"select\"");
    assert_eq!(quote_table_identifier(Some(pg), "accounts"), "\"accounts\"");
    assert_eq!(quote_table_identifier(Some(my), "select"), "`select`");
}

#[test]
pub(crate) fn binary_type_detection_ignores_length_params() {
    assert!(is_binary_type("BLOB"));
    assert!(is_binary_type("varbinary(255)"));
    assert!(is_binary_type("bytea"));
    assert!(!is_binary_type("varchar(255)"));
    assert!(!is_binary_type("text"));
}

#[test]
pub(crate) fn text_search_column_detection() {
    for ty in [
        "char(2)",
        "varchar(255)",
        "character varying(80)",
        "TEXT",
        "mediumtext",
        "nvarchar(20)",
        "citext",
    ] {
        assert!(is_text_search_column(ty), "{ty} should be searchable text");
    }
    // Numbers, dates, binary and arrays are never LIKE-scanned.
    for ty in [
        "int",
        "bigint",
        "numeric(10,2)",
        "date",
        "timestamp with time zone",
        "blob",
        "bytea",
        "varbinary(16)",
        "text[]",
        "jsonb",
    ] {
        assert!(
            !is_text_search_column(ty),
            "{ty} must not be scanned as text"
        );
    }
}

#[test]
pub(crate) fn search_like_pattern_escapes_wildcards() {
    assert_eq!(search_like_pattern("abc"), "%abc%");
    // `%` / `_` / the escape char itself are all escaped with `!`.
    assert_eq!(search_like_pattern("a%b"), "%a!%b%");
    assert_eq!(search_like_pattern("a_b"), "%a!_b%");
    assert_eq!(search_like_pattern("a!b"), "%a!!b%");
}

#[test]
pub(crate) fn search_scan_sql_is_dialect_and_schema_aware() {
    let pg = parse_database_type("postgres").unwrap();
    let my = parse_database_type("mysql").unwrap();
    let cols = vec!["name".to_string(), "note".to_string()];
    // PostgreSQL: ILIKE (case-insensitive), double quotes, schema-qualified.
    let sql = build_search_scan_sql(pg, "public", "users", &cols, "ali", 500);
    assert!(sql.contains("\"public\".\"users\""), "{sql}");
    assert!(sql.contains("\"name\" ILIKE '%ali%' ESCAPE '!'"), "{sql}");
    assert!(sql.contains(" OR "), "{sql}");
    assert!(sql.ends_with("LIMIT 500"), "{sql}");
    // MySQL: LIKE, backticks, no schema layer.
    let sql = build_search_scan_sql(my, "", "users", &cols, "ali", 1000);
    assert!(sql.contains("`users`"), "{sql}");
    assert!(sql.contains("`name` LIKE '%ali%' ESCAPE '!'"), "{sql}");
    assert!(!sql.contains("ILIKE"), "{sql}");
    assert!(sql.ends_with("LIMIT 1000"), "{sql}");
}

#[test]
pub(crate) fn search_scan_sql_escapes_quotes_in_the_needle() {
    let my = parse_database_type("mysql").unwrap();
    let cols = vec!["c".to_string()];
    let sql = build_search_scan_sql(my, "", "t", &cols, "O'Brien", 10);
    assert!(sql.contains("'%O''Brien%'"), "{sql}");
}

#[test]
pub(crate) fn search_estimates_sql_is_dialect_aware() {
    let pg = parse_database_type("postgres").unwrap();
    let my = parse_database_type("mysql").unwrap();
    let sql = build_search_estimates_sql(pg, "inv");
    assert!(
        sql.contains("pg_class") && sql.contains("reltuples"),
        "{sql}"
    );
    assert!(sql.contains("'inv'"), "{sql}");
    let sql = build_search_estimates_sql(my, "");
    assert!(sql.contains("information_schema.tables"), "{sql}");
    assert!(sql.contains("DATABASE()"), "{sql}");
}

#[test]
pub(crate) fn search_skip_reason_boundaries() {
    let max = 1_000_000u64;
    assert_eq!(search_skip_reason(None, max), None);
    assert_eq!(search_skip_reason(Some(0), max), None);
    // Exactly at the ceiling still scans; one over is skipped.
    assert_eq!(search_skip_reason(Some(max), max), None);
    assert_eq!(search_skip_reason(Some(max + 1), max), Some(max + 1));
}

#[test]
pub(crate) fn parse_positive_overrides_reject_zero_and_garbage() {
    assert_eq!(parse_positive_usize(Some("2500"), 1000), 2500);
    assert_eq!(parse_positive_usize(Some(" 2500 "), 1000), 2500);
    // A zero / negative / garbage override keeps the safe default.
    assert_eq!(parse_positive_usize(Some("0"), 1000), 1000);
    assert_eq!(parse_positive_usize(Some("-5"), 1000), 1000);
    assert_eq!(parse_positive_usize(Some("x"), 1000), 1000);
    assert_eq!(parse_positive_usize(None, 1000), 1000);
    assert_eq!(parse_positive_u64(Some("2000000"), 1_000_000), 2_000_000);
    assert_eq!(parse_positive_u64(Some("0"), 1_000_000), 1_000_000);
}

pub(crate) fn search_vals(columns: &[&str]) -> (Vec<String>, Vec<Val>) {
    (
        columns.iter().map(|s| s.to_string()).collect(),
        columns.iter().map(|s| Val::Text((*s).into())).collect(),
    )
}

#[test]
pub(crate) fn search_hit_filter_prefers_the_primary_key() {
    let my = parse_database_type("mysql").unwrap();
    let (columns, vals) = search_vals(&["id", "name"]);
    let mut dtypes = HashMap::new();
    dtypes.insert("id".to_string(), "int".to_string());
    dtypes.insert("name".to_string(), "varchar(20)".to_string());
    // Primary key wins, and a numeric key stays unquoted.
    let f = search_hit_filter(my, &columns, &vals, &dtypes, &["id".to_string()], "name");
    assert_eq!(f, "`id` = 'id'");
    // Without a primary key the matched column's value is used.
    let f = search_hit_filter(my, &columns, &vals, &dtypes, &[], "name");
    assert_eq!(f, "`name` = 'name'");
}

#[test]
pub(crate) fn search_hit_filter_falls_back_to_one_equals_one() {
    let pg = parse_database_type("postgres").unwrap();
    let columns = vec!["name".to_string()];
    let vals = vec![Val::Null];
    let dtypes = HashMap::new();
    let f = search_hit_filter(pg, &columns, &vals, &dtypes, &[], "name");
    assert_eq!(f, "1 = 1");
}

#[test]
pub(crate) fn parse_search_estimates_reads_numbers_and_strings() {
    let rows = vec![
        vec![serde_json::json!("a"), serde_json::json!(12)],
        vec![serde_json::json!("b"), serde_json::json!("340")],
        vec![serde_json::json!("c"), serde_json::Value::Null],
    ];
    let m = parse_search_estimates(&rows);
    assert_eq!(m.get("a").copied(), Some(12));
    assert_eq!(m.get("b").copied(), Some(340));
    assert_eq!(m.get("c").copied(), Some(0));
}

#[test]
pub(crate) fn global_search_is_sql_only_and_modal() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    open_global_search(&mut app);
    assert!(app.search_input.is_some(), "the term prompt should open");
    // Esc closes the prompt without starting a scan.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.search_input.is_none());
    assert!(!app.search_open);

    // Redis / Mongo have no information schema, so the feature declines.
    app.backend_kind = Backend::Redis;
    open_global_search(&mut app);
    assert!(app.search_input.is_none());
}

#[test]
pub(crate) fn esc_aborts_a_running_search_before_closing_it() {
    let mut app = test_app();
    app.search_open = true;
    app.search_running = true;
    app.search_query = "ali".into();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    search_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.search_cancel.load(Ordering::Relaxed));
    assert!(app.search_open, "the overlay stays until the scan stops");
    // Once stopped, Esc closes it.
    app.search_running = false;
    search_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.search_open);
}

#[test]
pub(crate) fn count_sql_statements_uses_the_dialect_splitter() {
    let my = parse_database_type("mysql").unwrap();
    assert_eq!(count_sql_statements("", my), 0);
    assert_eq!(count_sql_statements("   \n\t ", my), 0);
    assert_eq!(count_sql_statements("SELECT 1", my), 1);
    assert_eq!(count_sql_statements("SELECT 1; SELECT 2;", my), 2);
    // Semicolons inside a string literal or a comment do not split.
    assert_eq!(count_sql_statements("SELECT ';'; SELECT 2", my), 2);
    assert_eq!(count_sql_statements("-- a; b\nSELECT 1; -- tail;", my), 1);
}

#[test]
pub(crate) fn expand_tilde_only_touches_a_leading_tilde() {
    assert_eq!(expand_tilde("seed.sql"), PathBuf::from("seed.sql"));
    assert_eq!(expand_tilde("/tmp/a.sql"), PathBuf::from("/tmp/a.sql"));
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/x.sql"), home.join("x.sql"));
    }
}

#[test]
pub(crate) fn read_sql_file_strips_bom_and_tolerates_non_utf8() {
    let path = std::env::temp_dir().join(format!("dbxt-sql-{}.sql", std::process::id()));
    // A UTF-8 BOM is stripped; a stray non-UTF-8 byte decodes lossily rather
    // than failing the read.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"SELECT 1;\xFF");
    std::fs::write(&path, &bytes).unwrap();
    let text = read_sql_file(&path).unwrap();
    assert!(text.starts_with("SELECT 1;"), "{text:?}");
    assert!(!text.starts_with('\u{feff}'), "BOM must be stripped");
    let _ = std::fs::remove_file(&path);
    // A missing file is an error, not a panic.
    assert!(read_sql_file(&path).is_err());
}

pub(crate) fn column(
    name: &str,
    ty: &str,
    nullable: bool,
    default: Option<&str>,
    extra: Option<&str>,
) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        is_nullable: nullable,
        column_default: default.map(str::to_string),
        extra: extra.map(str::to_string),
        ..Default::default()
    }
}

#[test]
pub(crate) fn insert_template_skips_server_generated_columns() {
    // MySQL auto-increment, PostgreSQL serial, identity and generated.
    assert!(is_server_generated_column(&column(
        "id",
        "int",
        false,
        None,
        Some("auto_increment")
    )));
    assert!(is_server_generated_column(&column(
        "id",
        "bigint",
        false,
        None,
        Some("bigserial")
    )));
    assert!(is_server_generated_column(&column(
        "id",
        "bigint",
        false,
        None,
        Some("generated by default as identity")
    )));
    assert!(is_server_generated_column(&column(
        "total",
        "numeric",
        false,
        None,
        Some("generated always as (a + b) stored")
    )));
    // A plain nextval default also means the sequence owns the value.
    assert!(is_server_generated_column(&column(
        "id",
        "bigint",
        false,
        Some("nextval('t_id_seq'::regclass)"),
        None
    )));
    // Ordinary columns are not skipped.
    assert!(!is_server_generated_column(&column(
        "email", "text", false, None, None
    )));
    assert!(!is_server_generated_column(&column(
        "balance",
        "numeric(12,2)",
        false,
        Some("0.00"),
        None
    )));
}

#[test]
pub(crate) fn insert_placeholder_is_valid_for_the_column_type() {
    // PostgreSQL NOT NULL columns: the old rule emitted `''`, which the
    // server rejects for boolean / timestamp / numeric.
    assert_eq!(
        insert_placeholder(&column("active", "boolean", false, None, None)),
        "FALSE"
    );
    assert_eq!(
        insert_placeholder(&column(
            "created_at",
            "timestamp with time zone",
            false,
            None,
            None
        )),
        "CURRENT_TIMESTAMP"
    );
    assert_eq!(
        insert_placeholder(&column("qty", "integer", false, None, None)),
        "0"
    );
    assert_eq!(
        insert_placeholder(&column("meta", "jsonb", false, None, None)),
        "'{}'"
    );
    assert_eq!(
        insert_placeholder(&column("tags", "text[]", false, None, None)),
        "'{}'"
    );
    assert_eq!(
        insert_placeholder(&column("email", "text", false, None, None)),
        "''"
    );
    // A declared default is delegated to the server.
    assert_eq!(
        insert_placeholder(&column(
            "balance",
            "numeric(12,2)",
            false,
            Some("0.00"),
            None
        )),
        "DEFAULT"
    );
    // Nullable columns stay NULL.
    assert_eq!(
        insert_placeholder(&column("note", "text", true, None, None)),
        "NULL"
    );
    // An enum NOT NULL picks its first label.
    let mut mood = column("feeling", "mood", false, None, None);
    mood.enum_values = Some(vec!["happy".into(), "sad".into()]);
    assert_eq!(insert_placeholder(&mood), "'happy'");
}

#[test]
pub(crate) fn table_name_is_guessed_from_common_statements() {
    assert_eq!(
        guess_table_from_sql("select * from users"),
        Some("users".into())
    );
    assert_eq!(
        guess_table_from_sql("SELECT a FROM `shop`.`orders` WHERE x=1"),
        Some("orders".into())
    );
    assert_eq!(guess_table_from_sql("select * from (select 1)"), None);
    assert_eq!(
        guess_table_from_sql("update public.t set a=1"),
        Some("t".into())
    );
    assert_eq!(guess_table_from_sql("select 1"), None);
}

#[test]
pub(crate) fn base64_matches_the_rfc_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

#[test]
pub(crate) fn row_search_matches_any_cell_and_null_as_text() {
    let row = vec![Val::Text("Alice".into()), Val::Null, Val::Text("42".into())];
    assert!(row_matches(&row, "alice"));
    assert!(row_matches(&row, "null"));
    assert!(row_matches(&row, "4"));
    assert!(!row_matches(&row, "bob"));
    assert!(row_matches(&row, ""));
}

#[test]
pub(crate) fn row_search_keeps_only_matching_rows_and_is_case_insensitive() {
    let grid = Grid {
        columns: vec!["name".into(), "city".into()],
        rows: vec![
            vec![Val::Text("Alice".into()), Val::Text("Beijing".into())],
            vec![Val::Text("Bob".into()), Val::Text("Shanghai".into())],
            vec![Val::Text("alice2".into()), Val::Null],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    // Empty / whitespace-only needles are a no-op.
    assert_eq!(apply_row_filters(grid.clone(), "", None).rows.len(), 3);
    assert_eq!(apply_row_filters(grid.clone(), "  ", None).rows.len(), 3);
    // Case-insensitive substring across any column.
    let hits = apply_row_filters(grid.clone(), "ALICE", None);
    assert_eq!(hits.rows.len(), 2);
    assert!(matches!(&hits.rows[0][0], Val::Text(s) if s == "Alice"));
    assert!(matches!(&hits.rows[1][0], Val::Text(s) if s == "alice2"));
    // `null` finds real NULLs.
    assert_eq!(apply_row_filters(grid.clone(), "null", None).rows.len(), 1);
    // No match yields an empty grid (but keeps the columns).
    let none = apply_row_filters(grid, "zzz", None);
    assert!(none.rows.is_empty());
    assert_eq!(none.columns.len(), 2);
}

/// R52: the single-column filter keeps only the rows whose cell in the named
/// column contains the needle, composes (AND) with the whole-row search, and
/// resolves the column by *name* so a hidden-column toggle cannot retarget
/// it. An unknown / blank column or needle is a no-op.
#[test]
pub(crate) fn column_filter_scopes_rows_to_one_named_column() {
    let grid = Grid {
        columns: vec!["name".into(), "city".into()],
        rows: vec![
            vec![Val::Text("Alice".into()), Val::Text("Beijing".into())],
            vec![Val::Text("Bob".into()), Val::Text("Beijing".into())],
            vec![Val::Text("alice2".into()), Val::Null],
        ],
        note: String::new(),
        types: Vec::new(),
    };
    // `name` containing "alice" keeps Alice + alice2; Bob's city also holds
    // "Beijing" but the filter only looks at `name`.
    let by_name = apply_row_filters(grid.clone(), "", Some(("name", "ALICE")));
    assert_eq!(by_name.rows.len(), 2);
    // `city` "beijing" keeps the two Beijing rows and drops the NULL city.
    let by_city = apply_row_filters(grid.clone(), "", Some(("city", "beijing")));
    assert_eq!(by_city.rows.len(), 2);
    // NULL matches the text `null`, same as the whole-row search.
    let nulls = apply_row_filters(grid.clone(), "", Some(("city", "null")));
    assert_eq!(nulls.rows.len(), 1);
    // A column filter ANDs with the whole-row search.
    let both = apply_row_filters(grid.clone(), "bob", Some(("city", "beijing")));
    assert_eq!(both.rows.len(), 1);
    assert!(matches!(&both.rows[0][0], Val::Text(s) if s == "Bob"));
    // An unknown column name (e.g. after a result swap) keeps every row.
    assert_eq!(
        apply_row_filters(grid.clone(), "", Some(("missing", "x")))
            .rows
            .len(),
        3
    );
    // A blank needle is inactive.
    assert_eq!(
        apply_row_filters(grid.clone(), "", Some(("name", "  ")))
            .rows
            .len(),
        3
    );
    // The kept-index map lines up with the retained rows.
    let map = kept_row_indices(&grid, "", Some(("name", "alice")));
    assert_eq!(map, vec![0, 2]);
}

/// R52: `*` seeds the prompt from the focused cell, and `clear_col_filter`
/// reports whether anything was active so a caller only rebuilds when needed.
#[test]
pub(crate) fn col_filter_state_transitions_and_is_cleared() {
    let mut app = test_app();
    assert!(app.col_filter_spec().is_none());
    assert!(!app.clear_col_filter(), "nothing to clear initially");
    app.col_filter_name = Some("city".into());
    app.col_filter_needle = "Beijing".into();
    assert_eq!(app.col_filter_spec(), Some(("city", "Beijing")));
    assert!(app.clear_col_filter());
    assert!(app.col_filter_spec().is_none());
    assert!(app.col_filter_needle.is_empty());
    // A prompt-only state (open, not yet typed) still counts as active.
    app.col_filter_prompt = Some(TextArea::default());
    assert!(app.clear_col_filter());
    assert!(app.col_filter_prompt.is_none());
}

/// R52: the statement jumper steps statement by statement and lands on the
/// first code char (a leading comment block is skipped over), reporting
/// `语句 i/n` in the status line.
#[test]
pub(crate) fn jump_statement_steps_over_comments_to_code_start() {
    let mut app = test_app();
    app.set_editor_text("SELECT 1;\n/* block ; */\nSELECT 2;\n-- note\nSELECT 3");
    app.editor.move_cursor(CursorMove::Jump(0, 0));
    assert!(jump_statement(&mut app, 1));
    // Statement 2's leading comment is skipped: the caret sits on `SELECT`.
    assert_eq!(app.editor.cursor(), (2, 0));
    assert_eq!(app.status, "语句 2/3");
    assert!(jump_statement(&mut app, 1));
    assert_eq!(app.editor.cursor(), (4, 0));
    assert_eq!(app.status, "语句 3/3");
    // At the last statement a further step only reports it.
    assert!(jump_statement(&mut app, 1));
    assert_eq!(app.status, "已是最后一条语句（共 3 条）");
    assert!(jump_statement(&mut app, -1));
    assert_eq!(app.editor.cursor(), (2, 0));
    assert_eq!(app.status, "语句 2/3");
    assert!(jump_statement(&mut app, -1));
    assert_eq!(app.editor.cursor(), (0, 0));
    assert!(jump_statement(&mut app, -1));
    assert_eq!(app.status, "已是第一条语句");
    // No statement at all is reported, not panicked on.
    app.set_editor_text("   \n  ");
    assert!(jump_statement(&mut app, 1));
    assert_eq!(app.status, "编辑器里没有语句");
}

/// R52: the one-shot version query is dialect-correct and the parsers pull
/// the version out of both a scalar row and a Redis `INFO` bulk string.
#[test]
pub(crate) fn server_version_query_and_parsers() {
    assert_eq!(server_version_query("sqlite"), "SELECT sqlite_version()");
    assert_eq!(server_version_query("MySQL"), "SELECT version()");
    assert_eq!(server_version_query("postgres"), "SELECT version()");
    assert_eq!(server_version_query("sqlserver"), "SELECT @@VERSION");
    // A scalar grid's first cell is the version; NULL / empty is `None`.
    let one = vec![vec![serde_json::json!("8.0.36")]];
    assert_eq!(first_cell_text(&one), Some("8.0.36".to_string()));
    assert_eq!(first_cell_text(&[]), None);
    assert_eq!(first_cell_text(&[vec![serde_json::json!("  ")]]), None);
    // Redis: `redis_version:` out of a bulk `INFO server` string, whether it
    // arrives as one string or an array of lines.
    let info = serde_json::json!("# Server\nredis_version:7.2.4\nos:Linux");
    assert_eq!(parse_redis_version(&info).as_deref(), Some("7.2.4"));
    let lines = serde_json::json!(["# Server", "redis_version:6.0.9"]);
    assert_eq!(parse_redis_version(&lines).as_deref(), Some("6.0.9"));
    assert_eq!(parse_redis_version(&serde_json::json!("no version")), None);
}

#[test]
pub(crate) fn completion_candidates_are_tagged_deduped_and_case_insensitive() {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let names = vec!["id".to_string(), "ID".to_string(), "name".to_string()];
    push_names(&mut out, &mut seen, &names, 'C', "i", None);
    // `id` and `ID` collapse to one candidate (case-insensitive dedupe).
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "id");
    assert_eq!(out[0].kind, 'C');
    // A table candidate keeps its `T` tag and matches case-insensitively.
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    push_item(&mut out, &mut seen, "Users", "Users", 'T', "us");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "Users");
    assert_eq!(out[0].kind, 'T');
}

/// R54: a completed identifier is quoted only when it needs it — an
/// uppercase or reserved name gets the dialect's quote (backtick for MySQL,
/// double quote for PostgreSQL), a plain lowercase name stays bare.
#[test]
pub(crate) fn completion_quotes_uppercase_and_reserved_identifiers_only() {
    // Plain lowercase names need no quoting.
    assert!(!identifier_needs_quote("users"));
    assert!(!identifier_needs_quote("user_id2"));
    assert!(!identifier_needs_quote("_tmp"));
    assert_eq!(
        completion_quote("users", Some(DatabaseType::Mysql)),
        "users"
    );
    // Uppercase, punctuation and reserved words all need it.
    assert!(identifier_needs_quote("Users"));
    assert!(identifier_needs_quote("my-table"));
    assert!(identifier_needs_quote("2fa"));
    assert!(identifier_needs_quote("order"));
    assert!(identifier_needs_quote("key"));
    // Dialect spellings: MySQL backticks, PostgreSQL double quotes.
    assert_eq!(
        completion_quote("Users", Some(DatabaseType::Mysql)),
        "`Users`"
    );
    assert_eq!(
        completion_quote("Users", Some(DatabaseType::Postgres)),
        "\"Users\""
    );
    assert_eq!(
        completion_quote("order", Some(DatabaseType::Mysql)),
        "`order`"
    );
    assert_eq!(
        completion_quote("order", Some(DatabaseType::Postgres)),
        "\"order\""
    );
    // A quoted candidate is still found by its lowercase prefix.
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    push_names(
        &mut out,
        &mut seen,
        &["Users".to_string()],
        'T',
        "us",
        Some(DatabaseType::Postgres),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "\"Users\"");
}

// ── R13: persistent config ──

#[test]
pub(crate) fn config_round_trips_and_tolerates_corruption() {
    let path = std::env::temp_dir().join(format!("dbxt-test-{}.json", Uuid::new_v4()));
    let mut cfg = TuiConfig::default();
    cfg.set_compact(Some(true));
    let e = cfg.entry("shop", "", "orders");
    e.hidden = ["secret".to_string()].into_iter().collect();
    e.compact = Some(false);
    e.order_by = Some("\"id\" DESC".into());
    cfg.save(&path);
    let back = TuiConfig::load(&path);
    assert_eq!(back.compact, Some(true));
    let p = back.table("shop", "", "orders").unwrap();
    assert_eq!(p.hidden.len(), 1);
    assert!(p.hidden.contains("secret"));
    assert_eq!(p.compact, Some(false));
    assert_eq!(p.order_by.as_deref(), Some("\"id\" DESC"));

    // A truncated / garbage file falls back to defaults instead of failing.
    std::fs::write(&path, "{ not json").unwrap();
    let broken = TuiConfig::load(&path);
    assert!(broken.tables.is_empty());
    assert_eq!(broken.compact, None);
    // A missing file is fine too.
    let _ = std::fs::remove_file(&path);
    assert!(TuiConfig::load(&path).tables.is_empty());
}

#[test]
pub(crate) fn config_tolerates_wrong_shapes() {
    let path = std::env::temp_dir().join(format!("dbxt-shape-{}.json", Uuid::new_v4()));
    // Valid JSON but every field has the wrong type: no panic, defaults only.
    std::fs::write(
            &path,
            r#"{"version":1,"compact":"yes","tables":{"db":"nope","db2":{"t":42},"db3":{"t":{"hidden":"x","compact":7,"order_by":false}}}}"#,
        )
        .unwrap();
    let cfg = TuiConfig::load(&path);
    assert_eq!(cfg.compact, None);
    // A wrong-shaped table map / entry is skipped, not fatal.
    assert!(cfg.table("db", "", "t").is_none());
    // A nested entry with every field the wrong type degrades to defaults.
    let p = cfg.table("db3", "", "t").expect("entry kept as defaults");
    assert!(p.hidden.is_empty() && p.compact.is_none() && p.order_by.is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
pub(crate) fn config_save_merges_concurrent_sessions_and_clears_entries() {
    let path = std::env::temp_dir().join(format!("dbxt-merge-{}.json", Uuid::new_v4()));
    // Session A stores a sort for one table.
    let mut a = TuiConfig::default();
    a.entry("db", "", "a").order_by = Some("\"id\" ASC".into());
    a.save(&path);
    // Session B knows nothing about table `a` (older snapshot) and writes `b`.
    // Its save must not wipe A's entry.
    let mut b = TuiConfig::default();
    b.entry("db", "", "b").hidden = ["x".to_string()].into_iter().collect();
    b.save(&path);
    let after = TuiConfig::load(&path);
    assert!(after.table("db", "", "a").is_some(), "B must not clobber A");
    assert!(after.table("db", "", "b").is_some());
    // Resetting a table to defaults removes its stored entry instead of
    // silently keeping the stale one.
    let mut c = TuiConfig::default();
    c.entry("db", "", "a");
    c.save(&path);
    let cleared = TuiConfig::load(&path);
    assert!(cleared.table("db", "", "a").is_none());
    assert!(
        cleared.table("db", "", "b").is_some(),
        "unrelated entry survives"
    );
    let _ = std::fs::remove_file(&path);
}

// ── R26: schema-aware browsing ──

#[test]
pub(crate) fn schema_picker_engine_is_opt_in() {
    assert!(schema_picker_engine(
        parse_database_type("postgres").unwrap()
    ));
    assert!(schema_picker_engine(
        parse_database_type("sqlserver").unwrap()
    ));
    assert!(schema_picker_engine(parse_database_type("oracle").unwrap()));
    // Embedded / single-namespace engines keep the flat list.
    assert!(!schema_picker_engine(
        parse_database_type("sqlite").unwrap()
    ));
    assert!(!schema_picker_engine(
        parse_database_type("duckdb").unwrap()
    ));
    assert!(!schema_picker_engine(parse_database_type("mysql").unwrap()));
}

#[test]
pub(crate) fn qualified_table_reference_quotes_the_schema() {
    let pg = parse_database_type("postgres").unwrap();
    let mysql = parse_database_type("mysql").unwrap();
    assert_eq!(table_ref(pg, "inv", "items"), "\"inv\".\"items\"");
    // No schema → the pre-R26 unqualified form, so MySQL / SQLite are unchanged.
    assert_eq!(table_ref(pg, "", "items"), "\"items\"");
    assert_eq!(table_ref(mysql, "", "items"), "`items`");
    // A MySQL database qualifier is valid too.
    assert_eq!(table_ref(mysql, "shop", "items"), "`shop`.`items`");
}

#[test]
pub(crate) fn qualified_display_and_pref_key_fold_in_the_schema() {
    assert_eq!(qualified_display("inv", "items"), "inv.items");
    assert_eq!(qualified_display("", "items"), "items");
    assert_eq!(table_pref_key("public", "orders"), "public.orders");
    assert_eq!(table_pref_key("", "orders"), "orders");
}

#[test]
pub(crate) fn import_insert_sql_qualifies_the_schema() {
    let cfg = test_conn("postgres");
    let cols = vec![
        ImportCol {
            name: "item_id".into(),
            src: Some(0),
            ty: ColType::Int,
            data_type: "integer".into(),
        },
        ImportCol {
            name: "sku".into(),
            src: Some(1),
            ty: ColType::Text,
            data_type: "text".into(),
        },
    ];
    let sql = import_insert_sql(&cfg, "inv", "items", &cols, &["1".into(), "SKU-1".into()]);
    assert_eq!(
        sql,
        "INSERT INTO \"inv\".\"items\" (\"item_id\", \"sku\") VALUES (1, 'SKU-1');"
    );
}

#[test]
pub(crate) fn update_sql_qualifies_the_schema() {
    let cfg = test_conn("postgres");
    let sql = build_update_sql(
        &cfg,
        "inv",
        "items",
        "qty",
        Some("integer"),
        "5",
        "\"item_id\" = 1",
    );
    assert_eq!(
        sql,
        "UPDATE \"inv\".\"items\"\nSET \"qty\" = 5\nWHERE \"item_id\" = 1;"
    );
}

#[test]
pub(crate) fn batch_insert_sql_qualifies_the_schema() {
    let cfg = test_conn("postgres");
    let columns = vec!["id".to_string(), "name".to_string()];
    let rows = vec![vec![Val::Text("1".into()), Val::Text("a".into())]];
    let types = vec![Some("integer".to_string()), Some("text".to_string())];
    let out = batch_insert_sql(&cfg, "inv", "items", &columns, &rows, &types, 10);
    assert!(
        out.starts_with("INSERT INTO \"inv\".\"items\" (\"id\", \"name\") VALUES"),
        "{out}"
    );
}

#[test]
pub(crate) fn column_type_is_matched_per_schema() {
    let mut app = test_app();
    app.table_meta = Some(TableMeta {
        table: "orders".into(),
        schema: "public".into(),
        columns: vec![ColumnInfo {
            name: "amount".into(),
            data_type: "numeric(10,2)".into(),
            ..Default::default()
        }],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    assert_eq!(
        column_type(&app, "public", "orders", "amount").as_deref(),
        Some("numeric(10,2)")
    );
    // Same table name in another schema must not inherit the metadata.
    assert!(column_type(&app, "inv", "orders", "amount").is_none());
}

#[test]
pub(crate) fn picker_lists_schemas_before_databases() {
    let mut app = test_app();
    app.selected = Some(test_conn("postgres"));
    app.databases = vec!["shop".into(), "postgres".into()];
    app.schemas = vec!["public".into(), "inv".into()];
    app.schema = "inv".into();
    let entries = picker_entries(&app);
    assert_eq!(
        entries,
        vec![
            (PickerKind::Schema, "public".into()),
            (PickerKind::Schema, "inv".into()),
            (PickerKind::Database, "shop".into()),
            (PickerKind::Database, "postgres".into()),
        ]
    );
    // The highlight starts on the current schema, not the current database.
    assert_eq!(db_current_index(&app), 1);
    // Labels separate the two kinds once the schema layer is on.
    let labels = db_entries(&app);
    assert!(labels[0].starts_with("模式"), "{labels:?}");
    assert!(labels[2].starts_with("数据库"), "{labels:?}");
}

#[test]
pub(crate) fn picker_is_flat_without_schemas() {
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.databases = vec!["shop".into()];
    app.schemas.clear();
    assert_eq!(
        picker_entries(&app),
        vec![(PickerKind::Database, "shop".into())]
    );
    // MySQL labels keep their pre-R26 plain form.
    assert_eq!(db_entries(&app), vec!["shop".to_string()]);
}

#[test]
pub(crate) fn stale_table_list_is_discarded() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    let table = |name: &str| TableInfo {
        name: name.into(),
        table_type: "TABLE".into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    };
    app.tables_gen = 2;
    app.tables_all = vec![table("inv_items")];
    // A reply for an older generation (a slow `public` list the user already
    // left) must not replace the current one.
    apply_op_result(
        &mut app,
        OpResult::TablesFor {
            tables: vec![table("public_orders")],
            gen: 1,
        },
        &tx,
    );
    assert_eq!(app.tables_all.len(), 1);
    assert_eq!(app.tables_all[0].name, "inv_items");
    // The current generation is applied.
    apply_op_result(
        &mut app,
        OpResult::TablesFor {
            tables: vec![table("inv_items"), table("inv_orders")],
            gen: 2,
        },
        &tx,
    );
    assert_eq!(app.tables_all.len(), 2);
}

/// R36 seam: an `Alt-G` hit (or a recent-table jump) must still land when
/// the sidebar `/` filter hides the table — the global search scans every
/// table, so a leftover filter must not report “table not found”.
#[test]
pub(crate) fn sidebar_jump_reveals_a_table_hidden_by_the_name_filter() {
    let table = |name: &str| TableInfo {
        name: name.into(),
        table_type: "TABLE".into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    };
    let mut app = test_app();
    app.tables_all = vec![table("orders"), table("users")];
    app.table_filter = "users".into();
    apply_table_filter(&mut app);
    // The active filter hides `orders`.
    assert_eq!(app.tables.len(), 1);
    assert!(app.tables.iter().all(|t| t.name == "users"));
    // Jumping to the hit clears it and returns the now-visible index.
    assert_eq!(focus_table_in_sidebar(&mut app, "orders"), Some(0));
    assert!(app.table_filter.is_empty());
    assert_eq!(app.tables.len(), 2);
    // A genuinely absent table leaves the filter untouched.
    app.table_filter = "users".into();
    apply_table_filter(&mut app);
    assert_eq!(focus_table_in_sidebar(&mut app, "missing"), None);
    assert_eq!(app.table_filter, "users");
}

// ── R39 table quick-locate ──

pub(crate) fn table_info(name: &str, kind: &str) -> TableInfo {
    TableInfo {
        name: name.into(),
        table_type: kind.into(),
        valid: None,
        comment: None,
        parent_schema: None,
        parent_name: None,
    }
}

#[test]
pub(crate) fn table_sort_cycles_name_then_type() {
    let mut list = vec![
        table_info("beta", "VIEW"),
        table_info("alpha", "TABLE"),
        table_info("a_view", "VIEW"),
        table_info("zeta", "TABLE"),
    ];
    sort_table_list(&mut list, TableSort::Name);
    let names: Vec<&str> = list.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["a_view", "alpha", "beta", "zeta"]);
    // Type mode groups tables before views, each alphabetical.
    sort_table_list(&mut list, TableSort::Type);
    let names: Vec<&str> = list.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zeta", "a_view", "beta"]);
    // `s` cycles name → type → name.
    assert_eq!(TableSort::Name.next(), TableSort::Type);
    assert_eq!(TableSort::Type.next(), TableSort::Name);
}

#[test]
pub(crate) fn one_step_type_to_filter_then_enter_opens_the_first_hit() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    // Enter routes through `open_table_data`, which defers its page load via
    // `App::spawn`; that needs a Tokio context on the test thread.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let mut app = test_app();
    app.selected = Some(test_conn("mysql"));
    app.tables_all = vec![
        table_info("orders", "TABLE"),
        table_info("users", "TABLE"),
        table_info("user_logs", "TABLE"),
    ];
    apply_table_filter(&mut app);
    // A single printable key enters the filter with that character typed.
    open_table_filter_with(&mut app, Some('u'));
    assert_eq!(app.table_filter, "u");
    assert!(app.table_prompt.is_some());
    assert_eq!(app.tables.len(), 2);
    // Enter opens the first hit (open_table_data defers the page for the
    // column metadata, which is what `pending_open_page` records).
    table_filter_key(&mut app, &tx, KeyEvent::from(KeyCode::Enter));
    assert!(app.table_prompt.is_none());
    assert_eq!(app.table_filter, "u");
    assert!(app.pending_open_page);
    // Ctrl-U inside the prompt clears the filter (grep muscle memory).
    open_table_filter(&mut app);
    table_filter_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert!(app.table_filter.is_empty());
    assert!(app.table_prompt.is_none());
    assert_eq!(app.tables.len(), 3);
}

#[test]
pub(crate) fn first_letter_jump_cycles_forwards_backwards_and_wraps() {
    let mut app = test_app();
    app.tables = vec![
        table_info("alpha", "TABLE"),
        table_info("beta", "TABLE"),
        table_info("atom", "TABLE"),
        table_info("gamma", "TABLE"),
    ];
    app.table_list.select(Some(0));
    // Forward from alpha finds the next `a` (atom), then wraps to alpha.
    assert_eq!(table_jump_by_letter(&mut app, 'a', 1), Some(2));
    assert_eq!(app.table_jump_letter, Some('a'));
    assert_eq!(table_jump_by_letter(&mut app, 'a', 1), Some(0));
    // Backward wraps the other way.
    assert_eq!(table_jump_by_letter(&mut app, 'a', -1), Some(2));
    // Uppercase matches case-insensitively and pins the repeat letter lower.
    app.table_list.select(Some(0));
    assert_eq!(table_jump_by_letter(&mut app, 'A', 1), Some(2));
    assert_eq!(app.table_jump_letter, Some('a'));
    // No match is a no-op.
    assert_eq!(table_jump_by_letter(&mut app, 'z', 1), None);
}

#[test]
pub(crate) fn filter_highlight_underlines_the_matched_substring() {
    let base = Style::default();
    let hit = Style::default().add_modifier(Modifier::UNDERLINED);
    let spans = highlight_match_spans("inv_orders", "ord", base, hit);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "inv_orders");
    let underlined: Vec<&str> = spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(underlined, vec!["ord"]);
    // No match (or an empty needle) leaves the text untouched in one span.
    assert_eq!(highlight_match_spans("abc", "zzz", base, hit).len(), 1);
    assert_eq!(highlight_match_spans("abc", "", base, hit).len(), 1);
}

// ── R39 grid value locate / column jump ──

pub(crate) fn locate_grid() -> Grid {
    Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec![Val::Text("1".into()), Val::Text("alice".into())],
            vec![Val::Text("42".into()), Val::Text("bob".into())],
            vec![Val::Text("143".into()), Val::Text("carol".into())],
            vec![Val::Null, Val::Text("dave".into())],
        ],
        note: String::new(),
        types: Vec::new(),
    }
}

#[test]
pub(crate) fn locate_matches_searches_only_the_target_column() {
    let grid = locate_grid();
    // Substring, not prefix: `4` hits 42 and 143 but not 1.
    assert_eq!(locate_matches(&grid, 0, "4"), vec![1, 2]);
    assert_eq!(locate_matches(&grid, 0, "1"), vec![0, 2]);
    // Case-insensitive on a text column.
    assert_eq!(locate_matches(&grid, 1, "BO"), vec![1]);
    // A NULL cell never matches, and a blank needle is no search at all.
    assert!(locate_matches(&grid, 0, "  ").is_empty());
    assert!(!locate_matches(&grid, 1, "dave").is_empty());
}

#[test]
pub(crate) fn locate_target_prefers_sort_then_pk_then_first_column() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(locate_grid());
    app.page_state = Some(PageState {
        table: "t".into(),
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
    });
    // No sort / no metadata → first column.
    assert_eq!(locate_target_col(&app), Some(0));
    // A primary key in the metadata wins over the first column when it is a
    // different index.
    let pk_col = ColumnInfo {
        name: "name".into(),
        data_type: "text".into(),
        is_nullable: false,
        is_primary_key: true,
        ..Default::default()
    };
    app.table_meta = Some(TableMeta {
        table: "t".into(),
        schema: String::new(),
        columns: vec![pk_col],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    });
    assert_eq!(locate_target_col(&app), Some(1));
    // An explicit sort column wins over the primary key.
    app.page_state.as_mut().unwrap().order_by = Some("id".into());
    assert_eq!(locate_target_col(&app), Some(0));
}

#[test]
pub(crate) fn locate_move_cycles_hits_and_anchors_on_the_cursor() {
    let mut app = test_app();
    app.grid_kind = GridKind::TableData;
    app.set_grid(locate_grid());
    app.locate_col = Some(0);
    app.locate_needle = "4".into();
    // Hits are rows 1 and 2. From a non-hit cursor (0) forward lands on the
    // next hit; a further step wraps.
    app.sel = 0;
    locate_move(&mut app, 1);
    assert_eq!(app.sel, 1);
    locate_move(&mut app, 1);
    assert_eq!(app.sel, 2);
    locate_move(&mut app, 1);
    assert_eq!(app.sel, 1);
    // Backward from a non-hit cursor lands on the previous hit.
    app.sel = 3;
    locate_move(&mut app, -1);
    assert_eq!(app.sel, 2);
}

#[test]
pub(crate) fn parse_col_jump_accepts_number_and_name_prefix() {
    let cols: Vec<String> = vec![
        "id".into(),
        "user_name".into(),
        "email_address".into(),
        "created_at".into(),
    ];
    assert_eq!(parse_col_jump(&cols, "2"), Ok(1));
    assert_eq!(parse_col_jump(&cols, "user"), Ok(1));
    assert_eq!(parse_col_jump(&cols, "EMAIL"), Ok(2));
    // Substring fallback: "ate" only appears inside created_at.
    assert_eq!(parse_col_jump(&cols, "ate"), Ok(3));
    // A number out of range and a miss both fail with a message.
    assert!(parse_col_jump(&cols, "9").is_err());
    assert!(parse_col_jump(&cols, "0").is_err());
    assert!(parse_col_jump(&cols, "nope").is_err());
    assert!(parse_col_jump(&cols, "").is_err());
}

// ── R64 in-result cell find (`\`) ──

pub(crate) fn cell_find_grid() -> Grid {
    Grid {
        columns: vec!["id".into(), "name".into(), "city".into()],
        rows: vec![
            vec![
                Val::Text("1".into()),
                Val::Text("alice".into()),
                Val::Text("Beijing".into()),
            ],
            vec![
                Val::Text("42".into()),
                Val::Text("bob".into()),
                Val::Text("Shanghai".into()),
            ],
            vec![
                Val::Text("143".into()),
                Val::Text("Alice".into()),
                Val::Null,
            ],
        ],
        note: String::new(),
        types: Vec::new(),
    }
}

#[test]
pub(crate) fn cell_find_hits_scan_every_column_in_reading_order() {
    let grid = cell_find_grid();
    // Case-insensitive substring across any column, row-major order.
    assert_eq!(cell_find_hits(&grid, "ALI", 500).0, vec![(0, 1), (2, 1)]);
    assert_eq!(
        cell_find_hits(&grid, "a", 500).0,
        vec![(0, 1), (1, 2), (2, 1)]
    );
    // NULL matches the text `null`, like the `/` row search.
    assert_eq!(cell_find_hits(&grid, "null", 500).0, vec![(2, 2)]);
    // An empty / whitespace needle is no search at all.
    assert!(cell_find_hits(&grid, "", 500).0.is_empty());
    assert!(cell_find_hits(&grid, "  ", 500).0.is_empty());
}

#[test]
pub(crate) fn cell_find_hits_caps_and_reports_truncation() {
    let grid = cell_find_grid();
    // A limit below the match count truncates and flags it.
    let (hits, capped) = cell_find_hits(&grid, "a", 1);
    assert_eq!(hits, vec![(0, 1)]);
    assert!(capped, "the hit list must report truncation");
    // A limit at/above the match count is complete and not capped.
    let (hits, capped) = cell_find_hits(&grid, "a", 3);
    assert_eq!(hits.len(), 3);
    assert!(!capped);
}

/// `compute_cell_find` stops at the configured ceiling and records it, so the
/// status/title can print `≥n` instead of a count that reads as exact.
#[test]
pub(crate) fn compute_cell_find_stops_at_the_ceiling() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    let rows = (0..DEFAULT_CELL_FIND_LIMIT + 50)
        .map(|_| vec![Val::Text("hit".into())])
        .collect();
    app.set_grid(Grid {
        columns: vec!["c".into()],
        rows,
        note: String::new(),
        types: Vec::new(),
    });
    app.cell_find_needle = "hit".into();
    compute_cell_find(&mut app);
    assert_eq!(app.cell_find_hits.len(), DEFAULT_CELL_FIND_LIMIT);
    assert!(
        app.cell_find_capped,
        "a full list must be flagged as capped"
    );
    // A needle with fewer matches than the ceiling is not flagged.
    app.cell_find_needle = "zzz".into();
    compute_cell_find(&mut app);
    assert!(app.cell_find_hits.is_empty());
    assert!(!app.cell_find_capped);
}

#[test]
pub(crate) fn cell_find_step_cycles_across_rows_and_columns() {
    let mut app = test_app();
    app.grid_kind = GridKind::Query;
    app.set_grid(cell_find_grid());
    app.cell_find_needle = "a".into();
    compute_cell_find(&mut app);
    assert_eq!(app.cell_find_hits, vec![(0, 1), (1, 2), (2, 1)]);
    // From the top-left (not a hit) forward lands on the first hit.
    app.sel = 0;
    app.col_cursor = 0;
    cell_find_step(&mut app, 1);
    assert_eq!((app.sel, app.col_cursor), (0, 1));
    assert_eq!(app.cell_find_idx, 0);
    // Steps move down the row-major list, changing column too.
    cell_find_step(&mut app, 1);
    assert_eq!((app.sel, app.col_cursor), (1, 2));
    cell_find_step(&mut app, 1);
    assert_eq!((app.sel, app.col_cursor), (2, 1));
    // Forward wraps to the first; backward from the first wraps to the last.
    cell_find_step(&mut app, 1);
    assert_eq!((app.sel, app.col_cursor), (0, 1));
    cell_find_step(&mut app, -1);
    assert_eq!((app.sel, app.col_cursor), (2, 1));
    assert_eq!(app.cell_find_idx, 2);
}

#[test]
pub(crate) fn cell_find_prompt_enter_jumps_and_esc_clears() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.grid_kind = GridKind::Query;
    app.set_grid(cell_find_grid());
    app.focus = Focus::Preview;
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE),
    );
    assert!(app.cell_find_prompt.is_some());
    // The modal prompt receives the keystrokes and recomputes live.
    for c in "shang".chars() {
        key(
            &mut app,
            &tx,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        );
    }
    assert_eq!(app.cell_find_needle, "shang");
    assert_eq!(app.cell_find_hits, vec![(1, 2)]);
    // Enter closes the prompt, keeps the needle and lands on the hit.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.cell_find_prompt.is_none());
    assert_eq!(app.cell_find_needle, "shang");
    assert_eq!((app.sel, app.col_cursor), (1, 2));
    // Esc in the grid clears the whole find state.
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.cell_find_needle.is_empty());
    assert!(app.cell_find_hits.is_empty());
}

/// `\` and `/` are distinct keys: `\` opens the cell find (no row hiding),
/// `/` still opens the row filter, and starting either drops the other.
#[test]
pub(crate) fn backslash_and_slash_stay_distinct_and_mutually_exclusive() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.grid_kind = GridKind::Query;
    app.set_grid(cell_find_grid());
    app.focus = Focus::Preview;
    // `/` opens the row filter (unchanged behaviour).
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert!(app.result_filter.is_some());
    app.result_needle = "alice".into();
    app.rebuild_view();
    assert_eq!(app.grid.as_ref().unwrap().rows.len(), 2);
    // `\` drops the row filter so every row is visible again, then opens.
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE),
    );
    assert!(app.result_needle.is_empty(), "`\\` clears the row filter");
    assert!(app.result_filter.is_none());
    assert_eq!(app.grid.as_ref().unwrap().rows.len(), 3);
    assert!(app.cell_find_prompt.is_some());
    // Esc (through the modal dispatcher, as a real key press) clears it;
    // `/` still opens the row filter afterwards.
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.cell_find_prompt.is_none());
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    assert!(app.result_filter.is_some());
    assert!(app.cell_find_prompt.is_none());
}

/// The matched cells are actually painted: a non-current hit gets the yellow
/// search background and the current hit the light-green accent.
#[test]
pub(crate) fn cell_find_paints_hits_and_the_accent_match() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.grid_kind = GridKind::Query;
    app.set_grid(cell_find_grid());
    app.focus = Focus::Preview;
    preview_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let buf = draw_buffer(&mut app, 100, 30);
    let results = app.rects.results;
    let bg_count = |bg: Color| {
        (results.x..results.x + results.width)
            .flat_map(|x| (results.y..results.y + results.height).map(move |y| (x, y)))
            .filter(|&(x, y)| buf.cell((x, y)).map(|c| c.bg) == Some(bg))
            .count()
    };
    assert!(
        bg_count(Color::LightGreen) >= 1,
        "the current hit is accented"
    );
    assert!(bg_count(Color::Yellow) >= 1, "other hits are highlighted");
}

/// R56: `:` accepts a 1-based row number (`0` / `1` = first) or `$` / `end`
/// (last); R83 clamps an out-of-range number to the last row and still reports
/// nonsense input.
#[test]
pub(crate) fn parse_row_jump_covers_bounds_and_last() {
    assert_eq!(parse_row_jump(10, "1"), Ok(0));
    assert_eq!(parse_row_jump(10, "0"), Ok(0));
    assert_eq!(parse_row_jump(10, "10"), Ok(9));
    assert_eq!(parse_row_jump(10, "11"), Ok(9));
    assert_eq!(parse_row_jump(10, "9999"), Ok(9));
    assert_eq!(parse_row_jump(10, "$"), Ok(9));
    assert_eq!(parse_row_jump(10, " $ "), Ok(9));
    assert_eq!(parse_row_jump(10, "END"), Ok(9));
    assert!(parse_row_jump(10, "").is_err());
    assert!(parse_row_jump(10, "abc").is_err());
    // No rows is reported, never a silent `sel = 0`.
    assert!(parse_row_jump(0, "1").is_err());
}

/// R83: an absolute row index splits into its page and in-page offset.
#[test]
pub(crate) fn row_jump_page_splits_page_and_offset() {
    assert_eq!(row_jump_page(0, 50), (0, 0));
    assert_eq!(row_jump_page(49, 50), (0, 49));
    assert_eq!(row_jump_page(50, 50), (1, 0));
    assert_eq!(row_jump_page(123, 50), (2, 23));
    // A zero page size never divides by zero.
    assert_eq!(row_jump_page(7, 0), (7, 0));
}

/// R56/R83: `:` in the results pane opens the jump prompt, Enter lands on the
/// row, an out-of-range input clamps to the last row, and Esc cancels.
#[test]
pub(crate) fn colon_jump_lands_on_the_row() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<OpResult>();
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE),
    );
    assert!(app.goto_prompt.is_some());
    // It renders at both widths.
    let phone: String = draw(&mut app, 42, 22)
        .join("\n")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(phone.contains("跳行"), "{phone}");
    let _ = draw(&mut app, 110, 30);
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
    );
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.goto_prompt.is_none());
    assert_eq!(app.sel, 2);

    // R83: an out-of-range number clamps to the last row instead of reporting.
    app.sel = 0;
    open_goto_row(&mut app);
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE),
    );
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 3);

    // `$` goes to the last row.
    open_goto_row(&mut app);
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE),
    );
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(app.sel, 3);

    // Esc cancels.
    open_goto_row(&mut app);
    goto_row_key(
        &mut app,
        &tx,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.goto_prompt.is_none());

    // With no rows there is nothing to open.
    app.clear_grid();
    open_goto_row(&mut app);
    assert!(app.goto_prompt.is_none());
}

/// R39 overlay small-screen sweep: every bottom-anchored prompt owns a
/// short title that fits 42×22, and the frame still shows the pinned `?`
/// help hint (key hints survive on small screens).
#[test]
pub(crate) fn prompt_overlays_keep_titles_and_hints_at_42x22() {
    let mut app = test_app();
    app.picker_open = false;
    app.selected = Some(test_conn("mysql"));
    app.backend_kind = Backend::Sql;
    app.grid_kind = GridKind::TableData;
    app.set_grid(sample_grid());
    app.focus = Focus::Preview;
    app.tables_all = vec![table_info("orders", "TABLE"), table_info("users", "TABLE")];
    apply_table_filter(&mut app);

    // (fixture, expected short-title fragment)
    type PromptCase = (&'static str, Box<dyn Fn(&mut App)>, &'static str);
    let cases: Vec<PromptCase> = vec![
        (
            "table-filter",
            Box::new(|a| a.table_prompt = Some(TextArea::default())),
            "过滤表名",
        ),
        (
            "result-filter",
            Box::new(|a| a.result_filter = Some(TextArea::default())),
            "搜索",
        ),
        (
            "locate",
            Box::new(|a| {
                a.locate_col = Some(0);
                a.locate_prompt = Some(TextArea::default());
            }),
            "定位值",
        ),
        (
            "col-jump",
            Box::new(|a| a.col_jump = Some(TextArea::default())),
            "跳列",
        ),
        (
            "goto-row",
            Box::new(|a| a.goto_prompt = Some(TextArea::default())),
            "跳行",
        ),
    ];
    for (name, open, fragment) in cases {
        app.table_prompt = None;
        app.result_filter = None;
        app.locate_prompt = None;
        app.col_jump = None;
        app.goto_prompt = None;
        open(&mut app);
        let rows = draw(&mut app, 42, 22);
        // The TestBackend pads each wide CJK glyph with a space cell, so
        // strip whitespace before matching a multi-character title.
        let text: String = rows
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            text.contains(fragment),
            "{name}: short title {fragment:?} missing at 42×22\n{text}"
        );
        // The pinned help hint is present somewhere (footer tier Mini keeps
        // the escape hatch even when it trims other hints).
        assert!(text.contains('?'), "{name}: help hint dropped\n{text}");
    }
}
