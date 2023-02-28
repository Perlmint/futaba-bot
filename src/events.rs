use std::{io::BufWriter, sync::Arc};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    body::HttpBody,
    extract::Path,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Extension, Router,
};
use chrono::{NaiveDate, NaiveDateTime};
use ics::components::Property;
use log::error;
use serenity::{
    model::prelude::{
        interaction::{
            application_command::{ApplicationCommandInteraction, CommandDataOption},
            autocomplete::AutocompleteInteraction,
            InteractionResponseType,
        },
        GuildId,
    },
    prelude::Context,
};
use sqlx::{query, sqlite::SqliteRow, FromRow, Row, SqlitePool};

use crate::discord::{
    application_command::*, ChannelHelper, CommandDataOptionHelper, CommandHelper, SubApplication,
};

pub(crate) struct DiscordHandler {
    db_pool: SqlitePool,
    domain: String,
}

const COMMAND_NAME: &str = "event";

fn parse_date_optional_time(
    s: &str,
) -> anyhow::Result<(chrono::NaiveDate, Option<chrono::NaiveTime>)> {
    for format in &[
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%d %H:%M",
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(s, format) {
            return Ok((datetime.date(), Some(datetime.time())));
        }
    }

    for format in &["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(s, format) {
            return Ok((date, None));
        }
    }

    anyhow::bail!("Failed to parse - {s}")
}

impl DiscordHandler {
    pub fn new(db_pool: SqlitePool, config: &crate::Config) -> Self {
        Self {
            db_pool,
            domain: config.web.domain.clone(),
        }
    }

    async fn handle_link_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id.get_parent_or_self(&context).await.0 as i64;

