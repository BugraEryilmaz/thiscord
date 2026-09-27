//! Durable email outbox. Development files contain sensitive one-time codes.
use super::{
    Failure,
    store::{connection, execute, query},
};
use crate::db::DbPool;
use lettre::{Message, SmtpTransport, Transport, transport::smtp::authentication::Credentials};
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
        loop {
            let pool = pool.clone();
            if !matches!(
                tokio::task::spawn_blocking(move || deliver(&pool)).await,
                Ok(Ok(()))
            ) {
                tracing::warn!("email delivery failed; queued messages will retry");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}
fn deliver(pool: &DbPool) -> Result<(), Failure> {
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
    drop(c);
    for mail in messages {
        match std::env::var("MAIL_MODE").as_deref().unwrap_or("file") {
            "file" => {
                let path = PathBuf::from(
                    std::env::var_os("MAIL_DIRECTORY").unwrap_or_else(|| ".mail".into()),
                );
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
        execute(
            &mut *connection(pool)?,
            "DELETE FROM mail_outbox WHERE id=$1::uuid",
            &[&mail.id.to_string()],
        )?;
    }
    Ok(())
}
