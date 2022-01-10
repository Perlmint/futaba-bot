use anyhow::Context as _;
use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Utc};
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

// See https://discord.com/developers/docs/reference#snowflakes
fn datetime_to_snowflakes<TZ: TimeZone>(datetime: DateTime<TZ>) -> i64 {
    let ts = datetime.with_timezone(&Utc).timestamp() * 1000;

    (ts - 1420070400000i64) << 22
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

// statistics obtains counting statistics from the DB and does some shit
fn create_statistics<'b, M: EmbeddableMessage>(
    msg: &'b mut M,
    title: &str,
    stats: Vec<(String, i64)>,
) -> &'b mut M {
    if stats.is_empty() {
        msg.content("Empty records")
    } else {
        msg.embed(move |e| {
            e.title(title);
            for stat in stats {
                e.field(stat.0, stat.1, true);
            }
            e
        })
    }
}

impl Handler {
    async fn incr_counter(&self, message: &Message) -> anyhow::Result<bool> {
        trace!("insert {}", &message.id);
        let message_id = *message.id.as_u64() as i64;
        let author_id = *message.author.id.as_u64() as i64;
        let offset = FixedOffset::east(9 * 3600);
        let message_date = message.timestamp.with_timezone(&offset).date();
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

    async fn fetch_yearly_statistics(&self, year: Option<i32>) -> (i32, Vec<(String, i64)>) {
        let offset = FixedOffset::east(9 * 3600);
        let year = year.unwrap_or_else(|| chrono::Local::now().year());
        let begin_date = datetime_to_snowflakes(offset.ymd(year, 1, 1).and_hms(0, 0, 0));
        let end_date = datetime_to_snowflakes(offset.ymd(year + 1, 1, 1).and_hms(0, 0, 0));
        info!(
            "yearly stats {}-01-01({}) ~ {}-01-01({})",
            year,
            begin_date,
            year + 1,
            end_date
        );

        let stats = sqlx::query!(
            r#"SELECT
                users.name,
                count(history.message_id) AS "count: i64"
            FROM
                history
            INNER JOIN
                users ON history.user_id = users.user_id
            WHERE
                history.message_id >= ? AND
                history.message_id < ?
            GROUP BY
                history.user_id;
            "#,
            begin_date,
            end_date
        )
        .fetch_all(&self.db_pool)
        .await
        .unwrap();

        // order by is not works correctly.
        let mut stats = stats
            .into_iter()
            .map(|stat| (stat.name, stat.count.unwrap()))
            .collect::<Vec<_>>();

        stats.sort_by_cached_key(|i| i.1);
        stats.reverse();

        (year, stats)
    }

    async fn fetch_streaks(&self, longest: bool) -> Vec<(String, i64)> {
        macro_rules! fetch_streaks {
            ($query:literal) => {{
                let stats = sqlx::query!($query).fetch_all(&self.db_pool).await.unwrap();
                stats
                    .into_iter()
                    .map(|stat| (stat.name, stat.streaks))
                    .collect()
            }};
        }

        if longest {
            fetch_streaks!(
                r#"SELECT
                    name,
                    longest_streaks as streaks
                FROM
                    users
                ORDER BY
                    longest_streaks DESC;
                "#
            )
        } else {
            fetch_streaks!(
                r#"SELECT
                    name,
                    current_streaks as streaks
                FROM
                    users
                ORDER BY
                    current_streaks DESC;
                "#
            )
        }
    }

    async fn fetch_streaks_details(&self, user_id: i64) -> (String, i64, i64) {
        let ret = sqlx::query!(
            r#"SELECT
                name,
                longest_streaks,
                current_streaks
            FROM
                users
            WHERE
                user_id = ?"#,
            user_id
        )
        .fetch_one(&self.db_pool)
        .await
        .unwrap();

        (ret.name, ret.longest_streaks, ret.current_streaks)
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
            name: COMMAND_NAME,
            description: "show eueoeo stats",
            options: vec![
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "year",
                    description: "yearly count",
                    required: None,
                    choices: vec![],
                    options: vec![ApplicationCommandOption {
                        kind: ApplicationCommandOptionType::Integer,
                        name: "yaer",
                        description: "default is current year.",
                        required: Some(false),
                        choices: vec![],
                        options: vec![],
                    }],
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommandGroup,
                    name: "streaks",
                    description: "streak deatils of specified user or streaks ranking",
                    required: None,
                    choices: vec![],
                    options: vec![
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::SubCommand,
                            name: "ranking",
                            description: "ranking",
                            required: None,
                            choices: vec![],
                            options: vec![ApplicationCommandOption {
                                kind: ApplicationCommandOptionType::String,
                                name: "type",
                                description: "ranking basis",
                                required: Some(true),
                                choices: vec![
                                    ApplicationCommandOptionChoice {
                                        name: "current",
                                        value: serde_json::json!("current"),
                                    },
                                    ApplicationCommandOptionChoice {
                                        name: "longest",
                                        value: serde_json::json!("longest"),
                                    },
                                ],
                                options: vec![],
                            }],
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::SubCommand,
                            name: "details",
                            description: "details",
                            required: None,
                            choices: vec![],
                            options: vec![ApplicationCommandOption {
                                kind: ApplicationCommandOptionType::User,
                                name: "user",
                                description: "user",
                                required: Some(true),
                                choices: vec![],
                                options: vec![],
                            }],
                        },
                    ],
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "total",
                    description: "total ranking",
                    required: None,
                    choices: vec![],
                    options: vec![],
                },
            ],
        };

        if let Some(cmd) = commands.iter().find(|cmd| cmd.name == command.name) {
            if PartialEq::ne(&command, cmd) {
                ctx.http
                    .edit_guild_application_command(
                        *self.guild_id.as_u64(),
                        *cmd.id.as_u64(),
                        &serde_json::to_value(command).unwrap(),
                    )
                    .await
                    .unwrap();
            } else {
                return;
            }
        } else {
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

        let option = interaction
            .data
            .options
            .first()
            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
        match option.name.as_str() {
            "year" => {
                let year_arg = option
                    .options
                    .first()
                    .and_then(|opt| {
                        if let Some(v) = &opt.value {
                            v.as_u64()
                        } else {
                            None
                        }
                    })
                    .map(|v| v as i32);
                let (year, stats) = self.fetch_yearly_statistics(year_arg).await;
                if let Err(e) = interaction
                    .create_interaction_response(&context.http, |r| {
                        r.kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|d| {
                                create_statistics(d, &format!("으어어 {}", year), stats)
                            })
                    })
                    .await
                {
                    error!("Failed to send message: {:?}", e);
                }
            }
            "streaks" => {
                let sub_command = option
                    .options
                    .first()
                    .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                match sub_command.name.as_str() {
                    "ranking" => {
                        let ranking_basis = sub_command
                            .options
                            .first()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let ranking_basis = ranking_basis
                            .value
                            .as_ref()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let ranking_basis = ranking_basis
                            .as_str()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let (stat_name, streak_arg) = match ranking_basis {
                            "current" => ("현재 연속", false),
                            "longest" => ("최장 연속", true),
                            _ => unsafe { std::hint::unreachable_unchecked() },
                        };
                        let stats = self.fetch_streaks(streak_arg).await;
                        if let Err(e) = interaction
                            .create_interaction_response(&context.http, |r| {
                                r.kind(InteractionResponseType::ChannelMessageWithSource)
                                    .interaction_response_data(|d| {
                                        create_statistics(
                                            d,
                                            &format!("{} 으어어", stat_name),
                                            stats,
                                        )
                                    })
                            })
                            .await
                        {
                            error!("Failed to send message: {:?}", e);
                        }
                    }
                    "details" => {
                        let user = sub_command
                            .options
                            .first()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let user = user
                            .value
                            .as_ref()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let user = user
                            .as_str()
                            .unwrap_or_else(|| unsafe { std::hint::unreachable_unchecked() });
                        let user: i64 = user
                            .parse()
                            .unwrap_or_else(|_| unsafe { std::hint::unreachable_unchecked() });
                        let (name, longest_streaks, current_streaks) =
                            self.fetch_streaks_details(user).await;

                        if let Err(e) = interaction
                            .create_interaction_response(&context.http, |r| {
                                r.kind(InteractionResponseType::ChannelMessageWithSource)
                                    .interaction_response_data(|d| {
                                        d.create_embed(|e| {
                                            e.title(format!("으어어 by {}", &name))
                                                .field("최장 연속", longest_streaks, false)
                                                .field("현재 연속", current_streaks, false)
                                        })
                                    })
                            })
                            .await
                        {
                            error!("Failed to send message: {:?}", e);
                        }
                    }
                    _ => unsafe { std::hint::unreachable_unchecked() },
                }
            }
            "total" => {
                let stats = self.fetch_statistics().await;
                if let Err(e) = interaction
                    .create_interaction_response(&context.http, |r| {
                        r.kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|d| create_statistics(d, "으어어", stats))
                    })
                    .await
                {
                    error!("Failed to send message: {:?}", e);
                }
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
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
