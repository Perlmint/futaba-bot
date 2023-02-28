use std::sync::Arc;

use log::{error, info};
use serde::Deserialize;
use sqlx::sqlite::SqlitePoolOptions;

mod discord;
mod eueoeo;
mod events;
mod web;

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    discord: discord::Config,
    web: web::Config,
    eueoeo: eueoeo::Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pretty_env_logger::init();

    let config = Arc::new(toml::from_str::<Config>(
        &tokio::fs::read_to_string("futaba.toml").await?,
    )?);

    let db_pool = SqlitePoolOptions::new()
        .connect(&{
            let mut dir = std::env::current_dir().unwrap();
            dir.push("db.db");
            let path = format!("sqlite://{}?mode=rwc", dir.display());
            path
        })
        .await?;

    // run DB migration
    sqlx::migrate!().run(&db_pool).await?;

    let (stop_sender, _) = tokio::sync::broadcast::channel(1);

    let discord_join = tokio::task::spawn({
        let db_pool = db_pool.clone();
        let stop_receiver = stop_sender.subscribe();
        let stop_sender = stop_sender.clone();
        let config = config.clone();
        async move {
            if let Err(e) = discord::start(db_pool, &config, stop_receiver).await {
                error!("Discord task failed with - {e:?}");
                let _ = stop_sender.send(());
            }
        }
    });
    let web_join = tokio::task::spawn({
        let db_pool = db_pool.clone();
        let stop_receiver = stop_sender.subscribe();
        let stop_sender = stop_sender.clone();
        async move {
            if let Err(e) = web::start(db_pool, config, stop_receiver).await {
                error!("Web task failed with - {e:?}");
                let _ = stop_sender.send(());
            }
        }
    });

    tokio::task::spawn(async move {
        let sig_int = tokio::signal::ctrl_c();
        #[cfg(target_family = "windows")]
        {
            sig_int.await.expect("Ctrl-C receiver is broken");
        }
        #[cfg(target_family = "unix")]
        {
            let mut sig_term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Failed to register SIGTERM handler");
            tokio::select! {
                _ = sig_int => (),
                _ = sig_term.recv() => (),
            };
        }

        if stop_sender.send(()).is_err() {
            error!("Already all services are stopped");
        }
    });

    if let Err(e) = discord_join.await {
        error!("Discord task is broken - {e:?}")
    }
    if let Err(e) = web_join.await {
        error!("Web task is broken - {e:?}")
    }

    db_pool.close().await;
    info!("db closed");

    Ok(())
}
