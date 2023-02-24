use anyhow::Context as _;
use chrono::{DateTime, Duration, TimeZone, Utc};

use async_trait::async_trait;
use log::info;
use serenity::{
    client::{Context, EventHandler},
    model::{
        application::interaction::{Interaction, InteractionType},
        channel::Message,
        gateway::GatewayIntents,
        guild::Member,
        id::{ChannelId, GuildId, MessageId, UserId},
        prelude::{
            interaction::{
                application_command::ApplicationCommandInteraction,
                autocomplete::AutocompleteInteraction,
            },
            Ready, ResumedEvent,
        },
    },
    Client,
};
use sqlx::SqlitePool;

pub mod application_command;

use crate::eueoeo::DiscordHandler as EueoeoHandler;
use crate::events::DiscordHandler as EventsHandler;

#[async_trait]
pub trait SubApplication {
    async fn cache_ready(&self, _context: &Context, _guild_id: GuildId) {}
    async fn ready(&self, _context: &Context, _guild_id: GuildId) {}
    async fn resume(&self, _context: &Context) {}
    async fn message(&self, _context: &Context, _message: &Message) {}
    async fn application_command_interaction_create(
        &self,
        _context: &Context,
        _interaction: &ApplicationCommandInteraction,
    ) -> bool {
        false
    }
    async fn autocomplete(
        &self,
        _context: &Context,
        _interaction: &AutocompleteInteraction,
    ) -> bool {
        false
    }
    async fn update_member(&self, _member: &Member) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Handler {
    eueoeo: EueoeoHandler,
    calendar: EventsHandler,
    guild_id: GuildId,
}

pub trait IntoSnowflakes {
    fn into_snowflakes(self) -> i64;
}

impl<TZ: TimeZone> IntoSnowflakes for DateTime<TZ> {
    // See https://discord.com/developers/docs/reference#snowflakes
    fn into_snowflakes(self) -> i64 {
        let ts = self.with_timezone(&Utc).timestamp() * 1000;

        (ts - 1420070400000i64) << 22
    }
}

impl IntoSnowflakes for Duration {
    fn into_snowflakes(self) -> i64 {
        self.num_milliseconds() << 22
    }
}

pub fn from_snowflakes<TZ: TimeZone>(tz: &TZ, snowflakes: i64) -> chrono::DateTime<TZ> {
    tz.from_utc_datetime(&chrono::NaiveDateTime::from_timestamp(
        ((snowflakes >> 22) + 1420070400000i64) / 1000,
        0,
    ))
}

#[async_trait]
impl EventHandler for Handler {
    // on connected to discord and cache system is ready
    // note: serenity makes a caching system for discord API to store discord information (i.e. member, channel info)
    async fn cache_ready(&self, context: Context, _: Vec<GuildId>) {
        let guild = context
            .cache
            .guild(self.guild_id)
            .expect("Specified guild is not found");
        {
            let mut user_id = None;
            loop {
                let members = guild
                    .members(&context.http, None, user_id)
                    .await
                    .expect("Failed to retrieve member info");

                let iter = members.into_iter();
                let mut largest_user_id: Option<UserId> = None;
                for member in iter {
                    if largest_user_id.unwrap_or_else(|| 0.into()) < member.user.id {
                        largest_user_id = Some(member.user.id);
                    }
                    self.eueoeo
                        .update_member(&member)
                        .await
                        .expect("Failed to update member");
                }

                if largest_user_id.is_none() {
                    break;
                }
                user_id = largest_user_id;
            }
        }

        info!("Ready!");
    }

    async fn resume(&self, context: Context, _: ResumedEvent) {
        self.eueoeo.resume(&context).await;
    }

    // on connected to discord
    async fn ready(&self, ctx: Context, _data_about_bot: Ready) {
        self.eueoeo.ready(&ctx, self.guild_id).await;

        info!("ready");
    }

    async fn guild_member_addition(&self, _: Context, new_member: Member) {
        self.eueoeo
            .update_member(&new_member)
            .await
            .expect("Failed to update member");
    }

    // run on any message event
    async fn message(&self, ctx: Context, message: Message) {
        if message
            .guild_id
            .map(|id| id != self.guild_id)
            .unwrap_or(false)
        {
            return;
        }

        self.eueoeo.message(&ctx, &message).await;
    }

    // run on firing slash command
    async fn interaction_create(&self, context: Context, interaction: Interaction) {
        match interaction.kind() {
            InteractionType::ApplicationCommand => {
                let interaction = if let Some(command) = interaction.application_command() {
                    command
                } else {
                    return;
                };
                if interaction.guild_id != Some(self.guild_id) {
                    return;
                }

                if self
                    .eueoeo
                    .application_command_interaction_create(&context, &interaction)
                    .await
                {
                    return;
                }
                self.calendar
                    .application_command_interaction_create(&context, &interaction)
                    .await;
            }
            InteractionType::Autocomplete => {
                let autocomplete = if let Some(autocomplete) = interaction.autocomplete() {
                    autocomplete
                } else {
                    return;
                };

                self.calendar.autocomplete(&context, &autocomplete).await;
            }
            _ => {}
        }
    }
}

pub async fn start(
    db_pool: SqlitePool,
    mut stop_signal: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let token = std::env::var("DISCORD_BOT_TOKEN").context("DISCORD_BOT_TOKEN is mandatory")?;
    let guild_id = std::env::var("GUILD_ID")
        .context("GUILD_ID is mandatory")?
        .parse()?;
    let channel_id = std::env::var("EUEOEO_CHANNEL_ID")
        .context("EUEOEO_CHANNEL_ID is mandatory")?
        .parse()?;
    let application_id = std::env::var("APPLICATION_ID")
        .context("APPLICATION_ID is mandatory")?
        .parse()?;

    // Get last saved message_id from DB. If not exists, got 0.
    let last_message_id = MessageId(
        match sqlx::query!(
            "SELECT message_id as `message_id:i64` FROM history order by message_id desc limit 1"
        )
        .fetch_one(&db_pool)
        .await
        {
            Ok(row) => {
                let last_id = row.message_id as u64;
                info!("Previous last_message_id from db = {}", last_id);
                last_id
            }
            Err(e) => {
                info!("Failed to get last_id from db - {:?}", e);
                info!("Use last id from env config");
                let id: u64 = std::env::var("EUEOEO_INIT_MESSAGE_ID")
                    .context("EUEOEO_INIT_MESSAGE_ID is mandatory for initial run")?
                    .parse()?;
                id
            }
        },
    );
    info!("Previous last_message_id = {}", last_message_id);

    // prepare serenity(discord api framework)
    let mut client = Client::builder(
        &token,
        GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MEMBERS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_PRESENCES
            | GatewayIntents::MESSAGE_CONTENT,
    )
    .application_id(application_id)
    .event_handler(Handler {
        guild_id: GuildId(guild_id),
        eueoeo: EueoeoHandler {
            db_pool: db_pool.clone(),
            channel_id: ChannelId(channel_id),
            init_message_id: last_message_id,
        },
        calendar: EventsHandler {
            db_pool: db_pool.clone(),
        },
    })
    .await?;

    let shard_manager = client.shard_manager.clone();

    // stop the bot when SIGINT occurred.
    tokio::spawn(async move {
        stop_signal.recv().await.expect("Stop signal is broken");
        info!("stop discord");
        shard_manager.lock().await.shutdown_all().await;
        info!("discord closed");
    });

    client.start().await?;

    Ok(())
}
