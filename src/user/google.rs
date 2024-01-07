use std::{collections::BTreeMap, pin::Pin, sync::Arc};

use crate::jwt_util::{RsAlgorithm, RsaVerifying};
use anyhow::Context;
use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension,
};
use dashmap::DashMap;
use futures::Future;
use google_calendar3::oauth2::{self, authenticator_delegate::InstalledFlowDelegate};
use sqlx::SqlitePool;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

#[repr(transparent)]
#[derive(Debug, Clone)]
struct LoginCallbackCode(String);

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct RedirectUrl(pub String);

type LoginStateMap = DashMap<
    Uuid,
    (
        oneshot::Sender<LoginCallbackCode>,
        oneshot::Receiver<Option<String>>,
    ),
>;

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
    pub(super) key_store: Arc<BTreeMap<String, RsaVerifying>>,
}

impl GoogleUserHandler {
    pub async fn new(application_secret_path: &str, redirect_prefix: &str) -> Self {
        Self {
            secret: google_calendar3::oauth2::read_application_secret(application_secret_path)
                .await
                .unwrap(),
            redirect_prefix: redirect_prefix.to_string(),
            key_store: Arc::new(fetch_google_key_store().await.unwrap()),
        }
    }

    pub async fn auth(&self, user_id: i64, db_pool: SqlitePool) -> anyhow::Result<RedirectUrl> {
        let (url_sender, url_receiver) = oneshot::channel();
        let (code_sender, code_receiver) = oneshot::channel();
        let (user_id_sender, user_id_receiver) = oneshot::channel();

        let id = Uuid::new_v4();
        LOGIN_STATE.insert(id, (code_sender, user_id_receiver));

        let secret = self.secret.clone();
        let key_store = self.key_store.clone();
        let redirect_uri = format!("{}/user/google/login_callback", self.redirect_prefix);

        tokio::spawn(async move {
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
            .context("Failed to installed flow")
            .unwrap();

            let (_subject, email) = {
                use jwt::VerifyWithStore;

                let id_token = auth.id_token(CALENDAR_SCOPE).await.unwrap().unwrap();
                let mut claims: BTreeMap<String, serde_json::Value> = id_token
                    .verify_with_store(key_store.as_ref())
                    .context("jwt verification failed")
                    .unwrap();
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
                .context("Failed to get access token")
                .unwrap();

            log::info!("Login succeed {email}");

            sqlx::query!(
                "UPDATE `users` SET `google_email` = ? WHERE `user_id` = ?",
                email,
                user_id
            )
            .execute(&db_pool)
            .await
            .context("Failed to store google email to DB")
            .unwrap();

            // user_id_sender
            //     .send(Some(user_id))
            //     .map_err(|_| anyhow::anyhow!("Failed to send user_id to callback handler"))
            //     .unwrap();
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
    if let Some((_, (code_sender, user_id_receiver))) = LOGIN_STATE.remove(&query.state) {
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
