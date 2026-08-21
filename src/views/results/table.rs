use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_component::{Icon, IconName, Sizable as _};
use mongodb::bson::{Bson, Document};

use crate::views::documents::export::{
    CopyFormat, ViewExportSnapshot, render_csv_with_headers, render_to_clipboard,
    render_tsv_with_headers,
};
use crate::views::documents::table::cell_renderer;
use crate::views::documents::table::column_schema::discover_columns_raw;
use crate::views::documents::table::table_columns::TableColumns;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultCopyFormat {
    Json,
    ExcelHeaders,
    ExcelNoHeaders,
    CsvHeaders,
    CsvNoHeaders,
}

impl ResultCopyFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::ExcelHeaders => "Excel (headers)",
            Self::ExcelNoHeaders => "Excel (no headers)",
            Self::CsvHeaders => "CSV (headers)",
            Self::CsvNoHeaders => "CSV (no headers)",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultTableSelection {
    selected_rows: BTreeSet<usize>,
    anchor_row: Option<usize>,
}

impl ResultTableSelection {
    pub fn click(&mut self, row: usize, row_count: usize, shift: bool, command: bool) {
        if row >= row_count {
            return;
        }
        if shift {
            let anchor = self.anchor_row.unwrap_or(row);
            let low = anchor.min(row);
            let high = anchor.max(row).min(row_count.saturating_sub(1));
            self.selected_rows = (low..=high).collect();
        } else if command {
            if !self.selected_rows.remove(&row) {
                self.selected_rows.insert(row);
            }
            self.anchor_row = Some(row);
        } else {
            self.selected_rows.clear();
            self.selected_rows.insert(row);
            self.anchor_row = Some(row);
        }
    }

    pub fn select_all(&mut self, row_count: usize) {
        self.selected_rows = (0..row_count).collect();
        self.anchor_row = if row_count == 0 { None } else { Some(0) };
    }

    pub fn clear(&mut self) {
        self.selected_rows.clear();
        self.anchor_row = None;
    }

    pub fn contains(&self, row: usize) -> bool {
        self.selected_rows.contains(&row)
    }

    pub fn rows_or_all(&self, row_count: usize) -> Vec<usize> {
        if self.selected_rows.is_empty() {
            (0..row_count).collect()
        } else {
            self.selected_rows.iter().copied().filter(|row| *row < row_count).collect()
        }
    }
}

pub type ResultCellEditCallback = Rc<dyn Fn(usize, String, &mut Window, &mut App)>;
pub type ResultBoolEditCallback = Rc<dyn Fn(usize, String, bool, &mut Window, &mut App)>;

pub struct ResultTableDelegate {
    pub table_cols: TableColumns,
    documents: Vec<Document>,
    source_rows: Vec<usize>,
    selection: ResultTableSelection,
    editable: bool,
    on_edit: ResultCellEditCallback,
    on_bool_edit: ResultBoolEditCallback,
    database: String,
    collection: String,
    inline_editor: Option<(usize, String, Entity<InputState>)>,
}

impl ResultTableDelegate {
    pub fn new(on_edit: ResultCellEditCallback, on_bool_edit: ResultBoolEditCallback) -> Self {
        Self {
            table_cols: TableColumns::new(),
            documents: Vec::new(),
            source_rows: Vec::new(),
            selection: ResultTableSelection::default(),
            editable: false,
            on_edit,
            on_bool_edit,
            database: String::new(),
            collection: String::new(),
            inline_editor: None,
        }
    }

    pub fn refresh_data(
        &mut self,
        documents: Vec<Document>,
        editable: bool,
        database: String,
        collection: String,
    ) {
        self.table_cols = TableColumns::new();
        self.table_cols.refresh_columns(discover_columns_raw(&documents));
        self.source_rows = (0..documents.len()).collect();
        self.documents = documents;
        self.editable = editable;
        self.database = database;
        self.collection = collection;
        self.selection.clear();
    }

