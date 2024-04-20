use axum::async_trait;
use futures::stream::StreamExt;
use google_generative_ai_rs::v1::{
    api::Client as GoogleAiClient,
    gemini::{
        request::Request, response::GeminiResponse, Content, Model, Part, ResponseType, Role,
    },
};
use log::error;
use serde::Deserialize;
use serenity::{
    client::Context,
    model::{
        application::interaction::{
            application_command::ApplicationCommandInteraction, InteractionResponseType,
        },
        channel::Message,
        id::GuildId,
    },
};
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::discord::{
    application_command::{
        ApplicationCommand, ApplicationCommandOption, ApplicationCommandOptionType,
    },
    SubApplication,
};

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct Config {
    api_key: String,
    setting_role_ids: Vec<u64>,
}

pub struct DiscordHandler {
    db_pool: SqlitePool,
    cached_prompt: RwLock<Option<String>>,
    config: Config,
}

const COMMAND_NAME: &str = "llm";

impl DiscordHandler {
    pub async fn new(db_pool: SqlitePool, config: &super::Config) -> anyhow::Result<Self> {
        let cached_prompt = sqlx::query!("SELECT `prompt` FROM `llm_config`")
            .fetch_optional(&db_pool)
            .await?
            .map(|r| {
                let mut prompt = r.prompt;
                prompt.push('\n');
                prompt
            });

        Ok(Self {
            db_pool,
            cached_prompt: RwLock::new(cached_prompt),
            config: config.llm.clone(),
        })
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        // register or update slash command
        let command = ApplicationCommand {
            name: COMMAND_NAME,
            description: "LLM 설정",
            options: vec![ApplicationCommandOption {
                kind: ApplicationCommandOptionType::SubCommand,
                name: "prompt",
                description: "프롬프트 설정",
                options: vec![ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::String,
                    name: "new_prompt",
                    description: "입력 시 새로 설정하며, 없을 경우 현재 값을 보여줍니다.",
                    required: Some(false),
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
        let mut authorized = false;
        for role in &self.config.setting_role_ids {
            match interaction
                .user
                .has_role(context, interaction.guild_id.unwrap(), *role)
                .await
            {
                Ok(true) => {
                    authorized = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    error!("Failed to check role - {e:?}");
                    return true;
                }
            }
        }

        if !authorized {
            if let Err(e) = interaction
                .create_interaction_response(context, |builder| {
                    builder
                        .kind(InteractionResponseType::Modal)
                        .interaction_response_data(|builder| {
                            builder.content("권한이 없는 명령입니다.")
                        })
                })
                .await
            {
                error!("Failed to send error response - {e:?}");
            }
            return true;
        }

        match option.name.as_str() {
            "prompt" => {
                if let Some(new_prompt) = option.options.first().and_then(|v| v.value.as_ref()) {
                    let new_prompt = new_prompt.as_str().unwrap();
                    if let Err(e) = sqlx::query!(
                        "INSERT INTO `llm_config` (`prompt`, `id`) VALUES (?, 0)
                        ON CONFLICT (`id`) DO UPDATE
                        SET `prompt` = `excluded`.`prompt`
                        WHERE `id` = `excluded`.`id`",
                        new_prompt
                    )
                    .execute(&self.db_pool)
                    .await
                    {
                        error!("Failed to write new prompt to DB - {e:?}");
                        return true;
                    }

                    let _ = self
                        .cached_prompt
                        .write()
                        .await
                        .insert(format!("{new_prompt}\n"));

                    if let Err(e) = interaction
                        .create_interaction_response(context, |builder| {
                            builder
                                .kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|builder| {
                                    builder.content("설정 되었습니다.").ephemeral(true)
                                })
                        })
                        .await
                    {
                        error!("Failed to send interaction response - {e:?}");
                    }
                } else {
                    let cached_prompt = self.cached_prompt.read().await;

                    if let Err(e) = interaction
                        .create_interaction_response(context, |builder| {
                            builder
                                .kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|builder| {
                                    builder
                                        .content(if let Some(prompt) = cached_prompt.as_ref() {
                                            format!("PROMPT: {}", prompt)
                                        } else {
                                            "NO PROMPT".to_string()
                                        })
                                        .ephemeral(true)
                                })
                        })
                        .await
                    {
                        error!("Failed to send interaction response - {e:?}");
                    }
                }
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
        }

        true
    }

    async fn message(&self, context: &Context, message: &Message) {
        const WORKING_INDICATOR: &str = "`<...>`";
        const END_INDICATOR: &str = "`<DONE>`";

        let mentioned = match message.mentions_me(context).await {
            Ok(mentioned) => mentioned,
            Err(e) => {
                error!("Failed while calling API - {e:?}");
                return;
            }
        };

        let client = GoogleAiClient::new_from_model_response_type(
            Model::GeminiPro,
            self.config.api_key.clone(),
            ResponseType::StreamGenerateContent,
        );
        if !mentioned {
            return;
        }

        let mut contents = vec![Content {
            role: Role::User,
            parts: vec![Part {
                text: Some(message.content.to_string()),
                inline_data: None,
                file_data: None,
                video_metadata: None,
            }],
        }];

        let mut message_reference = message.message_reference.clone();
        while let Some(ref_msg) = message_reference {
            let message = context
                .http
                .get_message(
                    *ref_msg.channel_id.as_u64(),
                    *ref_msg.message_id.unwrap().as_u64(),
                )
                .await
                .unwrap();
            contents.push(Content {
                role: if message.author.bot {
                    Role::Model
                } else {
                    Role::User
                },
                parts: vec![Part {
                    text: Some(message.content.trim_end_matches(END_INDICATOR).to_string()),
                    inline_data: None,
                    file_data: None,
                    video_metadata: None,
                }],
            });
            message_reference = message.message_reference;
        }

        contents.reverse();

        {
            let cached_prompt = self.cached_prompt.read().await;
            if let Some(cached_prompt) = cached_prompt.as_ref() {
                let content = unsafe { contents.get_mut(0).unwrap_unchecked() };
                let part = unsafe { content.parts.get_mut(0).unwrap_unchecked() };
                let text = unsafe { part.text.as_mut().unwrap_unchecked() };
                text.insert_str(0, cached_prompt);
            }
        }

        let request = Request {
            contents,
            tools: vec![],
            safety_settings: vec![],
            generation_config: None,
        };

        let mut joined_response = String::from(WORKING_INDICATOR);
        let mut reply = match message.reply(context, &joined_response).await {
            Ok(message) => message,
            Err(e) => {
                error!("Failed to create reply - {e:?}");
                return;
            }
        };

        let response = client.post(30, &request);
        let response = match response.await {
            Ok(response) => response,
            Err(e) => {
                error!("Received error from Google AI - {e:?}");
                if let Err(e) = reply
                    .edit(context, |builder| {
                        builder.content("`ERROR: Received error from Google AI`")
                    })
                    .await
                {
                    error!("Failed to report error by reply - {e:?}");
                }
                return;
            }
        };

        let context = context.clone();
        tokio::task::spawn(async move {
            if let Some(stream_response) = response.streamed() {
                if let Some(mut json_stream) = stream_response.response_stream {
                    while let Some(response) = json_stream.next().await {
                        let response = match response {
                            Ok(response) => response,
                            Err(e) => {
                                error!("Received error from Google AI - {e:?}");
                                return;
                            }
                        };

                        let response: GeminiResponse = match serde_json::from_value(response) {
                            Ok(response) => response,
                            Err(e) => {
                                error!("Failed to parse received response from Google AI - {e:?}");
                                return;
                            }
                        };

                        joined_response.truncate(joined_response.len() - WORKING_INDICATOR.len());
                        joined_response.extend(
                            response.candidates.into_iter().next().into_iter().flat_map(
                                |candidate| {
                                    candidate
                                        .content
                                        .parts
                                        .into_iter()
                                        .filter_map(|part| part.text)
                                },
                            ),
                        );
                        joined_response.push_str(WORKING_INDICATOR);

                        if let Err(e) = reply
                            .edit(&context, |builder| builder.content(&joined_response))
                            .await
                        {
                            error!("Failed to report error by reply - {e:?}");
                        }
                    }
                }
            }

            joined_response.truncate(joined_response.len() - WORKING_INDICATOR.len());
            joined_response.push_str(END_INDICATOR);
            if let Err(e) = reply
                .edit(context, |builder| builder.content(joined_response))
                .await
            {
                error!("Failed to report error by reply - {e:?}");
            }
        });
    }
}
