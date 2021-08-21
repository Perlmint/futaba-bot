use std::{
    collections::{hash_map::Entry, HashMap},
    ops::Deref,
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
};

use async_trait::async_trait;
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

const EUEOEO: &str = "으어어";
const COMMAND_NAME: &str = "eueoeo";
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
    members: RwLock<HashMap<UserId, (AtomicU64, String)>>,
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
    async fn incr_counter(&self, message: &Message) {
        {
            let counter = self.members.read().unwrap();
            if let Some(counter) = counter.get(&message.author.id) {
                counter.0.fetch_add(1, Ordering::AcqRel);
            }
        }

        println!("insert {}", &message.id);
        let message_id = *message.id.as_u64() as i64;
        let author_id = *message.author.id.as_u64() as i64;
        let timestamp = message.timestamp.timestamp();
        sqlx::query!(
            "INSERT INTO history (message_id, user_id, date) VALUES (?, ?, ?);
        UPDATE users SET count = count + 1 WHERE user_id = ?;",
            message_id,
            author_id,
            timestamp,
            author_id
        )
        .persistent(true)
        .execute(&self.db_pool)
        .await
        .unwrap();
    }

    async fn update_last_id(&self, message_id: &MessageId) {
        let message_id = *message_id.as_u64() as i64;
        sqlx::query!(
            "INSERT OR REPLACE INTO last_id (id, message_id) VALUES (0, ?)",
            message_id
        )
        .persistent(true)
        .execute(&self.db_pool)
        .await
        .unwrap();
    }

    fn statistics<'b, M: EmbeddableMessage>(&self, msg: &'b mut M) -> &'b mut M {
        let mut stats = {
            let counter = self.members.read().unwrap();
            counter
                .iter()
                .filter_map(|(_, (count, name))| {
                    let count = count.load(Ordering::Acquire);
                    (count != 0).then(|| (name.clone(), count))
                })
                .collect::<Vec<_>>()
        };

        stats.sort_by_key(|i| i.1);
        stats.reverse();

        if stats.is_empty() {
            msg.content("Empty records")
        } else {
            msg.embed(move |e| {
                e.title("Eueoeo records");
                for (name, count) in stats {
                    e.field(name, count, true);
                }
                e
            })
        }
    }

    async fn update_members<T: IntoIterator<Item = Member>>(&self, members: T) {
        let iter = {
            let mut counter = self.members.write().unwrap();

            members
                .into_iter()
                .filter_map(move |member| {
                    let cache = counter.entry(member.user.id);
                    let nickname = member.nick.unwrap_or(member.user.name);
                    match cache {
                        Entry::Occupied(mut i) => {
                            i.get_mut().1 = nickname;
                            None
                        }
                        Entry::Vacant(i) => {
                            i.insert((AtomicU64::from(0), nickname.clone()));

                            Some((*member.user.id.as_u64() as i64, nickname))
                        }
                    }
                })
                .collect::<Vec<_>>()
        };

        for (user_id, name) in iter {
            sqlx::query!(
                "INSERT INTO users (user_id, count, name) VALUES (?, 0, ?)",
                user_id,
                name
            )
            .execute(&self.db_pool)
            .await
            .unwrap();
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    // Connected to discrod & cache system is ready
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
            // TODO: should receive all members
            let members = guild.members(&context.http, None, None).await.unwrap();
            self.update_members(members).await;
        }

        // When channel has any message
        // crawl all messages
        if let Some(last_message_id) = channel.last_message_id {
            // saved last message id
            let mut query_message_id = MessageId(
                self.last_message_id
                    .swap(last_message_id.0, Ordering::AcqRel),
            );

            while query_message_id < last_message_id {
                println!("get history after {}", query_message_id);
                const MESSAGES_LIMIT: u64 = 100;
                let messages = channel
                    .messages(context.http.as_ref(), |req| {
                        req.after(query_message_id).limit(MESSAGES_LIMIT)
                    })
                    .await
                    .expect("Failed to get message history");

                let mut most_new_id = u64::MAX;
                let queries = (&messages).iter().filter_map(|message| {
                    most_new_id = std::cmp::max(most_new_id, *message.id.as_u64());

                    if check_message(&message) {
                        Some(self.incr_counter(&message))
                    } else {
                        None
                    }
                });
                for query in queries {
                    query.await;
                }

                if messages.len() < MESSAGES_LIMIT as _ {
                    break;
                }

                query_message_id = most_new_id.into();
            }

            let _ = self.update_last_id(&last_message_id).await;
        }

        println!("Ready!");
    }

    // on connected to discrod
    async fn ready(&self, ctx: Context, data_about_bot: Ready) {
        // register or update slash command
        let commands = ctx
            .http
            .get_guild_application_commands(*self.guild_id.as_u64())
            .await
            .unwrap();

        // TODO: check the command is latest. If not, override it
        if commands
            .iter()
            .find(|cmd| cmd.name == COMMAND_NAME)
            .is_none()
        {
            ctx.http
                .create_guild_application_command(
                    *self.guild_id.as_u64(),
                    &serde_json::json! ({
                        "name": COMMAND_NAME,
                        "description": "show eueoeo stats",
                    }),
                )
                .await
                .unwrap();
        }
    }

    // run on any message event
    async fn message(&self, _: Context, message: Message) {
        if message
            .guild_id
            .map_or_else(|| false, |id| id != self.guild_id)
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
        self.incr_counter(&message).await;
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

        if let Err(e) = interaction
            .create_interaction_response(&context.http, |r| {
                r.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|d| self.statistics(d))
            })
            .await
        {
            eprintln!("Failed to send message: {:?}", e);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = std::env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN is mandatory");
    let guild_id = std::env::var("GUILD_ID")
        .expect("GUILD_ID is mandatory")
        .parse()
        .unwrap();
    let channel_id = std::env::var("CHANNEL_ID")
        .expect("CHANNEL_ID is mandatory")
        .parse()
        .unwrap();
    let application_id = std::env::var("APPLICATION_ID")
        .expect("APPLICATION_ID is mandatory")
        .parse()
        .unwrap();
    let db_pool = SqlitePoolOptions::new()
        .connect(&{
            let mut dir = std::env::current_dir().unwrap();
            dir.push("db");
            let path = format!("sqlite://{}?mode=rwc", dir.display());
            path
        })
        .await
        .unwrap();

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
                println!("Previous last_message_id = {}", last_id);
                last_id.into()
            }
            Err(_) => 0.into(),
        },
    );
    // Load saved all members
    let members = RwLock::new(
        sqlx::query!("SELECT user_id as `user_id:i64`, count, name FROM users")
            .fetch_all(&db_pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    (UserId::from(row.user_id as u64)),
                    (AtomicU64::new(row.count as u64), row.name),
                )
            })
            .collect::<HashMap<_, _>>(),
    );

    // prepare serenity(discord api framework)
    let mut client = Client::builder(&token)
        .application_id(application_id)
        .event_handler(Handler {
            db_pool,
            guild_id: GuildId(guild_id),
            channel_id: ChannelId(channel_id),

            last_message_id,
            members,
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
