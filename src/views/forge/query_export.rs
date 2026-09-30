//! Export a Forge query straight to an Excel file, and page through a result the Forge runtime
//! keeps.
//!
//! Neither path draws the documents it moves. An export reads the query a chunk at a time from
//! the sidecar, spills each chunk to a temporary BSON file while learning the columns, and then
//! writes the workbook row by row in constant memory. The query runs once, and the app holds at
//! most one chunk of documents at a time.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::*;
use mongodb::bson::{Bson, Document};
use uuid::Uuid;

use crate::components::file_picker::{FileFilter, FilePickerMode, open_file_dialog_async};
use crate::components::{WriteRequest, request_connection_write};
use crate::connection::csv_utils::collect_document_columns;
use crate::state::StatusMessage;

use super::ForgeView;
use super::mongosh::{MongoshBridge, PageRequest};

/// Documents asked of the sidecar per round trip during an export.
const EXPORT_CHUNK: u64 = 5_000;
const EXPORT_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
const EXCEL_MAX_ROWS: u64 = 1_048_576;
const EXCEL_MAX_COLUMNS: usize = 16_384;
const EXCEL_MAX_STRING_LEN: usize = 32_767;
// Whole numbers beyond this lose digits as an Excel number, so they are written as text.
const EXCEL_EXACT_INTEGER: i64 = 1 << 53;

/// An export in progress, shown in the Forge status bar with a way to stop it.
#[derive(Clone)]
pub struct ForgeExportProgress {
    pub rows: Arc<AtomicU64>,
    pub cancelled: Arc<AtomicBool>,
    pub path: PathBuf,
}

