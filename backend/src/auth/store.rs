use super::Failure;
use crate::db::DbPool;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use diesel::{
    pg::Pg,
    prelude::*,
    sql_types::{Jsonb, Text},
};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiscord_shared::{AccountId, SessionId, account::*};
use uuid::Uuid;

#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = Jsonb)]
    data: serde_json::Value,
}

// Only fixed SQL strings reach these helpers. Every user value is a bound parameter.
pub(crate) fn query<T: DeserializeOwned>(
    c: &mut PgConnection,
    sql: &str,
    params: &[&str],
) -> Result<Vec<T>, Failure> {
    let mut q = diesel::sql_query(sql).into_boxed::<Pg>();
    for p in params {
        q = q.bind::<Text, _>(*p);
    }
    q.load::<Row>(c)?
        .into_iter()
        .map(|r| serde_json::from_value(r.data).map_err(|_| Failure::Unavailable))
        .collect()
}
pub(crate) fn execute(c: &mut PgConnection, sql: &str, params: &[&str]) -> Result<usize, Failure> {
    let mut q = diesel::sql_query(sql).into_boxed::<Pg>();
    for p in params {
        q = q.bind::<Text, _>(*p);
    }
    Ok(q.execute(c)?)
}
pub(super) fn secret() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}
pub(crate) fn digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}
pub(crate) fn connection(
    pool: &DbPool,
) -> Result<diesel::r2d2::PooledConnection<diesel::r2d2::ConnectionManager<PgConnection>>, Failure>
{
    pool.get().map_err(|_| Failure::Unavailable)
}
fn password_hash(password: &str) -> Result<String, Failure> {
    validate_password(password)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|_| Failure::Unavailable)
}
fn verify(password: &str, hash: &str) -> bool {
    password.len() <= 1024
        && PasswordHash::new(hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
}
fn validate_password(password: &str) -> Result<(), Failure> {
    if password.chars().count() < 12 || password.len() > 1024 {
        return Err(Failure::Invalid(
            "Password must have at least 12 characters and at most 1024 bytes",
        ));
    }
    Ok(())
}
pub(super) fn email(value: &str) -> Result<String, Failure> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() > 254 || !value.is_ascii() || value.parse::<lettre::Address>().is_err() {
        return Err(Failure::Invalid("Enter a valid email address"));
    }
    Ok(value)
}
pub(super) fn device(value: &str) -> Result<&str, Failure> {
    if value.trim().is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control)
    {
        return Err(Failure::Invalid(
            "Device name must contain 1 to 80 characters",
        ));
    }
    Ok(value.trim())
}
pub(super) fn account(c: &mut PgConnection, id: AccountId) -> Result<Account, Failure> {
    query(c, "SELECT to_jsonb(a) || jsonb_build_object('identities', (SELECT jsonb_agg(provider ORDER BY provider) FROM identities WHERE account_id=a.id)) AS data FROM accounts a WHERE id=$1::uuid", &[&id.to_string()])?.pop().ok_or(Failure::Unauthorized)
}
#[derive(Deserialize, Clone)]
pub(crate) struct Session {
    pub id: SessionId,
    pub account_id: AccountId,
    pub reauthenticated_at: Option<DateTime<Utc>>,
}

