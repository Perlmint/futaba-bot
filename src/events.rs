use anyhow::Context as _;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use google_calendar3::{
    api::{AclRule, Event as GoogleEvent},
    hyper::{self, client::HttpConnector},
    hyper_rustls::{self, HttpsConnector},
    oauth2::{self, authenticator::HyperClientBuilder},
    CalendarHub,
};
use log::error;
use serde::Deserialize;
use serenity::{
    model::prelude::{GuildId, ScheduledEvent, ScheduledEventId, UserId},
    prelude::Context,
};
use sqlx::{query, sqlite::SqliteRow, FromRow, Row, SqlitePool};

use super::user::DiscordHandler as UserHandler;
use crate::discord::{ScheduledEventUpdated, SubApplication};

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Config {
    calendar_id: String,
}

pub(crate) struct DiscordHandler {
    db_pool: SqlitePool,
    service_account: google_calendar3::oauth2::ServiceAccountKey,
    config: Config,
}

impl DiscordHandler {
    pub async fn new(db_pool: SqlitePool, config: &crate::Config) -> anyhow::Result<Self> {
        Ok(Self {
            db_pool,
            service_account: google_calendar3::oauth2::read_service_account_key(
                &config.google_service_account_path,
            )
            .await?,
            config: config.events.clone(),
        })
    }

    async fn google_service_account_auth(
        &self,
    ) -> anyhow::Result<
        oauth2::authenticator::Authenticator<
            <oauth2::authenticator::DefaultHyperClient as HyperClientBuilder>::Connector,
        >,
    > {
        oauth2::ServiceAccountAuthenticator::builder(self.service_account.clone())
            .build()
            .await
            .context("Failed to get service account auth")
    }

    async fn calendar_hub(&self) -> anyhow::Result<CalendarHub<HttpsConnector<HttpConnector>>> {
        let auth = self.google_service_account_auth().await?;

        Ok(CalendarHub::new(
            hyper::Client::builder().build(
                hyper_rustls::HttpsConnectorBuilder::new()
                    .with_native_roots()
                    .https_or_http()
                    .enable_http1()
                    .enable_http2()
                    .build(),
            ),
            auth,
        ))
    }

    async fn discord_event_to_google_event(
        db_pool: &SqlitePool,
        context: &Context,
        discord_event: &ScheduledEvent,
    ) -> anyhow::Result<GoogleEvent> {
        let users = context
            .http
            .get_scheduled_event_users(
                discord_event.guild_id.0,
                discord_event.id.0,
                None,
                None,
                Some(false),
            )
            .await?;
        let users =
            UserHandler::get_google_ids(db_pool, users.into_iter().map(|user| user.user.id))
                .await?;
        // map google-id
        fn discord_ts_to_google_date_time(
            ts: serenity::model::Timestamp,
        ) -> google_calendar3::api::EventDateTime {
            let ts = ts.naive_utc().timestamp();
            google_calendar3::api::EventDateTime {
                date: None,
                date_time: NaiveDateTime::from_timestamp_opt(ts, 0).map(|dt| dt.and_utc()),
                time_zone: None,
            }
        }
        let start = discord_ts_to_google_date_time(discord_event.start_time);
        let end = discord_event
            .end_time
            .map(discord_ts_to_google_date_time)
            .or_else(|| Some(start.clone()));
        Ok(GoogleEvent {
            description: discord_event.description.clone(),
            end,
            start: Some(start),
            summary: Some(discord_event.name.clone()),
            location: discord_event.metadata.as_ref().map(|d| d.location.clone()),
            attendees: Some(
                users
                    .into_iter()
                    .map(|google_id| google_calendar3::api::EventAttendee {
                        additional_guests: None,
                        comment: None,
                        display_name: None,
                        email: Some(google_id),
                        id: None,
                        optional: None,
                        organizer: None,
                        resource: None,
                        response_status: None,
                        self_: None,
                    })
                    .collect(),
            ),
            ..Default::default()
        })
    }

    async fn update_server_event(
        &self,
        context: &Context,
        event: &ScheduledEvent,
    ) -> anyhow::Result<()> {
        log::info!("Update event");
        let discord_id = *event.id.as_u64() as i64;
        let saved_event = sqlx::query!(
            "SELECT `google_event_id` FROM `server_events` WHERE `discord_id` = ?",
            discord_id
        )
        .fetch_optional(&self.db_pool)
        .await?
        .map(|d| d.google_event_id);
        let now = chrono::Utc::now().naive_utc();
        let hub = self.calendar_hub().await?;
        let event = Self::discord_event_to_google_event(&self.db_pool, context, &event).await?;
        log::debug!("converted event: {event:?}");
        let calendar_id = self.config.calendar_id.as_str();
        if let Some(google_event_id) = saved_event {
            hub.events()
                .update(event, &calendar_id, &google_event_id)
                .doit()
                .await?;
            sqlx::query!(
                r#"
                UPDATE `server_events`
                    SET `google_event_id` = ?, `synced_at` = ?
                    WHERE `discord_id` = ?
                "#,
                google_event_id,
                now,
                discord_id
            )
            .execute(&self.db_pool)
            .await?;
        } else {
            let event = hub.events().insert(event, &calendar_id).doit().await?.1;
            let google_event_id = event.id.as_ref().unwrap();
            sqlx::query!(
                r#"
                INSERT INTO `server_events`
                    (`discord_id`, `google_event_id`, `synced_at`)
                    VALUES 
                    (?, ?, ?)
                "#,
                discord_id,
                google_event_id,
                now,
            )
            .execute(&self.db_pool)
            .await?;
        };

        Ok(())
    }

    async fn update_server_event_user(
        &self,
        context: &Context,
        event_id: ScheduledEventId,
        guild_id: GuildId,
        _user_id: UserId,
        _added: bool,
    ) -> anyhow::Result<()> {
        let event = context
            .http
            .get_scheduled_event(guild_id.0, event_id.0, false)
            .await?;

        self.update_server_event(context, &event).await?;

        Ok(())
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        let Ok(old_command) = context
            .http
            .get_guild_application_commands(*guild_id.as_u64())
            .await
        else {
            return;
        };

        let Some(old_command) = old_command
            .into_iter()
            .find(|command| command.name == "event")
        else {
            return;
        };

        let Err(e) = context
            .http
            .delete_guild_application_command(*guild_id.as_u64(), old_command.id.0)
            .await
        else {
            return;
        };

        log::error!("Failed to old command - {e:?}");
    }

    async fn guild_scheduled_event(&self, context: &Context, event: ScheduledEventUpdated<'_>) {
        match event {
            ScheduledEventUpdated::Created(event)
            | ScheduledEventUpdated::Updated(event)
            | ScheduledEventUpdated::Deleted(event) => {
                if let Err(e) = self.update_server_event(context, event).await {
                    error!("Failed to handle scheduled event update: {e:?}");
                }
            }
            ScheduledEventUpdated::UserAdded(event) => {
                if let Err(e) = self
                    .update_server_event_user(
                        context,
                        event.scheduled_event_id,
                        event.guild_id,
                        event.user_id,
                        true,
                    )
                    .await
                {
                    error!("Failed to handle scheduled event user add event: {e:?}");
                }
            }
            ScheduledEventUpdated::UserRemoved(event) => {
                if let Err(e) = self
                    .update_server_event_user(
                        context,
                        event.scheduled_event_id,
                        event.guild_id,
                        event.user_id,
                        false,
                    )
                    .await
                {
                    error!("Failed to handle scheduled event user add event: {e:?}");
                }
            }
        }
    }
}
