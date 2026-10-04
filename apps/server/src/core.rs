//! The existing browser HTTP contract over the preserved `PostgreSQL` domain schema.

pub mod account;
pub mod cards;
pub(crate) mod facts;
pub mod model;
pub mod schedule;
pub mod sync;
pub mod workspaces;

pub use cards::{get_card, mutate_card_in_tx, submit_review};
pub use model::{Card, CardSnapshot, Mutation, SchedulerConfig};
pub use sync::ensure_system_replica;
pub use workspaces::{list_workspaces, resolve_workspace};

use crate::AppState;
use axum::{
    Router,
    routing::{get, patch, post},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me", get(account::me))
        .route("/v1/me/preferences", patch(account::preferences))
        .route("/v1/me/delete", post(account::delete_account))
        .route(
            "/v1/workspaces",
            get(workspaces::list).post(workspaces::create),
        )
        .route(
            "/v1/workspaces/{workspace}/select",
            post(workspaces::select),
        )
        .route(
            "/v1/workspaces/{workspace}/rename",
            post(workspaces::rename),
        )
        .route(
            "/v1/workspaces/{workspace}/delete-preview",
            get(workspaces::delete_preview),
        )
        .route(
            "/v1/workspaces/{workspace}/delete",
            post(workspaces::delete),
        )
        .route(
            "/v1/workspaces/{workspace}/reset-progress-preview",
            get(workspaces::reset_preview),
        )
        .route(
            "/v1/workspaces/{workspace}/reset-progress",
            post(workspaces::reset),
        )
        .route("/v1/workspaces/{workspace}/cards/query", post(cards::query))
        .route("/v1/workspaces/{workspace}/tags", get(cards::tags))
        .route("/v1/workspaces/{workspace}/sync/push", post(sync::push))
        .route("/v1/workspaces/{workspace}/sync/pull", post(sync::pull))
        .route(
            "/v1/workspaces/{workspace}/sync/bootstrap",
            post(sync::bootstrap),
        )
        .route(
            "/v1/workspaces/{workspace}/sync/review-history/pull",
            post(sync::history_pull),
        )
        .route(
            "/v1/workspaces/{workspace}/sync/review-history/import",
            post(sync::history_import),
        )
}
