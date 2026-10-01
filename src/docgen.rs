//! R100: table-structure Markdown and full-database data-dictionary generation.
//!
//! Every renderer here is a pure function over metadata dbxt has already fetched
//! (`TableMeta` and the kernel's `ColumnInfo` / `IndexInfo` / `ForeignKeyInfo`),
//! so they are unit-testable without a terminal or a connection. The dictionary
//! worker in `main.rs` walks the tables and calls
//! [`database_dictionary_markdown`] once at the end.
//!
//! Both renderers go through the bilingual table (`t` / `tf`), so an
//! `DBXT_LANG=en` run emits an English document and the default run a Chinese
//! one. All Markdown template strings live here, not inline at the call sites.
use crate::prelude::*;

/// R100: a database with more tables than this asks for the red confirmation
/// before an export walks them all — a fat-finger on a 5,000-table warehouse
/// must not launch a full metadata scan.
pub(crate) const DICT_CONFIRM_TABLES: usize = 200;

/// R100: a `>200`-table dictionary export awaiting the red confirmation. Holds
/// everything the confirmed export needs, so the confirmation layer issues no
/// query of its own.
#[derive(Clone)]
pub(crate) struct DictConfirm {
    pub(crate) cfg: Box<ConnectionConfig>,
    pub(crate) db: String,
    pub(crate) schema: String,
    /// Table count the gate saw (from the cached sidebar list), shown in the
    /// warning so the user knows how large the scan is.
    pub(crate) tables: usize,
}

/// Escape one Markdown table cell: a literal `|` would split the row and a
/// newline would break the table, so both are neutralised. Leading / trailing
/// whitespace is trimmed for a tidy table.
pub(crate) fn md_cell(s: &str) -> String {
    s.replace('|', "\\|")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string()
}

fn has_comment(c: &ColumnInfo) -> bool {
    c.comment
        .as_deref()
        .map(str::trim)
        .is_some_and(|v| !v.is_empty())
}

/// R100: the `schema.table` heading of one table's structure section (schema
/// omitted when the engine has none, e.g. MySQL).
pub(crate) fn table_structure_title(meta: &TableMeta) -> String {
    // R106: the schema / table name is backend-controlled metadata, so it goes
    // through the same cell escape as the body — a newline in an identifier
    // would otherwise break the `## ` heading into a second line and a `|`
    // would read as a table-cell separator. Ordinary names are unchanged.
    md_cell(&qualified_display(
        &fix_double_encoding(&meta.schema),
        &fix_double_encoding(&meta.table),
    ))
}

