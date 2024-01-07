use anyhow::Context as _;
use async_trait::async_trait;
use log::error;
use serde::Deserialize;
use serenity::{
    model::{
        application::{
            component::{ButtonStyle, InputTextStyle},
            interaction::MessageFlags,
        },
        prelude::{
            interaction::{
                application_command::{ApplicationCommandInteraction, CommandDataOption},
                InteractionResponseType,
            },
            GuildId, UserId,
        },
    },
    prelude::Context,
};
use sqlx::{Row, SqlitePool};

mod google;

use crate::discord::{
    application_command::{
        ApplicationCommand, ApplicationCommandOption, ApplicationCommandOptionType,
    },
    SubApplication,
};

use self::google::GoogleUserHandler;

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Config {
    google_oauth_secret_path: String,
    redirect_prefix: String,
}

pub struct DiscordHandler {
    db_pool: SqlitePool,
    google: GoogleUserHandler,
}

const COMMAND_NAME: &str = "user";

impl DiscordHandler {
    pub async fn new(db_pool: SqlitePool, config: &super::Config) -> Self {
        Self {
            db_pool,
            google: GoogleUserHandler::new(
                &config.user.google_oauth_secret_path,
                &config.user.redirect_prefix,
            )
            .await,
        }
    }

    async fn handle_google_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        let user_id = *interaction.user.id.as_u64() as i64;

        let url = self.google.auth(user_id, self.db_pool.clone()).await?;

        interaction
            .create_interaction_response(context, |c| {
                c.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|c| {
                        c.components(|c| {
                            c.create_action_row(|c| {
                                c.create_button(|c| {
                                    c.label("Login").style(ButtonStyle::Link).url(url.0)
                                })
                            })
                        })
                        .flags(MessageFlags::EPHEMERAL)
                    })
            })
            .await
            .context("Failed to send interaction response")?;

        Ok(())
    }

    pub async fn get_google_id(db: &SqlitePool, user_id: UserId) -> anyhow::Result<Option<String>> {
        let user_id = *user_id.as_u64() as i64;
        let ret = sqlx::query!(
            "SELECT `google_email` FROM `users` WHERE `user_id` = ?",
            user_id
        )
        .fetch_optional(db)
        .await?;

        Ok(ret.and_then(|d| d.google_email))
    }

    pub async fn get_google_ids(
        db: &SqlitePool,
        user_ids: impl Iterator<Item = UserId>,
    ) -> anyhow::Result<Vec<String>> {
        let mut builder =
            sqlx::QueryBuilder::new("SELECT `google_email` FROM `users` WHERE `user_id` IN ");
        let mut users = builder.separated(",");
        users.push_unseparated("(");
        for user_id in user_ids {
            users.push(*user_id.as_u64() as i64);
        }
        users.push_unseparated(")");

        Ok(builder
            .build()
            .fetch_all(db)
            .await?
            .into_iter()
            .map(|record| record.get::<'_, String, _>(0))
            .collect())
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        // register or update slash command
        let command = ApplicationCommand {
            name: COMMAND_NAME,
            description: "user setting",
            options: vec![ApplicationCommandOption {
                kind: ApplicationCommandOptionType::SubCommand,
                name: "google",
                description: "link google id",
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

    async fn application_command_interaction_create(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> bool {
        if interaction.data.name != COMMAND_NAME {
            return false;
        }

        let option = unsafe { interaction.data.options.first().unwrap_unchecked() };
        if let Err(e) = match option.name.as_str() {
            "google" => {
                self.handle_google_command(context, interaction, option)
                    .await
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
        } {
            error!("Failed to handle message: {:?}", e);
        }

        true
    }
}

pub fn web_router<S: Sync + Send + Clone + 'static>() -> axum::Router<S> {
    axum::Router::new().nest("/google", google::web_router())
}
