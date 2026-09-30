use crate::prelude::*;
use crate::*;

/// Keys for a MongoDB document grid: JSON filter, pagination and the shared
/// search / copy / popup infrastructure.
pub(crate) fn mongo_docs_key(app: &mut App, tx: &Tx, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        match k.code {
            KeyCode::Char('e') => app.focus = Focus::Editor,
            KeyCode::Char('f') => page_turn(app, tx, true),
            KeyCode::Char('b') => page_turn(app, tx, false),
            KeyCode::Char('d') => mongo_confirm_delete(app),
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
        KeyCode::Char('f') => open_mongo_filter_prompt(app),
        // Document CRUD: edit / insert / delete (all confirmed).
        KeyCode::Char('e') => open_mongo_edit(app),
        KeyCode::Char('i') => open_mongo_insert(app),
        KeyCode::Delete => mongo_confirm_delete(app),
        // R42: `y` copies the focused document's full JSON (the natural unit for
        // a document store), not the flattened grid row.
        KeyCode::Char('y') => copy_mongo_doc_json(app),
        KeyCode::Char('Y') => copy_cell_value(app),
        KeyCode::Char('o') => open_row_popup(app),
        KeyCode::Char('v') => open_cell_popup(app),
        KeyCode::Char('/') => open_result_filter(app),
        KeyCode::Char('\\') => open_cell_find(app),
        KeyCode::Char(':') => open_goto_row(app),
        // R67: `z` pins the first column here too, matching the result / Redis
        // grids (with the same status flash).
        KeyCode::Char('z') => toggle_freeze_first(app),
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
        KeyCode::Char('n') => page_turn(app, tx, true),
        KeyCode::Char('p') => page_turn(app, tx, false),
        // Enter opens the whole document row (R42b); `v` opens the cell directly.
        KeyCode::Enter => open_row_popup(app),
        _ => {}
    }
}

/// Open the MongoDB JSON filter prompt (prefilled with the active filter).
pub(crate) fn open_mongo_filter_prompt(app: &mut App) {
    let mut ta = TextArea::from(vec![app.mongo_filter.clone()]);
    ta.set_placeholder_text(t("JSON 过滤，例: {\"age\": {\"$gt\": 30}}（留空 = 全部）"));
    ta.move_cursor(CursorMove::End);
    app.filter_prompt = Some(ta);
}

/// `y` in a MongoDB document grid: copy the focused document as pretty JSON.
pub(crate) fn copy_mongo_doc_json(app: &mut App) {
    let Some((_idx, doc)) = mongo_focused_doc(app) else {
        app.status = t("没有可复制的文档").into();
        return;
    };
    let json = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string());
    let n = json.chars().count();
    match clipboard_copy(&json) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制文档 JSON（{} 字符）· 兜底 {}",
                &[&n, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制文档 JSON（{} 字符）", &[&n]),
    }
}

/// The focused document from the retained page, plus its index in that page.
pub(crate) fn mongo_focused_doc(app: &App) -> Option<(usize, serde_json::Value)> {
    let idx = app.full_row_index()?;
    let doc = app.mongo_docs.get(idx)?.clone();
    Some((idx, doc))
}

/// `e` — open the JSON editor for the focused document.
pub(crate) fn open_mongo_edit(app: &mut App) {
    let Some((_idx, doc)) = mongo_focused_doc(app) else {
        app.status = t("没有可编辑的文档").into();
        return;
    };
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let id_value = doc.get("_id").cloned().unwrap_or(serde_json::Value::Null);
    let id = mongo_id_arg(&id_value);
    let text = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string());
    let mut editor = TextArea::from(text.split('\n').collect::<Vec<_>>());
    editor.move_cursor(CursorMove::Top);
    editor.set_placeholder_text(t("JSON 文档（_id 不可修改）"));
    app.mongo_dialog = Some(MongoDocDialog {
        mode: MongoDocMode::Edit,
        db: app.current_db(),
        collection: coll,
        original: doc,
        id,
        editor,
        error: None,
    });
    app.status = t("编辑文档 · Ctrl-S 校验并保存 · Esc 取消").into();
}

/// `i` — open the JSON editor with an empty document template.
pub(crate) fn open_mongo_insert(app: &mut App) {
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let mut editor = TextArea::from(vec!["{", "  ", "}"]);
    editor.move_cursor(CursorMove::Top);
    editor.move_cursor(CursorMove::Down);
    editor.move_cursor(CursorMove::End);
    editor.set_placeholder_text(t("新文档 JSON（省略 _id 则由 MongoDB 生成）"));
    app.mongo_dialog = Some(MongoDocDialog {
        mode: MongoDocMode::Insert,
        db: app.current_db(),
        collection: coll,
        original: serde_json::Value::Object(serde_json::Map::new()),
        id: String::new(),
        editor,
        error: None,
    });
    app.status = t("插入文档 · Ctrl-S 校验并保存 · Esc 取消").into();
}

