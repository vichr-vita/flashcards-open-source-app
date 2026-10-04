pub mod ai;
pub mod ancillary;
pub mod auth;
pub mod config;
pub mod core;
pub mod database;
pub mod error;
pub mod metadata;
pub mod migrations;
pub mod progress;

pub use config::Config;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth_pool: PgPool,
    pub config: Arc<Config>,
}
