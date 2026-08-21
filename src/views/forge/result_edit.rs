use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::WindowExt as _;
use gpui_component::dialog::Dialog;
use gpui_component::input::{Input, InputEvent, InputState};
use mongodb::bson::Bson;
use uuid::Uuid;

use crate::bson::{
    DottedPath, PathSegment, bson_value_for_edit, format_relaxed_json_value, get_bson_at_path,
    parse_bson_from_relaxed_json, parse_edited_value, set_bson_at_path,
};
use crate::components::{Button, WriteRequest, request_connection_write};
use crate::state::{AppCommands, StatusMessage};
use crate::theme::spacing;
use crate::views::results::ResultInlineEditorView;

use super::ForgeView;
use super::types::ResultOrigin;

#[derive(Clone, Debug, PartialEq)]
pub struct ResultEditTarget {
    pub page_id: Uuid,
    pub origin: ResultOrigin,
    pub doc_index: usize,
    pub path: Vec<PathSegment>,
    pub dotted_path: DottedPath,
    pub id: Bson,
    pub expected: Bson,
}

pub struct ResultInlineEdit {
    pub target: ResultEditTarget,
    pub input: Entity<InputState>,
}

impl ResultInlineEdit {
    pub fn view(&self) -> ResultInlineEditorView {
        ResultInlineEditorView {
            doc_index: self.target.doc_index,
            path: self.target.path.clone(),
            input: self.input.clone(),
        }
    }
}

impl ForgeView {
    pub fn current_result_editability_reason(&self, cx: &App) -> Option<String> {
        let page = self.state.output.result_pages.get(self.state.output.result_page_index)?;
        if !page.origin.exact_find {
            return Some(
                "Editing is disabled because this result was not produced by one exact find/findOne on the Forge tab collection."
                    .to_string(),
            );
        }
        let Some(active) = self.app_state.read(cx).active_forge_tab_key() else {
            return Some(
                "Editing is disabled because the originating Forge tab is not active.".into(),
            );
        };
        if active.id != page.origin.forge_tab_id
            || active.connection_id != page.origin.connection_id
            || active.database != page.origin.database
        {
            return Some(
                "These results belong to another Forge tab. They remain visible but are read-only."
                    .into(),
            );
        }
        let Some(collection) = page.origin.collection.as_deref() else {
            return Some("Editing is disabled because the result collection is ambiguous.".into());
        };
        if self.app_state.read(cx).forge_tab_collection(active.id) != Some(collection) {
            return Some(
                "Editing is disabled because the active Forge tab collection no longer matches the result origin."
                    .into(),
            );
        }
        if self.app_state.read(cx).connection_read_only(page.origin.connection_id) {
            return Some("Editing is disabled for read-only connections.".into());
        }
        None
    }

    pub fn current_result_is_editable(&self, cx: &App) -> bool {
        self.current_result_editability_reason(cx).is_none()
    }

    fn result_edit_target(
        &self,
        doc_index: usize,
        path: Vec<PathSegment>,
        cx: &App,
    ) -> Result<ResultEditTarget, String> {
        if let Some(reason) = self.current_result_editability_reason(cx) {
            return Err(reason);
        }
        if path.is_empty()
            || path.iter().any(|segment| matches!(segment, PathSegment::Key(key) if key == "_id"))
        {
            return Err("The _id field cannot be edited.".into());
        }
        let dotted_path = DottedPath::new(&path).map_err(|error| error.to_string())?;
        let page = self
            .state
            .output
            .result_pages
            .get(self.state.output.result_page_index)
            .ok_or_else(|| "The result page is no longer available.".to_string())?;
        let document = page
            .docs
            .get(doc_index)
            .ok_or_else(|| "The result document is no longer available.".to_string())?;
        let id = document
            .get("_id")
            .cloned()
            .ok_or_else(|| "Document missing _id; editing is disabled.".to_string())?;
        let expected = get_bson_at_path(document, &path)
            .cloned()
            .ok_or_else(|| "The result field is no longer available.".to_string())?;
        Ok(ResultEditTarget {
            page_id: page.id,
            origin: page.origin.clone(),
            doc_index,
            path,
            dotted_path,
            id,
            expected,
        })
    }

