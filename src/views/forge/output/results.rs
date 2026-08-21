use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::table::{Table, TableState};
use gpui_component::{Icon, IconName};
use mongodb::bson::Document;

use crate::components::Button;
use crate::theme::{fonts, spacing};
use crate::views::documents::tree::lazy_tree::collect_all_expandable_nodes;
use crate::views::results::table::ResultTableDelegate;
use crate::views::results::{
    ResultEmptyState, ResultViewMode, ResultViewProps, render_results_view,
};

use super::super::ForgeView;
use super::super::controller::ForgeController;
use super::super::types::{ForgeOutputTab, ResultOrigin, ResultPage};
use super::pipeline::ResultKind;
use super::pipeline::classify_result;
use crate::bson::DocumentKey;
use crate::state::SessionDocument;
use std::hash::{Hash, Hasher};

fn results_signature(documents: &[SessionDocument]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    documents.len().hash(&mut hasher);
    for doc in documents {
        doc.key.hash(&mut hasher);
    }
    hasher.finish()
}

impl ForgeView {
    fn ensure_result_table_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<ResultTableDelegate>> {
        if let Some(table) = &self.state.output.result_table_state {
            return table.clone();
        }
        let view = cx.entity();
        let on_edit = Rc::new(move |row: usize, key: String, window: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| {
                view.begin_result_edit(row, vec![crate::bson::PathSegment::Key(key)], window, cx);
            });
        });
        let view = cx.entity();
        let on_bool_edit = Rc::new(
            move |row: usize, key: String, value: bool, window: &mut Window, cx: &mut App| {
                view.update(cx, |view, cx| {
                    view.toggle_result_bool(
                        row,
                        vec![crate::bson::PathSegment::Key(key)],
                        value,
                        window,
                        cx,
                    );
                });
            },
        );
        let table = cx.new(|cx| {
            TableState::new(ResultTableDelegate::new(on_edit, on_bool_edit), window, cx)
                .col_selectable(false)
                .col_movable(true)
                .row_selectable(false)
        });
        self.state.output.result_table_state = Some(table.clone());
        table
    }

    fn sync_result_table(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TableState<ResultTableDelegate>>> {
        let page = self.state.output.result_pages.get(self.state.output.result_page_index)?.clone();
        let documents = self.current_result_documents()?;
        let signature = results_signature(&documents);
        let editable = self.current_result_is_editable(cx);
        let table = self.ensure_result_table_state(window, cx);
        if self.state.output.result_table_page_id != Some(page.id)
            || self.state.output.result_table_signature != Some(signature)
        {
            table.update(cx, |table, cx| {
                table.delegate_mut().refresh_data(
                    page.docs,
                    editable,
                    page.origin.database,
                    page.origin.collection.unwrap_or_default(),
                );
                table.refresh(cx);
            });
            self.state.output.result_table_page_id = Some(page.id);
            self.state.output.result_table_signature = Some(signature);
        }
        let inline_editor = if editable {
            self.state.output.result_inline_edit.as_ref().and_then(|edit| {
                match edit.target.path.as_slice() {
                    [crate::bson::PathSegment::Key(key)] => {
                        Some((edit.target.doc_index, key.clone(), edit.input.clone()))
                    }
                    _ => None,
                }
            })
        } else {
            None
        };
        table.update(cx, |table, _cx| {
            table.delegate_mut().set_editable(editable);
            table.delegate_mut().set_inline_editor(inline_editor);
        });
        Some(table)
    }

    pub fn ensure_results_search_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(state) = self.state.output.results_search_state.as_ref() {
            return state.clone();
        }

        let search_state = cx
            .new(|cx| InputState::new(window, cx).placeholder("Search results").clean_on_escape());

        let subscription =
            cx.subscribe_in(&search_state, window, move |this, state, event, _window, cx| {
                if let InputEvent::Change = event {
                    let value = state.read(cx).value().to_string();
                    if value != this.state.output.results_search_query {
                        this.state.output.results_search_query = value;
                        cx.notify();
                    }
                }
            });

        self.state.output.results_search_subscription = Some(subscription);
        self.state.output.results_search_state = Some(search_state.clone());
        search_state
    }

    pub fn current_result_documents(&self) -> Option<Arc<Vec<SessionDocument>>> {
        let page = self.state.output.result_pages.get(self.state.output.result_page_index)?;
        let documents: Arc<Vec<SessionDocument>> = Arc::new(
            page.docs
                .iter()
                .cloned()
                .enumerate()
                .map(|(idx, doc)| SessionDocument {
                    key: DocumentKey::from_document(&doc, idx),
                    doc,
                })
                .collect(),
        );
        Some(documents)
    }

    pub fn render_results_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut body =
            div().flex().flex_col().flex_1().min_w(px(0.0)).min_h(px(0.0)).overflow_hidden();

        if let Some(err) = &self.state.runtime.mongosh_error {
            body = body.child(
                div()
                    .px(spacing::sm())
                    .py(spacing::xs())
                    .text_sm()
                    .text_color(cx.theme().danger_foreground)
                    .child(format!("Forge runtime error: {err}")),
            );
        } else if let Some(err) = &self.state.output.last_error {
            body = body.child(
                div()
                    .px(spacing::sm())
                    .py(spacing::xs())
                    .text_sm()
                    .text_color(cx.theme().danger_foreground)
                    .child(err.clone()),
            );
        }

        if self.state.output.result_view_mode == ResultViewMode::Tree {
            let search_state = self.ensure_results_search_state(window, cx);
            let current_search = search_state.read(cx).value().to_string();
            if current_search != self.state.output.results_search_query {
                search_state.update(cx, |state, cx| {
                    state.set_value(self.state.output.results_search_query.clone(), window, cx);
                });
            }
            let search_input = Input::new(&search_state)
                .appearance(true)
                .bordered(true)
                .focus_bordered(true)
                .w(px(220.0));
            let view_entity = cx.entity();
            let documents_for_buttons = self.current_result_documents();
            let mut search_row = div()
                .flex()
                .items_center()
                .justify_end()
                .gap(spacing::xs())
                .px(spacing::sm())
                .py(spacing::xs());
            if let Some(documents) = documents_for_buttons {
                search_row = search_row
                    .child(
                        Button::new("forge-expand-all")
                            .ghost()
                            .compact()
                            .icon(Icon::new(IconName::ChevronDown).xsmall())
                            .tooltip("Expand all")
                            .on_click({
                                let view_entity = view_entity.clone();
                                let documents = documents.clone();
                                move |_: &ClickEvent, _window: &mut Window, cx: &mut App| {
                                    let nodes = collect_all_expandable_nodes(&documents);
                                    view_entity.update(cx, |view, cx| {
                                        view.state.output.result_expanded_nodes = nodes;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("forge-collapse-all")
                            .ghost()
                            .compact()
                            .icon(Icon::new(IconName::ChevronUp).xsmall())
                            .tooltip("Collapse all")
                            .on_click({
                                let view_entity = view_entity.clone();
                                move |_: &ClickEvent, _window: &mut Window, cx: &mut App| {
                                    view_entity.update(cx, |view, cx| {
                                        view.state.output.result_expanded_nodes.clear();
                                        cx.notify();
                                    });
                                }
                            }),
                    );
            }
            body = body.child(search_row.child(search_input));
        }

        if let Some(reason) = self.current_result_editability_reason(cx)
            && !self.state.output.result_pages.is_empty()
        {
            body = body.child(
                div()
                    .px(spacing::sm())
                    .py(spacing::xs())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(reason),
            );
        }

        if let Some(documents) = self.current_result_documents() {
            if self.state.output.result_view_mode == ResultViewMode::Table {
                if let Some(table) = self.sync_result_table(window, cx) {
                    body = body.child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(Table::new(&table).bordered(false)),
                    );
                }
                return body;
            }
            let editable = self.current_result_is_editable(cx);
            let props = ResultViewProps {
                documents,
                expanded_nodes: Arc::new(self.state.output.result_expanded_nodes.clone()),
                search_query: self.state.output.results_search_query.clone(),
                scroll_handle: self.state.output.result_scroll.clone(),
                empty_state: ResultEmptyState::NoDocuments,
                view_mode: self.state.output.result_view_mode,
                editable,
                inline_editor: self
                    .state
                    .output
                    .result_inline_edit
                    .as_ref()
                    .map(|edit| edit.view()),
            };
            let view_entity = cx.entity();
            let on_toggle = Arc::new(move |node_id: String, cx: &mut App| {
                view_entity.update(cx, |view, cx| {
                    if view.state.output.result_expanded_nodes.contains(&node_id) {
                        view.state.output.result_expanded_nodes.remove(&node_id);
                    } else {
                        view.state.output.result_expanded_nodes.insert(node_id.clone());
                    }
                    cx.notify();
                });
            });
            let view_entity = cx.entity();
            let on_edit = Rc::new(move |doc_index, path, window: &mut Window, cx: &mut App| {
                view_entity.update(cx, |view, cx| {
                    view.begin_result_edit(doc_index, path, window, cx);
                });
            });
            let view_entity = cx.entity();
            let on_bool =
                Rc::new(move |doc_index, path, value, window: &mut Window, cx: &mut App| {
                    view_entity.update(cx, |view, cx| {
                        view.toggle_result_bool(doc_index, path, value, window, cx);
                    });
                });
            body = body.child(render_results_view(props, on_toggle, on_edit, on_bool, cx));
        } else if self.state.runtime.is_running {
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(spacing::sm())
                    .child(Spinner::new().small())
                    .child(
                        div().text_sm().text_color(cx.theme().muted_foreground).child("Running..."),
                    ),
            );
        } else {
            body = body.child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scrollbar()
                    .text_xs()
                    .font_family(fonts::mono())
                    .text_color(if self.state.output.last_result.is_some() {
                        cx.theme().secondary_foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(if let Some(result) = &self.state.output.last_result {
                        result.clone()
                    } else {
                        "No output yet.".to_string()
                    }),
            );
        }

        body
    }
}

impl ForgeController {
    fn update_result_signature(view: &mut ForgeView, docs: &[Document]) {
        let documents: Vec<SessionDocument> = docs
            .iter()
            .cloned()
            .enumerate()
            .map(|(idx, doc)| SessionDocument { key: DocumentKey::from_document(&doc, idx), doc })
            .collect();
        let signature = results_signature(&documents);
        if view.state.output.result_signature != Some(signature) {
            view.state.output.result_signature = Some(signature);
            view.state.output.result_expanded_nodes.clear();
        }
    }

    pub fn clear_result_pages(view: &mut ForgeView, keep_pinned: bool) {
        view.state.output.result_inline_edit = None;
        view.state.output.result_inline_subscription = None;
        view.state.output.result_table_page_id = None;
        view.state.output.result_table_signature = None;
        if keep_pinned {
            view.state.output.result_pages.retain(|page| page.pinned);
        } else {
            view.state.output.result_pages.clear();
        }

        if view.state.output.result_pages.is_empty() {
            view.state.output.result_page_index = 0;
            Self::clear_results(view);
        } else {
            view.state.output.result_page_index = view
                .state
                .output
                .result_page_index
                .min(view.state.output.result_pages.len().saturating_sub(1));
            let docs =
                view.state.output.result_pages[view.state.output.result_page_index].docs.clone();
            Self::update_result_signature(view, &docs);
        }

        Self::sync_output_tab(view);
    }

    pub fn push_result_page(
        view: &mut ForgeView,
        label: String,
        docs: Vec<Document>,
        origin: ResultOrigin,
    ) {
        view.state.output.result_pages.push(ResultPage {
            id: uuid::Uuid::new_v4(),
            label,
            docs: docs.clone(),
            pinned: false,
            origin,
        });
        view.state.output.result_page_index =
            view.state.output.result_pages.len().saturating_sub(1);
        Self::update_result_signature(view, &docs);
        view.state.output.last_result = None;
        Self::sync_output_tab(view);
    }

    pub fn select_result_page(view: &mut ForgeView, index: usize) {
        if index >= view.state.output.result_pages.len() {
            return;
        }
        view.state.output.result_page_index = index;
        view.state.output.result_inline_edit = None;
        view.state.output.result_inline_subscription = None;
        let docs = view.state.output.result_pages[index].docs.clone();
        Self::update_result_signature(view, &docs);
        view.state.output.result_scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    pub fn toggle_result_pinned(view: &mut ForgeView, index: usize) {
        if let Some(page) = view.state.output.result_pages.get_mut(index) {
            page.pinned = !page.pinned;
        }
    }

    pub fn close_result_page(view: &mut ForgeView, index: usize) {
        if index >= view.state.output.result_pages.len() {
            return;
        }
        let was_active = index == view.state.output.result_page_index;
        view.state.output.result_pages.remove(index);

        if view.state.output.result_pages.is_empty() {
            view.state.output.result_page_index = 0;
            Self::clear_results(view);
            if view.state.output.last_result.is_some()
                || view.state.output.last_error.is_some()
                || view.state.runtime.mongosh_error.is_some()
            {
                view.state.output.output_tab = ForgeOutputTab::Results;
            } else {
                view.state.output.output_tab = ForgeOutputTab::Raw;
            }
        } else {
            if was_active {
                view.state.output.result_page_index =
                    index.min(view.state.output.result_pages.len().saturating_sub(1));
            } else if index < view.state.output.result_page_index {
                view.state.output.result_page_index =
                    view.state.output.result_page_index.saturating_sub(1);
            }
            let docs =
                view.state.output.result_pages[view.state.output.result_page_index].docs.clone();
            Self::update_result_signature(view, &docs);
        }
        Self::sync_output_tab(view);
    }

    pub fn clear_results(view: &mut ForgeView) {
        view.state.output.result_signature = None;
        view.state.output.result_expanded_nodes.clear();
        view.state.output.result_table_page_id = None;
        view.state.output.result_table_signature = None;
    }

    pub fn update_run_print_label(view: &mut ForgeView, run_id: u64, label: String) {
        if let Some(run) = view.state.output.output_runs.iter_mut().find(|run| run.id == run_id) {
            run.last_print_line = Some(label);
        }
    }

    pub fn take_run_print_label(view: &mut ForgeView, run_id: u64) -> Option<String> {
        view.state
            .output
            .output_runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .and_then(|run| run.last_print_line.take())
    }

    pub fn run_label(view: &ForgeView, run_id: u64) -> Option<String> {
        view.state
            .output
            .output_runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.code_preview.clone())
            .filter(|label| !label.trim().is_empty())
    }

    pub fn default_result_label(view: &ForgeView) -> String {
        format!("Result {}", view.state.output.result_pages.len() + 1)
    }

    pub fn has_results(view: &ForgeView) -> bool {
        !view.state.output.result_pages.is_empty()
            || view.state.output.last_result.is_some()
            || view.state.output.last_error.is_some()
    }

    pub fn sync_output_tab(view: &mut ForgeView) {
        if let Some(result) = &view.state.output.last_result
            && classify_result(&serde_json::Value::String(result.clone())) == ResultKind::None
        {
            view.state.output.last_result = None;
        }
        // Only auto-switch for positive results (documents/text), not errors alone
        let has_displayable_results =
            !view.state.output.result_pages.is_empty() || view.state.output.last_result.is_some();
        if has_displayable_results {
            if view.state.output.output_tab == ForgeOutputTab::Raw {
                view.state.output.output_tab = ForgeOutputTab::Results;
            }
        } else if !Self::has_results(view) {
            view.state.output.output_tab = ForgeOutputTab::Raw;
        }
        // When only errors exist: leave tab where it is (don't force-switch either way)
    }
}