pub(crate) fn authenticate(c: &mut PgConnection, token: &str) -> Result<Session, Failure> {
    if token.len() != 43 {
        return Err(Failure::Unauthorized);
    }
    let hash = digest(token);
    // Commit replay revocation even though authentication itself fails.
    let mut replay_revoked = false;
    let session=c.transaction::<_, Failure, _>(|c| {
        let sessions: Vec<Session> = query(c, "SELECT to_jsonb(s) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id WHERE t.token_hash=$1 AND NOT s.revoked AND s.expires_at>now() AND s.last_seen_at>now()-interval '7 days' FOR UPDATE OF s", &[&hash])?;
        let Some(session) = sessions.into_iter().next() else { return Ok(None); };
        let active: Vec<bool> = query(c, "SELECT to_jsonb(active) AS data FROM session_tokens WHERE token_hash=$1", &[&hash])?;
        if active != [true] {
            execute(c, "UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid", &[&session.id.to_string()])?;
            replay_revoked=true;
            return Ok(None);
        }
        Ok(Some(session))
    })?;
    if replay_revoked {
        crate::chat::invalidate();
    }
    session.ok_or(Failure::Unauthorized)
}
pub(crate) fn lock_session(
    c: &mut PgConnection,
    token: &str,
    session: &Session,
) -> Result<(), Failure> {
    // Serialize all account mutations, including linking, deletion and password reset.
    execute(
        c,
        "SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE",
        &[&session.account_id.to_string()],
    )?;
    let valid: Vec<bool> = query(
        c,
        "SELECT to_jsonb(TRUE) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id WHERE s.id=$1::uuid AND t.token_hash=$2 AND t.active AND NOT s.revoked AND s.expires_at>now() AND s.last_seen_at>now()-interval '7 days' FOR UPDATE OF s",
        &[&session.id.to_string(), &digest(token)],
    )?;
    if valid.is_empty() {
        return Err(Failure::Unauthorized);
    }
    execute(
        c,
        "UPDATE sessions SET last_seen_at=now() WHERE id=$1::uuid",
        &[&session.id.to_string()],
    )?;
    Ok(())
}
pub(crate) fn recent(session: &Session) -> Result<(), Failure> {
    if session
        .reauthenticated_at
        .is_some_and(|t| t > Utc::now() - chrono::Duration::minutes(5))
    {
        Ok(())
    } else {
        Err(Failure::Forbidden)
    }
}
pub(super) fn grant(
    c: &mut PgConnection,
    id: AccountId,
    device_name: &str,
) -> Result<AccountResponse, Failure> {
    let device_name = device(device_name)?;
    // A bounded number of devices per account; oldest active session is evicted.
    execute(
        c,
        "UPDATE sessions SET revoked=TRUE WHERE id IN (SELECT id FROM sessions WHERE account_id=$1::uuid AND NOT revoked ORDER BY created_at DESC OFFSET 19)",
        &[&id.to_string()],
    )?;
    let sid = SessionId::from_uuid(Uuid::new_v4());
    execute(
        c,
        "INSERT INTO sessions(id,account_id,device) VALUES($1::uuid,$2::uuid,$3)",
        &[&sid.to_string(), &id.to_string(), device_name],
    )?;
    session_grant(c, sid, id)
}
fn session_grant(
    c: &mut PgConnection,
    sid: SessionId,
    id: AccountId,
) -> Result<AccountResponse, Failure> {
    let token = secret();
    execute(
        c,
        "INSERT INTO session_tokens(token_hash,session_id) VALUES($1,$2::uuid)",
        &[&digest(&token), &sid.to_string()],
    )?;
    let expires_at = query(
        c,
        "SELECT to_jsonb(expires_at) AS data FROM sessions WHERE id=$1::uuid",
        &[&sid.to_string()],
    )?
    .pop()
    .ok_or(Failure::Unauthorized)?;
    Ok(AccountResponse::Session {
        session: SessionGrant {
            token,
            expires_at,
            account: account(c, id)?,
        },
    })
}
fn done(message: &str) -> AccountResponse {
    AccountResponse::Done {
        message: message.into(),
    }
}

pub(crate) fn rate_limit(
    pool: &DbPool,
    ip: &str,
    identifier: Option<&str>,
    sensitive: bool,
) -> Result<(), Failure> {
    let mut c = connection(pool)?;
    let mut keys = vec![(format!("ip:{ip}"), 180)];
    if sensitive {
        keys.push((format!("sensitive:{ip}"), 30));
    }
    if let Some(identifier) = identifier {
        keys.push((format!("account:{}", digest(identifier)), 6));
    }
    execute(
        &mut c,
        "DELETE FROM auth_limits WHERE window_start < now()-interval '2 minutes'",
        &[],
    )?;
    for (key, max) in keys {
        let counts: Vec<i32> = query(
            &mut c,
            "INSERT INTO auth_limits(key) VALUES($1) ON CONFLICT(key) DO UPDATE SET attempts=CASE WHEN auth_limits.window_start<now()-interval '1 minute' THEN 1 ELSE auth_limits.attempts+1 END, window_start=CASE WHEN auth_limits.window_start<now()-interval '1 minute' THEN now() ELSE auth_limits.window_start END RETURNING to_jsonb(attempts) AS data",
            &[&digest(&key)],
        )?;
        if counts[0] > max {
            return Err(Failure::Limited);
        }
    }
    Ok(())
}

