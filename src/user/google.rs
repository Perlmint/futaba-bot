use std::{collections::BTreeMap, pin::Pin, sync::Arc};

use crate::jwt_util::{RsAlgorithm, RsaVerifying};
use anyhow::Context;
use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use futures::Future;
use google_calendar3::{
    api::{AclRule, AclRuleScope, Calendar},
    hyper, hyper_rustls,
    oauth2::{self, authenticator_delegate::InstalledFlowDelegate},
    CalendarHub,
};
use log::{error, info};
use once_cell::sync::OnceCell;
use serenity::{
    http::Http,
    model::{
        application::interaction::{
            application_command::ApplicationCommandInteraction, InteractionResponseType,
        },
        id::UserId,
    },
};
use sqlx::SqlitePool;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

#[repr(transparent)]
#[derive(Debug, Clone)]
struct LoginCallbackCode(String);

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct RedirectUrl(pub String);

type LoginStateMap = DashMap<Uuid, oneshot::Sender<LoginCallbackCode>>;

const CALENDAR_SCOPE: &[&str] = &[
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/calendar.readonly",
    "https://www.googleapis.com/auth/calendar.events",
    "openid",
    "email",
];

struct LoginDelegate {
    channels: Mutex<
        Option<(
            oneshot::Sender<RedirectUrl>,
            oneshot::Receiver<LoginCallbackCode>,
        )>,
    >,
    redirect_uri: String,
    context_id: Uuid,
}

impl InstalledFlowDelegate for LoginDelegate {
    fn redirect_uri(&self) -> Option<&str> {
        Some(&self.redirect_uri)
    }

    fn present_user_url<'a>(
        &'a self,
        url: &'a str,
        _need_code: bool,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let (redirect_url_sender, code_receiver) = self
                .channels
                .lock()
                .await
                .take()
                .ok_or_else(|| "already used".to_string())?;

            if let Err(e) =
                redirect_url_sender.send(RedirectUrl(format!("{url}&state={}", self.context_id)))
            {
                Err(format!("Failed to send redirect URL - {:?}", e))
            } else {
                let code = code_receiver
                    .await
                    .map_err(|e| format!("Failed to receive auth code - {:?}", e))?;

                Ok(code.0)
            }
        })
    }
}

async fn fetch_google_key_store() -> anyhow::Result<BTreeMap<String, RsaVerifying>> {
    #[derive(serde::Deserialize)]
    struct Key {
        n: String,
        e: String,
        kid: String,
        alg: String,
    }
    #[derive(serde::Deserialize)]
    struct R {
        keys: Vec<Key>,
    }
    let resp: R = reqwest::get("https://www.googleapis.com/oauth2/v3/certs")
        .await?
        .json()
        .await?;

    let mut ret = BTreeMap::new();

    for key in resp.keys {
        ret.insert(
            key.kid.to_string(),
            RsaVerifying(
                rsa::RsaPublicKey::new(
                    rsa::BigUint::from_bytes_be(&base64_url::decode(&key.n).unwrap()),
                    rsa::BigUint::from_bytes_be(&base64_url::decode(&key.e).unwrap()),
                )
                .unwrap(),
                match key.alg.as_str() {
                    "RS256" => RsAlgorithm::Rs256,
                    "RS384" => RsAlgorithm::Rs384,
                    "RS512" => RsAlgorithm::Rs512,
                    alg => unreachable!("Invalid algorithm type - {}", alg),
                },
            ),
        );
    }

    Ok(ret)
}

static LOGIN_STATE: once_cell::sync::Lazy<LoginStateMap> =
    once_cell::sync::Lazy::new(|| LoginStateMap::new());

pub struct GoogleUserHandler {
    secret: oauth2::ApplicationSecret,
    redirect_prefix: String,
    service_account: google_calendar3::oauth2::ServiceAccountKey,
    pub(super) calendar_name: OnceCell<String>,
    pub(super) key_store: Arc<BTreeMap<String, RsaVerifying>>,
}

impl GoogleUserHandler {
    pub async fn new(
        application_secret_path: &str,
        service_account_key_path: &str,
        redirect_prefix: &str,
    ) -> anyhow::Result<Self> {
        let service_account =
            google_calendar3::oauth2::read_service_account_key(service_account_key_path)
                .await
                .context("Failed to read service account info")?;
        let secret = google_calendar3::oauth2::read_application_secret(application_secret_path)
            .await
            .context("Failed to read application secret")?;

        Ok(Self {
            secret,
            service_account,
            redirect_prefix: redirect_prefix.to_string(),
            calendar_name: OnceCell::new(),
            key_store: Arc::new(
                fetch_google_key_store()
                    .await
                    .context("Failed to fetch google key store")?,
            ),
        })
    }

