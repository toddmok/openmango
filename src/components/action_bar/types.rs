use gpui::SharedString;
use uuid::Uuid;

/// Prefix for collection navigation action IDs produced by the palette.
///
/// The full format is `nav:col:<uuid>:<database>:<collection>`. Parsing
/// splits on the first `:` after the UUID, so a database name containing a
/// colon would mis-split; collection names keep any remaining colons.
pub const COLLECTION_NAVIGATION_PREFIX: &str = "nav:col:";

/// Build a palette action ID for navigating to a collection.
pub fn collection_navigation_action_id(
    connection_id: Uuid,
    database: &str,
    collection: &str,
) -> String {
    format!("{COLLECTION_NAVIGATION_PREFIX}{connection_id}:{database}:{collection}")
}

/// A single action in the palette.
#[derive(Clone, Default)]
pub struct ActionItem {
    pub id: SharedString,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub category: ActionCategory,
    pub shortcut: Option<SharedString>,
    pub available: bool,
    pub priority: i32,
    /// Highlighted items render with accent color and sort to the top.
    pub highlighted: bool,
}

/// Categories for grouping and ordering actions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ActionCategory {
    Navigation,
    #[default]
    Command,
    Tab,
    View,
}

impl ActionCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Command => "Commands",
            Self::Tab => "Tabs",
            Self::View => "View",
        }
    }

    pub fn sort_order(&self) -> u8 {
        match self {
            Self::Tab => 0,
            Self::Command => 1,
            Self::Navigation => 2,
            Self::View => 3,
        }
    }
}

/// Result of fuzzy matching with score for ranking.
#[derive(Clone)]
pub struct FilteredAction {
    pub item: ActionItem,
    pub score: usize,
}

/// Palette operating mode.
#[derive(Clone, Default, PartialEq)]
pub enum PaletteMode {
    #[default]
    All,
    Theme,
    Connect,
    Disconnect,
}

/// Payload returned when user executes an action.
pub struct ActionExecution {
    pub action_id: SharedString,
}
