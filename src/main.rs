mod config;
mod db;
mod error;
mod models;
mod routes;
mod services;
mod templates;

use std::sync::Arc;

use anyhow::Context;
use axum::{Router, extract::DefaultBodyLimit};
use config::Config;
use db::Database;
use services::AppServices;
use tokio::net::TcpListener;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Database,
    pub services: AppServices,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shiori=debug,tower_http=debug,axum::rejection=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env()?);
    config.ensure_directories()?;

    let config_for_db = config.clone();
    let db = tokio::task::spawn_blocking(move || Database::new(config_for_db))
        .await
        .context("join database init task")??;
    db.init().context("initialize qdrant collections")?;

    let services = AppServices::new(config.clone());
    let state = AppState {
        config: config.clone(),
        db,
        services,
    };

    let app = app_router(state);
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("bind {}", config.bind_addr))?;

    tracing::info!("listening on http://{}", config.bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}

fn app_router(state: AppState) -> Router {
    let assets_dir = state.config.static_dir.clone();

    Router::new()
        .merge(routes::router())
        .nest_service("/assets", ServeDir::new(assets_dir))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