    pub async fn auth(
        &self,
        user_id: UserId,
        db_pool: SqlitePool,
        context: impl AsRef<Http> + Send + 'static,
        response_message: ApplicationCommandInteraction,
    ) -> anyhow::Result<RedirectUrl> {
        let (url_sender, url_receiver) = oneshot::channel();
        let (code_sender, code_receiver) = oneshot::channel();

        let id = Uuid::new_v4();
        LOGIN_STATE.insert(id, code_sender);

        let secret = self.secret.clone();
        let key_store = self.key_store.clone();
        let redirect_uri = format!("{}/user/google/login_callback", self.redirect_prefix);
        let service_account = self.service_account.client_email.clone();
        let calendar_name = unsafe { self.calendar_name.get_unchecked() }.clone();

        tokio::spawn(async move {
            let result: anyhow::Result<()> = async move {
                let auth = oauth2::InstalledFlowAuthenticator::builder(
                    secret,
                    oauth2::InstalledFlowReturnMethod::Interactive,
                )
                .flow_delegate(Box::new(LoginDelegate {
                    channels: Mutex::new(Some((url_sender, code_receiver))),
                    redirect_uri,
                    context_id: id,
                }))
                .build()
                .await
                .context("Failed to installed flow")?;

                let (_subject, email) = {
                    use jwt::VerifyWithStore;

                    let id_token = auth.id_token(CALENDAR_SCOPE).await.unwrap().unwrap();
                    let mut claims: BTreeMap<String, serde_json::Value> = id_token
                        .verify_with_store(key_store.as_ref())
                        .context("jwt verification failed")?;
                    (
                        claims
                            .remove("sub")
                            .context("sub is not in received claims")
                            .unwrap(),
                        claims
                            .remove("email")
                            .context("email is not in received claims")
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string(),
                    )
                };

                auth.token(CALENDAR_SCOPE)
                    .await
                    .context("Failed to get access token")?;

                log::info!("Login succeed {email}");

                let raw_user_id = *user_id.as_u64() as i64;
                sqlx::query!(
                    "UPDATE `users` SET `google_email` = ? WHERE `user_id` = ?",
                    email,
                    raw_user_id
                )
                .execute(&db_pool)
                .await
                .context("Failed to store google email to DB")?;

                let calendar_hub = CalendarHub::new(
                    hyper::Client::builder().build(
                        hyper_rustls::HttpsConnectorBuilder::new()
                            .with_native_roots()
                            .https_or_http()
                            .enable_http1()
                            .build(),
                    ),
                    auth,
                );

                let record = sqlx::query!(
                    "SELECT `google_calendar_id`, `google_calendar_acl_id`
                    FROM `users`
                    WHERE `user_id` = ?",
                    raw_user_id
                )
                .fetch_one(&db_pool)
                .await
                .context("Failed to fetch google calendar id from DB")?;
                let calendar_id = record.google_calendar_id;
                let acl_id = record.google_calendar_acl_id;

                let (calendar_id, acl_id) = if let Some(calendar_id) = calendar_id {
                    if let Err(e) = calendar_hub.calendars().get(&calendar_id).doit().await {
                        info!("Saved calendar_id({calendar_id}) is invalid - {e:?}");
                        (None, None)
                    } else if let Some(acl_id) = acl_id {
                        let acl_id = if let Err(e) =
                            calendar_hub.acl().get(&calendar_id, &acl_id).doit().await
                        {
                            info!("Saved acl_id is invalid - {e:?}");
                            None
                        } else {
                            Some(acl_id)
                        };
                        (Some(calendar_id), acl_id)
                    } else {
                        (Some(calendar_id), None)
                    }
                } else {
                    (None, None)
                };

                let calendar_id = if let Some(calendar_id) = calendar_id {
                    calendar_id
                } else {
                    info!("Create new calendar");
                    calendar_hub
                        .calendars()
                        .insert(Calendar {
                            summary: Some(calendar_name),
                            ..Default::default()
                        })
                        .doit()
                        .await
                        .context("Failed to create calendar")?
                        .1
                        .id
                        .ok_or_else(|| anyhow::anyhow!("Mandatory field is missing"))?
                };

                let acl_id = if let Some(acl_id) = acl_id {
                    acl_id
                } else {
                    info!("Share calendar {calendar_id} to service account");
                    calendar_hub
                        .acl()
                        .insert(
                            AclRule {
                                etag: None,
                                id: None,
                                kind: None,
                                role: Some("writer".to_string()),
                                scope: Some(AclRuleScope {
                                    type_: Some("user".to_string()),
                                    value: Some(service_account),
                                }),
                            },
                            &calendar_id,
                        )
                        .doit()
                        .await
                        .context("Failed to set ACL of calendar")?
                        .1
                        .id
                        .expect("Id of AclRule in Response should be set")
                };

                sqlx::query!(
                    "UPDATE `users`
                    SET `google_calendar_id` = ?, `google_calendar_acl_id` = ?
                    WHERE `user_id` = ?",
                    calendar_id,
                    acl_id,
                    raw_user_id
                )
                .execute(&db_pool)
                .await
                .context("Failed to save calendar data into DB")?;

                Ok(())
            }
            .await;

            if let Err(e) = result {
                error!("Error occurred while login - {e:?}");
                if let Err(e) = response_message
                    .create_interaction_response(context, |b| {
                        b.kind(InteractionResponseType::DeferredUpdateMessage)
                            .interaction_response_data(|b| b.content("실패").ephemeral(true))
                    })
                    .await
                {
                    error!("Failed to update response - {e:?}");
                }
            } else {
                if let Err(e) = response_message
                    .create_interaction_response(context, |b| {
                        b.kind(InteractionResponseType::DeferredUpdateMessage)
                            .interaction_response_data(|b| b.content("완료").ephemeral(true))
                    })
                    .await
                {
                    error!("Failed to update response - {e:?}");
                }
            }
        });

        url_receiver.await.context("Url")
    }
}

#[derive(serde::Deserialize)]
struct LoginCallbackQuery {
    state: Uuid,
    code: String,
    #[allow(dead_code)]
    scope: String,
}

async fn login_callback(Query(query): Query<LoginCallbackQuery>) -> Response {
    if let Some((_, code_sender)) = LOGIN_STATE.remove(&query.state) {
        code_sender
            .send(LoginCallbackCode(query.code))
            .map_err(|e| format!("Failed to send auth code - {e:?}"))
            .unwrap();
        log::debug!("Successfully logged in");
        "Done. Close this page".into_response()
    } else {
        log::debug!("Invalid request");
        StatusCode::BAD_REQUEST.into_response()
    }
}

pub fn web_router<S: Sync + Send + Clone + 'static>() -> axum::Router<S> {
    axum::Router::new().route("/login_callback", axum::routing::get(login_callback))
}
