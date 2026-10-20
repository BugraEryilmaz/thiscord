//! Signaling recovery owns no media resources and never issues a Join.
use super::*;
use crate::native_voice::{closed, log_close, send_event, server_failure, socket_failure};
use futures_util::SinkExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use thiscord_shared::{ChannelId, GuildId, voice::*};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type Receiver = futures_util::stream::SplitStream<Socket>;
pub(crate) struct Credentials {
    pub base: String,
    pub token: String,
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub resume_token: Option<String>,
}
pub(crate) struct Session {
    commands: mpsc::Sender<ClientEvent>,
    pub events: mpsc::Receiver<Result<Message, Failure>>,
    recovering: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Session {
    pub fn start(
        send: Sender,
        receive: Receiver,
        credentials: Credentials,
        connection: Connection,
    ) -> Self {
        let (commands, mut requests) = mpsc::channel(32);
        let (events, incoming) = mpsc::channel(32);
        let (stop, stopped) = oneshot::channel();
        let recovering = Arc::new(AtomicBool::new(false));
        let state = recovering.clone();
        let wake = Arc::new(tokio::sync::Notify::new());
        let worker_wake = wake.clone();
        let task = tokio::spawn(async move {
            let mut worker = Worker {
                link: Some((send, receive)),
                credentials,
                connection,
                recovering: state,
                wake: worker_wake,
                sequence: 0,
            };
            let result = tokio::select! { biased;
                _ = stopped => None,
                result = worker.run(&mut requests, &events) => Some(result),
            };
            if let Some(error) = result {
                let _ = events.send(Err(error)).await;
            }
            if let Some((mut send, _)) = worker.link.take() {
                let _ = timeout(Duration::from_millis(500), async {
                    let _ = send_event(&mut send, ClientEvent::Leave {}, &worker.connection).await;
                    let _ = send.close().await;
                })
                .await;
            }
        });
        Self {
            commands,
            events: incoming,
            recovering,
            wake,
            stop: Some(stop),
            task,
        }
    }
    pub fn recovering(&self) -> bool {
        self.recovering.load(Ordering::Acquire)
    }
    /// An unanswered privacy boundary detects a half-open socket before the
    /// ordinary heartbeat. Resume the existing media session; never issue Join.
    pub fn recover(&self) {
        self.recovering.store(true, Ordering::Release);
        self.wake.notify_one();
    }
    pub fn send(&self, event: ClientEvent) {
        // State is resent from the current UI snapshot after Resumed. Never
        // replay stale screen/mute commands queued during an outage.
        if self.recovering() {
            return;
        }
        if matches!(
            self.commands.try_send(event),
            Err(mpsc::error::TrySendError::Full(_))
        ) {
            // Backpressure repairs signaling, never tears down a healthy peer.
            self.recover();
        }
        // If the worker ended, its ordered terminal result is in events. A send
        // racing that result must not turn Replaced/Revoked into a fresh Join.
    }
    pub async fn close(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = timeout(Duration::from_millis(600), &mut self.task).await;
        self.task.abort();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Worker {
    link: Option<(Sender, Receiver)>,
    credentials: Credentials,
    connection: Connection,
    recovering: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    sequence: u64,
}
impl Worker {
    async fn run(
        &mut self,
        requests: &mut mpsc::Receiver<ClientEvent>,
        events: &mpsc::Sender<Result<Message, Failure>>,
    ) -> Failure {
        let mut heard = tokio::time::Instant::now();
        loop {
            let (send, receive) = self.link.as_mut().expect("attached");
            let failure = if self.recovering.load(Ordering::Acquire) {
                Failure::temporary("Voice signaling stalled").with_reason("signaling_stalled")
            } else {
                tokio::select! {
                    _ = self.wake.notified() => continue,
                    command = requests.recv() => {
                        let Some(command) = command else { return Failure::from("Voice signaling stopped"); };
                        match send_event(send, command, &self.connection).await { Ok(()) => continue, Err(error) => error }
                    },
                    message = receive.next() => {
                        match message {
                            Some(Ok(message)) if message.is_close() => { log_close(&self.connection, &message); closed(&message) }
                            Some(Ok(message)) => {
                                heard = tokio::time::Instant::now();
                                if events.send(Ok(message)).await.is_err() { return Failure::from("Voice signaling stopped"); }
                                continue;
                            },
                            Some(Err(error)) => socket_failure(&self.connection, "receive", &error),
                            None => Failure::temporary("Voice signaling closed").with_reason("signaling_eof"),
                        }
                    },
                    _ = tokio::time::sleep_until(heard + Duration::from_secs(20)) => Failure::temporary("Voice signaling timed out").with_reason("signaling_heartbeat_timeout"),
                }
            };
            if !failure.retryable || self.credentials.resume_token.is_none() {
                return failure;
            }
            self.recovering.store(true, Ordering::Release);
            self.connection.event(
                "voice_signaling_recovery_started",
                serde_json::json!({"reason":failure.diagnostic_reason()}),
            );
            // Drop only the signaling socket. No Leave, device stop or peer close.
            self.link.take();
            let recovered = timeout(Duration::from_secs(20), self.recover()).await;
            match recovered {
                Ok(Ok((link, ack))) => {
                    while requests.try_recv().is_ok() {}
                    self.link = Some(link);
                    heard = tokio::time::Instant::now();
                    self.recovering.store(false, Ordering::Release);
                    self.connection.event(
                        "voice_signaling_recovered",
                        serde_json::json!({"sequence":self.sequence}),
                    );
                    if events.send(Ok(ack)).await.is_err() {
                        return Failure::from("Voice signaling stopped");
                    }
                }
                Ok(Err(error)) => return error,
                Err(_) => {
                    return Failure::from(
                        "Voice signaling recovery expired. Join again to reconnect.",
                    )
                    .with_reason("signaling_recovery_expired");
                }
            }
        }
    }
    async fn recover(&mut self) -> Result<((Sender, Receiver), Message), Failure> {
        let mut failures = 0_u32;
        loop {
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or("Voice recovery sequence exhausted")?;
            let result = timeout(Duration::from_secs(5), self.attach()).await;
            match result {
                Ok(Ok(link)) => return Ok(link),
                Ok(Err(error))
                    if !error.retryable || error.diagnostic_reason() == "resume_unavailable" =>
                {
                    return Err(error);
                }
                _ => {}
            }
            failures += 1;
            tokio::time::sleep(Duration::from_millis(
                (250_u64 << failures.min(2)) + u64::from(rand::random::<u8>()),
            ))
            .await;
        }
    }
    async fn attach(&self) -> Result<((Sender, Receiver), Message), Failure> {
        let socket = connect_for(&self.credentials.base, &self.connection).await?;
        let (mut send, mut receive) = socket.split();
        send_event(
            &mut send,
            ClientEvent::Resume {
                token: self.credentials.token.clone(),
                guild_id: self.credentials.guild_id,
                channel_id: self.credentials.channel_id,
                resume_token: self.credentials.resume_token.clone().expect("supported"),
                sequence: self.sequence,
            },
            &self.connection,
        )
        .await?;
        let frame = next_data(&mut receive)
            .await
            .ok_or_else(|| Failure::temporary("Voice resume closed"))?
            .map_err(|error| socket_failure(&self.connection, "resume", &error))?;
        if frame.is_close() {
            return Err(closed(&frame));
        }
        let response: ServerFrame =
            serde_json::from_str(frame.to_text().map_err(|_| "Invalid resume response")?)
                .map_err(|_| "Invalid resume response")?;
        if response.version != VOICE_VERSION {
            return Err("Unsupported voice version".into());
        }
        match response.event {
            ServerEvent::Resumed {} => Ok(((send, receive), frame)),
            ServerEvent::ResumeUnavailable {} => Err(Failure::from(
                "Voice session ended. Join again to reconnect.",
            )
            .with_reason("resume_unavailable")),
            ServerEvent::Replaced {} => Err(Failure::replaced()),
            ServerEvent::Error { error } => Err(server_failure(&self.connection, error)),
            _ => Err("Unexpected resume response".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn credentials(base: String) -> Credentials {
        Credentials {
            base,
            token: "test-account-token".into(),
            guild_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            channel_id: "00000000-0000-0000-0000-000000000002".parse().unwrap(),
            resume_token: Some("test-media-capability".into()),
        }
    }
    async fn accept(listener: &TcpListener) -> WebSocketStream<TcpStream> {
        tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap()
    }
    async fn command(socket: &mut WebSocketStream<TcpStream>) -> ClientEvent {
        let message = socket.next().await.unwrap().unwrap();
        serde_json::from_str::<ClientFrame>(message.to_text().unwrap())
            .unwrap()
            .event
    }
    async fn reply(socket: &mut WebSocketStream<TcpStream>, event: ServerEvent) {
        socket
            .send(Message::Text(
                serde_json::to_string(&ServerFrame {
                    version: VOICE_VERSION,
                    event,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
    }
    fn assert_resume(event: ClientEvent, expected: u64) {
        let ClientEvent::Resume {
            sequence,
            resume_token,
            token,
            ..
        } = event
        else {
            panic!("Recovery must never issue Join");
        };
        assert_eq!(sequence, expected);
        assert_eq!(resume_token, "test-media-capability");
        assert_eq!(token, "test-account-token");
    }

    #[tokio::test]
    async fn unanswered_boundary_resumes_open_socket_and_requires_fresh_ack() {
        use thiscord_frontend::audio::red::boundary::{Boundary, RequestError};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen, wait) = oneshot::channel();
        let server = tokio::spawn(async move {
            // Keep the old socket open, simulating a proxy silently losing ACKs.
            let mut old = accept(&listener).await;
            assert!(matches!(
                command(&mut old).await,
                ClientEvent::MicrophoneBoundary { generation: 1 }
            ));
            seen.send(()).unwrap();
            let mut resumed = accept(&listener).await;
            assert_resume(command(&mut resumed).await, 1);
            reply(&mut resumed, ServerEvent::Resumed {}).await;
            assert!(matches!(
                command(&mut resumed).await,
                ClientEvent::MicrophoneBoundary { generation: 2 }
            ));
            reply(
                &mut resumed,
                ServerEvent::MicrophoneBoundaryAccepted {
                    generation: 2,
                    microphone_epoch: 3,
                    source_epoch: 3,
                    challenge: None,
                    accepted: true,
                },
            )
            .await;
            assert!(matches!(command(&mut resumed).await, ClientEvent::Leave {}));
        });
        let (send, receive) = connect(&base).await.unwrap().split();
        let mut session = Session::start(send, receive, credentials(base), Connection::default());
        let now = std::time::Instant::now();
        let mut boundary = Boundary::default();
        boundary.control(1).unwrap();
        let (generation, _) = boundary.request(now).unwrap().unwrap();
        session.send(ClientEvent::MicrophoneBoundary { generation });
        timeout(Duration::from_secs(2), wait)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            boundary.request(now + Duration::from_secs(5)),
            Err(RequestError::AcknowledgmentTimeout)
        );
        session.recover();
        assert!(session.recovering());
        boundary.resume();
        boundary.control(2).unwrap();
        assert!(!boundary.acknowledge(1, None, true));
        assert!(!boundary.ready());
        let resumed = timeout(Duration::from_secs(2), session.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(resumed.to_text().unwrap())
                .unwrap()
                .event,
            ServerEvent::Resumed {}
        ));
        boundary.resume();
        let (generation, _) = boundary
            .request(now + Duration::from_secs(6))
            .unwrap()
            .unwrap();
        session.send(ClientEvent::MicrophoneBoundary { generation });
        let ack = timeout(Duration::from_secs(2), session.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ServerEvent::MicrophoneBoundaryAccepted {
            generation,
            challenge,
            accepted,
            ..
        } = serde_json::from_str::<ServerFrame>(ack.to_text().unwrap())
            .unwrap()
            .event
        else {
            panic!("expected boundary ACK")
        };
        assert!(boundary.acknowledge(generation, challenge, accepted));
        assert!(boundary.ready_control(2));
        session.close().await;
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn reset_and_lost_resume_ack_recover_without_join_or_leave() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            drop(accept(&listener).await); // Abrupt transport loss.
            let mut first = accept(&listener).await;
            assert_resume(command(&mut first).await, 1);
            drop(first); // Server installed this socket, but the ACK was lost.
            let mut second = accept(&listener).await;
            assert_resume(command(&mut second).await, 2);
            reply(&mut second, ServerEvent::Resumed {}).await;
            assert!(matches!(
                command(&mut second).await,
                ClientEvent::State {
                    muted: true,
                    deafened: false
                }
            ));
            reply(&mut second, ServerEvent::Pong {}).await;
            assert!(matches!(command(&mut second).await, ClientEvent::Leave {}));
            assert!(second.next().await.unwrap().unwrap().is_close());
        });
        let (send, receive) = connect(&base).await.unwrap().split();
        let mut session = Session::start(send, receive, credentials(base), Connection::default());
        let ack = timeout(Duration::from_secs(4), session.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(ack.to_text().unwrap())
                .unwrap()
                .event,
            ServerEvent::Resumed {}
        ));
        assert!(!session.recovering());
        session.send(ClientEvent::State {
            muted: true,
            deafened: false,
        });
        let _ = timeout(Duration::from_secs(2), session.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        session.close().await;
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn replaced_or_missing_session_is_terminal_and_cannot_join_automatically() {
        for response in [ServerEvent::Replaced {}, ServerEvent::ResumeUnavailable {}] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                drop(accept(&listener).await);
                let mut socket = accept(&listener).await;
                assert_resume(command(&mut socket).await, 1);
                reply(&mut socket, response).await;
                assert!(
                    timeout(Duration::from_millis(150), listener.accept())
                        .await
                        .is_err()
                );
            });
            let (send, receive) = connect(&base).await.unwrap().split();
            let mut session =
                Session::start(send, receive, credentials(base), Connection::default());
            let error = timeout(Duration::from_secs(2), session.events.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(!error.retryable);
            session.close().await;
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn leave_cancels_a_pending_resume_without_waiting_for_its_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (ready, wait) = oneshot::channel();
        let server = tokio::spawn(async move {
            drop(accept(&listener).await);
            let mut socket = accept(&listener).await;
            assert_resume(command(&mut socket).await, 1);
            ready.send(()).unwrap();
            // No ACK: closing the Session must drop this in-progress socket.
            assert!(socket.next().await.is_none_or(|frame| frame.is_err()));
            assert!(
                timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_err()
            );
        });
        let (send, receive) = connect(&base).await.unwrap().split();
        let mut session = Session::start(send, receive, credentials(base), Connection::default());
        timeout(Duration::from_secs(2), wait)
            .await
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(1), session.close())
            .await
            .unwrap();
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn persistent_outage_has_a_deadline_and_never_escalates_to_join() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            drop(accept(&listener).await);
            let mut sequence = 0;
            loop {
                let mut socket = accept(&listener).await;
                sequence += 1;
                assert_resume(command(&mut socket).await, sequence);
                // Fail every attachment without an acknowledgement.
            }
        });
        let (send, receive) = connect(&base).await.unwrap().split();
        let mut session = Session::start(send, receive, credentials(base), Connection::default());
        let error = timeout(Duration::from_secs(22), session.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.diagnostic_reason(), "signaling_recovery_expired");
        assert!(!error.retryable);
        session.close().await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
