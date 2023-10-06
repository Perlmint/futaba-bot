use anyhow::Context as _;
use async_trait::async_trait;
use log::error;
use serenity::{
    model::prelude::{
        interaction::{
            application_command::{ApplicationCommandInteraction, CommandDataOption},
            InteractionResponseType,
        },
        GuildId,
    },
    prelude::Context,
};
use sqlx::SqlitePool;

use crate::discord::{
    application_command::{
        ApplicationCommand, ApplicationCommandOption, ApplicationCommandOptionType,
    },
    SubApplication,
};

pub struct DiscordHandler {
    db_pool: SqlitePool,
}

const COMMAND_NAME: &str = "user";

impl DiscordHandler {
    pub fn new(db_pool: SqlitePool) -> Self {
        Self {
            db_pool,
        }
    }
    async fn handle_google_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        let user_id = *interaction.user.id.as_u64() as i64;
        let google_email = option
            .options
            .first()
            .unwrap()
            .value
            .as_ref()
            .unwrap()
            .as_str()
            .unwrap();
        sqlx::query!(
            "UPDATE `users` SET `google_email` = ? WHERE `user_id` = ?",
            google_email,
            user_id
        )
        .execute(&self.db_pool)
        .await
        .context("Failed to store google email to DB")?;

        interaction
            .create_interaction_response(context, |f| {
                f.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|f| f.content("Google email registered"))
            })
            .await
            .context("Failed to send interaction response")?;

        Ok(())
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
                options: vec![ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::String,
                    name: "google id",
                    required: Some(true),
                    description: "google id",
                    ..Default::default()
                }],
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
