use std::net::{Ipv4Addr, SocketAddrV4};

use anyhow::Context;
use axum::{extract::Extension, routing::get};
use sqlx::SqlitePool;

async fn root() -> &'static str {
    "Futaba web index"
}

pub async fn start(
    db_pool: SqlitePool,
    mut stop_signal: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let port: u16 = std::env::var("WEB_PORT")
        .ok()
        .map(|port_str| port_str.parse::<u16>())
        .unwrap_or(Ok(80))
        .context("Failed to parse WEB_PORT")?;

    let router = axum::Router::new()
        .route("/", get(root))
        .layer(Extension(db_pool));

    axum::Server::bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
        .serve(router.into_make_service())
        .with_graceful_shutdown(async {
            let _ = stop_signal.recv().await;
        })
        .await?;

    Ok(())
}