impl ForgeExportProgress {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub enum ExportError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Export cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl<E: std::fmt::Display> From<E> for ExportError
where
    E: Into<Box<dyn std::error::Error>>,
{
    fn from(error: E) -> Self {
        Self::Failed(error.to_string())
    }
}

/// Where the documents of an export come from, a chunk at a time. `None` means the end.
pub trait DocumentSource {
    fn next_chunk(&mut self) -> Result<Option<Vec<Document>>, ExportError>;
}

struct SidecarExport {
    bridge: Arc<MongoshBridge>,
    session_id: Uuid,
    export_id: Option<String>,
}

impl DocumentSource for SidecarExport {
    fn next_chunk(&mut self) -> Result<Option<Vec<Document>>, ExportError> {
        let Some(export_id) = self.export_id.clone() else {
            return Ok(None);
        };
        let chunk = self
            .bridge
            .export_next(self.session_id, &export_id, EXPORT_CHUNK, EXPORT_CHUNK_TIMEOUT)
            .map_err(|error| ExportError::Failed(error.to_string()))?;
        if chunk.done {
            self.export_id = None;
        }
        let documents = super::logic::paged_documents(&chunk.documents);
        Ok(if documents.is_empty() && chunk.done { None } else { Some(documents) })
    }
}

impl Drop for SidecarExport {
    fn drop(&mut self) {
        // A cancelled or failed export closes its cursor on the server.
        if let Some(export_id) = self.export_id.take() {
            self.bridge.export_close(self.session_id, &export_id);
        }
    }
}

/// Write every document from `source` to an .xlsx file at `path`, atomically: the file only
/// appears once the whole workbook is written. Returns the number of rows.
pub fn write_documents_to_xlsx(
    source: &mut dyn DocumentSource,
    path: &Path,
    sheet_name: &str,
    rows: &AtomicU64,
    cancelled: &AtomicBool,
) -> Result<u64, ExportError> {
    use rust_xlsxwriter::{Format, Workbook};

    let check = || {
        if cancelled.load(Ordering::Relaxed) { Err(ExportError::Cancelled) } else { Ok(()) }
    };

    // Pass 1: read the query once, keep the documents on disk, and learn every column.
    let mut spill = BufWriter::new(tempfile::tempfile()?);
    let mut seen = HashSet::new();
    let mut columns = Vec::new();
    let mut count = 0u64;
    while let Some(chunk) = source.next_chunk()? {
        check()?;
        for document in chunk {
            collect_document_columns(&document, &mut seen, &mut columns);
            document.to_writer(&mut spill)?;
            count += 1;
        }
        if count >= EXCEL_MAX_ROWS {
            return Err(ExportError::Failed(format!(
                "The query returns more than {} rows, Excel's limit for one sheet. Narrow it with a filter or $limit.",
                super::mongosh::group_thousands(EXCEL_MAX_ROWS - 1)
            )));
        }
        rows.store(count, Ordering::Relaxed);
    }
    if columns.len() > EXCEL_MAX_COLUMNS {
        return Err(ExportError::Failed(format!(
            "The documents have {} different fields, more than Excel's {} columns.",
            super::mongosh::group_thousands(columns.len() as u64),
            super::mongosh::group_thousands(EXCEL_MAX_COLUMNS as u64)
        )));
    }

    // Pass 2: write the workbook from the spill file, one row in memory at a time.
    let mut spill = spill.into_inner().map_err(|error| ExportError::Failed(error.to_string()))?;
    spill.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(spill);

    let xlsx = |error: rust_xlsxwriter::XlsxError| ExportError::Failed(error.to_string());
    let mut workbook = Workbook::new();
    let header = Format::new().set_bold();
    let date = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let worksheet = workbook.add_worksheet_with_constant_memory();
    worksheet.set_name(excel_sheet_name(sheet_name)).map_err(xlsx)?;
    for (index, column) in columns.iter().enumerate() {
        worksheet.write_string_with_format(0, index as u16, column, &header).map_err(xlsx)?;
    }
    worksheet.set_freeze_panes(1, 0).map_err(xlsx)?;

    let column_index: std::collections::HashMap<&str, u16> =
        columns.iter().enumerate().map(|(index, name)| (name.as_str(), index as u16)).collect();
    for row in 1..=count {
        if row % 1000 == 0 {
            check()?;
        }
        let document = Document::from_reader(&mut reader)?;
        let mut cells = Vec::new();
        flatten_cells(&document, "", &mut cells);
        let row = row as u32;
        for (name, value) in cells {
            let Some(&col) = column_index.get(name.as_str()) else {
                continue;
            };
            write_cell(worksheet, row, col, &name, value, &date)?;
        }
    }
    check()?;

    let output = crate::connection::ops::export::AtomicExportFile::new(path)
        .map_err(|error| ExportError::Failed(error.to_string()))?;
    workbook.save(output.temporary_path()).map_err(xlsx)?;
    check()?;
    output.commit().map_err(|error| ExportError::Failed(error.to_string()))?;
    rows.store(count, Ordering::Relaxed);
    Ok(count)
}

/// Excel sheet names are at most 31 characters and cannot hold `[]:*?/\`.
fn excel_sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| if "[]:*?/\\".contains(ch) { '_' } else { ch })
        .collect::<String>()
        .trim_matches('\'')
        .chars()
        .take(31)
        .collect();
    if cleaned.trim().is_empty() { "Results".to_string() } else { cleaned }
}

/// The same columns `collect_document_columns` names: nested documents become dotted paths and
/// everything else, arrays included, is one cell.
fn flatten_cells<'a>(document: &'a Document, prefix: &str, out: &mut Vec<(String, &'a Bson)>) {
    for (key, value) in document {
        let name = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match value {
            Bson::Document(nested) => flatten_cells(nested, &name, out),
            other => out.push((name, other)),
        }
    }
}