/// R100: one table's structure as Markdown — a column table plus an index list
/// and a foreign-key list, each emitted only when the backend reported rows.
///
/// The `注释` column is dropped entirely when no column carries a comment, so a
/// comment-less schema does not get a blank column on every table.
pub(crate) fn table_structure_markdown(meta: &TableMeta) -> String {
    let mut out = String::new();
    out.push_str("## ");
    out.push_str(&table_structure_title(meta));
    out.push_str("\n\n");

    let show_comment = meta.columns.iter().any(has_comment);
    out.push_str("| ");
    out.push_str(t("列名"));
    out.push_str(" | ");
    out.push_str(t("类型"));
    out.push_str(" | ");
    out.push_str(t("键"));
    out.push_str(" | ");
    out.push_str(t("可空"));
    out.push_str(" | ");
    out.push_str(t("默认值"));
    out.push_str(" |");
    if show_comment {
        out.push(' ');
        out.push_str(t("注释"));
        out.push_str(" |");
    }
    out.push('\n');
    out.push_str("| --- | --- | --- | --- | --- |");
    if show_comment {
        out.push_str(" --- |");
    }
    out.push('\n');
    for c in &meta.columns {
        out.push_str("| ");
        out.push_str(&md_cell(&fix_double_encoding(&c.name)));
        out.push_str(" | ");
        out.push_str(&md_cell(&c.data_type));
        out.push_str(" | ");
        out.push_str(column_key_mark(c, &meta.indexes));
        out.push_str(" | ");
        out.push_str(if c.is_nullable { t("是") } else { t("否") });
        out.push_str(" | ");
        out.push_str(&md_cell(c.column_default.as_deref().unwrap_or("")));
        out.push_str(" |");
        if show_comment {
            out.push(' ');
            out.push_str(&md_cell(c.comment.as_deref().unwrap_or("")));
            out.push_str(" |");
        }
        out.push('\n');
    }

    if !meta.indexes.is_empty() {
        out.push('\n');
        out.push_str("### ");
        out.push_str(t("索引"));
        out.push_str("\n\n");
        out.push_str("| ");
        out.push_str(t("名称"));
        out.push_str(" | ");
        out.push_str(t("列"));
        out.push_str(" | ");
        out.push_str(t("唯一"));
        out.push_str(" |\n| --- | --- | --- |\n");
        for ix in &meta.indexes {
            out.push_str("| ");
            out.push_str(&md_cell(&ix.name));
            out.push_str(" | ");
            out.push_str(&md_cell(&ix.columns.join(", ")));
            out.push_str(" | ");
            out.push_str(if ix.is_unique { t("是") } else { t("否") });
            out.push_str(" |\n");
        }
    }

    if !meta.foreign_keys.is_empty() {
        out.push('\n');
        out.push_str("### ");
        out.push_str(t("外键"));
        out.push_str("\n\n");
        out.push_str("| ");
        out.push_str(t("列"));
        out.push_str(" | ");
        out.push_str(t("引用"));
        out.push_str(" |\n| --- | --- |\n");
        for fk in &meta.foreign_keys {
            let target = qualified_display(fk.ref_schema.as_deref().unwrap_or(""), &fk.ref_table);
            out.push_str("| ");
            out.push_str(&md_cell(&fix_double_encoding(&fk.column)));
            out.push_str(" | ");
            out.push_str(&md_cell(&format!(
                "{target}.{}",
                fix_double_encoding(&fk.ref_column)
            )));
            out.push_str(" |\n");
        }
    }
    out
}

/// R100: the whole-database data dictionary. The first section is the overview
/// (database / table count / engine dialect / generated-at), then one structure
/// section per table.
pub(crate) fn database_dictionary_markdown(
    db: &str,
    dialect: &str,
    generated_at: &str,
    tables: &[TableMeta],
) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(&tf("{} 数据字典", &[&db]));
    out.push_str("\n\n");
    out.push_str("- ");
    out.push_str(t("数据库"));
    out.push_str(": ");
    out.push_str(&md_cell(db));
    out.push('\n');
    out.push_str("- ");
    out.push_str(t("表数量"));
    out.push_str(": ");
    out.push_str(&tables.len().to_string());
    out.push('\n');
    out.push_str("- ");
    out.push_str(t("引擎方言"));
    out.push_str(": ");
    out.push_str(dialect);
    out.push('\n');
    out.push_str("- ");
    out.push_str(t("生成时间"));
    out.push_str(": ");
    out.push_str(generated_at);
    out.push('\n');
    for m in tables {
        out.push_str("\n---\n\n");
        out.push_str(&table_structure_markdown(m));
    }
    out
}

/// R100: the prefilled dictionary filename — `{db}-dictionary.md`. Any path
/// separator in the database name is neutralised so the default stays a single
/// filename rather than escaping the working directory.
pub(crate) fn dict_default_filename(db: &str) -> String {
    let safe: String = db
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("{safe}-dictionary.md")
}

/// R100: `y` inside the `g c` popup — copy the open table's structure as
/// Markdown through the shared clipboard channel. A bare query result has no
/// table metadata, so it reports instead of emitting a columns-only stub.
pub(crate) fn copy_table_structure_markdown(app: &mut App) {
    let Some(meta) = app.table_meta.as_ref() else {
        app.status = t("当前没有表结构可复制（查询结果无表元数据）").into();
        return;
    };
    let md = table_structure_markdown(meta);
    let n = md.chars().count();
    match clipboard_copy(&md) {
        Some(p) => {
            app.status = tf(
                "✓ 已复制表结构 Markdown（{} 字符）· 兜底 {}",
                &[&n, &(p.display())],
            )
        }
        None => app.status = tf("✓ 已复制表结构 Markdown（{} 字符）", &[&n]),
    }
}