    pub fn begin_result_edit(
        &mut self,
        doc_index: usize,
        path: Vec<PathSegment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match self.result_edit_target(doc_index, path, cx) {
            Ok(target) => target,
            Err(error) => {
                self.set_result_edit_status(error, true, cx);
                return;
            }
        };
        if matches!(target.expected, Bson::Document(_) | Bson::Array(_)) {
            ResultValueDialog::open(cx.entity(), target, window, cx);
            return;
        }
        if matches!(target.expected, Bson::Boolean(_)) {
            return;
        }

        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| {
            state.set_value(bson_value_for_edit(&target.expected), window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |view, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                view.commit_result_inline_edit(window, cx);
            }
        });
        let focus = input.read(cx).focus_handle(cx);
        self.state.output.result_inline_edit = Some(ResultInlineEdit { target, input });
        self.state.output.result_inline_subscription = Some(subscription);
        window.defer(cx, move |window, _cx| window.focus(&focus));
        cx.notify();
    }

    pub fn commit_result_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.state.output.result_inline_edit.take() else {
            return;
        };
        self.state.output.result_inline_subscription = None;
        let replacement =
            match parse_edited_value(&edit.target.expected, edit.input.read(cx).value().as_ref()) {
                Ok(value) => value,
                Err(error) => {
                    self.set_result_edit_status(error, true, cx);
                    cx.notify();
                    return;
                }
            };
        self.request_result_edit(edit.target, replacement, window, cx);
        cx.notify();
    }

    pub fn toggle_result_bool(
        &mut self,
        doc_index: usize,
        path: Vec<PathSegment>,
        value: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match self.result_edit_target(doc_index, path, cx) {
            Ok(target) => target,
            Err(error) => {
                self.set_result_edit_status(error, true, cx);
                return;
            }
        };
        if !matches!(target.expected, Bson::Boolean(_)) {
            self.set_result_edit_status("The result field is no longer Boolean.".into(), true, cx);
            return;
        }
        self.request_result_edit(target, Bson::Boolean(value), window, cx);
    }

    fn request_result_edit(
        &mut self,
        target: ResultEditTarget,
        replacement: Bson,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.result_target_still_matches(&target, cx) {
            self.set_result_edit_status(
                "The result changed before the edit started. Rerun the Forge query.".into(),
                true,
                cx,
            );
            return;
        }
        let Some(collection) = target.origin.collection.clone() else {
            return;
        };
        let app_state = self.app_state.clone();
        let view = cx.entity();
        let namespace = format!("{}.{}", target.origin.database, collection);
        request_connection_write(
            app_state.clone(),
            WriteRequest::new(
                target.origin.connection_id,
                namespace,
                format!("Update result field {}", target.dotted_path),
                None,
            ),
            window,
            cx,
            move |_window, cx| {
                let can_start = view.read(cx).result_target_still_matches(&target, cx);
                if !can_start {
                    view.update(cx, |view, cx| {
                        view.set_result_edit_status(
                            "The result or active Forge tab changed. Rerun the query before editing."
                                .into(),
                            true,
                            cx,
                        );
                    });
                    return;
                }
                let Some(task) = AppCommands::update_forge_result_field(
                    app_state.clone(),
                    target.origin.connection_id,
                    target.origin.database.clone(),
                    collection.clone(),
                    target.id.clone(),
                    target.path.clone(),
                    target.dotted_path.clone(),
                    target.expected.clone(),
                    replacement.clone(),
                    cx,
                ) else {
                    return;
                };
                cx.spawn({
                    let view = view.clone();
                    async move |cx: &mut AsyncApp| {
                        let result = task.await;
                        let _ = cx.update(|cx| {
                            view.update(cx, |view, cx| match result {
                                Ok(true) => view.finish_result_edit(target, replacement, cx),
                                Ok(false) => view.set_result_edit_status(
                                    "Edit conflict: the server value changed. Rerun the Forge query."
                                        .into(),
                                    true,
                                    cx,
                                ),
                                Err(error) => view.set_result_edit_status(error.to_string(), true, cx),
                            });
                        });
                    }
                })
                .detach();
            },
        );
    }

    fn result_target_still_matches(&self, target: &ResultEditTarget, cx: &App) -> bool {
        let active_matches = self.app_state.read(cx).active_forge_tab_key().is_some_and(|active| {
            active.id == target.origin.forge_tab_id
                && active.connection_id == target.origin.connection_id
                && active.database == target.origin.database
                && target.origin.exact_find
                && target.origin.collection.as_deref()
                    == self.app_state.read(cx).forge_tab_collection(active.id)
        });
        if !active_matches
            || self.app_state.read(cx).connection_read_only(target.origin.connection_id)
        {
            return false;
        }
        self.state
            .output
            .result_pages
            .iter()
            .find(|page| page.id == target.page_id && page.origin == target.origin)
            .and_then(|page| page.docs.get(target.doc_index))
            .is_some_and(|document| {
                document.get("_id") == Some(&target.id)
                    && get_bson_at_path(document, &target.path) == Some(&target.expected)
            })
    }

    fn finish_result_edit(
        &mut self,
        target: ResultEditTarget,
        replacement: Bson,
        cx: &mut Context<Self>,
    ) {
        let active_matches = self.app_state.read(cx).active_forge_tab_key().is_some_and(|active| {
            active.id == target.origin.forge_tab_id
                && active.connection_id == target.origin.connection_id
                && active.database == target.origin.database
                && target.origin.collection.as_deref()
                    == self.app_state.read(cx).forge_tab_collection(active.id)
        });
        let patched = active_matches
            && self
                .state
                .output
                .result_pages
                .iter_mut()
                .find(|page| page.id == target.page_id && page.origin == target.origin)
                .and_then(|page| page.docs.get_mut(target.doc_index))
                .is_some_and(|document| {
                    document.get("_id") == Some(&target.id)
                        && get_bson_at_path(document, &target.path) == Some(&target.expected)
                        && set_bson_at_path(document, &target.path, replacement)
                });
        self.state.output.result_table_signature = None;
        if patched {
            self.set_result_edit_status("Result field updated.".into(), false, cx);
        } else {
            self.set_result_edit_status(
                "The write succeeded, but the visible result changed. Rerun the Forge query to refresh it."
                    .into(),
                false,
                cx,
            );
        }
        cx.notify();
    }

    fn set_result_edit_status(&self, message: String, error: bool, cx: &mut App) {
        self.app_state.update(cx, |state, cx| {
            state.set_status_message(Some(if error {
                StatusMessage::error(message)
            } else {
                StatusMessage::info(message)
            }));
            cx.notify();
        });
    }
}

