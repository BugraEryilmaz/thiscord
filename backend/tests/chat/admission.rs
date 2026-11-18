//! Socket admission: unauthenticated upgrades hold only the per-address pending
//! budget, never the socket type's main slots, and one account holds at most
//! `Caps::SOCKETS.per_account` sockets of each type (docs/load-benchmarks.md).
use super::*;
use std::{net::SocketAddr, time::Duration};
use thiscord_backend::limits::Limits;
use tokio_tungstenite::{MaybeTlsStream, tungstenite::Error as WsError};

/// `Caps::SOCKETS` (crate-private): pending upgrades per address and sockets
/// per account.
const PER_ADDRESS: usize = 32;
const PER_ACCOUNT: usize = 16;
const PATHS: [&str; 3] = [
    SOCKET_PATH,
    thiscord_shared::friends::FRIENDS_SOCKET_PATH,
    thiscord_shared::notifications::SOCKET_PATH,
];

/// Serves with peer addresses attached, as `main.rs` does.
async fn serve(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    (
        address,
        tokio::spawn(async move { axum::serve(listener, service).await.unwrap() }),
    )
}

/// Opens `path` from the loopback address `local` (a distinct client address).
async fn upgrade(local: [u8; 4], server: SocketAddr, path: &str) -> Result<Socket, WsError> {
    let tcp = tokio::net::TcpSocket::new_v4().unwrap();
    tcp.bind(SocketAddr::from((local, 0))).unwrap();
    let stream = tcp.connect(server).await.unwrap();
    let mut request = format!("ws://{server}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    tokio_tungstenite::client_async(request, MaybeTlsStream::Plain(stream))
        .await
        .map(|(socket, _)| socket)
}

fn refused(result: Result<Socket, WsError>, path: &str) {
    match result {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{path}")
        }
        other => panic!("{path}: expected 429, got {:?}", other.map(|_| ())),
    }
}

fn connect_frame(path: &str, token: &str) -> WsMessage {
    let event = if path == thiscord_shared::notifications::SOCKET_PATH {
        json!({"type":"connect","token":token,"kinds":[]})
    } else {
        json!({"type":"connect","token":token})
    };
    WsMessage::Text(json!({"version":1,"event":event}).to_string().into())
}

/// The first frame an authenticated socket of each type receives.
fn ready(path: &str) -> &'static str {
    match path {
        SOCKET_PATH => "authenticated",
        thiscord_shared::friends::FRIENDS_SOCKET_PATH => "snapshot",
        _ => "ready",
    }
}

async fn authenticated(local: [u8; 4], server: SocketAddr, path: &str, token: &str) -> Socket {
    let mut socket = upgrade(local, server, path).await.unwrap();
    socket.send(connect_frame(path, token)).await.unwrap();
    event(&mut socket, ready(path)).await;
    socket
}

async fn ping(socket: &mut Socket) {
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"ping"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    event(socket, "pong").await;
}

/// The frames a refused socket receives before it closes.
async fn closed(socket: &mut Socket) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut frames = Vec::new();
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    frames.push(serde_json::from_str(&text).unwrap())
                }
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => return frames,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("a refused socket closes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn idle_upgrades_never_take_socket_slots_and_are_capped_per_address() {
    let db = database();
    let (_, token) = user(&db, "admission_idle");
    // One main slot per socket type: any idle upgrade holding one would show.
    let limits = Limits {
        sockets: 1,
        voice: 2,
    };
    let (server, serving) = serve(api::router_with_limits(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
        limits,
    ))
    .await;
    for path in PATHS {
        let mut idle = Vec::new();
        for _ in 0..PER_ADDRESS {
            idle.push(upgrade([127, 0, 0, 1], server, path).await.unwrap());
        }
        refused(upgrade([127, 0, 0, 1], server, path).await, path);
        // Another address still connects and takes the only main slot.
        let mut admitted = authenticated([127, 0, 0, 2], server, path, &token).await;
        refused(upgrade([127, 0, 0, 3], server, path).await, path);
        // Idle upgrades never authenticate; the admitted socket is unaffected.
        ping(&mut admitted).await;
        drop(idle);
        ping(&mut admitted).await;
        drop(admitted);
        // Every slot is returned once the sockets close.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let opened: Vec<_> = futures_util::future::join_all(
                    (0..PER_ADDRESS).map(|_| upgrade([127, 0, 0, 1], server, path)),
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
        .expect("closed sockets release their slots");
    }
    serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn one_account_is_capped_per_socket_type_and_others_are_not() {
    let db = database();
    let (_, heavy) = user(&db, "admission_heavy");
    let (_, other) = user(&db, "admission_other");
    let (server, serving) = serve(api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    ))
    .await;
    for path in PATHS {
        let mut held = Vec::new();
        for _ in 0..PER_ACCOUNT {
            held.push(authenticated([127, 0, 0, 1], server, path, &heavy).await);
        }
        // Past the cap the socket is refused after authentication: chat with a
        // retryable `rate_limited` error, friends and notifications by closing
        // (their clients reconnect as after any drop).
        let mut extra = upgrade([127, 0, 0, 1], server, path).await.unwrap();
        extra.send(connect_frame(path, &heavy)).await.unwrap();
        let frames = closed(&mut extra).await;
        assert!(
            frames.iter().all(|f| f["event"]["type"] != ready(path)),
            "{path}: {frames:?}"
        );
        if path == SOCKET_PATH {
            assert_eq!(frames.len(), 1, "{frames:?}");
            assert_eq!(frames[0]["event"]["type"], "error");
            assert_eq!(frames[0]["event"]["error"]["code"], "rate_limited");
        }
        // Another account is unaffected, and so are the held sockets.
        let mut unaffected = authenticated([127, 0, 0, 1], server, path, &other).await;
        ping(&mut unaffected).await;
        ping(held.last_mut().unwrap()).await;
        // Closing one returns its slot to the account.
        drop(held.pop());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut socket = upgrade([127, 0, 0, 1], server, path).await.unwrap();
                socket.send(connect_frame(path, &heavy)).await.unwrap();
                let frame = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
                if let Ok(Some(Ok(WsMessage::Text(text)))) = frame {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["event"]["type"] == ready(path) {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("a closed socket returns the account's slot");
    }
    serving.abort();
}
