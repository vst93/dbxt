//! Shared imports for every dbxt module.
//!
//! Each module (and `main.rs`) starts with `use crate::prelude::*;`, so the
//! external crate imports and the sibling modules stay reachable without a
//! long import block per file. Glob re-exports that no module happens to use
//! are expected here, hence the file-level allowance.
#![allow(unused_imports)]

pub(crate) use crate::ui_text::{t, tf};

pub(crate) use std::collections::{HashMap, HashSet, VecDeque};
pub(crate) use std::io::{BufWriter, Cursor, IsTerminal, Seek, Write};
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) use std::sync::Arc;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use anyhow::Result;
pub(crate) use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
pub(crate) use crossterm::execute;
pub(crate) use dbx_core::db::redis_driver::{
    RedisBlob, RedisBlobEncoding, RedisCollectionPage, RedisKeyInfo, RedisValue, RedisValueData,
};
pub(crate) use dbx_core::db::ssh_prompt::{
    self as ssh_prompt, SshHostKeyNotice, SshHostKeyNoticeKind, SshPromptAnswer, SshPromptEnvelope,
    SshPromptKind, SshPromptRequest,
};
pub(crate) use dbx_core::models::connection::{
    ConnectionConfig, DatabaseType, SshTunnelConfig, TransportLayerConfig,
};
pub(crate) use dbx_core::query::QueryExecutionOptions;
pub(crate) use dbx_core::sql_dialect::{
    build_count_table_sql, build_table_data_select_sql_with_database, is_schema_aware,
    normalize_where_input, qualified_table_name, quote_table_identifier, table_pagination_strategy,
    TableDataSelectSqlOptions, TablePaginationStrategy,
};
pub(crate) use dbx_core::types::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableInfo};
pub(crate) use dbx_core::xlsx_export::{
    start_streaming_xlsx_workbook_with_options, XlsxWorksheetData,
};
pub(crate) use dbx_mcp::backend::{
    new_connection_config, parse_database_type, BatchStatementResult, DbxBackend, LocalBackend,
};
pub(crate) use dbx_mcp::paths::storage_db_path;
pub(crate) use futures::StreamExt;
pub(crate) use ratatui::layout::{Constraint, Layout, Rect};
pub(crate) use ratatui::style::{Color, Modifier, Style};
pub(crate) use ratatui::symbols::border;
pub(crate) use ratatui::text::{Line, Span};
pub(crate) use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Padding, Paragraph, Row, Table, Wrap,
};
pub(crate) use ratatui::Frame;
pub(crate) use tui_textarea::{CursorMove, Scrolling, TextArea};
pub(crate) use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
pub(crate) use uuid::Uuid;

pub(crate) use crate::batch_export::*;
pub(crate) use crate::comments::*;
pub(crate) use crate::csv_io::*;
pub(crate) use crate::ddl_export::*;
pub(crate) use crate::diffui::*;
pub(crate) use crate::docgen::*;
pub(crate) use crate::editor::*;
pub(crate) use crate::filter::*;
pub(crate) use crate::input::*;
pub(crate) use crate::jsonview::*;
pub(crate) use crate::last_session::*;
pub(crate) use crate::materialize::*;
pub(crate) use crate::mongo::*;
pub(crate) use crate::nav::*;
pub(crate) use crate::numfmt::*;
pub(crate) use crate::parity::*;
pub(crate) use crate::redis::*;
pub(crate) use crate::render::*;
pub(crate) use crate::render_help::*;
pub(crate) use crate::render_overlay::*;
pub(crate) use crate::results::*;
pub(crate) use crate::rowops::*;
pub(crate) use crate::runner::*;
pub(crate) use crate::search::*;
pub(crate) use crate::sidebar::*;
pub(crate) use crate::sqlfmt::*;
pub(crate) use crate::sqlite_open::*;
pub(crate) use crate::state::*;
pub(crate) use crate::textutil::*;
pub(crate) use crate::transfer::*;
pub(crate) use crate::tui_config::*;
