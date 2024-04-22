use std::collections::HashMap;

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::DateTime;
use google_calendar3::{
    api::Event as GoogleEvent,
    hyper::{self, client::HttpConnector},
    hyper_rustls::{self, HttpsConnector},
    oauth2::{self, authenticator::HyperClientBuilder},
    CalendarHub,
};
use log::error;
use serde::Deserialize;
use serenity::{
    model::{
        application::{
            component::{ActionRowComponent, InputTextStyle},
            interaction::{
                application_command::{ApplicationCommandInteraction, CommandDataOption},
                modal::ModalSubmitInteraction,
                InteractionResponseType,
            },
        },
        prelude::{GuildId, ScheduledEvent, ScheduledEventId, UserId},
    },
    prelude::Context,
};
use sqlx::{Row, SqlitePool};

use crate::discord::{
    application_command::{
        ApplicationCommand, ApplicationCommandOption, ApplicationCommandOptionType,
    },
    ScheduledEventUpdated, SubApplication,
};

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Config {
    google_service_account_path: String,
}

pub(crate) struct DiscordHandler {
    db_pool: SqlitePool,
    service_account: google_calendar3::oauth2::ServiceAccountKey,
}

const COMMAND_NAME: &str = "event";

impl DiscordHandler {
    pub async fn new(db_pool: SqlitePool, config: &crate::Config) -> anyhow::Result<Self> {
        Ok(Self {
            db_pool,
            service_account: google_calendar3::oauth2::read_service_account_key(
                &config.events.google_service_account_path,
            )
            .await?,
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
                    .build(),
            ),
            auth,
        ))
    }

    async fn discord_event_to_google_event(
        discord_event: &ScheduledEvent,
    ) -> anyhow::Result<GoogleEvent> {
        fn discord_ts_to_google_date_time(
            ts: serenity::model::Timestamp,
        ) -> google_calendar3::api::EventDateTime {
            let ts = ts.timestamp();
            google_calendar3::api::EventDateTime {
                date: None,
                date_time: DateTime::from_timestamp(ts, 0),
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
        let mut saved_events: HashMap<_, _> = sqlx::query!(
            "SELECT `user_id`, `google_event_id` FROM `server_events` WHERE `discord_id` = ?",
            discord_id
        )
        .fetch_all(&self.db_pool)
        .await
        .context("Failed to get saved events from DB")?
        .into_iter()
        .map(|d| (d.user_id, d.google_event_id))
        .collect();

        let users = context
            .http
            .get_scheduled_event_users(event.guild_id.0, event.id.0, None, None, Some(false))
            .await
            .context("Failed to get attendees")?;
        log::debug!("saved_events: {saved_events:?}");

        let hub = self
            .calendar_hub()
            .await
            .context("Failed to create google calendar hub")?;
        let google_event = Self::discord_event_to_google_event(&event)
            .await
            .context("Filed to convert discord event to google event")?;
        log::debug!("converted event: {event:?}");
        let mut update_attendees = HashMap::new();
        let new_attendees: Vec<_> = users
            .into_iter()
            .filter_map(|attendee| {
                let id: i64 = attendee.user.id.0 as i64;
                if let Some((user_id, event_id)) = saved_events.remove_entry(&id) {
                    update_attendees.insert(user_id, event_id);
                    None
                } else {
                    Some(id)
                }
            })
            .collect();
        let resigned_attendees = saved_events;
        log::debug!("attendees\n\tnew: {new_attendees:?}\n\tresign: {resigned_attendees:?}\n\tupdate: {update_attendees:?}");
        let user_calendar_map: HashMap<i64, String> = sqlx::query_builder::QueryBuilder::new(
            "SELECT `user_id`, `google_calendar_id`
            FROM `users`
            WHERE
                `google_calendar_id` IS NOT NULL
                AND `user_id` IN ",
        )
        .push_tuples(
            new_attendees
                .iter()
                .copied()
                .chain(resigned_attendees.keys().copied())
                .chain(update_attendees.keys().copied()),
            |mut b, id| {
                b.push_bind(id);
            },
        )
        .build()
        .fetch_all(&self.db_pool)
        .await
        .context("Failed to get user calendars from DB")?
        .into_iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();

        for (user_id, event_id) in resigned_attendees {
            if let Some(calendar_id) = user_calendar_map.get(&user_id) {
                hub.events()
                    .delete(calendar_id, &event_id)
                    .doit()
                    .await
                    .with_context(|| format!("Failed delete google event for user({user_id})"))?;

                sqlx::query!(
                    "DELETE FROM `server_events`
                    WHERE `discord_id` = ? AND `user_id` = ?",
                    discord_id,
                    user_id
                )
                .execute(&self.db_pool)
                .await
                .context("Failed to delete events in discord")?;
            } else {
                log::warn!("Linked outdated google event is found. but user({user_id}) does not connected to google");
            }
        }

        for user_id in new_attendees {
            if let Some(calendar_id) = user_calendar_map.get(&user_id) {
                let event = hub
                    .events()
                    .insert(google_event.clone(), &calendar_id)
                    .doit()
                    .await
                    .with_context(|| format!("Failed to insert new event in google(calendar - {calendar_id}) for user({user_id})"))?
                    .1;
                let google_event_id = event.id.as_ref().unwrap();
                sqlx::query!(
                    r#"
                    INSERT INTO `server_events`
                        (`discord_id`, `google_event_id`, `user_id`)
                        VALUES 
                        (?, ?, ?)
                    "#,
                    discord_id,
                    google_event_id,
                    user_id,
                )
                .execute(&self.db_pool)
                .await
                .context("Failed to insert google event in DB")?;
            } else {
                log::info!("Google calendar is not connected. Do not create google event for user({user_id}).");
            }
        }

        for (user_id, event_id) in update_attendees {
            if let Some(calendar_id) = user_calendar_map.get(&user_id) {
                hub.events()
                    .update(google_event.clone(), calendar_id, &event_id)
                    .doit()
                    .await
                    .with_context(|| format!("Failed update google event for user({user_id})"))?;
            } else {
                log::warn!("Linked google event is found. but user({user_id}) does not connected to google");
            }
        }

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
            .await
            .context("Failed to get event detail")?;

        self.update_server_event(context, &event).await?;

        Ok(())
    }

    async fn handle_register_google_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        _option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        interaction
            .create_interaction_response(context, |b| {
                b.kind(InteractionResponseType::Modal)
                    .interaction_response_data(|b| {
                        b.custom_id("register_google_calendar")
                            .title("Google 캘린더 등록")
                            .components(|b| {
                                b.create_action_row(|b| {
                                    b.create_input_text(|b| {
                                        b.label("설명")
                                            .required(false)
                                            .custom_id("description")
                                            .placeholder(
                                                "후타바가 이벤트를 동기화 할 캘린더에 대해서 후타바ID 에게 일정 편집 권한을 주세요. 캘린더 ID는 캘린더 설정에서 확인 할 수 있습니다.",
                                            )
                                            .style(InputTextStyle::Paragraph)
                                    })
                                })
                                .create_action_row(|b| {
                                    b.create_input_text(|b| {
                                        b.label("후타바ID")
                                            .required(false)
                                            .custom_id("futaba_id")
                                            .value(self.service_account.client_email.clone())
                                            .style(InputTextStyle::Short)
                                    })
                                })
                                .create_action_row(|b| {
                                    b.create_input_text(|b| {
                                        b.label("캘린더 ID")
                                            .required(true)
                                            .custom_id("calendar_id")
                                            .style(InputTextStyle::Short)
                                    })
                                })
                            })
                            .ephemeral(true)
                    })
            })
            .await?;
        Ok(())
    }

    async fn handle_register_google_calendar_modal_submit(
        &self,
        modal: &ModalSubmitInteraction,
    ) -> anyhow::Result<()> {
        let calendar_id = modal
            .data
            .components
            .iter()
            .find_map(|r| {
                let ActionRowComponent::InputText(input) = r.components.first()? else {
                    return None;
                };

                (input.custom_id == "calendar_id").then_some(input.value.clone())
            })
            .ok_or_else(|| anyhow::anyhow!("Could not find required field"))?;

        let raw_user_id = modal.user.id.0 as i64;
        sqlx::query!(
            "UPDATE `users` SET `google_calendar_id` = ? WHERE `user_id` = ?",
            calendar_id,
            raw_user_id
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to store google calendar id to DB")?;

        Ok(())
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        // register or update slash command
        let command = ApplicationCommand {
            name: COMMAND_NAME,
            description: "event setting",
            options: vec![ApplicationCommandOption {
                kind: ApplicationCommandOptionType::SubCommand,
                name: "register_google",
                description: "register google calendar",
                ..Default::default()
            }],
        };

        context
            .http
            .create_guild_application_command(
                *guild_id.as_u64(),
                &serde_json::to_value(command).unwrap(),
            )
            .await
            .unwrap();
    }

    async fn modal_submit(&self, context: &Context, modal: &ModalSubmitInteraction) -> bool {
        if modal.data.custom_id == "register_google_calendar" {
            if let Err(e) = self
                .handle_register_google_calendar_modal_submit(modal)
                .await
            {
                error!(
                    "Error occurred while handling register google calendar modal submit - {e:?}"
                );
                if let Err(e) = modal
                    .create_interaction_response(context, |b| {
                        b.kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|b| {
                                b.content("등록 실패. 오류 발생").ephemeral(true)
                            })
                    })
                    .await
                {
                    error!("Failed to send response about handling modal submit failure - {e:?}");
                }
            } else {
                if let Err(e) = modal
                    .create_interaction_response(context, |b| {
                        b.kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|b| b.content("등록 완료").ephemeral(true))
                    })
                    .await
                {
                    error!("Failed to send response about handling modal submit success - {e:?}");
                }
            }

            return true;
        }

        false
    }

    async fn application_command_interaction_create(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> bool {
        if interaction.data.name != COMMAND_NAME {
            log::debug!("{interaction:?}");
            return false;
        }

        let option = unsafe { interaction.data.options.first().unwrap_unchecked() };
        if let Err(e) = match option.name.as_str() {
            "register_google" => {
                self.handle_register_google_command(context, interaction, option)
                    .await
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
        } {
            error!("Failed to handle message: {:?}", e);
        }

        true
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
