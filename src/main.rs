use anyhow::Context as _;
use std::{
    ops::Deref,
    sync::atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use log::{debug, error, info, trace};
use serenity::{
    builder::{CreateEmbed, CreateInteractionResponseData, CreateMessage},
    client::{Context, EventHandler},
    model::{
        channel::Message,
        guild::Member,
        id::{ChannelId, GuildId, MessageId, UserId},
        interactions::{Interaction, InteractionResponseType, InteractionType},
        prelude::Ready,
    },
    Client,
};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

mod application_command;
use self::application_command::*;

const EUEOEO: &str = "으어어";
const COMMAND_NAME: &str = "eueoeo";

const MESSAGES_LIMIT: u64 = 100;

#[repr(transparent)]
struct AtomicMessageId(AtomicU64);

impl Deref for AtomicMessageId {
    type Target = AtomicU64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct Handler {
    db_pool: SqlitePool,
    last_message_id: AtomicMessageId,
    guild_id: GuildId,
    channel_id: ChannelId,
}

// Is eueoeo by human?
fn check_message(message: &Message) -> bool {
    !(message.author.bot || message.edited_timestamp.is_some() || message.content != EUEOEO)
}

// common interface for message
trait EmbeddableMessage {
    fn content<D: ToString>(&mut self, content: D) -> &mut Self;
    fn embed<F: FnOnce(&mut CreateEmbed) -> &mut CreateEmbed>(&mut self, f: F) -> &mut Self;
}

impl EmbeddableMessage for CreateInteractionResponseData {
    fn content<D: ToString>(&mut self, content: D) -> &mut Self {
        self.content(content)
    }

    fn embed<F: FnOnce(&mut CreateEmbed) -> &mut CreateEmbed>(&mut self, f: F) -> &mut Self {
        // workaround. It would be fixed after 0.10.5
        let mut embed = CreateEmbed::default();
        f(&mut embed);
        let map = serenity::utils::hashmap_to_json_map(embed.0);
        let embed = serde_json::Value::Array(vec![serde_json::Value::Object(map)]);

        self.0.insert("embeds", embed);

        self
    }
}

impl<'a> EmbeddableMessage for CreateMessage<'a> {
    fn content<D: ToString>(&mut self, content: D) -> &mut Self {
        self.content(content)
    }

    fn embed<F: FnOnce(&mut CreateEmbed) -> &mut CreateEmbed>(&mut self, f: F) -> &mut Self {
        self.embed(f)
    }
}

impl Handler {
    async fn incr_counter(&self, message: &Message) -> anyhow::Result<bool> {
        trace!("insert {}", &message.id);
        let message_id = *message.id.as_u64() as i64;
        let author_id = *message.author.id.as_u64() as i64;
        let message_date = message.timestamp.date();
        let prev_date = message_date.pred().and_hms(0, 0, 0).timestamp();
        let message_date = message_date.and_hms(0, 0, 0).timestamp();
        let affected = match sqlx::query!(
            "INSERT INTO history (message_id, user_id, date) VALUES (?, ?, ?)",
            message_id,
            author_id,
            message_date
        )
        .execute(&self.db_pool)
        .await
        {
            Ok(_) => true,
            Err(sqlx::Error::Database(e)) => {
                let msg = e.message();
                if msg.contains("constraint") {
                    info!(
                        "Duplicated item - user: {}, message_id: {}, date: {}",
                        author_id, message_id, message_date
                    );
                    false
                } else {
                    return Err(sqlx::Error::Database(e)).context("Unknown database error");
                }
            }
            Err(e) => return Err(e).context("unknown sqlx error"),
        };
        if affected {
            let data = sqlx::query!(
                "SELECT longest_streaks, current_streaks, last_date FROM users WHERE user_id = ?",
                author_id
            )
            .fetch_optional(&self.db_pool)
            .await
            .context("Failed to query user info")?;
            let data = if let Some(data) = data {
                data
            } else {
                info!(
                    "Try to increase counter for unknown user - {}({})",
                    &message.author.name, author_id
                );

                return Ok(false);
            };
            let (longest_streaks, current_streaks) = if data.last_date == prev_date {
                let current_streaks = data.current_streaks + 1;
                (
                    std::cmp::max(data.longest_streaks, current_streaks),
                    current_streaks,
                )
            } else {
                (data.longest_streaks, 1)
            };
            sqlx::query!(
                r#"UPDATE users SET 
                    count = count + 1, 
                    longest_streaks = ?, 
                    current_streaks = ?, 
                    last_date = ? 
                WHERE user_id = ?"#,
                longest_streaks,
                current_streaks,
                message_date,
                author_id
            )
            .execute(&self.db_pool)
            .await?;

            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn update_last_id(&self, message_id: &MessageId) {
        let message_id = *message_id.as_u64() as i64;
        sqlx::query!(
            "INSERT OR REPLACE INTO last_id (id, message_id) VALUES (0, ?)",
            message_id
        )
        .execute(&self.db_pool)
        .await
        .unwrap();
    }

    async fn fetch_statistics(&self) -> Vec<(String, i64)> {
        let stats =
            sqlx::query!("SELECT name, count from users WHERE count > 0 ORDER BY count desc")
                .fetch_all(&self.db_pool)
                .await
                .unwrap();

        stats
            .into_iter()
            .map(|stat| (stat.name, stat.count))
            .collect()
    }

    // statistics obtains counting statistics from the DB and does some shit
    fn create_statistics<'b, M: EmbeddableMessage>(
        &self,
        msg: &'b mut M,
        stats: Vec<(String, i64)>,
    ) -> &'b mut M {
        if stats.is_empty() {
            msg.content("Empty records")
        } else {
            msg.embed(move |e| {
                e.title("Eueoeo records");
                for stat in stats {
                    e.field(stat.0, stat.1, true);
                }
                e
            })
        }
    }

    // update_members takes a member list and updates DB with it
    async fn update_members<T: IntoIterator<Item = Member>>(
        &self,
        members: T,
    ) -> anyhow::Result<Option<UserId>> {
        let iter = members.into_iter();
        let mut largest_user_id: Option<UserId> = None;
        for member in iter {
            let user_id = *member.user.id.as_u64() as i64;
            if largest_user_id.unwrap_or(0.into()) < member.user.id {
                largest_user_id = Some(member.user.id.clone());
            }

            // if there is no nickname, use member's name
            let name = member.nick.unwrap_or(member.user.name);

            info!(
                "Try insert or update name for user {} - id: {}",
                &name, user_id
            );

            sqlx::query!(
                "INSERT INTO users (user_id, name) VALUES (?, ?) ON CONFLICT (user_id) DO UPDATE SET name = ?",
                user_id,
                name,
                name
            )
            .execute(&self.db_pool)
            .await
            .context("Failed to insert user")?;
        }

        Ok(largest_user_id)
    }

    async fn process_message_history(
        &self,
        messages: &[Message],
    ) -> anyhow::Result<Option<MessageId>> {
        let mut most_new_id = 0;
        let queries = (&messages).iter().filter_map(|message| {
            most_new_id = std::cmp::max(most_new_id, *message.id.as_u64());

            if check_message(message) {
                Some(self.incr_counter(message))
            } else {
                None
            }
        });
        for query in queries {
            query.await.context("Failed to increase counter")?;
        }

        Ok(if messages.len() < MESSAGES_LIMIT as _ {
            None
        } else {
            Some(most_new_id.into())
        })
    }
}

#[async_trait]
impl EventHandler for Handler {
    // on connected to discord and cache system is ready
    // note: serenity makes a caching system for discord API to store discord information (i.e. member, channel info)
    async fn cache_ready(&self, context: Context, _: Vec<GuildId>) {
        let channel = context
            .cache
            .guild_channel(self.channel_id)
            .await
            .expect("Specified channel name is not found");
        let guild = context
            .cache
            .guild(self.guild_id)
            .await
            .expect("Specified guild is not found");
        {
            let mut user_id = None;
            loop {
                let members = guild
                    .members(&context.http, None, user_id)
                    .await
                    .expect("Failed to retrieve member info");
                let id = self
                    .update_members(members)
                    .await
                    .expect("Failed to update member");
                if id.is_none() {
                    break;
                }
                user_id = id;
            }
        }

        // When channel has any message
        // crawl all messages
        if let Some(last_message_id) = channel.last_message_id {
            // saved last message id
            let mut prev_message_id = MessageId(
                self.last_message_id
                    .swap(*last_message_id.as_u64(), Ordering::AcqRel),
            );
            debug!("current last message id is {}", last_message_id);

            while prev_message_id < last_message_id {
                debug!("get history after {}", prev_message_id);
                let mut messages = channel
                    .messages(context.http.as_ref(), |req| {
                        req.after(prev_message_id).limit(MESSAGES_LIMIT)
                    })
                    .await
                    .expect("Failed to get message history");
                messages.sort_by_cached_key(|i| i.id);

                if let Some(message_id) = self
                    .process_message_history(&messages)
                    .await
                    .expect("Failed to process messages")
                {
                    prev_message_id = message_id;
                } else {
                    break;
                }
            }

            let _ = self.update_last_id(&last_message_id).await;
        }

        info!("Ready!");
    }

    // on connected to discord
    async fn ready(&self, ctx: Context, _data_about_bot: Ready) {
        // register or update slash command
        let commands = ctx
            .http
            .get_guild_application_commands(*self.guild_id.as_u64())
            .await
            .unwrap();

        let command = ApplicationCommand {
            name: COMMAND_NAME.to_string(),
            description: "show eueoeo stats".to_string(),
            options: vec![],
        };

        // TODO: check the command is latest. If not, override it
        if commands.iter().any(|cmd| PartialEq::eq(&command, &cmd)) {
            ctx.http
                .create_guild_application_command(
                    *self.guild_id.as_u64(),
                    &serde_json::to_value(command).unwrap(),
                )
                .await
                .unwrap();
        }
    }

    // run on any message event
    async fn message(&self, _: Context, message: Message) {
        if message
            .guild_id
            .map(|id| id != self.guild_id)
            .unwrap_or(false)
        {
            return;
        }

        let _ = self.update_last_id(&message.id).await;

        if message.channel_id != self.channel_id {
            return;
        }

        if !check_message(&message) {
            return;
        }

        self.last_message_id
            .store(message.id.into(), Ordering::SeqCst);
        self.incr_counter(&message)
            .await
            .expect("Failed to increase counter");
    }

    // run on firing slash command
    async fn interaction_create(&self, context: Context, interaction: Interaction) {
        let interaction = if let Some(command) = interaction.application_command() {
            command
        } else {
            return;
        };
        if interaction.guild_id != Some(self.guild_id) {
            return;
        }

        // futaba uses only application command.
        if interaction.kind != InteractionType::ApplicationCommand {
            return;
        }

        if interaction.data.name != COMMAND_NAME {
            return;
        }

        let stats = self.fetch_statistics().await;
        if let Err(e) = interaction
            .create_interaction_response(&context.http, |r| {
                r.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|d| self.create_statistics(d, stats))
            })
            .await
        {
            error!("Failed to send message: {:?}", e);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pretty_env_logger::init();

    let token = std::env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN is mandatory");
    let guild_id = std::env::var("GUILD_ID")
        .context("GUILD_ID is mandatory")?
        .parse()?;
    let channel_id = std::env::var("CHANNEL_ID")
        .context("CHANNEL_ID is mandatory")?
        .parse()?;
    let application_id = std::env::var("APPLICATION_ID")
        .context("APPLICATION_ID is mandatory")?
        .parse()?;
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

    // Get last saved message_id from DB. If not exists, got 0.
    let last_message_id = AtomicMessageId(
        match sqlx::query!("SELECT message_id as `message_id:i64` FROM last_id WHERE id = 0")
            .fetch_one(&db_pool)
            .await
        {
            Ok(row) => {
                let last_id = row.message_id as u64;
                debug!("Previous last_message_id = {}", last_id);
                last_id.into()
            }
            Err(_) => {
                let id: u64 = std::env::var("INIT_MESSAGE_ID")
                    .context("INIT_MESSAGE_ID")?
                    .parse()?;
                id.into()
            }
        },
    );

    // prepare serenity(discord api framework)
    let mut client = Client::builder(&token)
        .application_id(application_id)
        .event_handler(Handler {
            db_pool,
            guild_id: GuildId(guild_id),
            channel_id: ChannelId(channel_id),

            last_message_id,
        })
        .await?;

    let shard_manager = client.shard_manager.clone();

    // stop the bot when SIGINT occured.
    tokio::spawn(async move {
        // wait SIGINT on another running context(thread)
        tokio::signal::ctrl_c()
            .await
            .expect("Could not register ctrl+c handler");
        shard_manager.lock().await.shutdown_all().await;
    });

    client.start().await?;

    Ok(())
}
