//! R108: export *every* result tab at once.
//!
//! The single-tab `Ctrl-Y` export writes one grid. A script / query run leaves
//! several `ResultTab`s in `App::result_tabs`, and this module packages them in
//! one action: an `.xlsx` workbook with one worksheet per tab, or a `.zip` with
//! one `.sql` file per tab (INSERT statements).
//!
//! Everything here is client-side and explicit: the grids are already in
//! memory, so no query is ever issued. The XLSX path reuses the kernel's
//! `StreamingXlsxWriter` (one streamed data sheet + the remaining tabs as
//! trailing sheets); the ZIP path reuses the kernel's `sql_file_zip_package`
//! manifest format so a reader can inspect the archive. The ZIP container itself
//! is written with a tiny STORED-only writer in this file — the `zip` crate is
//! not a normal dependency of dbxt, and R108 must not add one.

use crate::prelude::*;
use crate::*;

/// Which multi-tab export the picker's "all tabs" section runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BatchExportKind {
    /// One worksheet per result tab, in one `.xlsx` workbook.
    Xlsx,
    /// One `.sql` file per result tab, packaged in one `.zip`.
    SqlZip,
}

impl BatchExportKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            BatchExportKind::Xlsx => t("全部 Tab Excel"),
            BatchExportKind::SqlZip => t("全部 Tab SQL zip"),
        }
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            BatchExportKind::Xlsx => "xlsx",
            BatchExportKind::SqlZip => "zip",
        }
    }
}

/// More than this many tabs asks for a red confirmation first.
pub(crate) const BATCH_EXPORT_MAX_TABS: usize = 20;
/// More than this many rows across every tab asks for a red confirmation first.
pub(crate) const BATCH_EXPORT_MAX_ROWS: usize = 200_000;

/// One exportable result tab, snapshotted for the background worker so the
/// render thread never touches the grids again.
#[derive(Clone)]
pub(crate) struct BatchTab {
    pub(crate) title: String,
    pub(crate) grid: Grid,
    pub(crate) schema: String,
    /// Table the INSERT statements target (`result` when the statement has no
    /// determinable `FROM`).
    pub(crate) table: String,
    /// Declared type per column, for INSERT literal quoting.
    pub(crate) types: Vec<Option<String>>,
}

impl BatchTab {
    pub(crate) fn rows(&self) -> usize {
        self.grid.rows.len()
    }
}

/// The grids of every result tab that can be exported. A tab without a grid
/// (the script statement list, a table browse) is skipped, and so is a grid
/// with no columns — there is nothing to write for either.
pub(crate) fn collect_batch_tabs(app: &App) -> Vec<BatchTab> {
    app.result_tabs
        .iter()
        .filter_map(|tab| {
            let grid = tab.grid.as_ref()?;
            if grid.columns.is_empty() {
                return None;
            }
            let table = tab
                .sql
                .as_deref()
                .and_then(guess_table_from_sql)
                .unwrap_or_else(|| "result".to_string());
            let types = grid_column_types(app, "", &table, grid);
            Some(BatchTab {
                title: tab.title.clone(),
                grid: grid.clone(),
                schema: String::new(),
                table,
                types,
            })
        })
        .collect()
}

/// The guard values (`tabs`, `rows`) when a batch export needs confirmation.
pub(crate) fn batch_guard(tabs: &[BatchTab]) -> Option<(usize, usize)> {
    let rows: usize = tabs.iter().map(BatchTab::rows).sum();
    if tabs.len() > BATCH_EXPORT_MAX_TABS || rows > BATCH_EXPORT_MAX_ROWS {
        Some((tabs.len(), rows))
    } else {
        None
    }
}

/// Total rows across the given tabs.
pub(crate) fn batch_rows(tabs: &[BatchTab]) -> usize {
    tabs.iter().map(BatchTab::rows).sum()
}

/// Excel sheet-name rules: illegal characters `: \ / ? * [ ]` are replaced, the
/// name is trimmed and capped at 31 characters.
pub(crate) fn sanitize_sheet_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            '\n' | '\r' | '\t' => ' ',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('\'');
    let base = if trimmed.is_empty() { "Sheet" } else { trimmed };
    base.chars().take(31).collect()
}