    pub fn select_all(&mut self) {
        self.selection.select_all(self.documents.len());
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub fn set_editable(&mut self, editable: bool) {
        self.editable = editable;
        if !editable {
            self.inline_editor = None;
        }
    }

    pub fn set_inline_editor(&mut self, editor: Option<(usize, String, Entity<InputState>)>) {
        self.inline_editor = editor;
    }

    pub fn copy_text(&self, format: ResultCopyFormat) -> String {
        let documents = self
            .selection
            .rows_or_all(self.documents.len())
            .into_iter()
            .filter_map(|row| self.documents.get(row).cloned())
            .collect();
        let columns = self.table_cols.columns.iter().map(|column| column.key.clone()).collect();
        render_result_copy(documents, columns, &self.database, &self.collection, format)
    }

    pub fn apply_column_move(&mut self, from: usize, to: usize) {
        self.table_cols.apply_column_move(from, to);
    }

    fn cell_value(&self, row: usize, column: usize) -> Option<&Bson> {
        let key = &self.table_cols.columns.get(column)?.key;
        self.documents.get(row)?.get(key)
    }

    fn source_row(&self, row: usize) -> Option<usize> {
        self.source_rows.get(row).copied()
    }
}

pub fn render_result_copy(
    documents: Vec<Document>,
    columns: Vec<String>,
    database: &str,
    collection: &str,
    format: ResultCopyFormat,
) -> String {
    let snapshot = ViewExportSnapshot::from_documents_with_columns(
        documents,
        columns,
        collection.to_string(),
        database.to_string(),
    );
    match format {
        ResultCopyFormat::Json => render_to_clipboard(&snapshot, CopyFormat::Json),
        ResultCopyFormat::ExcelHeaders => render_tsv_with_headers(&snapshot, true),
        ResultCopyFormat::ExcelNoHeaders => render_tsv_with_headers(&snapshot, false),
        ResultCopyFormat::CsvHeaders => render_csv_with_headers(&snapshot, true),
        ResultCopyFormat::CsvNoHeaders => render_csv_with_headers(&snapshot, false),
    }
}

impl TableDelegate for ResultTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.table_cols.column_defs.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.documents.len()
    }

    fn column(&self, column: usize, _cx: &App) -> &Column {
        &self.table_cols.column_defs[column]
    }

    fn perform_sort(
        &mut self,
        column: usize,
        sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(key) = self.table_cols.columns.get(column).map(|column| column.key.clone()) else {
            return;
        };
        let mut rows: Vec<_> = self.source_rows.drain(..).zip(self.documents.drain(..)).collect();
        match sort {
            ColumnSort::Ascending => {
                rows.sort_by(|(_, left), (_, right)| compare_bson(left.get(&key), right.get(&key)));
                self.table_cols.active_sort = Some((key, sort));
            }
            ColumnSort::Descending => {
                rows.sort_by(|(_, left), (_, right)| compare_bson(right.get(&key), left.get(&key)));
                self.table_cols.active_sort = Some((key, sort));
            }
            ColumnSort::Default => {
                rows.sort_by_key(|(source_row, _)| *source_row);
                self.table_cols.active_sort = None;
            }
        }
        (self.source_rows, self.documents) = rows.into_iter().unzip();
        self.selection.clear();
        self.table_cols.rebuild_column_defs();
        cx.notify();
    }

    fn render_tr(
        &mut self,
        row: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let selected = self.selection.contains(row);
        div()
            .id(("forge-result-row", row))
            .when(selected, |element| element.bg(cx.theme().list_active))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    let row_count = table.delegate().documents.len();
                    table.delegate_mut().selection.click(
                        row,
                        row_count,
                        event.modifiers.shift,
                        event.modifiers.secondary() || event.modifiers.control,
                    );
                    cx.notify();
                }),
            )
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(value) = self.cell_value(row, column).cloned() else {
            return div().text_xs().text_color(cx.theme().muted_foreground).into_any_element();
        };
        let Some(source_row) = self.source_row(row) else {
            return div().into_any_element();
        };
        let key = self.table_cols.columns[column].key.clone();
        let safe_path =
            crate::bson::DottedPath::new(&[crate::bson::PathSegment::Key(key.clone())]).is_ok();
        if let Some((edit_row, edit_key, input)) = &self.inline_editor
            && *edit_row == source_row
            && edit_key == &key
        {
            return Input::new(input).small().flex_1().into_any_element();
        }
        let has_id = self.documents.get(row).is_some_and(|document| document.contains_key("_id"));
        let can_edit = self.editable
            && has_id
            && safe_path
            && key != "_id"
            && crate::bson::is_editable_value(
                &value,
                &[crate::bson::PathSegment::Key(key.clone())],
            )
            && !matches!(value, Bson::Boolean(_));
        let on_edit = self.on_edit.clone();
        if self.editable
            && has_id
            && safe_path
            && key != "_id"
            && let Bson::Boolean(current) = value
        {
            let on_bool_edit = self.on_bool_edit.clone();
            return div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    Switch::new(("forge-table-bool", row * 64 + column))
                        .checked(current)
                        .small()
                        .on_click(move |checked, window, cx| {
                            on_bool_edit(source_row, key.clone(), *checked, window, cx);
                        }),
                )
                .child(if current { "true" } else { "false" })
                .into_any_element();
        }
        div()
            .size_full()
            .when(can_edit, |element| {
                element.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    if event.click_count == 2 {
                        cx.stop_propagation();
                        on_edit(source_row, key.clone(), window, cx);
                    }
                })
            })
            .child(cell_renderer::render_cell(&value, row, column, cx))
            .into_any_element()
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No documents returned")
    }

    fn context_menu(
        &mut self,
        row: usize,
        selected_column: Option<usize>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let formats = [
            ResultCopyFormat::Json,
            ResultCopyFormat::ExcelHeaders,
            ResultCopyFormat::ExcelNoHeaders,
            ResultCopyFormat::CsvHeaders,
            ResultCopyFormat::CsvNoHeaders,
        ];
        let table = cx.entity();
        let copy_menu = PopupMenu::build(window, cx, move |mut submenu, _window, _cx| {
            for format in formats {
                let table = table.clone();
                submenu = submenu.item(PopupMenuItem::new(format.label()).on_click(
                    move |_, _window, cx| {
                        let text = table.read(cx).delegate().copy_text(format);
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    },
                ));
            }
            submenu
        });
        menu = menu.item(PopupMenuItem::submenu("Copy", copy_menu).icon(Icon::new(IconName::Copy)));

        if self.editable
            && self.documents.get(row).is_some_and(|document| document.contains_key("_id"))
            && let Some(column) = selected_column
            && let Some(key) = self.table_cols.columns.get(column).map(|column| column.key.clone())
            && key != "_id"
            && crate::bson::DottedPath::new(&[crate::bson::PathSegment::Key(key.clone())]).is_ok()
            && matches!(self.cell_value(row, column), Some(Bson::Document(_) | Bson::Array(_)))
        {
            let on_edit = self.on_edit.clone();
            let source_row = self.source_row(row).unwrap_or(row);
            menu = menu.separator().item(
                PopupMenuItem::new("Edit Value…")
                    .on_click(move |_, window, cx| on_edit(source_row, key.clone(), window, cx)),
            );
        }
        menu
    }

    fn move_column(
        &mut self,
        column: usize,
        to: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.apply_column_move(column, to);
        self.table_cols.rebuild_column_defs();
        cx.notify();
    }
}