/// Numbers stay numbers, booleans booleans and dates dates, so the sheet sorts and sums. Text
/// that looks like a number stays text: an account number keeps its leading zeros.
fn write_cell(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    column: &str,
    value: &Bson,
    date: &rust_xlsxwriter::Format,
) -> Result<(), ExportError> {
    let xlsx = |error: rust_xlsxwriter::XlsxError| ExportError::Failed(error.to_string());
    match value {
        Bson::Null | Bson::Undefined => {}
        Bson::Boolean(value) => {
            sheet.write_boolean(row, col, *value).map_err(xlsx)?;
        }
        Bson::Int32(value) => {
            sheet.write_number(row, col, *value).map_err(xlsx)?;
        }
        Bson::Int64(value) if value.unsigned_abs() <= EXCEL_EXACT_INTEGER as u64 => {
            sheet.write_number(row, col, *value as f64).map_err(xlsx)?;
        }
        Bson::Double(value) if value.is_finite() => {
            sheet.write_number(row, col, *value).map_err(xlsx)?;
        }
        Bson::DateTime(value) => {
            // Excel has no dates before 1900, so those are written as text.
            let whole_value = Bson::DateTime(*value);
            match chrono::DateTime::from_timestamp_millis(value.timestamp_millis())
                .filter(|date| chrono::Datelike::year(date) >= 1900)
                .and_then(|date| {
                    rust_xlsxwriter::ExcelDateTime::from_timestamp(date.timestamp())
                        .ok()
                        .map(|whole| (whole, date.timestamp_subsec_millis()))
                }) {
                Some((whole, millis)) => {
                    let serial = whole.to_excel() + f64::from(millis) / 86_400_000.0;
                    sheet.write_number_with_format(row, col, serial, date).map_err(xlsx)?;
                }
                None => write_text(sheet, row, col, column, &text_of(&whole_value))?,
            }
        }
        other => write_text(sheet, row, col, column, &text_of(other))?,
    }
    Ok(())
}

fn text_of(value: &Bson) -> String {
    match value {
        Bson::String(text) => text.clone(),
        Bson::ObjectId(id) => id.to_hex(),
        Bson::DateTime(date) => crate::bson::format_datetime_utc(*date),
        Bson::Decimal128(decimal) => decimal.to_string(),
        Bson::Int64(value) => value.to_string(),
        Bson::Double(value) => value.to_string(),
        other => crate::bson::bson_to_plain_json(other).to_string(),
    }
}

fn write_text(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    column: &str,
    text: &str,
) -> Result<(), ExportError> {
    if text.chars().count() > EXCEL_MAX_STRING_LEN {
        return Err(ExportError::Failed(format!(
            "A value in column '{column}' (row {row}) is longer than Excel's {EXCEL_MAX_STRING_LEN} characters per cell. Project it out or shorten it with $substrCP, then export again."
        )));
    }
    sheet.write_string(row, col, text).map_err(|error| ExportError::Failed(error.to_string()))?;
    Ok(())
}

fn default_export_name(collection: Option<&str>) -> String {
    let base = collection.filter(|name| !name.trim().is_empty()).unwrap_or("forge-results");
    let safe: String =
        base.chars().map(|ch| if "/\\:*?\"<>|".contains(ch) { '_' } else { ch }).collect();
    format!("{safe}_{}.xlsx", chrono::Local::now().format("%Y%m%d_%H%M%S"))
}