struct ResultValueDialog {
    view: Entity<ForgeView>,
    target: ResultEditTarget,
    input: Entity<InputState>,
    error: Option<String>,
}

impl ResultValueDialog {
    fn open(view: Entity<ForgeView>, target: ResultEditTarget, window: &mut Window, cx: &mut App) {
        let value = format_relaxed_json_value(&target.expected.clone().into_canonical_extjson());
        let dialog_view = cx.new(|cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("json")
                    .line_number(true)
                    .searchable(true)
                    .soft_wrap(false)
                    .default_value(value)
            });
            Self { view, target, input, error: None }
        });
        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog.title("Edit Value").w(px(720.0)).child(dialog_view.clone())
        });
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let replacement = parse_bson_from_relaxed_json(self.input.read(cx).value().as_ref())
            .and_then(|value| validate_nested_replacement(&self.target.expected, value));
        match replacement {
            Ok(value) => {
                let target = self.target.clone();
                let view = self.view.clone();
                window.close_dialog(cx);
                window.defer(cx, move |window, cx| {
                    view.update(cx, |view, cx| {
                        view.request_result_edit(target, value, window, cx);
                    });
                });
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

fn validate_nested_replacement(expected: &Bson, replacement: Bson) -> Result<Bson, String> {
    match (expected, &replacement) {
        (Bson::Document(_), Bson::Document(_)) | (Bson::Array(_), Bson::Array(_)) => {
            Ok(replacement)
        }
        (Bson::Document(_), _) => Err("Expected a JSON object for this document field.".into()),
        (Bson::Array(_), _) => Err("Expected a JSON array for this array field.".into()),
        _ => Err("Nested editor opened for an unsupported value type.".into()),
    }
}

impl Render for ResultValueDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap(spacing::sm())
            .p(spacing::md())
            .child(
                div()
                    .h(px(320.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(px(4.0))
                    .child(Input::new(&self.input).h_full()),
            )
            .when_some(self.error.clone(), |element, error| {
                element.child(div().text_sm().text_color(cx.theme().danger_foreground).child(error))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(spacing::xs())
                    .child(
                        Button::new("result-value-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("result-value-save").primary().label("Save").on_click(
                        move |_, window, cx| {
                            entity.update(cx, |dialog, cx| dialog.submit(window, cx));
                        },
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{Bson, doc};

    use super::validate_nested_replacement;

    #[test]
    fn nested_edit_preserves_document_or_array_container_type() {
        assert!(
            validate_nested_replacement(
                &Bson::Document(doc! { "value": 1 }),
                Bson::Document(doc! { "value": 2 }),
            )
            .is_ok()
        );
        assert!(
            validate_nested_replacement(
                &Bson::Array(vec![Bson::Int32(1)]),
                Bson::Array(vec![Bson::Int32(2)]),
            )
            .is_ok()
        );
        assert!(
            validate_nested_replacement(&Bson::Document(doc! { "value": 1 }), Bson::Array(vec![]),)
                .is_err()
        );
        assert!(validate_nested_replacement(&Bson::Array(vec![]), Bson::Null).is_err());
    }
}
