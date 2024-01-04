use chrono::{DateTime, Duration, TimeZone, Utc};

use async_trait::async_trait;
use log::info;
use serde::Deserialize;
use serenity::{
    client::{Context, EventHandler},
    http::CacheHttp,
    model::{
        application::interaction::{Interaction, InteractionType},
        channel::Message,
        gateway::GatewayIntents,
        guild::Member,
        id::{ChannelId, GuildId, UserId},
        prelude::{
            interaction::{
                application_command::{ApplicationCommandInteraction, CommandDataOption},
                autocomplete::AutocompleteInteraction,
            },
            Channel, GuildScheduledEventUserAddEvent, GuildScheduledEventUserRemoveEvent, Ready,
            ResumedEvent, ScheduledEvent,
        },
    },
    Client,
};

pub mod application_command;

pub enum ScheduledEventUpdated<'a> {
    Created(&'a ScheduledEvent),
    Updated(&'a ScheduledEvent),
    Deleted(&'a ScheduledEvent),
    UserAdded(&'a GuildScheduledEventUserAddEvent),
    UserRemoved(&'a GuildScheduledEventUserRemoveEvent),
}

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
    async fn guild_scheduled_event(&self, _context: &Context, _event: ScheduledEventUpdated<'_>) {}
}

struct Handler {
    applications: Vec<Box<dyn SubApplication + Send + Sync>>,
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

pub trait CommandHelper {
    fn get_options<const N: usize>(&self, names: &[&str; N]) -> [Option<&CommandDataOption>; N];
}

impl CommandHelper for CommandDataOption {
    fn get_options<const N: usize>(&self, names: &[&str; N]) -> [Option<&CommandDataOption>; N] {
        self.options.get_options(names)
    }
}

impl CommandHelper for Vec<CommandDataOption> {
    fn get_options<const N: usize>(&self, names: &[&str; N]) -> [Option<&CommandDataOption>; N] {
        let mut ret = [None; N];
        for option in self.iter() {
            if let Some(pos) = names.iter().position(|name| name == &option.name) {
                ret[pos] = Some(option);
            }
        }

        ret
    }
}

#[async_trait]
pub trait ChannelHelper {
    async fn get_parent_or_self(&self, cache: &impl CacheHttp) -> Self;
}

#[async_trait]
impl ChannelHelper for ChannelId {
    async fn get_parent_or_self(&self, cache: &impl CacheHttp) -> Self {
        let channel = self
            .to_channel(cache)
            .await
            .expect("Failed to get channel detail");
        if let Channel::Guild(channel) = channel {
            if channel.thread_metadata.is_some() {
                channel.parent_id.unwrap()
            } else {
                *self
            }
        } else {
            *self
        }
    }
}

pub trait CommandDataOptionHelper {
    fn as_str(&self) -> Option<&str>;
    fn as_u64(&self) -> Option<u64>;
    fn as_i64(&self) -> Option<i64>;
    unsafe fn as_str_unchecked(&self) -> &str;
    unsafe fn as_i64_unchecked(&self) -> i64;
}

impl CommandDataOptionHelper for CommandDataOption {
    fn as_str(&self) -> Option<&str> {
        self.value.as_ref().and_then(|v| v.as_str())
    }

    fn as_u64(&self) -> Option<u64> {
        self.value.as_ref().and_then(|v| v.as_u64())
    }

    fn as_i64(&self) -> Option<i64> {
        self.value.as_ref().and_then(|v| v.as_i64())
    }

    unsafe fn as_str_unchecked(&self) -> &str {
        self.value
            .as_ref()
            .unwrap_unchecked()
            .as_str()
            .unwrap_unchecked()
    }

    unsafe fn as_i64_unchecked(&self) -> i64 {
        self.value
            .as_ref()
            .unwrap_unchecked()
            .as_i64()
            .unwrap_unchecked()
    }
}

impl<T: CommandDataOptionHelper> CommandDataOptionHelper for Option<&T> {
    fn as_str(&self) -> Option<&str> {
        self.and_then(|o| o.as_str())
    }

    fn as_u64(&self) -> Option<u64> {
        self.and_then(|o| o.as_u64())
    }

    fn as_i64(&self) -> Option<i64> {
        self.and_then(|o| o.as_i64())
    }

    unsafe fn as_str_unchecked(&self) -> &str {
        self.unwrap_unchecked().as_str_unchecked()
    }

    unsafe fn as_i64_unchecked(&self) -> i64 {
        self.unwrap_unchecked().as_i64_unchecked()
    }
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

                    for app in &self.applications {
                        app.update_member(&member)
                            .await
                            .expect("Failed to update member");
                    }
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
        for app in &self.applications {
            app.resume(&context).await;
        }
    }

    // on connected to discord
    async fn ready(&self, ctx: Context, _data_about_bot: Ready) {
        for app in &self.applications {
            app.ready(&ctx, self.guild_id).await;
        }

        info!("ready");
    }

    async fn guild_member_addition(&self, _: Context, new_member: Member) {
        for app in &self.applications {
            app.update_member(&new_member)
                .await
                .expect("Failed to update member");
        }
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

        for app in &self.applications {
            app.message(&ctx, &message).await;
        }
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

                for app in &self.applications {
                    if app
                        .application_command_interaction_create(&context, &interaction)
                        .await
                    {
                        return;
                    }
                }
            }
            InteractionType::Autocomplete => {
                let autocomplete = if let Some(autocomplete) = interaction.autocomplete() {
                    autocomplete
                } else {
                    return;
                };

                for app in &self.applications {
                    app.autocomplete(&context, &autocomplete).await;
                }
            }
            _ => {}
        }
    }

    async fn guild_scheduled_event_create(&self, context: Context, event: ScheduledEvent) {
        for sub_app in &self.applications {
            sub_app
                .guild_scheduled_event(&context, ScheduledEventUpdated::Created(&event))
                .await;
        }
    }
    async fn guild_scheduled_event_update(&self, context: Context, event: ScheduledEvent) {
        for sub_app in &self.applications {
            sub_app
                .guild_scheduled_event(&context, ScheduledEventUpdated::Updated(&event))
                .await;
        }
    }
    async fn guild_scheduled_event_delete(&self, context: Context, event: ScheduledEvent) {
        for sub_app in &self.applications {
            sub_app
                .guild_scheduled_event(&context, ScheduledEventUpdated::Deleted(&event))
                .await;
        }
    }

    async fn guild_scheduled_event_user_add(
        &self,
        context: Context,
        subscribed: GuildScheduledEventUserAddEvent,
    ) {
        for sub_app in &self.applications {
            sub_app
                .guild_scheduled_event(&context, ScheduledEventUpdated::UserAdded(&subscribed))
                .await;
        }
    }
    async fn guild_scheduled_event_user_remove(
        &self,
        context: Context,
        unsubscribed: GuildScheduledEventUserRemoveEvent,
    ) {
        for sub_app in &self.applications {
            sub_app
                .guild_scheduled_event(&context, ScheduledEventUpdated::UserRemoved(&unsubscribed))
                .await;
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    token: String,
    guild_id: u64,
    application_id: u64,
}

pub(crate) async fn start(
    config: &super::Config,
    sub_applications: Vec<Box<dyn SubApplication + Send + Sync>>,
    mut stop_signal: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let token = &config.discord.token;
    let guild_id = config.discord.guild_id;
    let application_id = config.discord.application_id;

    // prepare serenity(discord api framework)
    let mut client = Client::builder(
        token,
        GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MEMBERS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::GUILD_PRESENCES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILD_SCHEDULED_EVENTS,
    )
    .application_id(application_id)
    .event_handler(Handler {
        guild_id: GuildId(guild_id),
        applications: sub_applications,
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