        if let Err(e) = interaction
            .create_interaction_response(context, |r| {
                r.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|d| {
                        d.content(format!("https://{}/events/{}", self.domain, channel_id))
                    })
            })
            .await
        {
            error!("Failed to send response - {e:?}");
        }

        Ok(())
    }

    async fn handle_add_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id.get_parent_or_self(&context).await.0 as i64;
        let now = chrono::Utc::now().naive_utc();

        let [name, description, begin_at, end_at] =
            option.get_options(&["name", "description", "begin_at", "end_at"]);
        let name = unsafe { name.as_str_unchecked() };
        let description = description.as_str();
        let (begin_date, begin_time) = {
            let begin_at = unsafe { begin_at.as_str_unchecked() };

            parse_date_optional_time(begin_at).context("Failed to parse begin_at")?
        };
        let (end_date, end_time) = end_at
            .as_str()
            .map(parse_date_optional_time)
            .transpose()
            .context("Failed to parse end_at")?
            .map(|(d, t)| (Some(d), t))
            .unwrap_or_default();

        match sqlx::query!(
            r#"INSERT INTO events
            (channel, name, created_at, modified_at, description, begin_date, begin_time, end_date, end_time)
            VALUES
            (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            channel_id,
            name,
            now,
            now,
            description,
            begin_date,
            begin_time,
            end_date,
            end_time
        )
        .execute(&self.db_pool)
        .await
        {
            Ok(r) => {
                let id = r.last_insert_rowid();
                if let Err(e) = interaction
                    .create_interaction_response(context, |r| {
                        r.kind(InteractionResponseType::ChannelMessageWithSource)
                            .interaction_response_data(|d| {
                                d.content(format!("새 이벤트 {name}(id: {id})가 생성되었습니다"))
                            })
                    })
                    .await
                {
                    error!("Failed to send response - {e:?}");
                }
            }
            Err(e) => {
                error!("Failed to create event - name({name}), description({description:?}), begin_date_time({begin_at:?}), end_date_time({end_at:?}) {e:?}")
            }
        }

        Ok(())
    }

    async fn handle_edit_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id.get_parent_or_self(&context).await.0 as i64;
        let now = chrono::Utc::now().naive_utc();

        let [id, name, description, begin_at, end_at] =
            option.get_options(&["id", "name", "description", "begin_at", "end_at"]);
        let id = unsafe { id.as_i64_unchecked() };
        let name = name.as_str();
        let description = description.as_str();
        let (begin_date, begin_time) = begin_at
            .as_str()
            .map(parse_date_optional_time)
            .transpose()
            .context("Failed to parse begin_at")?
            .map(|(d, t)| (Some(d), t))
            .unwrap_or_default();
        let (end_date, end_time) = end_at
            .as_str()
            .map(parse_date_optional_time)
            .transpose()
            .context("Failed to parse end_at")?
            .map(|(d, t)| (Some(d), t))
            .unwrap_or_default();

        let mut builder = sqlx::QueryBuilder::new("UPDATE events SET ");
        builder.push("modified_at = ").push_bind(now);
        if let Some(name) = name {
            builder.push(", name = ").push_bind(name);
        }
        if let Some(description) = description {
            builder.push(", description = ").push_bind(description);
        }
        if let Some(begin_date) = begin_date {
            builder.push(", begin_date = ").push_bind(begin_date);
        }
        if let Some(begin_time) = begin_time {
            builder.push(", begin_time = ").push_bind(begin_time);
        }
        if let Some(end_date) = end_date {
            builder.push(", end_date = ").push_bind(end_date);
        }
        if let Some(end_time) = end_time {
            builder.push(", end_time = ").push_bind(end_time);
        }
        builder
            .push("WHERE rowid = ")
            .push_bind(id)
            .push(" AND channel = ")
            .push_bind(channel_id);
        if let Err(e) = builder.build().execute(&self.db_pool).await {
            error!("Failed to update event - {e:?}");
        } else if let Err(e) = interaction
            .create_interaction_response(context, |r| {
                r.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|d| {
                        d.content("이벤트가 갱신되었습니다.".to_string())
                    })
            })
            .await
        {
            error!("Failed to send response - {e:?}");
        }

        Ok(())
    }

    async fn handle_delete_command(
        &self,
        context: &Context,
        interaction: &ApplicationCommandInteraction,
        option: &CommandDataOption,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id.get_parent_or_self(&context).await.0 as i64;
        let [id] = option.get_options(&["id"]);
        let id = unsafe { id.as_i64_unchecked() };
        if let Err(e) = query!(
            "DELETE FROM events WHERE rowid = ? AND channel = ?",
            id,
            channel_id
        )
        .execute(&self.db_pool)
        .await
        {
            error!("Failed to update event - {e:?}");
        } else if let Err(e) = interaction
            .create_interaction_response(context, |r| {
                r.kind(InteractionResponseType::ChannelMessageWithSource)
                    .interaction_response_data(|d| {
                        d.content("이벤트가 갱신되었습니다.".to_string())
                    })
            })
            .await
        {
            error!("Failed to send response - {e:?}");
        }

        Ok(())
    }

    async fn handle_autocomplete(
        &self,
        context: &Context,
        interaction: &AutocompleteInteraction,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id.get_parent_or_self(&context).await.0 as i64;
        let [id] = interaction.data.options.get_options(&["id"]);
        let name = id.as_str().unwrap_or("");
        let name_pattern = format!("%{name}%");
        match sqlx::query!(
            "SELECT rowid, name FROM events WHERE channel = ? AND name LIKE ?",
            channel_id,
            name_pattern
        )
        .fetch_all(&self.db_pool)
        .await
        {
            Ok(d) => {
                interaction
                    .create_autocomplete_response(context, move |r| {
                        for record in d {
                            r.add_int_choice(record.name, record.rowid);
                        }
                        r
                    })
                    .await?;
            }
            Err(e) => {
                error!("Failed to get autocomplete data from DB - {e:?}");
            }
        }

        Ok(())
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn ready(&self, context: &Context, guild_id: GuildId) {
        // register or update slash command
        let command = ApplicationCommand {
            name: COMMAND_NAME,
            description: "이벤트 관리",
            options: vec![
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "link",
                    description: "캘린더 링크",
                    ..Default::default()
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "add",
                    description: "이벤트 추가",
                    options: vec![
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "name",
                            description: "이벤트 이름",
                            required: Some(true),
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "begin_at",
                            description: "시작 날짜",
                            required: Some(true),
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "end_at",
                            description: "종료 날짜",
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "description",
                            description: "상세",
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "edit",
                    description: "이벤트 수정",
                    options: vec![
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::Integer,
                            name: "id",
                            description: "이벤트 id",
                            required: Some(true),
                            autocomplete: Some(true),
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "name",
                            description: "이벤트 이름",
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "description",
                            description: "상세",
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "begin_at",
                            description: "시작 날짜",
                            ..Default::default()
                        },
                        ApplicationCommandOption {
                            kind: ApplicationCommandOptionType::String,
                            name: "end_at",
                            description: "종료 날짜",
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                ApplicationCommandOption {
                    kind: ApplicationCommandOptionType::SubCommand,
                    name: "delete",
                    description: "이벤트 삭제",
                    options: vec![ApplicationCommandOption {
                        kind: ApplicationCommandOptionType::String,
                        name: "id",
                        description: "이벤트 id",
                        required: Some(true),
                        ..Default::default()
                    }],
                    ..Default::default()
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
            "link" => self.handle_link_command(context, interaction).await,
            "add" => self.handle_add_command(context, interaction, option).await,
            "edit" => self.handle_edit_command(context, interaction, option).await,
            "delete" => {
                self.handle_delete_command(context, interaction, option)
                    .await
            }
            _ => unsafe { std::hint::unreachable_unchecked() },
        } {
            error!("Failed to handle message: {:?}", e);
        }

        true
    }

    async fn autocomplete(&self, context: &Context, interaction: &AutocompleteInteraction) -> bool {
        if interaction.data.name != COMMAND_NAME {
            return false;
        }

        if let Err(e) = self.handle_autocomplete(context, interaction).await {
            error!("Failed to handle autocomplete request: {e:?}");
        }

        true
    }
}

#[derive(Debug)]
struct Event {
    rowid: i64,
    name: String,
    created_at: chrono::NaiveDateTime,
    modified_at: chrono::NaiveDateTime,
    description: Option<String>,
    begin_date: chrono::NaiveDate,
    begin_time: Option<chrono::NaiveTime>,
    end_date: Option<chrono::NaiveDate>,
    end_time: Option<chrono::NaiveTime>,
}

impl<'r> FromRow<'r, SqliteRow> for Event {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let rowid = row.get_unchecked("rowid");
        let name = row.get_unchecked("name");
        let created_at = chrono::NaiveDateTime::from_timestamp(row.get_unchecked("created_at"), 0);
        let modified_at =
            chrono::NaiveDateTime::from_timestamp(row.get_unchecked("modified_at"), 0);
        let description = row.get_unchecked("description");
        let begin_date = chrono::NaiveDate::parse_from_str(
            row.get_unchecked::<&str, _>("begin_date"),
            "%Y-%m-%d",
        )
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let begin_time = row
            .get_unchecked::<Option<&str>, _>("begin_time")
            .map(|s| chrono::NaiveTime::parse_from_str(s, "%H:%M:%S"))
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let end_date = row
            .get_unchecked::<Option<&str>, _>("end_date")
            .map(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let end_time = row
            .get_unchecked::<Option<&str>, _>("end_time")
            .map(|s| chrono::NaiveTime::parse_from_str(s, "%H:%M:%S"))
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        Ok(Self {
            rowid,
            name,
            created_at,
            modified_at,
            description,
            begin_date,
            begin_time,
            end_date,
            end_time,
        })
    }
}

impl Event {
    fn into_ics_event(self, namespace: &str) -> ics::Event<'static> {
        let created_at = self.created_at.format("%Y%m%dT%H%M%S").to_string();
        let modified_at = self.modified_at.format("%Y%m%dT%H%M%S").to_string();

        let mut event = ics::Event::new(format!("{}@{namespace}", self.rowid), created_at);
        event.push(Property::new("SUMMARY", self.name));
        if let Some(description) = self.description {
            event.push(Property::new("DESCRIPTION", description));
        }
        event.push(Property::new("LAST-MODIFIED", modified_at));

        fn write_date_time<'a>(
            event: &mut ics::Event<'a>,
            key: &'a str,
            date: &chrono::NaiveDate,
            time: &Option<chrono::NaiveTime>,
        ) {
            match time {
                Some(t) => event.push(Property::new(
                    format!("{key};TZID=Asia/Seoul"),
                    date.and_time(*t).format("%Y%m%dT%H%M%S").to_string(),
                )),
                None => event.push(Property::new(
                    format!("{key};VALUE=DATE"),
                    date.format("%Y%m%d").to_string(),
                )),
            }
        }
        write_date_time(&mut event, "DTSTART", &self.begin_date, &self.begin_time);
        if let Some(end_date) = self.end_date {
            write_date_time(&mut event, "DTEND", &end_date, &self.end_time);
        } else {
            write_date_time(&mut event, "DTENd", &self.begin_date, &self.begin_time);
        }

        event
    }
}

async fn events_to_ics(events: Vec<Event>, domain: &str) -> anyhow::Result<String> {
    let mut calendar = ics::ICalendar::new("2.0", "futaba");
    calendar.push(Property::new("X-WR-TIMEZONE", "Asia/Seoul"));
    calendar.push(Property::new("CALSCALE", "GREGORIAN"));
    calendar.add_timezone(ics::TimeZone::standard(
        "Asia/Seoul",
        ics::Standard::new("19700101T000000", "+1000", "+0900"),
    ));
    for event in events {
        calendar.add_event(event.into_ics_event(domain));
    }

    let mut writer = BufWriter::new(Vec::new());
    calendar.write(&mut writer).unwrap();
    let buffer = writer.into_inner()?;

    Ok(String::from_utf8(buffer)?)
}

async fn serve_events(
    Path(channel_id): Path<u64>,
    Extension(db_pool): Extension<SqlitePool>,
    Extension(config): Extension<Arc<crate::Config>>,
) -> impl IntoResponse {
    let events = {
        let channel_id = channel_id as i64;
        sqlx::query_as(
            r#"SELECT
                rowid, name, created_at, modified_at, description,
                begin_date, begin_time, end_date, end_time FROM events WHERE channel = ?"#,
        )
        .bind(channel_id)
        .fetch_all(&db_pool)
        .await
        .context("Failed to fetch events")
    };
    let ics = match events {
        Ok(events) => events_to_ics(events, &config.web.domain).await,
        Err(e) => Err(e),
    };
    match ics {
        Ok(ics) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/calendar")],
            ics,
        ),
        Err(e) => {
            error!("Failed to render calendar for Channel({channel_id}) - {e:?}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain")],
                "".to_string(),
            )
        }
    }
}

pub fn router<S: Sync + Send + Clone + 'static, B: HttpBody + Send + 'static>() -> Router<S, B> {
    Router::new().route("/:channel_id", get(serve_events))
}
