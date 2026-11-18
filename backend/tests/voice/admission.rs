//! Voice admission: unauthenticated upgrades hold only the per-address pending
//! budget, never the voice connection slots, and one account holds at most
//! `Caps::VOICE.per_account` connections across channels.
use super::*;
use std::net::SocketAddr;
use thiscord_backend::limits::Limits;
use thiscord_shared::ErrorCode;
use tokio_tungstenite::tungstenite::Error as WsError;

/// `Caps::VOICE` (crate-private): pending upgrades per address and voice
/// connections per account.
const PER_ADDRESS: usize = 16;
const PER_ACCOUNT: usize = 8;

/// Serves with peer addresses attached, as `main.rs` does.
async fn serve(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    (
        addr,
        tokio::spawn(async move {
            let _ = axum::serve(listener, service).await;
        }),
    )
}

/// Opens the voice socket from the loopback address `local`.
async fn upgrade(local: [u8; 4], server: SocketAddr) -> Result<Socket, WsError> {
    let tcp = tokio::net::TcpSocket::new_v4().unwrap();
    tcp.bind(SocketAddr::from((local, 0))).unwrap();
    let stream = tcp.connect(server).await.unwrap();
    let mut request = format!("ws://{server}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    tokio_tungstenite::client_async(request, MaybeTlsStream::Plain(stream))
        .await
        .map(|(socket, _)| socket)
}

async fn join_from(
    local: [u8; 4],
    server: SocketAddr,
    token: &str,
    guild: &Value,
    channel: &Value,
) -> Socket {
    let mut socket = upgrade(local, server).await.unwrap();
    send(
        &mut socket,
        ClientEvent::Join {
            token: token.into(),
            guild_id: serde_json::from_value(guild.clone()).unwrap(),
            channel_id: serde_json::from_value(channel.clone()).unwrap(),
            attempt: None,
        },
    )
    .await;
    socket
}

/// Admitted joins get the server's offer (they are never answered here).
async fn offered(socket: &mut Socket) {
    assert!(matches!(event(socket).await, ServerEvent::Offer { .. }));
}

/// A connection still negotiating stays open with nothing more to say (it
/// cannot ping before answering the offer).
async fn undisturbed(socket: &mut Socket) {
    let next = tokio::time::timeout(Duration::from_millis(500), socket.next()).await;
    assert!(next.is_err(), "unexpected frame or close: {next:?}");
}

fn refused(result: Result<Socket, WsError>) {
    match result {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS)
        }
        other => panic!("expected 429, got {:?}", other.map(|_| ())),
    }
}

/// Voice joins are also limited to six a minute per account (database
/// limiter); forget those counters so the test reaches the concurrency cap.
fn forget_join_limits(db: &Database) {
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("DELETE FROM auth_limits")
        .execute(&mut c)
        .unwrap();
}

/// A guild with `channels` voice channels and two members besides the owner.
async fn fixture(
    db: &Database,
    prefix: &str,
    channels: usize,
) -> (String, String, Value, Vec<Value>) {
    let owner_name = format!("{prefix}_owner");
    let (_, owner) = user(db, &owner_name);
    permissions::bootstrap_owner(&db.pool, &owner_name).unwrap();
    let member_name = format!("{prefix}_member");
    let (_, member) = user(db, &member_name);
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Admission"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":member_name}),
        StatusCode::OK,
    )
    .await;
    for index in 0..channels {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"create_channel","name":format!("voice-{index}"),"kind":"voice"}),
            StatusCode::OK,
        )
        .await;
    }
    let guild = state["guild"]["id"].clone();
    let channels = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|channel| channel["id"].clone())
        .collect();
    (owner, member, guild, channels)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn idle_voice_upgrades_never_take_connection_slots() {
    let db = database();
    let (owner, member, guild, channels) = fixture(&db, "admission_idle", 1).await;
    let channel = &channels[0];
    let limits = Limits {
        sockets: 512,
        voice: 2,
    };
    let (server, serving) = serve(api::router_with_limits(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
        limits,
    ))
    .await;
    let mut first = join_from([127, 0, 0, 1], server, &owner, &guild, channel).await;
    offered(&mut first).await;
    let mut idle = Vec::new();
    for _ in 0..PER_ADDRESS {
        idle.push(upgrade([127, 0, 0, 1], server).await.unwrap());
    }
    refused(upgrade([127, 0, 0, 1], server).await);
    // The idle upgrades hold no voice slot: another address takes the last one.
    let mut second = join_from([127, 0, 0, 2], server, &member, &guild, channel).await;
    offered(&mut second).await;
    refused(upgrade([127, 0, 0, 3], server).await);
    undisturbed(&mut first).await;
    undisturbed(&mut second).await;
    drop(idle);
    drop(second);
    // The closed connection's slot and the idle address budget are returned.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let opened: Vec<_> = futures_util::future::join_all(
                (0..PER_ADDRESS).map(|_| upgrade([127, 0, 0, 1], server)),
            )
            .await;
            if opened.iter().all(Result::is_ok) {
                break;
            }
            drop(opened);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("closed connections release their slots");
    serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn one_account_is_capped_across_voice_channels_and_others_are_not() {
    let db = database();
    let (owner, member, guild, channels) = fixture(&db, "admission_heavy", PER_ACCOUNT + 1).await;
    let (server, serving) = serve(api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    ))
    .await;
    let mut held = Vec::new();
    for channel in &channels[..PER_ACCOUNT] {
        forget_join_limits(&db);
        let mut socket = join_from([127, 0, 0, 1], server, &owner, &guild, channel).await;
        offered(&mut socket).await;
        held.push(socket);
    }
    forget_join_limits(&db);
    let extra = &channels[PER_ACCOUNT];
    let mut refused = join_from([127, 0, 0, 1], server, &owner, &guild, extra).await;
    assert!(matches!(
        event(&mut refused).await,
        ServerEvent::Error { error } if error.code == ErrorCode::RateLimited
    ));
    // Another account joins the same channel; the held connections continue.
    let mut other = join_from([127, 0, 0, 1], server, &member, &guild, extra).await;
    offered(&mut other).await;
    undisturbed(held.last_mut().unwrap()).await;
    // Leaving one channel returns the account's slot.
    drop(held.pop());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            forget_join_limits(&db);
            let mut socket = join_from([127, 0, 0, 1], server, &owner, &guild, extra).await;
            if matches!(event(&mut socket).await, ServerEvent::Offer { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a closed connection returns the account's slot");
    serving.abort();
}
