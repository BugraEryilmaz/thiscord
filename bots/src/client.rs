use super::*;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use thiscord_shared::{
    bots::{BOTS_PATH, BotRequest, BotResponse},
    chat::{CHAT_PATH, ChatRequest, ChatResponse},
};

#[derive(Clone)]
pub(crate) struct Transport {
    pub base: String,
    pub origin: String,
    pub token: Arc<str>,
    http: reqwest::Client,
}
impl Transport {
    pub fn new(base: &str, token: String) -> Result<Self, Error> {
        let base = endpoint(base)?;
        if token.is_empty() || token.len() > 128 || token.chars().any(char::is_control) {
            return Err(Error::message("Invalid credential"));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::message("Cannot initialize HTTP client"))?;
        Ok(Self {
            origin: base.origin().ascii_serialization(),
            base: base.to_string(),
            token: token.into(),
            http,
        })
    }
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        let url = endpoint(&self.base)?
            .join(path)
            .map_err(|_| Error::message("Invalid API path"))?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(self.token.as_ref())
            .json(body)
            .send()
            .await
            .map_err(|_| Error::message("HTTP request failed"))?;
        let status = response.status();
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::message("Cannot read HTTP response"))?
        {
            if data.len() + chunk.len() > 128 * 1024 {
                return Err(Error::message("Server response exceeded limit"));
            }
            data.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(serde_json::from_slice(&data)
                .map(Error::api)
                .unwrap_or_else(|_| Error::message("Server rejected request")));
        }
        serde_json::from_slice(&data).map_err(|_| Error::message("Invalid server response"))
    }
}

/// Bot actions, permanently scoped to the guild returned by the server.
#[derive(Clone)]
pub struct Client {
    pub(crate) transport: Transport,
    pub(crate) bot: BotInfo,
}
impl Client {
    pub fn info(&self) -> &BotInfo {
        &self.bot
    }
    pub async fn send(
        &self,
        channel: ChannelId,
        content: impl Into<String>,
    ) -> Result<ChatMessage, Error> {
        self.send_with_id(
            channel,
            ClientMessageId::from_uuid(uuid::Uuid::new_v4()),
            content,
        )
        .await
    }
    /// Reuse this ID when retrying an ambiguous send; the backend deduplicates it.
    pub async fn send_with_id(
        &self,
        channel: ChannelId,
        client_id: ClientMessageId,
        content: impl Into<String>,
    ) -> Result<ChatMessage, Error> {
        match self
            .transport
            .post(
                CHAT_PATH,
                &ChatRequest::Send {
                    guild_id: self.bot.guild_id,
                    channel_id: channel,
                    client_id,
                    content: content.into(),
                    attachment_ids: vec![],
                    reply_to: None,
                },
            )
            .await?
        {
            ChatResponse::Message { message } => Ok(*message),
            _ => Err(Error::message("Unexpected send response")),
        }
    }
    pub async fn join_voice(&self, channel: ChannelId) -> Result<Voice, Error> {
        Voice::join(
            &self.transport.base,
            &self.transport.origin,
            &self.transport.token,
            self.bot.guild_id,
            channel,
        )
        .await
    }
}

/// Provision and revoke bots with a human session. Tokens are returned once and
/// deliberately have no Debug implementation. Never include them in logs.
pub struct Manager(Transport);
impl Manager {
    pub fn new(server: &str, user_token: impl Into<String>) -> Result<Self, Error> {
        Ok(Self(Transport::new(server, user_token.into())?))
    }
    pub async fn request(&self, request: BotRequest) -> Result<BotResponse, Error> {
        self.0.post(BOTS_PATH, &request).await
    }
    pub async fn list(&self, guild: GuildId) -> Result<BotResponse, Error> {
        self.request(BotRequest::List { guild_id: guild }).await
    }
    pub async fn create(
        &self,
        guild: GuildId,
        revision: i64,
        name: impl Into<String>,
    ) -> Result<BotResponse, Error> {
        self.request(BotRequest::Create {
            guild_id: guild,
            revision,
            name: name.into(),
        })
        .await
    }
    pub async fn rotate(
        &self,
        guild: GuildId,
        revision: i64,
        account: AccountId,
    ) -> Result<BotResponse, Error> {
        self.request(BotRequest::Rotate {
            guild_id: guild,
            revision,
            account_id: account,
        })
        .await
    }
    pub async fn delete(
        &self,
        guild: GuildId,
        revision: i64,
        account: AccountId,
    ) -> Result<BotResponse, Error> {
        self.request(BotRequest::Delete {
            guild_id: guild,
            revision,
            account_id: account,
        })
        .await
    }
}