/// Make `base` unique against `used` (case-insensitively, as Excel does) by
/// appending `_2`, `_3`, … while staying inside the 31-character limit.
pub(crate) fn unique_sheet_name(base: &str, used: &mut Vec<String>) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
        let suffix = format!("_{n}");
        let max_base = 31usize.saturating_sub(suffix.chars().count());
        candidate = format!("{}{}", base.chars().take(max_base).collect::<String>(), suffix);
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// A filename stem with path separators / control characters neutralised. Used
/// for both the SQL entry names inside the ZIP and the default export filename.
pub(crate) fn sanitize_file_stem(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_control()
            || c.is_whitespace()
            || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '_' || c == ' ' || c == '.');
    let mut stem: String = trimmed.chars().take(60).collect();
    if stem.is_empty() || stem == "." || stem == ".." {
        stem = "result".to_string();
    }
    stem
}

/// `title` as a `.sql` entry name: the sanitized stem plus the extension.
pub(crate) fn sanitize_sql_file_name(title: &str) -> String {
    format!("{}.sql", sanitize_file_stem(title))
}

/// Deduplicate a `.sql` entry name against `used` (`name_2.sql`, …).
pub(crate) fn unique_sql_file_name(base: &str, used: &mut Vec<String>) -> String {
    let stem = base.strip_suffix(".sql").unwrap_or(base);
    let mut candidate = base.to_string();
    let mut n = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
        candidate = format!("{stem}_{n}.sql");
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// The default destination filename `{db}-results-{HHMMSS}.{ext}`.
pub(crate) fn batch_default_filename(db: &str, kind: BatchExportKind) -> String {
    let stem = sanitize_file_stem(db);
    let stem = if stem == "result" { "db".to_string() } else { stem };
    let stamp = chrono::Local::now().format("%H%M%S");
    format!("{stem}-results-{stamp}.{}", kind.extension())
}

/// One grid row as XLSX cell values (NULL = empty cell, text verbatim).
fn grid_row_values(columns: usize, row: &[Val]) -> Vec<serde_json::Value> {
    (0..columns)
        .map(|ci| match row.get(ci) {
            Some(Val::Text(s)) => serde_json::Value::String(s.clone()),
            _ => serde_json::Value::Null,
        })
        .collect()
}

/// Write every tab into one workbook: the first tab is streamed as the data
/// sheet, the rest become trailing sheets. Each sheet is capped at the R105
/// in-memory limit; the returned titles name the tabs that were truncated.
///
/// The kernel's `start_streaming_xlsx_workbook_with_trailing_sheets` helper is
/// `#[cfg(test)]` only, so this goes through the public `_with_options` entry
/// point, which takes the same `trailing_sheets` slice.
pub(crate) fn write_batch_xlsx<W: Write + Seek>(
    w: &mut W,
    tabs: &[BatchTab],
) -> Result<Vec<String>, String> {
    if tabs.is_empty() {
        return Err("no tabs to export".to_string());
    }
    let mut truncated: Vec<String> = Vec::new();
    let mut used_names: Vec<String> = Vec::new();
    let mut sheet_names: Vec<String> = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let base = sanitize_sheet_name(&tab.title);
        sheet_names.push(unique_sheet_name(&base, &mut used_names));
    }

    let first = &tabs[0];
    let first_comments: Vec<Option<String>> = vec![None; first.grid.columns.len()];
    let mut trailing: Vec<XlsxWorksheetData> = Vec::with_capacity(tabs.len() - 1);
    for (i, tab) in tabs.iter().enumerate().skip(1) {
        let columns = tab.grid.columns.clone();
        let types = tab.grid.types.clone();
        let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
        let limit = tab.grid.rows.len().min(EXPORT_XLSX_MAX_ROWS);
        if tab.grid.rows.len() > EXPORT_XLSX_MAX_ROWS {
            truncated.push(tab.title.clone());
        }
        for row in tab.grid.rows.iter().take(limit) {
            rows.push(grid_row_values(columns.len(), row));
        }
        trailing.push(XlsxWorksheetData {
            sheet_name: Some(sheet_names[i].clone()),
            column_comments: vec![None; columns.len()],
            columns,
            column_types: types,
            rows,
            numeric_column_right_align: false,
            auto_filter: None,
        });
    }

    let mut writer = start_streaming_xlsx_workbook_with_options(
        w,
        Some(&sheet_names[0]),
        &first.grid.columns,
        &first.grid.types,
        &first_comments,
        &trailing,
        None,
        false,
        false,
    )
    .map_err(|e| e.to_string())?;
    if first.grid.rows.len() > EXPORT_XLSX_MAX_ROWS {
        truncated.push(first.title.clone());
    }
    let columns = first.grid.columns.len();
    for row in first.grid.rows.iter().take(EXPORT_XLSX_MAX_ROWS) {
        let values = grid_row_values(columns, row);
        writer.write_row(&values).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(truncated)
}

/// Build the ZIP entries: one `.sql` per tab (INSERT statements), plus the
/// `manifest.json` the kernel's `sql_file_zip_package` reader expects.
pub(crate) fn build_sql_zip_entries(
    tabs: &[BatchTab],
    cfg: Option<&ConnectionConfig>,
    source_file_name: &str,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let cfg = cfg.ok_or_else(|| "no connection for INSERT export".to_string())?;
    let mut used: Vec<String> = Vec::new();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(tabs.len() + 1);
    let mut parts: Vec<String> = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let name = unique_sql_file_name(&sanitize_sql_file_name(&tab.title), &mut used);
        let mut content = String::new();
        content.push_str("-- ");
        content.push_str(&tab.title.replace(['\n', '\r'], " "));
        content.push('\n');
        for row in &tab.grid.rows {
            content.push_str(&build_insert_sql_types(
                cfg,
                &tab.schema,
                &tab.table,
                &tab.grid,
                row,
                &tab.types,
            ));
            content.push('\n');
        }
        entries.push((name.clone(), content.into_bytes()));
        parts.push(name);
    }
    let manifest = serde_json::json!({
        "format": "dbx-sql-export-parts-v1",
        "sourceFileName": source_file_name,
        "parts": parts,
    });
    let manifest = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    entries.push(("manifest.json".to_string(), manifest));
    Ok(entries)
}

