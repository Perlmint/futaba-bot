use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};

use anyhow::Context;
use axum::{extract::Extension, routing::get};
use log::info;
use serde::Deserialize;
use sqlx::SqlitePool;

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    pub(crate) domain: String,
}

async fn root() -> &'static str {
    "Futaba web index"
}

pub(crate) async fn start(
    db_pool: SqlitePool,
    config: Arc<crate::Config>,
    mut stop_signal: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let port: u16 = std::env::var("WEB_PORT")
        .ok()
        .map(|port_str| port_str.parse::<u16>())
        .unwrap_or(Ok(80))
        .context("Failed to parse WEB_PORT")?;

    let router = axum::Router::new()
        .route("/", get(root))
        .nest("/events", super::events::router())
        .layer(Extension(db_pool))
        .layer(Extension(config.clone()));

    info!("Serve web on {}:{port}", config.web.domain);

    axum::Server::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
        .serve(router.into_make_service())
        .with_graceful_shutdown(async {
            let _ = stop_signal.recv().await;
        })
        .await?;

    Ok(())
}
