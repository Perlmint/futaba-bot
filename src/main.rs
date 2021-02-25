use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, RwLock,
    },
};

use async_trait::async_trait;
use serenity::{
    builder::CreateMessage,
    client::{Context, EventHandler},
    model::{
        channel::Message,
        guild::Member,
        id::{ChannelId, GuildId, MessageId, UserId},
    },
    Client,
};

const EUEOEO: &'static str = "으어어";

struct Handler {
    last_message_id: Mutex<MessageId>,
    guild_id: GuildId,
    channel_id: ChannelId,
    counter: RwLock<HashMap<UserId, AtomicU64>>,
    member_cache: RwLock<HashMap<UserId, String>>,
}

fn check_message(message: &Message) -> bool {
    if message.author.bot {
        return false;
    }

    if message.edited_timestamp.is_some() {
        return false;
    }

    if message.content != EUEOEO {
        return false;
    }

    return true;
}

impl Handler {
    fn incr_counter(&self, user: &UserId) {
        let counter = self.counter.read().unwrap();
        if let Some(counter) = counter.get(user) {
            counter.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn statistics<'a, 'b>(&self, msg: &'b mut CreateMessage<'a>) -> &'b mut CreateMessage<'a> {
        let counter = {
            let counter = self.counter.read().unwrap();
            counter
                .iter()
                .map(|(user_id, count)| (*user_id, count.load(Ordering::Acquire)))
                .collect::<Vec<_>>()
        };
        let member_cache = self.member_cache.read().unwrap();
        let mut stats = Vec::new();
        for (user_id, count) in counter {
            if count != 0 {
                let user = member_cache.get(&user_id);
                if let Some(user) = user {
                    stats.push((user.clone(), count));
                } else {
                    eprintln!("Failed to find user {0:?}", user_id);
                }
            }
        }

        stats.sort_by_key(|i| i.1);
        stats.reverse();

        if stats.is_empty() {
            msg.content("Empty records")
        } else {
            msg.embed(move |e| {
                for (name, count) in stats {
                    e.field(name, count, false);
                }
                e
            })
        }
    }

    fn update_members<T: IntoIterator<Item = Member>>(&self, members: T) {
        let mut counter = self.counter.write().unwrap();
        let mut member_cache = self.member_cache.write().unwrap();
        for member in members {
            member_cache.insert(member.user.id, member.nick.unwrap_or(member.user.name));
            counter.insert(member.user.id, AtomicU64::from(0));
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
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
        let members = guild.members(&context.http, None, None).await.unwrap();
        self.update_members(members);

        if let Some(mut last_message_id) = channel.last_message_id {
            let prev_last_message_id = {
                let mut lm = self.last_message_id.lock().unwrap();

                std::mem::replace(&mut *lm, last_message_id)
            };

            if prev_last_message_id != last_message_id {
                last_message_id.0 += 1;
            }

            while prev_last_message_id != last_message_id {
                let messages = channel
                    .messages(context.http.as_ref(), |req| req.before(last_message_id))
                    .await
                    .expect("Failed to get message history");

                let most_old_id = messages
                    .iter()
                    .filter_map(|message| {
                        if message.id <= prev_last_message_id {
                            return Some(message.id);
                        }

                        if check_message(message) {
                            self.incr_counter(&message.author.id);
                        }

                        return Some(message.id);
                    })
                    .min();

                if most_old_id.is_none() {
                    break;
                }

                last_message_id = most_old_id.unwrap();
            }
        }

        println!("Ready!");
    }

    async fn message(&self, context: Context, message: Message) {
        if message.channel_id != self.channel_id {
            return;
        }

        if !check_message(&message) {
            if message.mentions_me(&context.http).await.unwrap_or(false) {
                if let Err(_) = context
                    .cache
                    .guild_channel(message.channel_id)
                    .await
                    .unwrap()
                    .send_message(&context.http, |m| self.statistics(m))
                    .await
                {
                    eprintln!("Failed to send message");
                }
            }
            return;
        }

        self.incr_counter(&message.author.id);
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

    let mut client = Client::builder(&token)
        .event_handler(Handler {
            guild_id: GuildId(guild_id),
            channel_id: ChannelId(channel_id),

            last_message_id: Mutex::new(0.into()),
            counter: Default::default(),
            member_cache: Default::default(),
        })
        .await?;
    client.start().await?;

    Ok(())
}
