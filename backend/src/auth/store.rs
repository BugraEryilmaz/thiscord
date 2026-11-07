use super::Failure;
use crate::access::{self, Scope};
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
    let _timing = crate::performance::QUERY.start();
    Prepared { sql, params }
        .load::<Row>(c)?
        .into_iter()
        .map(|r| serde_json::from_value(r.data).map_err(|_| Failure::Unavailable))
        .collect()
}
pub(crate) fn execute(c: &mut PgConnection, sql: &str, params: &[&str]) -> Result<usize, Failure> {
    let _timing = crate::performance::EXECUTE.start();
    Ok(Prepared { sql, params }.execute(c)?)
}

/// Fixed SQL whose `$n` parameters become Diesel text binds in order of use.
/// Unlike `sql_query`, Diesel may keep it as a prepared statement per pooled
/// connection, keyed by its text, so repeated queries skip parsing and, after
/// PostgreSQL's custom-plan trials, planning. That cache stays bounded because
/// only the fixed set of SQL strings in this crate reaches these helpers.
struct Prepared<'a> {
    sql: &'a str,
    params: &'a [&'a str],
}
impl diesel::query_builder::QueryId for Prepared<'_> {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}
impl diesel::query_builder::Query for Prepared<'_> {
    type SqlType = diesel::sql_types::Untyped;
}
impl RunQueryDsl<PgConnection> for Prepared<'_> {}
impl diesel::query_builder::QueryFragment<Pg> for Prepared<'_> {
    fn walk_ast<'b>(
        &'b self,
        mut out: diesel::query_builder::AstPass<'_, 'b, Pg>,
    ) -> QueryResult<()> {
        let mut rest = self.sql;
        while let Some(at) = rest.find('$') {
            let digits = rest[at + 1..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            if digits == 0 {
                out.push_sql(&rest[..=at]);
                rest = &rest[at + 1..];
                continue;
            }
            out.push_sql(&rest[..at]);
            let param = rest[at + 1..at + 1 + digits]
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| self.params.get(n))
                .ok_or_else(|| {
                    diesel::result::Error::QueryBuilderError("missing SQL parameter".into())
                })?;
            out.push_bind_param::<Text, str>(param)?;
            rest = &rest[at + 1 + digits..];
        }
        out.push_sql(rest);
        Ok(())
    }
}
pub(crate) fn secret() -> String {
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
    let _timing = crate::performance::POOL.start();
    pool.get().map_err(|_| {
        crate::performance::POOL_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Failure::Unavailable
    })
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
    if value.len() > 254
        || !value.is_ascii()
        || value.ends_with("@bots.invalid")
        || value.parse::<lettre::Address>().is_err()
    {
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
    query(c, "SELECT to_jsonb(a) || jsonb_build_object('avatar_id', (SELECT id FROM account_avatars WHERE account_id=a.id), 'identities', (SELECT jsonb_agg(provider ORDER BY provider) FROM identities WHERE account_id=a.id), 'custom_status', (SELECT jsonb_strip_nulls(jsonb_build_object('emoji',s.emoji,'text',s.status_text,'expires_at',s.expires_at)) FROM account_custom_statuses s WHERE s.account_id=a.id AND (s.expires_at IS NULL OR s.expires_at>now())), 'pending_email', (SELECT email FROM account_codes WHERE account_id=a.id AND purpose='email' AND expires_at>now())) AS data FROM accounts a WHERE id=$1::uuid", &[&id.to_string()])?.pop().ok_or(Failure::Unauthorized)
}
fn username(value: &str) -> Result<String, Failure> {
    normalize_username(value).map_err(Failure::Invalid)
}
fn taken(c: &mut PgConnection, column: &str, value: &str, id: AccountId) -> Result<bool, Failure> {
    let sql = if column == "email" {
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM accounts WHERE email=$1 AND id<>$2::uuid)) AS data"
    } else {
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM accounts WHERE username=$1 AND id<>$2::uuid)) AS data"
    };
    Ok(query::<bool>(c, sql, &[value, &id.to_string()])? == [true])
}
/// Starts (or replaces) a verified account's email change: the address takes
/// effect only once the link emailed to it is opened. Earlier links stop working.
fn enqueue_email_change(c: &mut PgConnection, a: &Account, address: &str) -> Result<(), Failure> {
    let code = secret();
    let link = super::verification::link(&code)?;
    let body = format!(
        "Thiscord confirm your new email (valid for 30 minutes):\n\n{link}\n\nClick the link to make this the email address of your Thiscord account @{}. Until then, your current address stays in use. If you did not request this, ignore this email.",
        a.username
    );
    // Withdraw unsent mail for an earlier pending address.
    execute(
        c,
        "DELETE FROM mail_outbox WHERE account_id=$1::uuid AND recipient IN (SELECT email FROM account_codes WHERE account_id=$1::uuid AND purpose='email')",
        &[&a.id.to_string()],
    )?;
    execute(
        c,
        "DELETE FROM account_codes WHERE account_id=$1::uuid AND purpose='email'",
        &[&a.id.to_string()],
    )?;
    execute(
        c,
        "INSERT INTO account_codes(token_hash,account_id,purpose,email) VALUES($1,$2::uuid,'email',$3)",
        &[&digest(&code), &a.id.to_string(), address],
    )?;
    execute(
        c,
        "INSERT INTO mail_outbox(account_id,recipient,body) VALUES($1::uuid,$2,$3)",
        &[&a.id.to_string(), address, &body],
    )?;
    Ok(())
}
/// Replaces (or with `None`, clears) the account's custom status. The caller
/// holds the account/session lock; expiry uses the database clock.
fn set_custom_status(
    c: &mut PgConnection,
    id: AccountId,
    status: Option<&CustomStatusUpdate>,
) -> Result<(), Failure> {
    let Some(status) = status else {
        execute(
            c,
            "DELETE FROM account_custom_statuses WHERE account_id=$1::uuid",
            &[&id.to_string()],
        )?;
        return Ok(());
    };
    let text = status.validate().map_err(Failure::Invalid)?;
    execute(
        c,
        "INSERT INTO account_custom_statuses(account_id,emoji,status_text,expires_at,updated_at) VALUES($1::uuid,NULLIF($2,''),$3,CASE WHEN $4='' THEN NULL ELSE now()+make_interval(secs=>$4::integer) END,now()) ON CONFLICT(account_id) DO UPDATE SET emoji=EXCLUDED.emoji,status_text=EXCLUDED.status_text,expires_at=EXCLUDED.expires_at,updated_at=EXCLUDED.updated_at",
        &[
            &id.to_string(),
            status.emoji.as_deref().unwrap_or_default(),
            text,
            &status.expires_in.map(|s| s.to_string()).unwrap_or_default(),
        ],
    )?;
    Ok(())
}
#[derive(Deserialize, Clone)]
pub(crate) struct Session {
    pub id: SessionId,
    pub account_id: AccountId,
    pub reauthenticated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub bot_guild: Option<thiscord_shared::GuildId>,
}

