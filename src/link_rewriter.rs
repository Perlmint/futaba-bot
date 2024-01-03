use std::borrow::Cow;

use async_trait::async_trait;
use serenity::{client::Context, model::channel::Message};

use crate::{discord::SubApplication, regex};

pub struct DiscordHandler;

impl DiscordHandler {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SubApplication for DiscordHandler {
    async fn message(&self, context: &Context, message: &Message) {
        let Cow::Owned(replaced_text) =
            regex!("://(x|twitter)\\.com/([^/]+)/status/(\\d+)(\\?t=[a-zA-Z0-9]+(&s=\\d+)?)?")
                .replace_all(&message.content, "://vxtwitter.com/$2/status/$3")
        else {
            return;
        };

        if let Err(e) = message.reply(&context.http, replaced_text).await {
            log::error!("Failed to reply rewritten message - {e:?}");
        }
    }
}
