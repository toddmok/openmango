use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use mongodb::bson::Bson;

use crate::bson::is_editable_value;
use crate::theme::{colors, spacing};
use crate::views::documents::tree::lazy_tree::VisibleRow;
use crate::views::results::types::{
    EditValueCallback, ResultInlineEditorView, ToggleBoolCallback, ToggleNodeCallback,
};

#[allow(clippy::too_many_arguments)]
pub fn render_result_row(
    ix: usize,
    row: &VisibleRow,
    meta: &crate::views::documents::tree::lazy_row::LazyRowMeta,
    value: Option<&Bson>,
    editable: bool,
    inline_editor: Option<&ResultInlineEditorView>,
    on_toggle_node: ToggleNodeCallback,
    on_edit_value: EditValueCallback,
    on_toggle_bool: ToggleBoolCallback,
    cx: &App,
) -> AnyElement {
    let node_id = row.node_id.clone();
    let depth = row.depth;
    let is_folder = row.is_folder;
    let is_expanded = row.is_expanded;
    let doc_index = row.doc_index;
    let path = row.path.clone();

    let key_label = meta.key_label.clone();
    let value_label = meta.value_label.clone();
    let value_color = meta.value_color;
    let type_label = meta.type_label.clone();

    let leading = if is_folder {
        let toggle_node_id = node_id.clone();
        let on_toggle = on_toggle_node.clone();
        div()
            .id(("result-row-chevron", ix))
            .w(px(14.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
                if event.click_count == 1 {
                    cx.stop_propagation();
                    on_toggle(toggle_node_id.clone(), cx);
                }
            })
            .child(
                Icon::new(if is_expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                    .xsmall()
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element()
    } else {
        div().w(px(14.0)).into_any_element()
    };

    let is_inline_editing =
        inline_editor.is_some_and(|editor| editor.doc_index == doc_index && editor.path == path);
    let scalar_editable = editable && value.is_some_and(|value| is_editable_value(value, &path));
    let nested_editable = editable && matches!(value, Some(Bson::Document(_) | Bson::Array(_)));

    let value_element = if is_inline_editing {
        Input::new(&inline_editor.expect("checked inline editor").input)
            .small()
            .font_family(crate::theme::fonts::mono())
            .flex_1()
            .into_any_element()
    } else if editable && let Some(Bson::Boolean(current)) = value {
        let current = *current;
        let callback = on_toggle_bool.clone();
        let path = path.clone();
        div()
            .flex()
            .items_center()
            .gap(spacing::xs())
            .child(Switch::new(("forge-result-bool", ix)).checked(current).small().on_click(
                move |checked, window, cx| {
                    callback(doc_index, path.clone(), *checked, window, cx);
                },
            ))
            .child(if current { "true" } else { "false" })
            .into_any_element()
    } else {
        render_value_column(&value_label, value_color).into_any_element()
    };

    let row_element = div()
        .id(("result-row", ix))
        .flex()
        .items_center()
        .w_full()
        .px(spacing::lg())
        .py(spacing::xs())
        .hover(|style| style.bg(cx.theme().list_hover))
        .when(scalar_editable && !matches!(value, Some(Bson::Boolean(_))), |element| {
            let on_edit = on_edit_value.clone();
            let path = path.clone();
            element.cursor_text().on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if event.click_count == 2 {
                    cx.stop_propagation();
                    on_edit(doc_index, path.clone(), window, cx);
                }
            })
        })
        .when(is_folder && !nested_editable, {
            let node_id = node_id.clone();
            let on_toggle = on_toggle_node.clone();
            move |element| {
                element.on_mouse_down(MouseButton::Left, move |event, _window, cx| {
                    if event.click_count == 2 {
                        on_toggle(node_id.clone(), cx);
                    }
                })
            }
        })
        .child(render_key_column(depth, leading, &key_label, cx))
        .child(value_element)
        .child(
            div()
                .w(px(120.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .overflow_hidden()
                .text_ellipsis()
                .child(type_label),
        );

    if nested_editable {
        let on_edit = on_edit_value;
        row_element
            .context_menu(move |menu, _window, _cx| {
                let on_edit = on_edit.clone();
                let path = path.clone();
                menu.item(PopupMenuItem::new("Edit Value…").on_click(move |_, window, cx| {
                    on_edit(doc_index, path.clone(), window, cx);
                }))
            })
            .into_any_element()
    } else {
        row_element.into_any_element()
    }
}

fn render_key_column(
    depth: usize,
    leading: AnyElement,
    key_label: &str,
    cx: &App,
) -> impl IntoElement {
    let key_color = colors::syntax_key(cx);
    let key_label = key_label.to_string();

    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .flex_1()
        .min_w(px(0.0))
        .overflow_hidden()
        .pl(px(14.0 * depth as f32))
        .child(leading)
        .child(
            div()
                .text_sm()
                .text_color(key_color)
                .overflow_hidden()
                .text_ellipsis()
                .child(key_label),
        )
}

fn render_value_column(value_label: &str, value_color: Hsla) -> impl IntoElement {
    div().flex_1().min_w(px(0.0)).overflow_hidden().child(
        div()
            .text_sm()
            .text_color(value_color)
            .overflow_hidden()
            .text_ellipsis()
            .child(value_label.to_string()),
    )
}
