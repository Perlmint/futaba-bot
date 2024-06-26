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
        .unwrap_or(Ok(8000))
        .context("Failed to parse WEB_PORT")?;

    let router = axum::Router::new()
        .route("/", get(root))
        .nest("/user", crate::user::web_router())
        .layer(Extension(db_pool))
        .layer(Extension(config.clone()));

    info!("Serve web on {port}");

    axum::serve(
        tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
            .await
            .unwrap(),
        router.into_make_service(),
    )
    .with_graceful_shutdown(async move {
        let _ = stop_signal.recv().await;
    })
    .await?;

    Ok(())
}