impl ForgeView {
    /// Run the code at hand (the selection, or else the whole editor) and write every document it
    /// returns to an .xlsx file, without showing them. The file is chosen first.
    pub fn export_query_to_excel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.runtime.export.is_some() {
            self.set_export_status("An export is already running.", true, cx);
            return;
        }
        if self.state.runtime.is_running {
            self.set_export_status("Wait for the running query to finish, then export.", true, cx);
            return;
        }
        let code = self.export_source_code(window, cx);
        if code.trim().is_empty() {
            self.set_export_status("There is no query to export.", true, cx);
            return;
        }
        let Some(key) = self.app_state.read(cx).active_forge_tab_key().cloned() else {
            return;
        };
        let collection = self.app_state.read(cx).forge_tab_collection(key.id).map(str::to_string);
        let default_name = default_export_name(collection.as_deref());
        // Choose the file first. The write gate comes after, so its confirmation (and a
        // Production authorization) is used at once by the export it approved, for the tab it
        // approved, rather than lapsing while a file dialog is open.
        let pick = cx.background_spawn(open_file_dialog_async(
            FilePickerMode::Save,
            vec![FileFilter::excel(), FileFilter::all()],
            Some(default_name),
        ));
        cx.spawn_in(window, async move |view, cx| {
            let Some(path) = pick.await else {
                return;
            };
            let _ = cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.gate_export(key, code, path, collection, window, cx);
                })
            });
        })
        .detach();
    }

    /// A Forge query can write, so exporting one goes through the same gate as running it, and
    /// the export starts inside the gate's callback against the tab that was approved.
    fn gate_export(
        &mut self,
        key: crate::state::ForgeTabKey,
        code: String,
        path: PathBuf,
        collection: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        request_connection_write(
            self.app_state.clone(),
            WriteRequest::new(
                key.connection_id,
                key.database.clone(),
                "Run Forge code that may write to MongoDB",
                None,
            ),
            window,
            cx,
            move |_window, cx| {
                view.update(cx, |view, cx| view.start_export(key, code, path, collection, cx));
            },
        );
    }

    fn export_source_code(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        if let Some(selection) = self.editor_selection_text(window, cx) {
            return selection;
        }
        self.state
            .editor
            .editor_state
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn start_export(
        &mut self,
        key: crate::state::ForgeTabKey,
        code: String,
        path: PathBuf,
        collection: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.state.runtime.export.is_some() || self.state.runtime.is_running {
            self.set_export_status(
                "Forge is busy. Try the export again when it is idle.",
                true,
                cx,
            );
            return;
        }
        // The approved tab must still be the one in front: the export runs in its shell.
        if self.app_state.read(cx).active_forge_tab_key() != Some(&key) {
            self.set_export_status(
                "The Forge tab changed before the export started. Nothing was run.",
                true,
                cx,
            );
            return;
        }
        // The same checks Run makes at the moment it executes.
        let (read_only, protected) = {
            let state = self.app_state.read(cx);
            (
                state.connection_read_only(key.connection_id),
                state.connection_requires_production_write_confirmation(key.connection_id),
            )
        };
        let authorized = !protected
            || self.app_state.update(cx, |state, _cx| {
                state.consume_production_write_authorization(key.connection_id)
            });
        if let Err(error) =
            super::runtime::ensure_forge_execution_allowed(read_only, protected, authorized)
        {
            self.set_export_status(&error.to_string(), true, cx);
            return;
        }
        let (session_id, uri, database, runtime_handle) = {
            let state = self.app_state.read(cx);
            match state.active_connection_tool_uri(key.connection_id) {
                Ok(uri) => {
                    (key.id, uri, key.database.clone(), state.connection_manager().runtime_handle())
                }
                Err(error) => {
                    self.set_export_status(&error.to_string(), true, cx);
                    return;
                }
            }
        };
        let Some(bridge) = self.ensure_mongosh() else {
            cx.notify();
            return;
        };
        let progress = ForgeExportProgress {
            rows: Arc::new(AtomicU64::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
            path: path.clone(),
        };
        self.state.runtime.export = Some(progress.clone());
        // The export uses the tab's shell session, so a run started meanwhile would interleave
        // with it. Marking the runtime busy keeps Run off until the export ends.
        self.state.runtime.is_running = true;
        self.set_export_status("Exporting to Excel…", false, cx);
        cx.notify();

        let sheet = collection.unwrap_or_else(|| "Results".to_string());
        let worker_progress = progress.clone();
        let task = runtime_handle.spawn_blocking(move || {
            bridge
                .ensure_session(session_id, &uri, &database)
                .map_err(|error| ExportError::Failed(error.to_string()))?;
            let opened = bridge
                .export_open(session_id, &code, Duration::from_secs(120))
                .map_err(|error| ExportError::Failed(error.to_string()))?;
            let mut source =
                SidecarExport { bridge, session_id, export_id: Some(opened.export_id) };
            write_documents_to_xlsx(
                &mut source,
                &path,
                &sheet,
                &worker_progress.rows,
                &worker_progress.cancelled,
            )
        });

        // Redraw the row count while the export runs.
        let ticker_progress = progress.clone();
        cx.spawn(async move |view: WeakEntity<ForgeView>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let running = view
                    .update(cx, |view, cx| {
                        let running =
                            view.state.runtime.export.as_ref().is_some_and(|export| {
                                Arc::ptr_eq(&export.rows, &ticker_progress.rows)
                            });
                        if running {
                            cx.notify();
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |view: WeakEntity<ForgeView>, cx: &mut AsyncApp| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(ExportError::Failed(error.to_string())),
            };
            let _ = view.update(cx, |view, cx| {
                // Only the export that set the busy state may clear it.
                let ours = view
                    .state
                    .runtime
                    .export
                    .as_ref()
                    .is_some_and(|export| Arc::ptr_eq(&export.rows, &progress.rows));
                if ours {
                    view.state.runtime.export = None;
                    view.state.runtime.is_running = false;
                }
                match result {
                    Ok(rows) => {
                        let name = progress
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default();
                        view.set_export_status(
                            &format!(
                                "Exported {} rows to {name}",
                                super::mongosh::group_thousands(rows)
                            ),
                            false,
                            cx,
                        );
                    }
                    Err(ExportError::Cancelled) => {
                        view.set_export_status("Export cancelled; no file was written.", false, cx)
                    }
                    Err(ExportError::Failed(message)) => {
                        view.set_export_status(&format!("Export failed: {message}"), true, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel_export(&mut self, cx: &mut Context<Self>) {
        if let Some(export) = &self.state.runtime.export {
            export.cancel();
            self.set_export_status("Cancelling export…", false, cx);
            cx.notify();
        }
    }

    fn set_export_status(&self, message: &str, error: bool, cx: &mut App) {
        let message = message.to_string();
        self.app_state.update(cx, |state, cx| {
            state.set_status_message(Some(if error {
                StatusMessage::error(message)
            } else {
                StatusMessage::info(message)
            }));
            cx.notify();
        });
    }

    /// First / previous / next / last and the page size, over a result the sidecar keeps. Shown
    /// only for paged results; a result that fits one page still says how many it holds.
    pub fn render_result_pager(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::ActiveTheme as _;
        use gpui_kit::component::Disableable as _;
        use gpui_kit::component::Sizable as _;
        use gpui_kit::component::button::ButtonVariants as _;
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
        use gpui_kit::prelude::FluentBuilder as _;

        let page = self.state.output.result_pages.get(self.state.output.result_page_index)?;
        let paging = page.paging.clone()?;
        let busy = page.paging_busy || self.state.runtime.is_running;
        let at_start = paging.page == 0;
        let at_end = !paging.has_more;
        let view = cx.entity();
        let nav = |id: &'static str,
                   icon: gpui_kit::assets::IconName,
                   tooltip: &'static str,
                   disabled: bool,
                   request: PageRequest| {
            let view = view.clone();
            crate::components::Button::new(id)
                .ghost()
                .xsmall()
                .icon(gpui_kit::component::Icon::new(icon).xsmall())
                .tooltip(tooltip)
                .disabled(busy || disabled)
                .on_click(move |_, _window, cx| {
                    view.update(cx, |view, cx| view.go_to_result_page(request, cx));
                })
        };

        let page_size = paging.page_size;
        let size_view = view.clone();
        let size_menu = crate::components::Button::new("forge-page-size")
            .ghost()
            .xsmall()
            .label(format!("{} / page", super::mongosh::group_thousands(page_size)))
            .dropdown_caret(true)
            .disabled(busy)
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu: PopupMenu, _, _| {
                for &size in crate::state::settings::FORGE_PAGE_SIZES {
                    let view = size_view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(super::mongosh::group_thousands(size))
                            .checked(size == page_size)
                            .on_click(move |_, _window, cx| {
                                view.update(cx, |view, cx| view.set_result_page_size(size, cx));
                            }),
                    );
                }
                menu
            });

        let page_label = match paging.total_pages() {
            Some(pages) => format!(
                "Page {} of {}",
                super::mongosh::group_thousands(paging.page + 1),
                super::mongosh::group_thousands(pages)
            ),
            None => format!("Page {}", super::mongosh::group_thousands(paging.page + 1)),
        };

        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(crate::theme::spacing::sm())
                .px(crate::theme::spacing::sm())
                .py(px(2.0))
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::theme::spacing::xs())
                        .child(size_menu)
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} documents", paging.range_label())),
                        )
                        .when(busy && page.paging_busy, |row| {
                            row.child(gpui_kit::component::spinner::Spinner::new().xsmall())
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(nav(
                            "forge-page-first",
                            gpui_kit::assets::IconName::ChevronFirst,
                            "First page",
                            at_start,
                            PageRequest::Index(0),
                        ))
                        .child(nav(
                            "forge-page-prev",
                            gpui_kit::assets::IconName::ChevronLeft,
                            "Previous page",
                            at_start,
                            PageRequest::Index(paging.page.saturating_sub(1)),
                        ))
                        .child(
                            div()
                                .px(crate::theme::spacing::xs())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(page_label),
                        )
                        .child(nav(
                            "forge-page-next",
                            gpui_kit::assets::IconName::ChevronRight,
                            "Next page",
                            at_end,
                            PageRequest::Index(paging.page + 1),
                        ))
                        .child(nav(
                            "forge-page-last",
                            gpui_kit::assets::IconName::ChevronLast,
                            "Last page (reads the rest of the result)",
                            at_end,
                            PageRequest::Last,
                        )),
                )
                .into_any_element(),
        )
    }

    /// Change how many documents a page holds, save it, and show the page holding the first
    /// document of the current one.
    pub fn set_result_page_size(&mut self, size: u64, cx: &mut Context<Self>) {
        self.app_state.update(cx, |state, cx| {
            state.settings.forge_page_size = size;
            state.save_settings();
            cx.notify();
        });
        let index = self.state.output.result_page_index;
        let Some(page) = self.state.output.result_pages.get_mut(index) else {
            return;
        };
        let Some(paging) = page.paging.as_mut() else {
            return;
        };
        let first = paging.offset;
        paging.page_size = size;
        self.go_to_result_page(PageRequest::Index(first / size.max(1)), cx);
    }

    /// Fetch another page of the current result from the sidecar.
    pub fn go_to_result_page(&mut self, request: PageRequest, cx: &mut Context<Self>) {
        let index = self.state.output.result_page_index;
        let Some(page) = self.state.output.result_pages.get(index) else {
            return;
        };
        let (Some(paging), Some(session_id)) = (page.paging.clone(), page.paging_session) else {
            return;
        };
        if page.paging_busy || self.state.runtime.is_running {
            return;
        }
        let page_id = page.id;
        let page_size = paging.page_size;
        let Some(bridge) = self.ensure_mongosh() else {
            cx.notify();
            return;
        };
        let runtime_handle = self.app_state.read(cx).connection_manager().runtime_handle();
        if let Some(page) = self.state.output.result_pages.get_mut(index) {
            page.paging_busy = true;
        }
        cx.notify();

        let result_id = paging.result_id.clone();
        let task = runtime_handle.spawn_blocking(move || {
            bridge.page(session_id, &result_id, request, page_size, Duration::from_secs(120))
        });
        cx.spawn(async move |view: WeakEntity<ForgeView>, cx: &mut AsyncApp| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                let Some(page) =
                    view.state.output.result_pages.iter_mut().find(|page| page.id == page_id)
                else {
                    return;
                };
                page.paging_busy = false;
                match result {
                    Ok(Ok(fetched)) => {
                        let documents = super::logic::paged_documents(&fetched.printable);
                        page.documents = Arc::new(
                            documents
                                .into_iter()
                                .enumerate()
                                .map(|(index, doc)| crate::state::SessionDocument {
                                    key: crate::bson::DocumentKey::from_document(
                                        &doc,
                                        fetched.paging.offset as usize + index,
                                    ),
                                    doc,
                                })
                                .collect(),
                        );
                        page.paging = Some(fetched.paging);
                        page.expanded_nodes.clear();
                        page.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        view.state.output.result_inline_edit = None;
                        view.state.output.result_inline_subscription = None;
                        view.state.output.result_table_page_id = None;
                    }
                    Ok(Err(error)) => {
                        view.set_export_status(&error.to_string(), true, cx);
                    }
                    Err(error) => {
                        view.set_export_status(&error.to_string(), true, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Let the sidecar close the cursors behind result pages that are going away.
    pub fn release_paged_results(&self, pages: &[super::types::ResultPage]) {
        let kept: Vec<(Uuid, String)> = pages
            .iter()
            .filter_map(|page| {
                Some((page.paging_session?, page.paging.as_ref()?.result_id.clone()))
            })
            .collect();
        if kept.is_empty() {
            return;
        }
        let Ok(bridge) = self.controller.runtime.ensure_bridge() else {
            return;
        };
        std::thread::spawn(move || {
            for (session_id, result_id) in kept {
                bridge.release_result(session_id, &result_id);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings gpui's `test` macro in over the standard one.
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use mongodb::bson::{Bson, DateTime, Decimal128, Document, doc, oid::ObjectId};

    use super::{
        DocumentSource, EXCEL_MAX_STRING_LEN, ExportError, default_export_name, excel_sheet_name,
        write_documents_to_xlsx,
    };

    struct Chunks(Vec<Vec<Document>>);

    impl DocumentSource for Chunks {
        fn next_chunk(&mut self) -> Result<Option<Vec<Document>>, ExportError> {
            Ok(if self.0.is_empty() { None } else { Some(self.0.remove(0)) })
        }
    }

    /// The cells of the first sheet, read back out of the .xlsx zip.
    fn sheet_xml(path: &Path) -> String {
        let file = std::fs::File::open(path).expect("xlsx exists");
        let mut archive = zip::ZipArchive::new(file).expect("xlsx is a zip");
        let mut sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("sheet1");
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut sheet, &mut xml).expect("utf8");
        xml
    }

    fn export(
        chunks: Vec<Vec<Document>>,
    ) -> (tempfile::TempDir, PathBuf, Result<u64, ExportError>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.xlsx");
        let result = write_documents_to_xlsx(
            &mut Chunks(chunks),
            &path,
            "orders",
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        );
        (dir, path, result)
    }

    #[test]
    fn every_document_across_chunks_becomes_a_row_with_columns_from_all_of_them() {
        let first: Vec<Document> =
            (0..5_000).map(|index| doc! { "_id": index, "name": index.to_string() }).collect();
        // A field that first appears in a later chunk still gets its column.
        let second: Vec<Document> =
            (5_000..18_000).map(|index| doc! { "_id": index, "late": true }).collect();
        let (_dir, path, result) = export(vec![first, second]);
        assert_eq!(result.unwrap(), 18_000);
        let xml = sheet_xml(&path);
        assert!(xml.contains(r#"<dimension ref="A1:C18001"/>"#), "{}", &xml[..400]);
    }

    #[test]
    fn cells_keep_their_types() {
        let when = DateTime::parse_rfc3339_str("2024-01-31T09:30:00.500Z").unwrap();
        let mut document = doc! {
            "_id": ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap(),
            "count": 7_i32,
            "big": 9_007_199_254_740_993_i64,
            "price": Decimal128::from_bytes([0; 16]),
            "ok": true,
        };
        document.insert("zip", "00501");
        document.insert("when", when);
        document.insert("address", doc! { "city": "Austin" });
        document.insert("tags", vec!["a", "b"]);
        document.insert("none", Bson::Null);
        let (_dir, path, result) = export(vec![vec![document]]);
        assert_eq!(result.unwrap(), 1);
        let xml = sheet_xml(&path);
        // Numbers and booleans are typed cells; text that looks numeric stays text.
        assert!(xml.contains("<v>7</v>"));
        assert!(xml.contains(r#"t="b"><v>1</v>"#));
        assert!(xml.contains("<is><t>00501</t></is>") || xml.contains(">00501<"), "{xml}");
        // An Int64 past 2^53 would lose digits as a number, so it is text.
        assert!(xml.contains("9007199254740993"));
        // The date is a formatted serial number, not text.
        assert!(xml.contains("<v>45322.39"), "{xml}");
        assert!(xml.contains("507f1f77bcf86cd799439011"));
        assert!(xml.contains("Austin"));
        assert!(xml.contains("[&quot;a&quot;,&quot;b&quot;]") || xml.contains(r#"["a","b"]"#));
    }

    #[test]
    fn a_cancelled_export_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.xlsx");
        let result = write_documents_to_xlsx(
            &mut Chunks(vec![vec![doc! { "a": 1 }]]),
            &path,
            "x",
            &AtomicU64::new(0),
            &AtomicBool::new(true),
        );
        assert!(matches!(result, Err(ExportError::Cancelled)));
        assert!(!path.exists());
    }

    #[test]
    fn an_over_long_cell_fails_loudly_instead_of_truncating() {
        let (_dir, path, result) =
            export(vec![vec![doc! { "note": "x".repeat(EXCEL_MAX_STRING_LEN + 1) }]]);
        let Err(ExportError::Failed(message)) = result else { panic!("expected failure") };
        assert!(message.contains("column 'note'"), "{message}");
        assert!(!path.exists());
    }

    #[test]
    fn sheet_and_file_names_are_made_safe() {
        assert_eq!(excel_sheet_name("orders/2024:[q1]"), "orders_2024__q1_");
        assert_eq!(excel_sheet_name(&"x".repeat(40)).len(), 31);
        assert_eq!(excel_sheet_name("''"), "Results");
        assert!(default_export_name(Some("a/b")).starts_with("a_b_"));
        assert!(default_export_name(None).starts_with("forge-results_"));
    }

    /// End to end against a real server, through the compiled sidecar and the real bridge:
    /// paging reads a page at a time and an export writes every row. Needs
    /// `FORGE_TEST_MONGODB_URI` pointing at a disposable server, and `just build-sidecar`.
    #[test]
    #[ignore = "needs FORGE_TEST_MONGODB_URI and a built sidecar"]
    fn live_paging_and_export_of_18k_documents() {
        use super::super::mongosh::{MongoshBridge, PageRequest};
        use std::time::Duration;

        let uri = std::env::var("FORGE_TEST_MONGODB_URI").expect("FORGE_TEST_MONGODB_URI");
        let bridge = MongoshBridge::new().expect("sidecar");
        let session = uuid::Uuid::new_v4();
        let database = "forge_large_results_test";
        bridge.ensure_session(session, &uri, database).expect("session");
        let timeout = Duration::from_secs(120);
        bridge
            .evaluate(
                session,
                "db.big.drop(); db.big.insertMany(Array.from({length: 18000}, (_, i) => ({_id: i, n: i, when: new Date(Date.UTC(2024, 0, 1)), nested: {k: 'v' + i}})))",
                None,
                timeout,
            )
            .expect("seed");

        // Control: without a page size the result prints the way it did before, whole.
        let whole = bridge
            .evaluate(session, "db.big.find({}).sort({_id: 1}).toArray()", None, timeout)
            .expect("unpaged");
        assert_eq!(whole.printable.as_array().map(Vec::len), Some(18_000));
        assert!(whole.paging.is_none());

        for code in ["db.big.find({}).sort({_id: 1})", "db.big.find({}).sort({_id: 1}).toArray()"] {
            let first = bridge
                .evaluate_paged(session, code, None, Some(1000), timeout)
                .expect("paged evaluate");
            let paging = first.paging.clone().expect("paging");
            assert_eq!(super::super::logic::paged_documents(&first.printable).len(), 1000);
            assert!(paging.has_more, "{code}");

            let second = bridge
                .page(session, &paging.result_id, PageRequest::Index(1), 1000, timeout)
                .expect("page 2");
            let docs = super::super::logic::paged_documents(&second.printable);
            assert_eq!(docs[0].get("_id"), Some(&Bson::Int32(1000)), "{code}");

            let last = bridge
                .page(session, &paging.result_id, PageRequest::Last, 1000, timeout)
                .expect("last");
            assert_eq!(last.paging.total, Some(18_000));
            assert_eq!(last.paging.page, 17);
            assert_eq!(last.paging.range_label(), "17,001–18,000 of 18,000");
        }

        // A query's own limit is kept; the old preview re-run replaced it with 50.
        let limited = bridge
            .evaluate_paged(session, "db.big.find({}).limit(5)", None, Some(1000), timeout)
            .expect("limited");
        assert_eq!(limited.paging.map(|paging| paging.total), Some(Some(5)));

        let opened =
            bridge.export_open(session, "db.big.find({}).sort({_id: 1})", timeout).expect("open");
        let mut source = super::SidecarExport {
            bridge: bridge.clone(),
            session_id: session,
            export_id: Some(opened.export_id),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.xlsx");
        let rows = write_documents_to_xlsx(
            &mut source,
            &path,
            "big",
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        )
        .expect("export");
        assert_eq!(rows, 18_000);
        let xml = sheet_xml(&path);
        assert!(xml.contains(r#"<dimension ref="A1:D18001"/>"#), "{}", &xml[..300]);

        let refused = bridge.export_open(session, "db.big.countDocuments()", timeout).unwrap_err();
        assert!(refused.to_string().contains("needs a query that returns documents"));

        let _ = bridge.evaluate(session, "db.dropDatabase()", None, timeout);
        let _ = bridge.dispose_session(session);
    }
}
