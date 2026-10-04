use axum::{
    Router,
    extract::State,
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware,
    response::Response,
    routing::get,
};
use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::Result;
use lingvichr::{AppState, Config, auth, core, error::ApiError, migrations};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate browser wire types from Rust without connecting to a database.
    GenerateTypes {
        #[arg(long, default_value = "apps/web/src/generated")]
        output: PathBuf,
    },
    /// Serve the private HTTP API, authentication, or combined loopback stack.
    Serve {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long, env = "AUTH_DATABASE_URL", hide_env_values = true)]
        auth_database_url: Option<String>,
        #[arg(long, default_value = "127.0.0.1:19400")]
        bind: SocketAddr,
        #[arg(long, value_enum, default_value = "all")]
        service: Service,
        #[arg(long, default_value = "apps/web/dist")]
        web_dir: PathBuf,
    },
    /// Apply reviewed SQL through the existing filename ledger using an owner connection.
    Migrate {
        #[arg(long, env = "MIGRATION_DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long, default_value = "db")]
        directory: PathBuf,
        #[arg(long, default_value = "backend_app")]
        backend_role: String,
        #[arg(long, default_value = "auth_app")]
        auth_role: String,
        #[arg(long, default_value = "reporting_readonly")]
        reporting_role: String,
    },
    /// Manage passkeys without changing the account identity or card data.
    Account {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[command(subcommand)]
        command: auth::AccountCommand,
    },
    /// Manage owner API credentials for the private MCP endpoint.
    AgentKey {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[command(subcommand)]
        command: lingvichr::ai::AgentKeyCommand,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Service {
    All,
    Backend,
    Auth,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lingvichr=info,tower_http=info".into()),
        )
        .init();
    match Cli::parse().command {
        Command::GenerateTypes { output } => {
            use ts_rs::TS;
            let config = ts_rs::Config::default().with_out_dir(output);
            core::Card::export_all(&config)?;
            core::CardSnapshot::export_all(&config)?;
            core::SchedulerConfig::export_all(&config)?;
            lingvichr::progress::ReviewHistoryWatermark::export_all(&config)?;
            lingvichr::progress::StreakFreeze::export_all(&config)?;
        }
        Command::Migrate {
            database_url,
            directory,
            backend_role,
            auth_role,
            reporting_role,
        } => {
            migrations::apply(
                &database_url,
                &directory,
                &migrations::Roles {
                    backend: backend_role,
                    auth: auth_role,
                    reporting: reporting_role,
                },
            )
            .await?;
        }
        Command::Account {
            database_url,
            command,
        } => {
            let config = Config::for_auth(PathBuf::new())?;
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await?;
            auth::account_command(&pool, &config, command).await?;
            pool.close().await;
        }
        Command::AgentKey {
            database_url,
            command,
        } => {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await?;
            lingvichr::ai::agent_key_command(&pool, command).await?;
            pool.close().await;
        }
        Command::Serve {
            database_url,
            auth_database_url,
            bind,
            service,
            web_dir,
        } => {
            serve(database_url, auth_database_url, bind, service, web_dir).await?;
        }
    }
    Ok(())
}

async fn serve(
    database_url: String,
    auth_database_url: Option<String>,
    bind: SocketAddr,
    service: Service,
    web_dir: PathBuf,
) -> Result<()> {
    let config = Arc::new(if matches!(service, Service::Auth) {
        Config::for_auth(web_dir)?
    } else {
        Config::from_env(web_dir)?
    });
    let max_connections = database_pool_limit()?;
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await?;
    let auth_url = match auth_database_url {
        Some(url) => url,
        None if !matches!(service, Service::All) => database_url.clone(),
        None => {
            return Err(color_eyre::eyre::eyre!(
                "Combined service requires a separate AUTH_DATABASE_URL"
            ));
        }
    };
    let auth_pool = if auth_url == database_url {
        pool.clone()
    } else {
        PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&auth_url)
            .await?
    };
    let state = AppState {
        pool,
        auth_pool,
        config,
    };
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "Private server listening");
    let request_id = HeaderName::from_static("x-request-id");
    axum::serve(
        listener,
        private_router(&state.config, service)
            .layer(middleware::from_fn(response_headers))
            .layer(cors(&state.config)?)
            .layer(PropagateRequestIdLayer::new(request_id.clone()))
            .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
            .with_state(state),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

fn database_pool_limit() -> Result<u32> {
    match std::env::var("DB_POOL_MAX_CONNECTIONS") {
        Err(std::env::VarError::NotPresent) => Ok(3),
        Ok(value) if value.is_empty() => Ok(3),
        Ok(value) => value
            .parse::<u32>()
            .ok()
            .filter(|value| *value >= 3)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("DB_POOL_MAX_CONNECTIONS must be an integer of at least 3")
            }),
        Err(error) => Err(error.into()),
    }
}

