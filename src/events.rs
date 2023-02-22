use async_trait::async_trait;
use log::error;
use serenity::{
    model::prelude::{
        interaction::{application_command::{ApplicationCommandInteraction, CommandDataOption}, autocomplete::AutocompleteInteraction},
        GuildId,
    },
    prelude::Context,
};
use sqlx::SqlitePool;

use crate::application_command::*;

pub struct Handler {
    pub db_pool: SqlitePool,
}

const COMMAND_NAME: &str = "event";

impl Handler {
    async fn handle_add_command(
        &self,
        _context: &Context,
        _interaction: &ApplicationCommandInteraction,
        _option: &CommandDataOption,
    ) -> serenity::Result<()> {
        Ok(())
    }

    async fn handle_edit_command(
        &self,
        _context: &Context,
        _interaction: &ApplicationCommandInteraction,
        _option: &CommandDataOption,
    ) -> serenity::Result<()> {
        Ok(())
    }

    async fn handle_delete_command(
        &self,
        _context: &Context,
        _interaction: &ApplicationCommandInteraction,
        _option: &CommandDataOption,
    ) -> serenity::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl super::SubApplication for Handler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        // register or update slash command
        let command = ApplicationCommand {
            name: COMMAND_NAME,
            description: "이벤트 관리",
            options: vec![
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "add",
                    description: "이벤트 추가",
                    required: None,
                    choices: vec![],
                    options: vec![
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "name",
                            description: "이벤트 이름",
                            required: Some(true),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "description",
                            description: "상세",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "begin_at",
                            description: "시작 날짜",
                            required: Some(true),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "end_at",
                            description: "종료 날짜",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                    ],
                    autocomplete: None,
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "edit",
                    description: "이벤트 수정",
                    required: None,
                    choices: vec![],
                    options: vec![
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::Integer,
                            name: "id",
                            description: "이벤트 id",
                            required: Some(true),
                            choices: vec![],
                            options: vec![],
                            autocomplete: Some(true),
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "name",
                            description: "이벤트 이름",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "description",
                            description: "상세",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "begin_at",
                            description: "시작 날짜",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "end_at",
                            description: "종료 날짜",
                            required: Some(false),
                            choices: vec![],
                            options: vec![],
                            autocomplete: None,
                        },
                    ],
                    autocomplete: None,
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "delete",
                    description: "이벤트 삭제",
                    required: None,
                    choices: vec![],
                    options: vec![ApplicationCommandOption {
                        kind: ApplicationCommandOptionType::String,
                        name: "id",
                        description: "이벤트 id",
                        required: Some(true),
                        choices: vec![],
                        options: vec![],
                        autocomplete: Some(true),
                    }],
                    autocomplete: None,
                },
            ],
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
            "add" => self.handle_add_command(context, interaction, option).await,
            "edit" => self.handle_edit_command(context, interaction, option).await,
            "delete" => {
                self.handle_delete_command(context, interaction, option)
                    .await
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
        } {
            error!("Failed to send message: {:?}", e);
        }

        true
    }

    async fn autocomplete(
        &self,
        _context: &Context,
        interaction: &AutocompleteInteraction,
    ) -> bool {
        if interaction.data.name != COMMAND_NAME {
            return false;
        }

        true
    }
}
