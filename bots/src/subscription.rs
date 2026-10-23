use super::{client::Transport, *};
use futures_util::StreamExt;
use thiscord_shared::bots::{
    ClientEvent, ClientFrame, MAX_CHANNELS, SOCKET_PATH, SOCKET_VERSION, ServerEvent, ServerFrame,
};
use tokio::sync::mpsc;

/// Live mentions only. Reconnect creates a fresh subscription; messages sent
/// while offline are not replayed. Drop closes the socket and cancels its worker.
pub struct Subscription {
    client: Client,
    events: mpsc::Receiver<ChatMessage>,
    task: tokio::task::JoinHandle<Result<(), Error>>,
    ended: bool,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Subscription {
    pub async fn connect(
        server: &str,
        token: impl Into<String>,
        channels: Vec<ChannelId>,
    ) -> Result<Self, Error> {
        Self::open(Transport::new(server, token.into())?, channels).await
    }
    pub async fn connect_with_origin(
        server: &str,
        origin: &str,
        token: impl Into<String>,
        channels: Vec<ChannelId>,
    ) -> Result<Self, Error> {
        let mut transport = Transport::new(server, token.into())?;
        transport.origin = endpoint(origin)?.origin().ascii_serialization();
        Self::open(transport, channels).await
    }
    pub(crate) async fn open(
        transport: Transport,
        channels: Vec<ChannelId>,
    ) -> Result<Self, Error> {
        if channels.is_empty() || channels.len() > MAX_CHANNELS {
            return Err(Error::message(
                "Subscribe to between 1 and 32 text channels",
            ));
        }
        let mut socket = connect_socket(&transport.base, &transport.origin, SOCKET_PATH).await?;
        send_frame(
            &mut socket,
            &ClientFrame {
                version: SOCKET_VERSION,
                event: ClientEvent::Authenticate {
                    token: transport.token.to_string(),
                    channels,
                },
            },
        )
        .await?;
        let bot = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match receive(&mut socket).await? {
                    Some(ServerEvent::Ready { bot }) => return Ok(bot),
                    Some(ServerEvent::Error { error }) => return Err(Error::api(error)),
                    Some(_) => return Err(Error::message("Unexpected authentication response")),
                    None => {}
                }
            }
        })
        .await
        .map_err(|_| Error::message("Bot authentication timed out"))??;
        let client = Client { transport, bot };
        let (sender, events) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            let mut ping = tokio::time::interval(Duration::from_secs(10));
            ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_seen = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = sender.closed() => return Ok(()),
                    _ = ping.tick() => {
                        if last_seen.elapsed() > Duration::from_secs(30) { return Err(Error::message("Bot subscription heartbeat timed out")); }
                        send_frame(&mut socket, &ClientFrame { version: SOCKET_VERSION, event: ClientEvent::Ping {} }).await?;
                    },
                    event = receive(&mut socket) => {
                        match event? {
                            Some(ServerEvent::Mention { message }) => { sender.try_send(message).map_err(|_| Error::message("Bot handler fell behind; subscription closed"))?; },
                            Some(ServerEvent::Pong {}) => {},
                            Some(ServerEvent::Revoked {}) => return Err(Error::message("Bot subscription access was revoked")),
                            Some(ServerEvent::Error { error }) => return Err(Error::api(error)),
                            Some(ServerEvent::Ready { .. }) => return Err(Error::message("Unexpected repeated authentication")),
                            None => {},
                        }
                        last_seen = tokio::time::Instant::now();
                    }
                }
            }
        });
        Ok(Self {
            client,
            events,
            task,
            ended: false,
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
    pub async fn next(&mut self) -> Result<Option<ChatMessage>, Error> {
        if self.ended {
            return Ok(None);
        }
        tokio::select! { biased;
            result = &mut self.task => {
                self.ended = true;
                self.events.close();
                while self.events.try_recv().is_ok() {}
                result.map_err(|_| Error::message("Bot subscription worker stopped"))??;
                Ok(None)
            },
            message = self.events.recv() => match message {
                Some(message) => Ok(Some(message)),
                None => {
                    self.ended = true;
                    (&mut self.task).await.map_err(|_| Error::message("Bot subscription worker stopped"))??;
                    Ok(None)
                }
            },
        }
    }
}
async fn receive(socket: &mut Socket) -> Result<Option<ServerEvent>, Error> {
    match socket.next().await {
        Some(Ok(tungstenite::Message::Text(text))) => {
            let frame: ServerFrame =
                serde_json::from_str(&text).map_err(|_| Error::message("Invalid bot event"))?;
            if frame.version != SOCKET_VERSION {
                return Err(Error::message("Unsupported bot protocol version"));
            }
            Ok(Some(frame.event))
        }
        Some(Ok(tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_))) => Ok(None),
        _ => Err(Error::message("Bot subscription disconnected")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;

    #[tokio::test]
    async fn revoked_or_disconnected_subscription_discards_queued_commands() {
        for revoke in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let bot_id: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
            let guild_id: GuildId = "00000000-0000-0000-0000-000000000002".parse().unwrap();
            let channel_id: ChannelId = "00000000-0000-0000-0000-000000000003".parse().unwrap();
            let (end, ending) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                socket.next().await.unwrap().unwrap();
                let ready = ServerFrame {
                    version: SOCKET_VERSION,
                    event: ServerEvent::Ready {
                        bot: BotInfo {
                            account_id: bot_id,
                            guild_id,
                            name: "bot".into(),
                        },
                    },
                };
                socket
                    .send(tungstenite::Message::Text(
                        serde_json::to_string(&ready).unwrap().into(),
                    ))
                    .await
                    .unwrap();
                for index in 0..3 {
                    let message: ChatMessage = serde_json::from_value(serde_json::json!({
                        "id":format!("00000000-0000-0000-0001-{index:012}"), "client_id":format!("00000000-0000-0000-0002-{index:012}"),
                        "guild_id":guild_id, "channel_id":channel_id, "author_id":"00000000-0000-0000-0000-000000000004", "username":"human", "content":"@bot /act",
                        "mentions":[bot_id], "created_at":"2026-10-23T00:00:00Z", "edited_at":null, "deleted":false, "revision":0, "sequence":index
                    })).unwrap();
                    let frame = ServerFrame {
                        version: SOCKET_VERSION,
                        event: ServerEvent::Mention { message },
                    };
                    socket
                        .send(tungstenite::Message::Text(
                            serde_json::to_string(&frame).unwrap().into(),
                        ))
                        .await
                        .unwrap();
                }
                ending.await.unwrap();
                if revoke {
                    let frame = ServerFrame {
                        version: SOCKET_VERSION,
                        event: ServerEvent::Revoked {},
                    };
                    socket
                        .send(tungstenite::Message::Text(
                            serde_json::to_string(&frame).unwrap().into(),
                        ))
                        .await
                        .unwrap();
                }
            });
            let mut subscription = Subscription::connect(&endpoint, "bot_test", vec![channel_id])
                .await
                .unwrap();
            // The dispatcher is occupied with its first handler while later
            // commands accumulate. End the server session before it asks again.
            subscription.next().await.unwrap().unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while subscription.events.len() != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            end.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !subscription.task.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(subscription.next().await.is_err());
            assert!(subscription.events.is_empty());
            assert!(subscription.next().await.unwrap().is_none());
            server.await.unwrap();
        }
    }
}
