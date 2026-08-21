use std::rc::Rc;
use std::sync::Arc;

use gpui::UniformListScrollHandle;
use gpui_component::input::InputState;

use crate::bson::PathSegment;
use crate::state::SessionDocument;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultViewMode {
    Tree,
    Table,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ResultEmptyState {
    NoDocuments,
    NoMatches,
    Custom(String),
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct ResultViewProps {
    pub documents: Arc<Vec<SessionDocument>>,
    pub expanded_nodes: Arc<std::collections::HashSet<String>>,
    pub search_query: String,
    pub scroll_handle: UniformListScrollHandle,
    pub empty_state: ResultEmptyState,
    pub view_mode: ResultViewMode,
    pub editable: bool,
    pub inline_editor: Option<ResultInlineEditorView>,
}

pub type ToggleNodeCallback = Arc<dyn Fn(String, &mut gpui::App) + Send + Sync>;
pub type EditValueCallback = Rc<dyn Fn(usize, Vec<PathSegment>, &mut gpui::Window, &mut gpui::App)>;
pub type ToggleBoolCallback =
    Rc<dyn Fn(usize, Vec<PathSegment>, bool, &mut gpui::Window, &mut gpui::App)>;

#[derive(Clone)]
pub struct ResultInlineEditorView {
    pub doc_index: usize,
    pub path: Vec<PathSegment>,
    pub input: gpui::Entity<InputState>,
}