impl Session {
    pub(crate) fn require_guild(&self, guild: thiscord_shared::GuildId) -> Result<(), Failure> {
        if self.bot_guild.is_some_and(|installed| installed != guild) {
            Err(Failure::Forbidden)
        } else {
            Ok(())
        }
    }
}

/// Only chat, voice and dedicated bot ingress may accept nonhuman credentials.
pub(crate) fn authenticate_actor(c: &mut PgConnection, token: &str) -> Result<Session, Failure> {
    if token.starts_with("bot_") {
        crate::bots::authenticate(c, token)
    } else {
        authenticate(c, token)
    }
}

pub(crate) fn authenticate_actor_retained(
    c: &mut PgConnection,
    token: &str,
) -> Result<Session, Failure> {
    if token.starts_with("bot_") {
        crate::bots::authenticate(c, token)
    } else {
        authenticate_retained(c, token)
    }
}

/// Resolve a barrier key only; callers must still authenticate after capturing its epoch.
pub(crate) fn token_session(c: &mut PgConnection, token: &str) -> Result<Session, Failure> {
    if token.len() != 43 {
        return Err(Failure::Unauthorized);
    }
    query(c, "SELECT to_jsonb(s) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id JOIN accounts a ON a.id=s.account_id WHERE t.token_hash=$1 AND NOT a.is_bot", &[&digest(token)])?
        .pop().ok_or(Failure::Unauthorized)
}

