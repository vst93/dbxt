// Tests for the whole crate, split by contiguous region of the original
// single test module. The shared helpers live here.
#![allow(unused_imports)]

use crate::prelude::*;
use crate::*;

/// A `LocalBackend` on a throwaway store so the render / key layers can be
/// exercised headlessly. Opened once per test process and shared; the tests
/// below never spawn an op, they only render.
pub(crate) fn test_backend() -> Arc<LocalBackend> {
    use std::sync::OnceLock;
    static B: OnceLock<Arc<LocalBackend>> = OnceLock::new();
    B.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("dbxt-render-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Build on a dedicated thread: some tests call `test_app()` from
        // inside their own `run_rt` runtime, and a nested `block_on` would
        // panic with "Cannot start a runtime from within a runtime".
        let path = dir.join("dbx.db");
        let backend = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(LocalBackend::open(&path))
                .expect("open test backend")
        })
        .join()
        .expect("test backend thread");
        Arc::new(backend)
    })
    .clone()
}

pub(crate) fn test_app() -> App {
    App::new(
        test_backend(),
        TuiConfig::default(),
        None,
        false,
        None,
        DragPan::Off,
    )
}

/// A connection config for a given driver, used to exercise the Redis / Mongo
/// view selection without opening a real socket.
pub(crate) fn test_conn(db_type: &str) -> ConnectionConfig {
    new_connection_config(
        format!("id-{db_type}"),
        format!("test-{db_type}"),
        parse_database_type(db_type).unwrap(),
        "127.0.0.1".into(),
        1,
        "u".into(),
        "p".into(),
        None,
        false,
        None,
    )
    .unwrap()
}

/// Draw the whole UI into a headless buffer. Returns the rendered text rows
/// so a test can assert what actually reached the screen.
pub(crate) fn draw(app: &mut App, w: u16, h: u16) -> Vec<String> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
    term.draw(|f| ui(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect()
}

/// Same as [`draw`] but hands back the raw buffer, so a test can inspect the
/// per-cell styles (the bracket highlight is a modifier, not a glyph).
pub(crate) fn draw_buffer(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
    term.draw(|f| ui(f, app)).unwrap();
    term.backend().buffer().clone()
}

pub(crate) fn sample_grid() -> Grid {
    Grid {
        columns: (0..8).map(|i| format!("column_{i}")).collect(),
        rows: (0..4)
            .map(|r| {
                (0..8)
                    .map(|c| Val::Text(format!("r{r}c{c}")))
                    .collect::<Vec<_>>()
            })
            .collect(),
        note: String::new(),
    }
}

mod part1;
mod part2;
mod part3;

pub(crate) use part1::*;
pub(crate) use part2::*;
pub(crate) use part3::*;
