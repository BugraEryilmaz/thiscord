use super::{client::Transport, *};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

type Handler = Arc<
    dyn Fn(Context, String) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>
        + Send
        + Sync,
>;

/// An owned command context, safe to move into async work.
#[derive(Clone)]
pub struct Context {
    pub client: Client,
    pub message: ChatMessage,
}
impl Context {
    pub async fn reply(&self, content: impl Into<String>) -> Result<ChatMessage, Error> {
        self.client.send(self.message.channel_id, content).await
    }
    pub async fn join_voice(&self, channel: ChannelId) -> Result<Voice, Error> {
        self.client.join_voice(channel).await
    }
}

pub struct Bot {
    transport: Transport,
    channels: Vec<ChannelId>,
    commands: BTreeMap<String, (String, Handler)>,
    mention: Option<Handler>,
}
impl Bot {
    pub fn new(server: &str, token: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            transport: Transport::new(server, token.into())?,
            channels: vec![],
            commands: BTreeMap::new(),
            mention: None,
        })
    }
    /// Must be in the backend's explicit CORS/WebSocket origin allowlist.
    pub fn origin(mut self, origin: &str) -> Result<Self, Error> {
        self.transport.origin = endpoint(origin)?.origin().ascii_serialization();
        Ok(self)
    }
    pub fn channel(mut self, channel: ChannelId) -> Self {
        if !self.channels.contains(&channel) {
            self.channels.push(channel);
        }
        self
    }
    pub fn command<F, Fut>(
        mut self,
        name: &str,
        description: &str,
        function: F,
    ) -> Result<Self, Error>
    where
        F: Fn(Context, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Error>> + Send + 'static,
    {
        if !valid_command(name) || name == "help" || self.commands.contains_key(name) {
            return Err(Error::message(
                "Command names must be unique lowercase ASCII identifiers (help is built in)",
            ));
        }
        if self.commands.len() >= 32
            || description.chars().count() > 60
            || description.chars().any(char::is_control)
        {
            return Err(Error::message(
                "At most 32 commands with descriptions up to 60 characters",
            ));
        }
        self.commands.insert(
            name.into(),
            (
                description.into(),
                Arc::new(move |ctx, args| Box::pin(function(ctx, args))),
            ),
        );
        Ok(self)
    }
    /// Called for mentioned messages that are not leading `/commands`.
    pub fn on_mention<F, Fut>(mut self, function: F) -> Self
    where
        F: Fn(Context, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Error>> + Send + 'static,
    {
        self.mention = Some(Arc::new(move |ctx, text| Box::pin(function(ctx, text))));
        self
    }
    /// Commands execute serially, with a 30-second deadline and a bounded inbox.
    /// Heartbeats run independently. Voice playback runs in its own worker.
    /// Subscription failure is returned; restarting does not replay old commands.
    pub async fn run(self) -> Result<(), Error> {
        let mut subscription = Subscription::open(self.transport, self.channels).await?;
        let client = subscription.client();
        while let Some(message) = subscription.next().await? {
            // Defense in depth for older/misconfigured servers.
            if message.deleted
                || message.revision != 0
                || message.author_id == Some(client.bot.account_id)
                || !message.mentions.contains(&client.bot.account_id)
            {
                continue;
            }
            let command = parse_command(&message.content, &client.bot.name)
                .map(|(name, args)| (name.to_string(), args.to_string()));
            let context = Context {
                client: client.clone(),
                message,
            };
            let handler = match command {
                Some((name, _)) if name == "help" => {
                    let mut help = String::from("/help — list commands");
                    for (name, (description, _)) in &self.commands {
                        help.push_str(&format!("\n/{name} — {description}"));
                    }
                    context.reply(help).await?;
                    continue;
                }
                Some((name, args)) => match self.commands.get(&name) {
                    Some((_, handler)) => Some((handler.clone(), args)),
                    None => {
                        context
                            .reply("Unknown command. Mention me with /help to list commands.")
                            .await?;
                        None
                    }
                },
                None => self
                    .mention
                    .as_ref()
                    .map(|handler| (handler.clone(), context.message.content.clone())),
            };
            if let Some((handler, args)) = handler {
                let reply = context.clone();
                match tokio::time::timeout(Duration::from_secs(30), handler(context, args)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        reply
                            .reply("Command failed. Check the bot configuration or permissions.")
                            .await?;
                    }
                    Err(_) => {
                        reply.reply("Command timed out.").await?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn valid_command(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Parse only `@exact_bot_name /command args`. Arguments are a borrowed slice;
/// internal whitespace, quotes, Unicode and trailing spaces are preserved.
pub fn parse_command<'a>(text: &'a str, bot_name: &str) -> Option<(&'a str, &'a str)> {
    let text = text.trim_start().strip_prefix('@')?;
    let end = text.find(char::is_whitespace)?;
    if !text[..end].eq_ignore_ascii_case(bot_name) {
        return None;
    }
    let command = text[end..].trim_start().strip_prefix('/')?;
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    let name = &command[..end];
    valid_command(name).then_some((name, command[end..].trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_require_exact_leading_mention_and_preserve_arguments() {
        assert_eq!(
            parse_command(" @Music /play  café  'two words' \n", "music"),
            Some(("play", "café  'two words' \n"))
        );
        assert_eq!(parse_command("@music\n/stop", "music"), Some(("stop", "")));
        for text in [
            "@music2 /play x",
            "hi @music /play x",
            "@music/play x",
            "@music /",
            "@music /bad-name x",
            "@music /PLAY x",
            "@music! /play x",
        ] {
            assert_eq!(parse_command(text, "music"), None, "{text}");
        }
    }
    #[test]
    fn invalid_duplicate_reserved_commands_are_rejected() {
        fn bot() -> Bot {
            Bot::new("http://localhost:3000", "placeholder").unwrap()
        }
        assert!(bot().command("help", "", |_, _| async { Ok(()) }).is_err());
        assert!(
            bot()
                .command("x", "", |_, _| async { Ok(()) })
                .unwrap()
                .command("x", "", |_, _| async { Ok(()) })
                .is_err()
        );
        assert!(
            bot()
                .command("bad name", "", |_, _| async { Ok(()) })
                .is_err()
        );
    }

    #[tokio::test]
    async fn websocket_mentions_dispatch_registered_functions_and_keep_args_intact() {
        use futures_util::{SinkExt, StreamExt};
        use thiscord_shared::bots::{
            BotInfo, ClientEvent, ClientFrame, SOCKET_VERSION, ServerEvent, ServerFrame,
        };
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let id = "00000000-0000-0000-0000-000000000001";
        let guild = "00000000-0000-0000-0000-000000000002";
        let channel = "00000000-0000-0000-0000-000000000003";
        let (invoked, mut invocations) = tokio::sync::mpsc::channel(4);
        let (finish, finished) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let Message::Text(first) = socket.next().await.unwrap().unwrap() else {
                panic!("authentication")
            };
            let frame: ClientFrame = serde_json::from_str(&first).unwrap();
            assert!(
                matches!(frame.event, ClientEvent::Authenticate { channels, .. } if channels == [channel.parse().unwrap()])
            );
            let ready = ServerFrame {
                version: SOCKET_VERSION,
                event: ServerEvent::Ready {
                    bot: BotInfo {
                        account_id: id.parse().unwrap(),
                        guild_id: guild.parse().unwrap(),
                        name: "echo".into(),
                    },
                },
            };
            socket
                .send(Message::Text(serde_json::to_string(&ready).unwrap().into()))
                .await
                .unwrap();
            for (index, content, revision) in [
                (1, "@echo /echo hello  世界 ", 0),
                (2, "@echo /echo edited", 1),
                (3, "@echo2 /echo wrong bot", 0),
            ] {
                let message: ChatMessage = serde_json::from_value(serde_json::json!({
                    "id":format!("00000000-0000-0000-0001-{index:012}"), "client_id":format!("00000000-0000-0000-0002-{index:012}"),
                    "guild_id":guild, "channel_id":channel, "author_id":"00000000-0000-0000-0000-000000000004", "username":"human", "content":content,
                    "mentions":[id], "created_at":"2026-10-23T00:00:00Z", "edited_at":null, "deleted":false, "revision":revision, "sequence":index
                })).unwrap();
                let frame = ServerFrame {
                    version: SOCKET_VERSION,
                    event: ServerEvent::Mention { message },
                };
                socket
                    .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                    .await
                    .unwrap();
            }
            finished.await.unwrap();
        });
        let bot = Bot::new(&format!("http://{address}"), "bot_placeholder")
            .unwrap()
            .channel(channel.parse().unwrap())
            .command("echo", "Echo", move |ctx, args| {
                let sender = invoked.clone();
                async move {
                    sender.send((ctx.message.channel_id, args)).await.unwrap();
                    Ok(())
                }
            })
            .unwrap();
        let task = tokio::spawn(bot.run());
        let (actual_channel, args) =
            tokio::time::timeout(Duration::from_secs(5), invocations.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(actual_channel, channel.parse().unwrap());
        assert_eq!(args, "hello  世界 ");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), invocations.recv())
                .await
                .is_err()
        );
        finish.send(()).unwrap();
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