fn compare_bson(left: Option<&Bson>, right: Option<&Bson>) -> std::cmp::Ordering {
    match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(Bson::Int32(left)), Some(Bson::Int32(right))) => left.cmp(right),
        (Some(Bson::Int64(left)), Some(Bson::Int64(right))) => left.cmp(right),
        (Some(Bson::Double(left)), Some(Bson::Double(right))) => left.total_cmp(right),
        (Some(Bson::String(left)), Some(Bson::String(right))) => left.cmp(right),
        (Some(Bson::Boolean(left)), Some(Bson::Boolean(right))) => left.cmp(right),
        (Some(Bson::DateTime(left)), Some(Bson::DateTime(right))) => {
            left.timestamp_millis().cmp(&right.timestamp_millis())
        }
        (Some(left), Some(right)) => crate::bson::bson_value_preview(left, 100)
            .cmp(&crate::bson::bson_value_preview(right, 100)),
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;
    use std::rc::Rc;

    use super::{ResultCopyFormat, ResultTableDelegate, ResultTableSelection, render_result_copy};

    #[test]
    fn selection_supports_single_command_shift_and_select_all() {
        let mut selection = ResultTableSelection::default();
        selection.click(2, 6, false, false);
        assert_eq!(selection.rows_or_all(6), vec![2]);
        selection.click(4, 6, false, true);
        assert_eq!(selection.rows_or_all(6), vec![2, 4]);
        selection.click(5, 6, true, false);
        assert_eq!(selection.rows_or_all(6), vec![4, 5]);
        selection.select_all(3);
        assert_eq!(selection.rows_or_all(3), vec![0, 1, 2]);
    }

    #[test]
    fn no_selection_copies_all_visible_rows_with_requested_headers() {
        let documents = vec![doc! { "_id": 1, "name": "Ada" }, doc! { "_id": 2, "name": "Lin" }];
        let columns = vec!["_id".into(), "name".into()];
        assert_eq!(
            render_result_copy(
                documents.clone(),
                columns.clone(),
                "test",
                "people",
                ResultCopyFormat::ExcelHeaders,
            ),
            "_id\tname\n1\tAda\n2\tLin\n"
        );
        assert_eq!(
            render_result_copy(
                documents,
                columns,
                "test",
                "people",
                ResultCopyFormat::CsvNoHeaders,
            ),
            "1,Ada\n2,Lin\n"
        );
    }

    #[test]
    fn delegate_copy_uses_selected_rows_instead_of_all_rows() {
        let mut delegate =
            ResultTableDelegate::new(Rc::new(|_, _, _, _| {}), Rc::new(|_, _, _, _, _| {}));
        delegate.refresh_data(
            vec![doc! { "_id": 1, "name": "Ada" }, doc! { "_id": 2, "name": "Lin" }],
            false,
            "test".into(),
            "people".into(),
        );
        delegate.selection.click(1, 2, false, false);
        assert_eq!(delegate.copy_text(ResultCopyFormat::ExcelHeaders), "_id\tname\n2\tLin\n");
    }
}