/// `Del` — confirm deleting the focused document by `_id`.
pub(crate) fn mongo_confirm_delete(app: &mut App) {
    let Some((_idx, doc)) = mongo_focused_doc(app) else {
        app.status = t("没有可删除的文档").into();
        return;
    };
    let id_value = doc.get("_id").cloned().unwrap_or(serde_json::Value::Null);
    let label = mongo_id_label(&id_value);
    let id = mongo_id_arg(&id_value);
    let Some(coll) = app.selected_table().map(|t| t.name.clone()) else {
        return;
    };
    let db = app.current_db();
    app.confirm = Some(Confirm {
        sql: format!(
            "db.{}.deleteOne({{_id: {}}})",
            fix_double_encoding(&coll),
            serde_json::to_string(&id_value).unwrap_or_default()
        ),
        reasons: vec![
            tf("将删除文档 _id={}（不可撤销）", &[&label]),
            t("Enter 后立即执行").into(),
        ],
        refresh: false,
        clear_batch: false,
        conn: None,
        redis: None,
        mongo: Some(MongoConfirm {
            db,
            collection: coll,
            action: MongoAction::Delete { id },
        }),
    });
    app.status = t("删除文档确认 · Enter 执行 · Esc 取消").into();
}

/// Handle keys in the MongoDB JSON editor. Ctrl-S validates and opens the
/// confirmation layer; everything else is text editing.
pub(crate) fn mongo_dialog_key(app: &mut App, k: KeyEvent) {
    let Some(mut d) = app.mongo_dialog.take() else {
        return;
    };
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('s') {
        mongo_dialog_submit(app, d);
        return;
    }
    if k.code == KeyCode::Esc {
        app.flash(t("已取消").into());
        return;
    }
    d.editor.input(k);
    d.error = None;
    app.mongo_dialog = Some(d);
}

/// Validate the editor's JSON and route a valid edit / insert through the red
/// confirmation layer (an edit previews its top-level diff first).
pub(crate) fn mongo_dialog_submit(app: &mut App, d: MongoDocDialog) {
    let text = d.editor.lines().join("\n");
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let mut d = d;
            d.error = Some(tf("JSON 非法：{}", &[&e]));
            app.mongo_dialog = Some(d);
            app.status = tf("✗ JSON 非法：{}", &[&e]);
            return;
        }
    };
    if !parsed.is_object() {
        let mut d = d;
        d.error = Some(t("文档必须是 JSON 对象 { … }").to_string());
        app.mongo_dialog = Some(d);
        app.status = t("✗ 文档必须是 JSON 对象").into();
        return;
    }
    let doc_json = serde_json::to_string(&parsed).unwrap_or(text);
    match d.mode {
        MongoDocMode::Insert => {
            let preview = serde_json::to_string_pretty(&parsed).unwrap_or_default();
            app.confirm = Some(Confirm {
                sql: preview,
                reasons: vec![
                    tf(
                        "将向 {}.{} 插入 1 个文档",
                        &[
                            &fix_double_encoding(&d.db),
                            &fix_double_encoding(&d.collection),
                        ],
                    ),
                    t("Enter 执行 · Esc 取消").into(),
                ],
                refresh: false,
                clear_batch: false,
                conn: None,
                redis: None,
                mongo: Some(MongoConfirm {
                    db: d.db.clone(),
                    collection: d.collection.clone(),
                    action: MongoAction::Insert { doc_json },
                }),
            });
            app.status = t("插入确认 · Enter 执行 · Esc 取消").into();
        }
        MongoDocMode::Edit => {
            let old_id = d.original.get("_id");
            let new_id = parsed.get("_id");
            if old_id != new_id {
                let mut d = d;
                d.error = Some(t("_id 不可修改（请恢复原值）").to_string());
                app.mongo_dialog = Some(d);
                app.status = t("✗ _id 不可修改").into();
                return;
            }
            let diff = mongo_doc_diff(&d.original, &parsed, 12);
            let mut body = String::new();
            if diff.is_empty() {
                body.push_str(t("（没有字段变化）"));
            } else {
                for line in &diff {
                    body.push_str(line);
                    body.push('\n');
                }
            }
            body.push('\n');
            body.push_str(&serde_json::to_string_pretty(&parsed).unwrap_or_default());
            app.confirm = Some(Confirm {
                sql: body,
                reasons: vec![
                    tf(
                        "将替换文档 _id={}",
                        &[&mongo_id_label(old_id.unwrap_or(&serde_json::Value::Null))],
                    ),
                    t("Enter 执行 · Esc 取消").into(),
                ],
                refresh: false,
                clear_batch: false,
                conn: None,
                redis: None,
                mongo: Some(MongoConfirm {
                    db: d.db.clone(),
                    collection: d.collection.clone(),
                    action: MongoAction::Update {
                        id: d.id.clone(),
                        doc_json,
                    },
                }),
            });
            app.status = t("更新确认（含 diff）· Enter 执行 · Esc 取消").into();
        }
    }
}