/// A STORED-only ZIP archive. No compression keeps the writer tiny and the
/// output readable by every ZIP implementation; `zip` is deliberately not a
/// normal dependency (R108 adds none).
pub(crate) fn zip_store(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let name_bytes = name.as_bytes();
        let size = data.len() as u32;
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        out.extend_from_slice(&0u16.to_le_bytes()); // STORED
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // mod date (1980-01-01)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Standard CRC-32 (IEEE 802.3, polynomial 0xEDB88320) used by the ZIP headers.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The chosen all-tabs export waiting for its destination path.
pub(crate) struct BatchExportPending {
    pub(crate) kind: BatchExportKind,
    pub(crate) tabs: Vec<BatchTab>,
}

/// A large all-tabs export awaiting the red confirmation (`>20` tabs or
/// `>200_000` rows). The collected tabs are kept so Enter starts immediately.
pub(crate) struct BatchExportConfirm {
    pub(crate) kind: BatchExportKind,
    pub(crate) tabs: Vec<BatchTab>,
}

/// Everything the background worker needs for one batch export.
pub(crate) struct BatchExportJob {
    pub(crate) kind: BatchExportKind,
    pub(crate) path: PathBuf,
    pub(crate) tabs: Vec<BatchTab>,
    pub(crate) cfg: Option<ConnectionConfig>,
    /// `{db}-results-{HHMMSS}` stem, used as the ZIP manifest source name.
    pub(crate) base: String,
}

/// What the worker produced: the tabs whose sheet was truncated to the R105
/// limit, and the number of rows written.
pub(crate) struct BatchExportOutcome {
    pub(crate) truncated: Vec<String>,
}

/// Run a batch export on the blocking worker. XLSX streams to the file; the ZIP
/// is assembled in memory (the SQL text is bounded by the grids already held)
/// and written in one call.
pub(crate) fn run_batch_export(job: &BatchExportJob) -> Result<BatchExportOutcome, String> {
    match job.kind {
        BatchExportKind::Xlsx => {
            let file = std::fs::File::create(&job.path).map_err(|e| e.to_string())?;
            let mut w = BufWriter::with_capacity(EXPORT_BUF_BYTES, file);
            let truncated = write_batch_xlsx(&mut w, &job.tabs)?;
            w.flush().map_err(|e| e.to_string())?;
            Ok(BatchExportOutcome { truncated })
        }
        BatchExportKind::SqlZip => {
            let entries = build_sql_zip_entries(&job.tabs, job.cfg.as_ref(), &job.base)?;
            let bytes = zip_store(&entries);
            std::fs::write(&job.path, &bytes).map_err(|e| e.to_string())?;
            Ok(BatchExportOutcome {
                truncated: Vec::new(),
            })
        }
    }
}
