//! Durable email outbox. Development files contain sensitive one-time codes.
use super::{
    Failure,
    store::{connection, execute, query},
};
use crate::db::DbPool;
use lettre::{Message, SmtpTransport, Transport, transport::smtp::authentication::Credentials};
use resend_rs::{ConfigBuilder, Resend, types::CreateEmailBaseOptions};
use serde::Deserialize;
use std::{io::Write, path::PathBuf, time::Duration};

#[derive(Deserialize)]
struct Mail {
    id: uuid::Uuid,
    recipient: String,
    body: String,
}

pub fn start(pool: DbPool) {
    tokio::spawn(async move {
        let resend = if std::env::var("MAIL_MODE").as_deref() == Ok("resend") {
            let key = std::env::var("RESEND_API_KEY").unwrap_or_default();
            if key.trim().is_empty() || key.trim() == "re_xxxxxxxxx" {
                tracing::warn!(
                    "Resend email is disabled: set RESEND_API_KEY in backend/.env and restart"
                );
                None
            } else {
                Some((
                    Resend::with_config(
                        ConfigBuilder::new(key.trim())
                            .base_url(
                                url::Url::parse("https://api.resend.com")
                                    .expect("static Resend URL"),
                            )
                            .build(),
                    ),
                    std::env::var("RESEND_FROM").unwrap_or_else(|_| "onboarding@resend.dev".into()),
                ))
            }
        } else {
            None
        };
        loop {
            if deliver(&pool, resend.as_ref()).await.is_err() {
                tracing::warn!("email delivery failed; queued messages will retry");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}
async fn deliver(pool: &DbPool, resend: Option<&(Resend, String)>) -> Result<(), Failure> {
    let pending_pool = pool.clone();
    let messages = tokio::task::spawn_blocking(move || pending(&pending_pool))
        .await
        .map_err(|_| Failure::Unavailable)??;
    for mail in messages {
        let id = mail.id;
        let result = if let Some((client, from)) = resend {
            send_resend(client, from, &mail).await
        } else {
            tokio::task::spawn_blocking(move || deliver_local(mail))
                .await
                .map_err(|_| Failure::Unavailable)?
        };
        if result.is_err() {
            // Do not log provider errors: they can include recipients or message content.
            tracing::warn!("email delivery failed; queued message will retry");
            continue;
        }
        let pool = pool.clone();
        tokio::task::spawn_blocking(move || {
            execute(
                &mut *connection(&pool)?,
                "DELETE FROM mail_outbox WHERE id=$1::uuid",
                &[&id.to_string()],
            )
        })
        .await
        .map_err(|_| Failure::Unavailable)??;
    }
    Ok(())
}

async fn send_resend(client: &Resend, from: &str, mail: &Mail) -> Result<(), Failure> {
    let email = CreateEmailBaseOptions::new(from, [&mail.recipient], "Thiscord account security")
        .with_text(&mail.body)
        .with_html(&html_body(&mail.body))
        .with_idempotency_key(&format!("thiscord-mail-{}", mail.id));
    tokio::time::timeout(Duration::from_secs(15), client.emails.send(email))
        .await
        .map_err(|_| Failure::Unavailable)?
        .map_err(|_| Failure::Unavailable)?;
    Ok(())
}

fn html_body(body: &str) -> String {
    body.split("\n\n")
        .map(|paragraph| {
            let escaped = paragraph
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;");
            if url::Url::parse(paragraph).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
            {
                format!("<p><a href=\"{escaped}\">Verify email</a></p>")
            } else {
                format!("<p>{escaped}</p>")
            }
        })
        .collect()
}

fn pending(pool: &DbPool) -> Result<Vec<Mail>, Failure> {
    if std::env::var("MAIL_MODE").as_deref().unwrap_or("file") == "file" {
        let directory =
            PathBuf::from(std::env::var_os("MAIL_DIRECTORY").unwrap_or_else(|| ".mail".into()));
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "txt")
                    && path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                    && entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(1800))
                {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
    let mut c = connection(pool)?;
    // Expired data is no longer useful; bound retained session/token and secret data.
    execute(
        &mut c,
        "DELETE FROM mail_outbox WHERE created_at<now()-interval '30 minutes'",
        &[],
    )?;
    execute(
        &mut c,
        "DELETE FROM account_codes WHERE expires_at<now()",
        &[],
    )?;
    execute(
        &mut c,
        "DELETE FROM oauth_attempts WHERE expires_at<now()",
        &[],
    )?;
    execute(&mut c, "DELETE FROM sessions WHERE expires_at<now()", &[])?;
    let messages: Vec<Mail> = query(
        &mut c,
        "SELECT to_jsonb(m) AS data FROM mail_outbox m ORDER BY created_at LIMIT 20",
        &[],
    )?;
    Ok(messages)
}

fn deliver_local(mail: Mail) -> Result<(), Failure> {
    match std::env::var("MAIL_MODE").as_deref().unwrap_or("file") {
        "file" => {
            let path =
                PathBuf::from(std::env::var_os("MAIL_DIRECTORY").unwrap_or_else(|| ".mail".into()));
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path).map_err(|_| Failure::Unavailable)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path.join(format!("{}.txt", mail.id))) {
                Ok(mut file) => {
                    write!(
                        file,
                        "To: {}\nSubject: Thiscord account security\n\n{}",
                        mail.recipient, mail.body
                    )
                    .map_err(|_| Failure::Unavailable)?;
                    file.sync_all().map_err(|_| Failure::Unavailable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Failure::Unavailable),
            }
        }
        "smtp" => {
            let get = |key| std::env::var(key).map_err(|_| Failure::Unavailable);
            let transport = SmtpTransport::relay(&get("SMTP_HOST")?)
                .map_err(|_| Failure::Unavailable)?
                .credentials(Credentials::new(
                    get("SMTP_USERNAME")?,
                    get("SMTP_PASSWORD")?,
                ))
                .timeout(Some(Duration::from_secs(15)))
                .build();
            let message = Message::builder()
                .from(
                    get("SMTP_FROM")?
                        .parse()
                        .map_err(|_| Failure::Unavailable)?,
                )
                .to(mail.recipient.parse().map_err(|_| Failure::Unavailable)?)
                .subject("Thiscord account security")
                .body(mail.body)
                .map_err(|_| Failure::Unavailable)?;
            transport.send(&message).map_err(|_| Failure::Unavailable)?;
        }
        _ => return Err(Failure::Unavailable),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn resend_request_and_failure_preserve_retry_identity() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let router = Router::new().route("/emails", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
            let captured = captured.clone();
            async move {
                let mut requests = captured.lock().unwrap();
                requests.push((headers, body));
                if requests.len() == 1 {
                    (StatusCode::FORBIDDEN, Json(serde_json::json!({"statusCode":403,"name":"validation_error","message":"test rejection"})))
                } else {
                    (StatusCode::OK, Json(serde_json::json!({"id":"aef8f7db-49d5-4d7a-9694-34652d26e6b3"})))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = Resend::with_config(
            ConfigBuilder::new("re_test_only")
                .base_url(url::Url::parse(&format!("http://{address}")).unwrap())
                .build(),
        );
        let mail = Mail {
            id: uuid::Uuid::new_v4(), recipient: "account@example.test".into(),
            body: "Verify your email:\n\nhttps://example.test/api/v1/account/verify-email?token=test&value=1\n\nIgnore <script> & \"quotes\".".into(),
        };
        assert!(
            send_resend(&client, "onboarding@resend.dev", &mail)
                .await
                .is_err()
        );
        assert!(
            send_resend(&client, "onboarding@resend.dev", &mail)
                .await
                .is_ok()
        );
        server.abort();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for (headers, body) in requests.iter() {
            assert_eq!(headers["authorization"], "Bearer re_test_only");
            assert_eq!(
                headers["idempotency-key"],
                format!("thiscord-mail-{}", mail.id)
            );
            assert_eq!(body["from"], "onboarding@resend.dev");
            assert_eq!(body["to"], serde_json::json!([mail.recipient]));
            assert_eq!(body["text"], mail.body);
            let html = body["html"].as_str().unwrap();
            assert!(html.contains("<a href=\"https://example.test/api/v1/account/verify-email?token=test&amp;value=1\">Verify email</a>"));
            assert!(html.contains("&lt;script&gt; &amp; &quot;quotes&quot;"));
        }
        assert_eq!(requests[0].1, requests[1].1);
    }
}