pub(crate) fn authenticate(c: &mut PgConnection, token: &str) -> Result<Session, Failure> {
    authenticate_token(c, token, true)
}

/// Recheck a credential already accepted at connection ingress. Rotation must
/// reject its retained old token, without treating our own background work as
/// an external replay and revoking the replacement token. New HTTP/socket
/// credentials must still use `authenticate` (including voice join/resume).
pub(crate) fn authenticate_retained(c: &mut PgConnection, token: &str) -> Result<Session, Failure> {
    authenticate_token(c, token, false)
}

fn authenticate_token(
    c: &mut PgConnection,
    token: &str,
    revoke_replay: bool,
) -> Result<Session, Failure> {
    if token.len() != 43 {
        return Err(Failure::Unauthorized);
    }
    let hash = digest(token);
    // Ordinary checks neither lock exclusively nor write activity. Actions recheck
    // under read_session/lock_session; replay and activity refresh take the slow path.
    if let Some(session) = query(c,
        "SELECT to_jsonb(s) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id JOIN accounts a ON a.id=s.account_id WHERE t.token_hash=$1 AND NOT a.is_bot AND t.active AND NOT s.revoked AND s.expires_at>now() AND s.last_seen_at>now()-interval '5 minutes'",
        &[&hash],
    )?.pop() {
        return Ok(session);
    }
    // Commit replay revocation even though authentication itself fails.
    let mut revocation = None;
    let session=c.transaction::<_, Failure, _>(|c| {
        let sessions: Vec<Session> = query(c, "SELECT to_jsonb(s) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id JOIN accounts a ON a.id=s.account_id WHERE t.token_hash=$1 AND NOT a.is_bot AND NOT s.revoked AND s.expires_at>now() AND s.last_seen_at>now()-interval '7 days' FOR UPDATE OF s", &[&hash])?;
        let Some(session) = sessions.into_iter().next() else { return Ok(None); };
        let active: Vec<bool> = query(c, "SELECT to_jsonb(active) AS data FROM session_tokens WHERE token_hash=$1", &[&hash])?;
        if active != [true] {
            if revoke_replay {
                execute(c, "UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid", &[&session.id.to_string()])?;
                revocation = Some(access::pause(Scope::Session(session.id)));
            }
            return Ok(None);
        }
        execute(c, "UPDATE sessions SET last_seen_at=now() WHERE id=$1::uuid AND last_seen_at<=now()-interval '5 minutes'", &[&session.id.to_string()])?;
        Ok(Some(session))
    })?;
    if let Some(change) = revocation {
        change.finish(true);
    }
    session.ok_or(Failure::Unauthorized)
}
pub(crate) fn lock_session(
    c: &mut PgConnection,
    token: &str,
    session: &Session,
) -> Result<(), Failure> {
    // Serialize account operations, while allowing foreign-key KEY SHARE locks.
    // A guild moderator may reference another account while that account waits
    // for this guild. FOR UPDATE here would invert their lock order and deadlock.
    // Deletion takes its stronger row lock only after locking affected guilds.
    execute(
        c,
        "SELECT id FROM accounts WHERE id=$1::uuid FOR NO KEY UPDATE",
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
    Ok(())
}
/// Compatible readers, ordered account -> session like account mutations. Must
/// remain inside the action transaction; never upgrade these locks to write locks.
pub(crate) fn read_session(
    c: &mut PgConnection,
    token: &str,
    session: &Session,
) -> Result<(), Failure> {
    execute(
        c,
        "SELECT id FROM accounts WHERE id=$1::uuid FOR SHARE",
        &[&session.account_id.to_string()],
    )?;
    // Lock the session first, then read the token with a fresh READ COMMITTED
    // snapshot so a rotation that we waited for cannot leave an old active token.
    let valid: Vec<bool> = query(
        c,
        "SELECT to_jsonb(TRUE) AS data FROM sessions WHERE id=$1::uuid AND account_id=$2::uuid AND NOT revoked AND expires_at>now() AND (last_seen_at>now()-interval '7 days' OR $3::boolean) FOR SHARE",
        &[
            &session.id.to_string(),
            &session.account_id.to_string(),
            &session.bot_guild.is_some().to_string(),
        ],
    )?;
    if valid.is_empty()
        || query::<bool>(
            c,
            "SELECT to_jsonb(active) AS data FROM session_tokens WHERE session_id=$1::uuid AND token_hash=$2",
            &[&session.id.to_string(), &digest(token)],
        )? != [true]
    {
        return Err(Failure::Unauthorized);
    }
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
    mutations: &mut Vec<access::Mutation>,
) -> Result<AccountResponse, Failure> {
    let device_name = device(device_name)?;
    // The account lock serializes eviction selection with session grants.
    let evicted: Vec<SessionId> = query(
        c,
        "SELECT to_jsonb(id) AS data FROM sessions WHERE account_id=$1::uuid AND NOT revoked ORDER BY created_at DESC OFFSET 19",
        &[&id.to_string()],
    )?;
    // Revoke exactly the selected rows (including ties in created_at), each
    // under its own drain before commit. Re-running OFFSET could choose other rows.
    for session in &evicted {
        execute(
            c,
            "UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid",
            &[&session.to_string()],
        )?;
    }
    let sid = SessionId::from_uuid(Uuid::new_v4());
    execute(
        c,
        "INSERT INTO sessions(id,account_id,device) VALUES($1::uuid,$2::uuid,$3)",
        &[&sid.to_string(), &id.to_string(), device_name],
    )?;
    let response = session_grant(c, sid, id)?;
    mutations.extend(
        evicted
            .into_iter()
            .map(|id| access::pause(Scope::Session(id))),
    );
    Ok(response)
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
    // One transaction: one commit (and WAL flush) per limited request. Every
    // counter increment commits, including on a limited request, as before.
    let limited = c.transaction(|c| {
        execute(
            c,
            "DELETE FROM auth_limits WHERE window_start < now()-interval '2 minutes'",
            &[],
        )?;
        let mut limited = false;
        for (key, max) in keys {
            let counts: Vec<i32> = query(
                c,
                "INSERT INTO auth_limits(key) VALUES($1) ON CONFLICT(key) DO UPDATE SET attempts=CASE WHEN auth_limits.window_start<now()-interval '1 minute' THEN 1 ELSE auth_limits.attempts+1 END, window_start=CASE WHEN auth_limits.window_start<now()-interval '1 minute' THEN now() ELSE auth_limits.window_start END RETURNING to_jsonb(attempts) AS data",
                &[&digest(&key)],
            )?;
            if counts[0] > max {
                limited = true;
                break;
            }
        }
        Ok::<_, Failure>(limited)
    })?;
    if limited {
        return Err(Failure::Limited);
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
        let link = super::verification::account_link(PASSWORD_RESET_PATH, &code)?;
        format!(
            "Thiscord reset password (valid for 30 minutes):\n\n{link}\n\nClick the link to choose a new password in your browser.\n\nBackup reset code:\n\n{code}\n\nIf the link does not work, open Forgot password in Thiscord and select I already have a reset code. The link and code are single-use; using either invalidates both. If you did not request this, ignore this email."
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
            let username = self::username(&username)?;
            let address = email(&address)?;
            device(&name)?;
            let hash = password_hash(&password)?;
            let mut mutations = Vec::new();
            let result = c.transaction(|c| {
                let id = AccountId::from_uuid(Uuid::new_v4());
                execute(c,"INSERT INTO accounts(id,username,email,display_name) VALUES($1::uuid,$2,$3,$2)", &[&id.to_string(), &username, &address])?;
                execute(c,"INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,$2)", &[&id.to_string(), &hash])?;
                let a = account(c,id)?;
                enqueue_code(c, &a, "verify")?;
                grant(c,id,&name,&mut mutations)
            });
            access::complete(result, &mut mutations)
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
            // A successful login can evict the oldest device session.
            let mut mutations = Vec::new();
            let result = c.transaction(|c| {
                execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE", &[&credential.account_id.to_string()])?;
                let unchanged: Vec<bool> = query(c,"SELECT to_jsonb(TRUE) AS data FROM identities WHERE account_id=$1::uuid AND provider='password' AND password_hash=$2", &[&credential.account_id.to_string(), &credential.password_hash])?;
                if unchanged.is_empty() { return Err(Failure::Unauthorized); }
                grant(c,credential.account_id,&name,&mut mutations)
            });
            access::complete(result, &mut mutations)
        }
        AccountRequest::ForgotPassword { email: address } => {
            let address = email(&address)?;
            c.transaction(|c| {
                let ids: Vec<AccountId> = query(c,"SELECT to_jsonb(id) AS data FROM accounts WHERE email=$1 AND email_verified FOR UPDATE", &[&address])?;
                if let Some(id) = ids.first() { let a = account(c,*id)?; enqueue_code(c,&a,"reset")?; }
                Ok(done("If a verified account exists, we’ll email a link to reset your password in your browser, plus a backup code"))
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
            let mut scope = match &command {
                AccountRequest::Logout | AccountRequest::Rotate => Some(Scope::Session(session.id)),
                AccountRequest::RevokeSession { session_id } => Some(Scope::Session(*session_id)),
                AccountRequest::LogoutAll
                | AccountRequest::DeleteAccount { .. }
                | AccountRequest::ChangePassword { .. }
                | AccountRequest::UnlinkIdentity { .. } => Some(Scope::Account(session.account_id)),
                _ => None,
            };
            let mut changes = Vec::new();
            // Friends list profiles; woken after commit.
            let mut related = Vec::new();
            // Guild member lists showing this profile; woken after commit.
            let mut guilds = Vec::new();
            let result = c.transaction(|c| {
                let result = (|| {
                if matches!(command, AccountRequest::Current | AccountRequest::Sessions) {
                    read_session(c,token,&session)?;
                } else {
                    lock_session(c,token,&session)?;
                }
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
                    AccountRequest::RevokeSession { session_id } => { if execute(c,"UPDATE sessions SET revoked=TRUE WHERE id=$1::uuid AND account_id=$2::uuid", &[&session_id.to_string(), &id.to_string()])? == 0 { scope = None; } Ok(done("Session revoked")) },
                    AccountRequest::SendVerification => {
                        let a = account(c,id)?;
                        if !a.email_verified { enqueue_code(c,&a,"verify")?; }
                        // A verified account resends its pending change, only to that address.
                        else if let Some(address) = a.pending_email.clone() { enqueue_email_change(c,&a,&address)?; }
                        Ok(done("Verification email queued"))
                    },
                    AccountRequest::ChangeEmail { email: address } => {
                        recent(&session)?;
                        let address = email(&address)?;
                        let current = account(c,id)?;
                        if current.email == address { return Err(Failure::Invalid("That is already your email address")); }
                        // Like registration, this reveals whether an address is taken
                        // (bounded by the per-address rate limit); a race still fails as 409.
                        if taken(c,"email",&address,id)? { return Err(Failure::Invalid("That email address is already in use")); }
                        if current.email_verified {
                            // The verified address stays until the new one is confirmed.
                            enqueue_email_change(c,&current,&address)?;
                            return Ok(AccountResponse::Account { account: account(c,id)? });
                        }
                        execute(c,"UPDATE accounts SET email=$2 WHERE id=$1::uuid", &[&id.to_string(), &address])?;
                        // Unsent mail to the mistyped address is withdrawn; replacing
                        // the code invalidates links already delivered there.
                        execute(c,"DELETE FROM mail_outbox WHERE account_id=$1::uuid AND recipient<>$2", &[&id.to_string(), &address])?;
                        let a = account(c,id)?;
                        enqueue_code(c,&a,"verify")?;
                        Ok(AccountResponse::Account { account: a })
                    }
                    AccountRequest::CancelEmailChange => {
                        execute(c,"DELETE FROM mail_outbox WHERE account_id=$1::uuid AND recipient IN (SELECT email FROM account_codes WHERE account_id=$1::uuid AND purpose='email')", &[&id.to_string()])?;
                        execute(c,"DELETE FROM account_codes WHERE account_id=$1::uuid AND purpose='email'", &[&id.to_string()])?;
                        Ok(AccountResponse::Account { account: account(c,id)? })
                    }
                    AccountRequest::ChangeUsername { username: value } => {
                        recent(&session)?;
                        let value = username(&value)?;
                        if account(c,id)?.username == value { return Err(Failure::Invalid("That is already your username")); }
                        // Reveals availability like registration, bounded by the
                        // per-username rate limit; a concurrent claim still fails as 409.
                        if taken(c,"username",&value,id)? { return Err(Failure::Invalid("That username is taken")); }
                        execute(c,"UPDATE accounts SET username=$2 WHERE id=$1::uuid", &[&id.to_string(), &value])?;
                        // Friends, member lists and this account's other devices show it.
                        related = crate::friends::store::related(c,id)?;
                        related.push(id);
                        guilds = crate::chat::members::guilds_of(c,id)?;
                        Ok(AccountResponse::Account { account: account(c,id)? })
                    }
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
                        related = crate::friends::store::related(c,id)?;
                        guilds = crate::chat::members::guilds_of(c,id)?;
                        Ok(AccountResponse::Account { account: account(c,id)? })
                    }
                    AccountRequest::SetAvatar { image_base64 } => {
                        super::avatar::set(c, id, image_base64.as_deref())?;
                        related = crate::friends::store::related(c,id)?;
                        guilds = crate::chat::members::guilds_of(c,id)?;
                        Ok(AccountResponse::Account { account: account(c,id)? })
                    }
                    AccountRequest::SetCustomStatus { status } => {
                        set_custom_status(c, id, status.as_ref())?;
                        // The account's own friends socket carries the status to its other devices.
                        related = crate::friends::store::related(c,id)?;
                        related.push(id);
                        guilds = crate::chat::members::guilds_of(c,id)?;
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
                        related = crate::friends::store::related(c,id)?;
                        guilds = crate::chat::members::guilds_of(c,id)?;
                        execute(c,"DELETE FROM accounts WHERE id=$1::uuid", &[&id.to_string()])?;
                        Ok(done("Account and credentials permanently deleted"))
                    }
                    _ => Err(Failure::Invalid("Unsupported operation")),
                }
                })();
                if result.is_ok() {
                    changes.extend(scope.into_iter().map(access::pause));
                }
                result
            });
            for change in changes {
                change.finish(result.is_ok());
            }
            if result.is_ok() {
                // Profile edits commit without an access change; voice reuses its
                // last check between rechecks, so wake this account's connections.
                if !related.is_empty() || !guilds.is_empty() {
                    crate::voice::profile_changed(session.account_id);
                }
                crate::friends::changed(related);
                crate::chat::members::changed(guilds);
            }
            result
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
pub(super) fn check_reset_code(pool: &DbPool, code: &str) -> Result<(), Failure> {
    let valid: Vec<bool> = query(
        &mut *connection(pool)?,
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM account_codes WHERE token_hash=$1 AND purpose='reset' AND expires_at>now())) AS data",
        &[&digest(code)],
    )?;
    if valid == [true] {
        Ok(())
    } else {
        Err(Failure::Unauthorized)
    }
}

fn consume_code(
    c: &mut PgConnection,
    code: &str,
    hash: Option<String>,
) -> Result<AccountResponse, Failure> {
    let mut change = None;
    let result = c.transaction(|c| {
        #[derive(Deserialize)]
        struct Code {
            id: AccountId,
            #[serde(default)]
            email: Option<String>,
        }
        // Verification links also confirm a verified account's email change.
        let (first, second) = if hash.is_some() {("reset","reset")} else {("verify","email")};
        let codes: Vec<Code> = query(c,"SELECT jsonb_build_object('id',a.id,'email',t.email) AS data FROM accounts a JOIN account_codes t ON t.account_id=a.id WHERE t.token_hash=$1 AND t.purpose IN ($2,$3) AND t.expires_at>now() FOR UPDATE OF a", &[&digest(code),first,second])?;
        let Code { id, email: changed } = codes.into_iter().next().ok_or(Failure::Unauthorized)?;
        if execute(c,"DELETE FROM account_codes WHERE token_hash=$1 AND expires_at>now()", &[&digest(code)])?!=1 { return Err(Failure::Unauthorized); }
        if let Some(address) = changed {
            if taken(c,"email",&address,id)? { return Err(Failure::Invalid("That email address is already in use")); }
            let previous = account(c,id)?;
            execute(c,"UPDATE accounts SET email=$2, email_verified=TRUE WHERE id=$1::uuid", &[&id.to_string(), &address])?;
            // Reset links went to the previous address; they stop working.
            execute(c,"DELETE FROM account_codes WHERE account_id=$1::uuid AND purpose='reset'", &[&id.to_string()])?;
            execute(c,"INSERT INTO mail_outbox(account_id,recipient,body) VALUES($1::uuid,$2,$3)", &[&id.to_string(), &previous.email, &format!(
                "Thiscord email address changed\n\nThe email address of your Thiscord account @{} was changed, and this address is no longer used to sign in or recover it.\n\nIf you did not make this change, contact your Thiscord administrator right away.",
                previous.username
            )])?;
            return Ok(done("Email changed"));
        }
        if let Some(hash) = hash {
            set_password(c,id,&hash)?; revoke_all(c,id)?;
            change = Some(access::pause(Scope::Account(id)));
            Ok(done("Password reset. Sign in again"))
        } else {
            execute(c,"UPDATE accounts SET email_verified=TRUE WHERE id=$1::uuid", &[&id.to_string()])?;
            Ok(done("Email verified"))
        }
    });
    if let Some(change) = change {
        change.finish(result.is_ok());
    }
    result
}

#[cfg(test)]
mod prepared_tests {
    use super::*;
    use diesel::query_builder::QueryFragment;

    #[test]
    fn parameters_become_ordered_binds_including_repeats() {
        let params = ["a", "b"];
        let query = Prepared {
            sql: "SELECT $2::text, $1, $2 WHERE note='$'",
            params: &params,
        };
        let text = diesel::debug_query::<Pg, _>(&query).to_string();
        assert!(
            text.starts_with("SELECT $1::text, $2, $3 WHERE note='$'"),
            "{text}"
        );
        assert!(text.contains(r#"["b", "a", "b"]"#), "{text}");
        let missing = Prepared {
            sql: "SELECT $3",
            params: &params,
        };
        let mut builder = diesel::pg::PgQueryBuilder::default();
        assert!(missing.to_sql(&mut builder, &Pg).is_err());
    }
}
