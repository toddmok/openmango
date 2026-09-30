use chrono::{DateTime, Utc};
use gpui_kit::UniformListScrollHandle;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use crate::state::SessionDocument;

pub use super::logic::{Suggestion, SuggestionKind};

pub const MAX_OUTPUT_RUNS: usize = 50;
pub const MAX_OUTPUT_LINES: usize = 5000;
pub const SYSTEM_RUN_ID: u64 = 0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ForgeOutputTab {
    Results,
    Raw,
}

pub struct ForgeRunOutput {
    pub id: u64,
    pub started_at: DateTime<Utc>,
    pub code_preview: String,
    pub raw_lines: Vec<String>,
    pub evaluation_lines: Vec<String>,
    pub error: Option<String>,
    pub last_print_line: Option<String>,
    pub result_origin: Option<ResultOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultOrigin {
    pub forge_tab_id: Uuid,
    pub connection_id: Uuid,
    pub database: String,
    pub collection: Option<String>,
    pub captured_at: DateTime<Utc>,
    pub exact_find: bool,
}

impl ResultOrigin {
    pub fn capture(
        forge_tab_id: Uuid,
        connection_id: Uuid,
        database: String,
        opened_collection: Option<&str>,
        code: &str,
    ) -> Self {
        let exact = super::logic::exact_find_origin(code);
        let exact_find = exact.as_ref().is_some_and(|origin| {
            opened_collection.is_some_and(|opened| opened == origin.collection)
        });
        Self {
            forge_tab_id,
            connection_id,
            database,
            collection: exact.map(|origin| origin.collection),
            captured_at: Utc::now(),
            exact_find,
        }
    }

    pub fn unattributed(forge_tab_id: Uuid, connection_id: Uuid, database: String) -> Self {
        Self {
            forge_tab_id,
            connection_id,
            database,
            collection: None,
            captured_at: Utc::now(),
            exact_find: false,
        }
    }
}

#[derive(Clone)]
pub struct ResultPage {
    pub id: Uuid,
    pub label: String,
    pub documents: Arc<Vec<SessionDocument>>,
    pub pinned: bool,
    pub print_run: Option<u64>,
    pub expanded_nodes: HashSet<String>,
    pub scroll: UniformListScrollHandle,
    pub origin: ResultOrigin,
    /// Set when the sidecar keeps the full result and this page is one slice of it.
    pub paging: Option<super::mongosh::ResultPaging>,
    /// The session the paged result lives in, for fetching another page.
    pub paging_session: Option<Uuid>,
    /// A page request is in flight; the pager is disabled until it lands.
    pub paging_busy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_origin_is_editable_only_for_exact_find_on_opened_collection() {
        let tab_id = Uuid::new_v4();
        let connection_id = Uuid::new_v4();
        let exact = ResultOrigin::capture(
            tab_id,
            connection_id,
            "shop".into(),
            Some("orders"),
            "db.orders.find({_id: 1})",
        );
        assert!(exact.exact_find);
        assert_eq!(exact.collection.as_deref(), Some("orders"));

        let wrong_collection = ResultOrigin::capture(
            tab_id,
            connection_id,
            "shop".into(),
            Some("customers"),
            "db.orders.find({_id: 1})",
        );
        assert!(!wrong_collection.exact_find);

        let aggregate = ResultOrigin::capture(
            tab_id,
            connection_id,
            "shop".into(),
            Some("orders"),
            "db.orders.aggregate([])",
        );
        assert!(!aggregate.exact_find);
        assert_eq!(aggregate.collection, None);
    }
}