fn enqueue_code(c: &mut PgConnection, a: &Account, purpose: &str) -> Result<(), Failure> {
    let code = secret();
    let body = if purpose == "verify" {
        let link = super::verification::link(&code)?;
        format!(
            "Thiscord verify email (valid for 30 minutes):\n\n{link}\n\nClick the link to automatically verify your email. If you did not create a Thiscord account, ignore this email."
        )
    } else {
        format!(
            "Thiscord {purpose} code (valid for 30 minutes):\n\n{code}\n\nPaste this code in Thiscord. If you did not request it, ignore this email."
        )
    };
    execute(
        c,
        "DELETE FROM account_codes WHERE account_id=$1::uuid AND purpose=$2",
        &[&a.id.to_string(), purpose],
    )?;
    execute(
        c,
        "INSERT INTO account_codes(token_hash,account_id,purpose) VALUES($1,$2::uuid,$3)",
        &[&digest(&code), &a.id.to_string(), purpose],
    )?;
    execute(
        c,
        "INSERT INTO mail_outbox(account_id,recipient,body) VALUES($1::uuid,$2,$3)",
        &[&a.id.to_string(), &a.email, &body],
    )?;
    Ok(())
}

pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    command: AccountRequest,
) -> Result<AccountResponse, Failure> {
    let mut c = connection(pool)?;
    match command {
        AccountRequest::Register {
            username,
            email: address,
            password,
            device: name,
        } => {
            let username = username.trim().to_ascii_lowercase();
            if !(3..=32).contains(&username.len())
                || !username
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(Failure::Invalid(
                    "Username must be 3 to 32 letters, numbers or underscores",
                ));
            }
            let address = email(&address)?;
            device(&name)?;
            let hash = password_hash(&password)?;
            c.transaction(|c| {
                let id = AccountId::from_uuid(Uuid::new_v4());
                execute(c,"INSERT INTO accounts(id,username,email,display_name) VALUES($1::uuid,$2,$3,$2)", &[&id.to_string(), &username, &address])?;
                execute(c,"INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,$2)", &[&id.to_string(), &hash])?;
                let a = account(c,id)?;
                enqueue_code(c, &a, "verify")?;
                grant(c,id,&name)
            })
        }
        AccountRequest::Login {
            login,
            password,
            device: name,
        } => {
            device(&name)?;
            let login = login.trim().to_ascii_lowercase();
            #[derive(Deserialize)]
            struct Credentials {
                account_id: AccountId,
                password_hash: String,
            }
            let credential: Option<Credentials> = query(&mut c,"SELECT to_jsonb(i) AS data FROM identities i JOIN accounts a ON a.id=i.account_id WHERE i.provider='password' AND (a.email=$1 OR a.username=$1)", &[&login])?.pop();
            let Some(credential) = credential else {
                // Match the expensive work for absent accounts without retaining a known password.
                let _ = password_hash("dummy-password-for-timing");
                return Err(Failure::Unauthorized);
            };
            if !verify(&password, &credential.password_hash) {
                return Err(Failure::Unauthorized);
            }
            c.transaction(|c| {
                execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE", &[&credential.account_id.to_string()])?;
                let unchanged: Vec<bool> = query(c,"SELECT to_jsonb(TRUE) AS data FROM identities WHERE account_id=$1::uuid AND provider='password' AND password_hash=$2", &[&credential.account_id.to_string(), &credential.password_hash])?;
                if unchanged.is_empty() { return Err(Failure::Unauthorized); }
                grant(c,credential.account_id,&name)
            })
        }
        AccountRequest::ForgotPassword { email: address } => {
            let address = email(&address)?;
            c.transaction(|c| {
                let ids: Vec<AccountId> = query(c,"SELECT to_jsonb(id) AS data FROM accounts WHERE email=$1 AND email_verified FOR UPDATE", &[&address])?;
                if let Some(id) = ids.first() { let a = account(c,*id)?; enqueue_code(c,&a,"reset")?; }
                Ok(done("If a verified account exists, a reset code will be sent"))
            })
        }
        AccountRequest::VerifyEmail { code }
        | AccountRequest::ResetPassword { code, password: _ }
            if code.len() != 43 =>
        {
            Err(Failure::Unauthorized)
        }
        AccountRequest::VerifyEmail { code } => consume_code(&mut c, &code, None),
        AccountRequest::ResetPassword { code, password } => {
            consume_code(&mut c, &code, Some(password_hash(&password)?))
        }
        AccountRequest::GoogleComplete { ticket } => super::google::complete(&mut c, &ticket),
        AccountRequest::GoogleCancel { ticket } => {
            execute(
                &mut c,
                "DELETE FROM oauth_attempts WHERE ticket_hash=$1",
                &[&digest(&ticket)],
            )?;
            Ok(done("Google sign-in cancelled"))
        }
        command => {
            let session = authenticate(&mut c, token)?;
            c.transaction(|c| {
                lock_session(c,token,&session)?;
                let id = session.account_id;
                match command {
                    AccountRequest::Current => Ok(AccountResponse::Account { account: account(c,id)? }),
                    AccountRequest::Logout => { execute(c,"UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid", &[&session.id.to_string()])?; Ok(done("Signed out")) },
                    AccountRequest::LogoutAll => { revoke_all(c,id)?; Ok(done("Signed out on all devices")) },
                    AccountRequest::Rotate => {
                        execute(c,"UPDATE session_tokens SET active=FALSE WHERE session_id=$1::uuid", &[&session.id.to_string()])?;
                        session_grant(c,session.id,id)
                    }
                    AccountRequest::Sessions => Ok(AccountResponse::Sessions { sessions: query(c,"SELECT to_jsonb(s) || jsonb_build_object('current',id=$2::uuid) AS data FROM sessions s WHERE account_id=$1::uuid AND NOT revoked AND expires_at>now() AND last_seen_at>now()-interval '7 days' ORDER BY created_at DESC", &[&id.to_string(), &session.id.to_string()])? }),
                    AccountRequest::RevokeSession { session_id } => { execute(c,"UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid AND account_id=$2::uuid", &[&session_id.to_string(), &id.to_string()])?; Ok(done("Session revoked")) },
                    AccountRequest::SendVerification => { let a = account(c,id)?; if !a.email_verified { enqueue_code(c,&a,"verify")?; } Ok(done("Verification email queued")) },
                    AccountRequest::Reauthenticate { password } => {
                        let hashes: Vec<String> = query(c,"SELECT to_jsonb(password_hash) AS data FROM identities WHERE account_id=$1::uuid AND provider='password'", &[&id.to_string()])?;
                        if !hashes.first().is_some_and(|h| verify(&password,h)) { return Err(Failure::Unauthorized); }
                        execute(c,"UPDATE sessions SET reauthenticated_at=now() WHERE id=$1::uuid", &[&session.id.to_string()])?;
                        Ok(done("Reauthenticated for five minutes"))
                    }
                    AccountRequest::ChangePassword { password } => {
                        recent(&session)?;
                        if !account(c,id)?.email_verified { return Err(Failure::Invalid("Verify your email first")); }
                        let hash = password_hash(&password)?;
                        set_password(c,id,&hash)?; revoke_all(c,id)?;
                        Ok(done("Password changed. Sign in again on each device"))
                    }
                    AccountRequest::UpdateProfile { display_name, bio } => {
                        if display_name.trim().is_empty() || display_name.chars().count()>64 || bio.chars().count()>500 || display_name.chars().any(char::is_control) { return Err(Failure::Invalid("Display name must be 1 to 64 characters; bio at most 500")); }
                        execute(c,"UPDATE accounts SET display_name=$2,bio=$3 WHERE id=$1::uuid", &[&id.to_string(),display_name.trim(),&bio])?;
                        Ok(AccountResponse::Account { account: account(c,id)? })
                    }
                    AccountRequest::UnlinkIdentity { provider } => {
                        recent(&session)?;
                        let a = account(c,id)?;
                        if a.identities.len()<2 || (provider==IdentityProvider::Google && !a.email_verified) { return Err(Failure::Forbidden); }
                        execute(c,"DELETE FROM identities WHERE account_id=$1::uuid AND provider=$2", &[&id.to_string(), if provider==IdentityProvider::Google {"google"} else {"password"}])?;
                        revoke_all(c,id)?;
                        Ok(done("Identity removed. Sign in again"))
                    }
                    AccountRequest::DeleteAccount { confirmation } => {
                        recent(&session)?;
                        if confirmation!=account(c,id)?.username { return Err(Failure::Invalid("Enter your username to confirm permanent deletion")); }
                        let owns: Vec<bool> = query(c,"SELECT to_jsonb(EXISTS(SELECT 1 FROM instance WHERE owner_account_id=$1::uuid) OR EXISTS(SELECT 1 FROM guilds WHERE owner=$1::uuid)) AS data", &[&id.to_string()])?;
                        if owns == [true] { return Err(Failure::Invalid("Transfer instance ownership and transfer or delete owned guilds before deleting your account")); }
                        // Membership cascades also invalidate editor revisions. Lock in UUID order.
                        execute(c,"SELECT id FROM guilds WHERE id IN (SELECT guild_id FROM guild_members WHERE account_id=$1::uuid) ORDER BY id FOR UPDATE", &[&id.to_string()])?;
                        execute(c,"UPDATE guilds SET revision=revision+1 WHERE id IN (SELECT guild_id FROM guild_members WHERE account_id=$1::uuid)", &[&id.to_string()])?;
                        execute(c,"DELETE FROM accounts WHERE id=$1::uuid", &[&id.to_string()])?;
                        Ok(done("Account and credentials permanently deleted"))
                    }
                    _ => Err(Failure::Invalid("Unsupported operation")),
                }
            })
        }
    }
}
fn set_password(c: &mut PgConnection, id: AccountId, hash: &str) -> Result<(), Failure> {
    execute(
        c,
        "INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,$2) ON CONFLICT(account_id,provider) DO UPDATE SET password_hash=EXCLUDED.password_hash",
        &[&id.to_string(), hash],
    )?;
    execute(
        c,
        "DELETE FROM account_codes WHERE account_id=$1::uuid AND purpose='reset'",
        &[&id.to_string()],
    )?;
    Ok(())
}
fn revoke_all(c: &mut PgConnection, id: AccountId) -> Result<(), Failure> {
    execute(
        c,
        "UPDATE sessions SET revoked=TRUE WHERE account_id=$1::uuid",
        &[&id.to_string()],
    )?;
    execute(
        c,
        "DELETE FROM oauth_attempts WHERE account_id=$1::uuid",
        &[&id.to_string()],
    )?;
    Ok(())
}
fn consume_code(
    c: &mut PgConnection,
    code: &str,
    hash: Option<String>,
) -> Result<AccountResponse, Failure> {
    c.transaction(|c| {
        let purpose = if hash.is_some() {"reset"} else {"verify"};
        let ids: Vec<AccountId> = query(c,"SELECT to_jsonb(a.id) AS data FROM accounts a JOIN account_codes t ON t.account_id=a.id WHERE t.token_hash=$1 AND t.purpose=$2 AND t.expires_at>now() FOR UPDATE OF a", &[&digest(code),purpose])?;
        let id = *ids.first().ok_or(Failure::Unauthorized)?;
        if execute(c,"DELETE FROM account_codes WHERE token_hash=$1 AND expires_at>now()", &[&digest(code)])?!=1 { return Err(Failure::Unauthorized); }
        if let Some(hash) = hash {
            set_password(c,id,&hash)?; revoke_all(c,id)?;
            Ok(done("Password reset. Sign in again"))
        } else {
            execute(c,"UPDATE accounts SET email_verified=TRUE WHERE id=$1::uuid", &[&id.to_string()])?;
            Ok(done("Email verified"))
        }
    })
}