fn cors(config: &Config) -> Result<CorsLayer> {
    let origins = config
        .allowed_origins
        .iter()
        .map(|origin| origin.parse::<HeaderValue>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("x-csrf-token"),
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-client-platform"),
            HeaderName::from_static("x-client-version"),
            HeaderName::from_static("sentry-trace"),
            HeaderName::from_static("baggage"),
            HeaderName::from_static("x-chat-request-id"),
            HeaderName::from_static("x-chat-resume-attempt-id"),
            HeaderName::from_static("x-chat-live-client-id"),
            HeaderName::from_static("x-media-asset-id"),
            HeaderName::from_static("x-media-source-url"),
            HeaderName::from_static("x-media-created-at"),
            HeaderName::from_static("x-media-client-updated-at"),
            HeaderName::from_static("x-media-last-modified-by-replica-id"),
            HeaderName::from_static("x-media-last-operation-id"),
            HeaderName::from_static("x-package-media-key"),
            HeaderName::from_static("x-openai-api-key"),
        ])
        .expose_headers([
            header::CACHE_CONTROL,
            header::CONTENT_DISPOSITION,
            header::CONTENT_ENCODING,
            header::CONTENT_LENGTH,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-amz-apigw-id"),
            HeaderName::from_static("x-amzn-requestid"),
            HeaderName::from_static("x-chat-request-id"),
            header::RETRY_AFTER,
        ]);

    Ok(cors)
}

fn private_router(config: &Config, service: Service) -> Router<AppState> {
    let mut router = Router::new().route("/v1/health", get(health)).route(
        "/v1/{*path}",
        get(not_found)
            .post(not_found)
            .patch(not_found)
            .delete(not_found),
    );
    router = if matches!(service, Service::Auth) {
        router.route("/health", get(auth_health))
    } else {
        router.route("/health", get(health))
    };
    if matches!(service, Service::All | Service::Backend) {
        router = router
            .merge(core::router())
            .merge(lingvichr::progress::router())
            .merge(lingvichr::ai::router())
            .merge(lingvichr::ancillary::router())
            .merge(lingvichr::metadata::router());
    }
    if matches!(service, Service::All | Service::Auth) {
        router = router.merge(auth::router()).route_service(
            "/assets/local-passkey.js",
            ServeFile::new(config.web_dir.join("assets/local-passkey.js")),
        );
    }
    if matches!(service, Service::All) {
        router = router.fallback_service(
            ServeDir::new(&config.web_dir)
                .not_found_service(ServeFile::new(config.web_dir.join("index.html"))),
        );
    }

    router
}

async fn health(State(state): State<AppState>) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let now: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&state.pool)
        .await?;
    Ok(axum::Json(
        serde_json::json!({ "status": "ok", "service": "flashcards-open-source-app-backend", "cloudContractVersion": 1, "dbTime": now }),
    ))
}

async fn auth_health(
    State(state): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    sqlx::query("SELECT 1").execute(&state.auth_pool).await?;
    Ok(axum::Json(
        serde_json::json!({"ok":true,"authMode":"local"}),
    ))
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found")
}

async fn response_headers(request: axum::extract::Request, next: middleware::Next) -> Response {
    let private = request.uri().path().starts_with("/v1/");
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if private && !response.headers().contains_key(header::CACHE_CONTROL) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { result = tokio::signal::ctrl_c() => { if let Err(error) = result { tracing::error!(%error, "Unable to listen for shutdown"); } }, _ = terminate.recv() => {} }
            }
            Err(error) => tracing::error!(%error, "Unable to listen for termination"),
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "Unable to listen for shutdown");
    }
}
