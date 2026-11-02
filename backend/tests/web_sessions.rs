//! HttpOnly cookie sessions for the same-origin web client (docs/web.md).
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use diesel::{connection::SimpleConnection, prelude::*};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use thiscord_backend::{api, db};
use thiscord_shared::account::{ACCOUNT_PATH, SESSION_MODE_COOKIE, SESSION_MODE_HEADER};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tower::ServiceExt;
use uuid::Uuid;

const ALLOWED: &str = "http://127.0.0.1:1420";
const FOREIGN: &str = "https://attacker.example";
const COOKIE: &str = "__Host-thiscord_session";

struct Database {
    connection: PgConnection,
    schema: String,
    pool: db::DbPool,
}
impl Drop for Database {
    fn drop(&mut self) {
        self.connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .unwrap();
    }
}
fn database() -> Database {
    if std::env::var_os("TEST_DATABASE_URL").is_none() {
        dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    }
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("web_session_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = db::connect_and_migrate(url.as_str()).unwrap();
    Database {
        connection,
        schema,
        pool,
    }
}

#[derive(Default)]
struct Call<'a> {
    method: &'a str,
    path: &'a str,
    origin: Option<&'a str>,
    cookie: Option<&'a str>,
    bearer: Option<&'a str>,
    cookie_mode: bool,
    body: Option<Value>,
}
async fn send(app: &Router, call: Call<'_>) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(if call.method.is_empty() {
            "POST"
        } else {
            call.method
        })
        .uri(if call.path.is_empty() {
            ACCOUNT_PATH
        } else {
            call.path
        })
        .header("content-type", "application/json");
    if let Some(origin) = call.origin {
        builder = builder.header("origin", origin);
    }
    if let Some(cookie) = call.cookie {
        builder = builder.header("cookie", format!("theme=dark; {COOKIE}={cookie}"));
    }
    if let Some(token) = call.bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if call.cookie_mode {
        builder = builder.header(SESSION_MODE_HEADER, SESSION_MODE_COOKIE);
    }
    let body = call.body.map(|b| b.to_string()).unwrap_or_default();
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn current<'a>() -> Call<'a> {
    Call {
        body: Some(json!({"action":"current"})),
        ..Default::default()
    }
}
/// Returns the issued cookie value, checking every required attribute.
fn issued(headers: &HeaderMap) -> String {
    let cookie = headers["set-cookie"].to_str().unwrap();
    let (pair, attributes) = cookie.split_once(';').unwrap();
    let token = pair.strip_prefix(&format!("{COOKIE}=")).unwrap();
    assert_eq!(token.len(), 43);
    for attribute in ["Path=/", "HttpOnly", "Secure", "SameSite=Strict"] {
        assert!(attributes.contains(attribute), "{attribute} in {cookie}");
    }
    let age: i64 = attributes
        .split("Max-Age=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    // Matches the 30-day absolute session expiry.
    assert!((29 * 86_400..=30 * 86_400).contains(&age), "{age}");
    token.to_owned()
}
fn cleared(headers: &HeaderMap) -> bool {
    headers.get("set-cookie").is_some_and(|c| {
        let c = c.to_str().unwrap();
        c.starts_with(&format!("{COOKIE}=;")) && c.contains("Max-Age=0") && c.contains("Path=/")
    })
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn web_cookie_sessions_enforce_origin_rules_and_reach_sockets() {
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![ALLOWED.parse().unwrap()]);
    let register = |name: &str| {
        json!({"action":"register","username":name,"email":format!("{name}@example.test"),
               "password":"correct horse battery staple","device":"Web"})
    };

    // Cookie mode from an allowed origin: HttpOnly cookie, empty body token.
    let (status, headers, body) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie_mode: true,
            body: Some(register("web_user")),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session"]["token"], "");
    assert_eq!(body["session"]["account"]["username"], "web_user");
    assert_eq!(headers["cache-control"], "no-store");
    let cookie = issued(&headers);

    // Without the mode header, or from a foreign origin, no cookie is issued.
    // An ambient web cookie does not change a same-origin bearer login (admin).
    let login = json!({"action":"login","login":"web_user",
        "password":"correct horse battery staple","device":"Desktop"});
    for (origin, mode) in [(Some(ALLOWED), false), (Some(FOREIGN), true), (None, true)] {
        let (status, headers, body) = send(
            &app,
            Call {
                origin,
                cookie: Some(&cookie),
                cookie_mode: mode,
                body: Some(login.clone()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!headers.contains_key("set-cookie"), "{origin:?} {mode}");
        assert_eq!(body["session"]["token"].as_str().unwrap().len(), 43);
    }
    let (_, _, other) = send(
        &app,
        Call {
            body: Some(register("other_user")),
            ..Default::default()
        },
    )
    .await;
    let other_token = other["session"]["token"].as_str().unwrap().to_owned();

    // The cookie authenticates a POST only from an allowed Origin.
    let (status, headers, body) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            ..current()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["account"]["username"], "web_user");
    assert!(!headers.contains_key("set-cookie"));
    for origin in [Some(FOREIGN), Some("null"), None] {
        let (status, headers, _) = send(
            &app,
            Call {
                origin,
                cookie: Some(&cookie),
                ..current()
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{origin:?}");
        // An ignored cookie is not cleared: it was never used to authenticate.
        assert!(!headers.contains_key("set-cookie"));
    }

    // Safe methods without Origin may use the cookie. Diagnostics distinguish
    // an authenticated non-admin (403) from an anonymous request (401).
    let diagnostics = |origin, cookie| Call {
        method: "GET",
        path: "/api/v1/admin/diagnostics",
        origin,
        cookie,
        ..Default::default()
    };
    assert_eq!(
        send(&app, diagnostics(None, Some(&cookie))).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, diagnostics(Some(FOREIGN), Some(&cookie)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, diagnostics(None, None)).await.0,
        StatusCode::UNAUTHORIZED
    );

    // An explicit Authorization header always wins over the cookie.
    let (status, _, body) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            bearer: Some(&other_token),
            ..current()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["account"]["username"], "other_user");
    let (status, _, _) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            bearer: Some(&"x".repeat(43)),
            ..current()
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A cookie-authenticated rotation never returns a readable token, even
    // without the mode header; the consumed cookie stops working.
    let (status, headers, body) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            body: Some(json!({"action":"rotate"})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session"]["token"], "");
    let rotated = issued(&headers);
    assert_ne!(rotated, cookie);
    let cookie = rotated;

    // Sockets: an empty frame token uses the cookie captured at upgrade.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let socket = |origin: &'static str, cookie: Option<String>, token: String| async move {
        let mut request = format!("ws://{addr}/api/v1/socket")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert("cookie", format!("{COOKIE}={cookie}").parse().unwrap());
        }
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
        socket
            .send(Message::Text(
                json!({"version":1,"event":{"type":"connect","token":token}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                    break serde_json::from_str::<Value>(&text).unwrap();
                }
            }
        })
        .await
        .unwrap();
        Ok::<_, tokio_tungstenite::tungstenite::Error>(frame["event"]["type"].clone())
    };
    assert_eq!(
        socket(ALLOWED, Some(cookie.clone()), String::new())
            .await
            .unwrap(),
        "authenticated"
    );
    // Without a cookie an empty token remains unauthenticated.
    assert_eq!(socket(ALLOWED, None, String::new()).await.unwrap(), "error");
    // A non-empty frame token wins over the cookie.
    assert_eq!(
        socket(ALLOWED, Some(cookie.clone()), "y".repeat(43))
            .await
            .unwrap(),
        "error"
    );
    // Disallowed origins never reach the cookie: the upgrade is refused.
    match socket(FOREIGN, Some(cookie.clone()), String::new()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN)
        }
        other => panic!("foreign socket upgrade was not refused: {other:?}"),
    }
    serving.abort();

    // Logout clears the cookie; a later session check clears it again.
    let (status, headers, _) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            body: Some(json!({"action":"logout"})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared(&headers));
    let (status, headers, _) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            ..current()
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(cleared(&headers));

    // Ending every session clears too; bearer clients never get cookie headers.
    let (_, headers, _) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie_mode: true,
            body: Some(login.clone()),
            ..Default::default()
        },
    )
    .await;
    let cookie = issued(&headers);
    let (status, headers, _) = send(
        &app,
        Call {
            origin: Some(ALLOWED),
            cookie: Some(&cookie),
            body: Some(json!({"action":"logout_all"})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared(&headers));
    let (status, headers, _) = send(
        &app,
        Call {
            bearer: Some(&other_token),
            body: Some(json!({"action":"logout"})),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key("set-cookie"));
}
