use gpui::{App, AppContext as _, Entity, Task};
use mongodb::bson::Bson;
use uuid::Uuid;

use crate::bson::{DottedPath, PathSegment};
use crate::connection::ops::documents::FieldCasUpdate;
use crate::error::Result;
use crate::state::{AppCommands, AppState};

impl AppCommands {
    /// Start a single-field Forge result edit after the command-level writable check.
    #[allow(clippy::too_many_arguments)]
    pub fn update_forge_result_field(
        state: Entity<AppState>,
        connection_id: Uuid,
        database: String,
        collection: String,
        id: Bson,
        path: Vec<PathSegment>,
        dotted_path: DottedPath,
        expected: Bson,
        replacement: Bson,
        cx: &mut App,
    ) -> Option<Task<Result<bool>>> {
        if !Self::ensure_writable(&state, Some(connection_id), cx) {
            return None;
        }
        let client = Self::active_client(&state, connection_id, cx)?;
        let manager = state.read(cx).connection_manager();
        Some(cx.background_spawn(async move {
            manager.update_field_if_current_matches(
                &client,
                FieldCasUpdate {
                    database,
                    collection,
                    id,
                    path_segments: path,
                    dotted_path,
                    expected: Some(expected),
                    replacement,
                },
            )
        }))
    }
}
