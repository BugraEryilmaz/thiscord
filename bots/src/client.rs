use super::*;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use thiscord_shared::{
    bots::{BOTS_PATH, BotRequest, BotResponse, CHANNELS_PATH, ChannelsRequest, ChannelsResponse},
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
    pub async fn channels(&self) -> Result<Vec<Channel>, Error> {
        let response: ChannelsResponse = self.post(CHANNELS_PATH, &ChannelsRequest {}).await?;
        Ok(response.channels)
    }

    pub async fn find_channel(
        &self,
        name_or_id: &str,
        kind: ChannelKind,
    ) -> Result<ChannelId, Error> {
        resolve_channel(&self.channels().await?, name_or_id, kind)
    }
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
    /// Fetch the bot's currently visible guild channels; hidden channels are excluded.
    pub async fn channels(&self) -> Result<Vec<Channel>, Error> {
        self.transport.channels().await
    }
    /// Resolve an exact name (case-insensitive) or UUID within one channel kind.
    /// Duplicate names are errors; pass a UUID to disambiguate.
    pub async fn find_channel(
        &self,
        name_or_id: &str,
        kind: ChannelKind,
    ) -> Result<ChannelId, Error> {
        self.transport.find_channel(name_or_id, kind).await
    }
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

fn resolve_channel(
    channels: &[Channel],
    name_or_id: &str,
    kind: ChannelKind,
) -> Result<ChannelId, Error> {
    let name = name_or_id.trim();
    if name.is_empty() {
        return Err(Error::message("Enter a channel name or UUID"));
    }
    let id = name.parse::<ChannelId>().ok();
    let name = name.to_lowercase();
    let mut matches = channels.iter().filter(|channel| {
        channel.kind == kind
            && match id {
                Some(id) => channel.id == id,
                None => channel.name.to_lowercase() == name,
            }
    });
    let channel = matches.next().ok_or_else(|| Error::message(
        "No visible channel matches that name or UUID and type; check the name and ViewChannel permission"
    ))?;
    if matches.next().is_some() {
        return Err(Error::message(
            "Multiple visible channels have that name and type; rename one or use its UUID",
        ));
    }
    Ok(channel.id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_resolve_by_kind_without_guessing() {
        let channel = |id, name: &str, kind| Channel {
            id: ChannelId::from_uuid(uuid::Uuid::from_u128(id)),
            name: name.into(),
            kind,
            category_id: None,
            position: 0,
        };
        let mut channels = vec![
            channel(1, "Music", ChannelKind::Text),
            channel(2, "Music", ChannelKind::Voice),
        ];
        assert_eq!(
            resolve_channel(&channels, " music ", ChannelKind::Text).unwrap(),
            channels[0].id
        );
        assert_eq!(
            resolve_channel(&channels, "MUSIC", ChannelKind::Voice).unwrap(),
            channels[1].id
        );
        assert!(
            resolve_channel(&channels, &channels[0].id.to_string(), ChannelKind::Voice).is_err()
        );
        assert!(resolve_channel(&channels, "missing", ChannelKind::Text).is_err());
        assert!(resolve_channel(&channels, " ", ChannelKind::Text).is_err());
        channels.push(channel(3, "music", ChannelKind::Text));
        assert!(resolve_channel(&channels, "music", ChannelKind::Text).is_err());
        assert_eq!(
            resolve_channel(&channels, &channels[0].id.to_string(), ChannelKind::Text).unwrap(),
            channels[0].id
        );
        channels.push(channel(4, "Müzik Odası", ChannelKind::Voice));
        assert_eq!(
            resolve_channel(&channels, "müzik odası", ChannelKind::Voice).unwrap(),
            channels[3].id
        );
    }
}
