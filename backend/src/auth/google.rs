use super::{Failure, cookie, store::*};
use crate::db::DbPool;
use axum::{
    Extension,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use diesel::prelude::*;
use openidconnect::{
    AccessToken, AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl,
    Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
    core::{
        CoreAuthenticationFlow, CoreClient, CoreIdToken, CoreIdTokenVerifier, CoreProviderMetadata,
    },
};
use serde::Deserialize;
use std::time::Duration;
use thiscord_shared::{AccountId, RequestId, SessionId, account::*};

fn config() -> Result<(String, String, String), Failure> {
    let get = |name: &'static str| {
        std::env::var(name)
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(Failure::Configuration(name))
    };
    let redirect = get("GOOGLE_REDIRECT_URL")?;
    let url =
        url::Url::parse(&redirect).map_err(|_| Failure::Configuration("GOOGLE_REDIRECT_URL"))?;
    if url.scheme() != "https"
        && !(url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
    {
        return Err(Failure::Configuration("GOOGLE_REDIRECT_URL"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != GOOGLE_CALLBACK_PATH
    {
        return Err(Failure::Configuration("GOOGLE_REDIRECT_URL"));
    }
    Ok((
        get("GOOGLE_CLIENT_ID")?,
        get("GOOGLE_CLIENT_SECRET")?,
        redirect,
    ))
}
fn http() -> Result<reqwest::Client, Failure> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| Failure::Unavailable)
}
pub(super) fn valid_callback(callback: &str) -> bool {
    url::Url::parse(callback).ok().is_some_and(|u| {
        u.scheme() == "http"
            && u.host_str() == Some("127.0.0.1")
            && u.port().is_some_and(|p| p >= 1024)
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && u.path().strip_prefix("/thiscord/").is_some_and(|s| {
                s.len() == 43
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
    })
}

pub(super) async fn start(
    pool: DbPool,
    token: String,
    purpose: GooglePurpose,
    callback: Option<String>,
    name: String,
    permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(AccountResponse, Option<String>), Failure> {
    device(&name)?;
    if callback.as_ref().is_some_and(|c| !valid_callback(c)) {
        return Err(Failure::Invalid("Invalid desktop callback"));
    }
    let (client_id, client_secret, redirect) = config()?;
    let client_http = http()?;
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new("https://accounts.google.com".into()).unwrap(),
        &client_http,
    )
    .await
    .map_err(|_| Failure::Unavailable)?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
    )
    .set_redirect_uri(RedirectUrl::new(redirect).map_err(|_| Failure::Unavailable)?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (authorization_url, state, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".into()))
        .set_pkce_challenge(challenge)
        .add_extra_param("prompt", "select_account")
        .add_extra_param("max_age", "0")
        .url();
    let state_hash = digest(state.secret());
    let (ticket, browser) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        record(
            &mut *connection(&pool)?,
            &token,
            purpose,
            callback.as_deref(),
            &name,
            [&state_hash, verifier.secret(), nonce.secret()],
        )
    })
    .await
    .map_err(|_| Failure::Unavailable)??;
    Ok((
        AccountResponse::GoogleStarted {
            authorization_url: authorization_url.to_string(),
            ticket,
        },
        browser,
    ))
}

/// Records a new attempt. Returns its ticket and, for a browser attempt (no
/// loopback `callback`), the attempt-cookie secret binding it to the caller's
/// browser. A loopback attempt has no completion secret yet: only a successful
/// callback creates one, so whoever called `start` (possibly an attacker who
/// forwards the Google link to a victim) never receives it.
fn record(
    c: &mut PgConnection,
    token: &str,
    purpose: GooglePurpose,
    callback: Option<&str>,
    name: &str,
    [state_hash, verifier, nonce]: [&str; 3],
) -> Result<(String, Option<String>), Failure> {
    let session = if purpose != GooglePurpose::Login {
        Some(authenticate(c, token)?)
    } else {
        None
    };
    let ticket = secret();
    let browser = callback.is_none().then(secret);
    c.transaction::<_,Failure,_>(|c| {
        if let Some(s) = &session { lock_session(c,token,s)?; if purpose==GooglePurpose::Link { recent(s)?; } }
        execute(c,"DELETE FROM oauth_attempts WHERE expires_at<now()", &[])?;
        execute(c,"INSERT INTO oauth_attempts(state_hash,ticket_hash,verifier,nonce,purpose,session_id,account_id,device,callback,browser_hash) VALUES($1,$2,$3,$4,$5,NULLIF($6,'')::uuid,NULLIF($7,'')::uuid,$8,NULLIF($9,''),NULLIF($10,''))", &[state_hash,&digest(&ticket),verifier,nonce,match purpose {GooglePurpose::Login=>"login",GooglePurpose::Link=>"link",GooglePurpose::Reauthenticate=>"reauthenticate"},&session.as_ref().map(|s|s.id.to_string()).unwrap_or_default(),&session.as_ref().map(|s|s.account_id.to_string()).unwrap_or_default(),name,callback.unwrap_or(""),&browser.as_deref().map(digest).unwrap_or_default()])?;
        Ok(())
    })?;
    Ok((ticket, browser))
}

#[derive(Deserialize)]
pub(super) struct Attempt {
    state_hash: String,
    verifier: String,
    nonce: String,
    purpose: String,
    account_id: Option<AccountId>,
    session_id: Option<SessionId>,
    device: String,
    callback: Option<String>,
    completion_hash: Option<String>,
    subject: Option<String>,
    email: Option<String>,
    status: String,
}
#[derive(Deserialize)]
pub(super) struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}
/// What the provider exchange needs from a claimed attempt.
struct ProviderCheck {
    verifier: String,
    nonce: String,
    reauthenticate: bool,
}
/// Plain-text page for a browser callback without the attempt's own cookie.
const OTHER_BROWSER: &str = "This Google sign-in was not started in this browser, so it was stopped and nothing was signed in. If you started it, try again from Thiscord in this same browser. You can close this tab.";
/// Desktop clients released before completion secrets existed cannot send one.
/// Only they ever see this, so it is a message their bundled catalogs (0.1.63
/// and earlier) already translate ("Önce güncellemeleri kontrol et").
pub(super) const UPDATE_REQUIRED: &str = "Check for an update first";
const FAILED: &str = "Google sign-in failed. If the email already has an account, sign in there first and link Google";

pub(super) async fn callback(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    query: Result<Query<Callback>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let input = query.map(|Query(q)| q).map_err(|_| Failure::Unauthorized);
    finish_callback(pool, id, &headers, input, verify_provider).await
}

/// The callback, with the provider exchange as a parameter so tests can drive it
/// with the offline test provider.
async fn finish_callback<F, Fut>(
    pool: Option<DbPool>,
    id: RequestId,
    headers: &HeaderMap,
    input: Result<Callback, Failure>,
    provider: F,
) -> Response
where
    F: FnOnce(String, ProviderCheck) -> Fut,
    Fut: std::future::Future<Output = Result<GoogleIdentity, Failure>>,
{
    let browser = cookie::google_attempt(headers)
        .map(|b| digest(&b))
        .unwrap_or_default();
    let result = async {
        let input = input?;
        if input.state.len()>128 || input.code.as_ref().is_some_and(|s|s.len()>4096) { return Err(Failure::Unauthorized); }
        let pool = pool.ok_or(Failure::Unavailable)?;
        let p = pool.clone();
        let hash = digest(&input.state);
        let claimed = tokio::task::spawn_blocking(move || claim(&mut *connection(&p)?, &hash, &browser)).await.map_err(|_| Failure::Unavailable)??;
        let Some(attempt) = claimed else {
            return Ok((StatusCode::FORBIDDEN, OTHER_BROWSER).into_response());
        };
        let check = ProviderCheck { verifier: attempt.verifier.clone(), nonce: attempt.nonce.clone(), reauthenticate: attempt.purpose == "reauthenticate" };
        let verified = if input.error.is_some() { Err(Failure::Unauthorized) } else {
            provider(input.code.unwrap_or_default(), check).await
        };
        // A loopback attempt's completion secret is created only now, after Google's
        // response verified, and travels only to that loopback.
        let completion = (verified.is_ok() && attempt.callback.is_some()).then(secret);
        let completion_hash = completion.as_deref().map(digest);
        let p = pool.clone();
        let hash = attempt.state_hash.clone();
        let finished = tokio::task::spawn_blocking(move || ready(&mut *connection(&p)?,&hash,verified,completion_hash.as_deref())).await.map_err(|_| Failure::Unavailable)?;
        if finished.is_err() {
            let hash = attempt.state_hash.clone();
            tokio::task::spawn_blocking(move || execute(&mut *connection(&pool)?,"UPDATE oauth_attempts SET status='failed',verifier='',nonce='' WHERE state_hash=$1", &[&hash])).await.map_err(|_| Failure::Unavailable)??;
        }
        let completion = completion.filter(|_| finished.is_ok());
        Ok(match attempt.callback {
            // The loopback is notified either way but receives the one-time completion
            // secret only on success; never a session or provider token.
            Some(callback) => {
                let mut response = match completion {
                    Some(completion) => Redirect::to(&format!("{callback}?{GOOGLE_COMPLETION_PARAM}={completion}")),
                    None => Redirect::to(&callback),
                }.into_response();
                if let Some(location) = response.headers_mut().get_mut(header::LOCATION) { location.set_sensitive(true); }
                response
            }
            // This browser presented the attempt's own cookie, which is now consumed.
            None => ([(header::SET_COOKIE, cookie::clear_google_attempt())], "Google sign-in finished. Return to Thiscord to see the result. You can close this tab.").into_response(),
        })
    }.await;
    let mut response = result.unwrap_or_else(|e: Failure| e.response(id));
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}
/// Claims a pending attempt for this callback request. A browser attempt is
/// claimed only with its own attempt cookie; a request from any other browser
/// fails it and returns `None`. Loopback attempts are bound later, by the
/// completion secret.
fn claim(
    c: &mut PgConnection,
    state_hash: &str,
    browser_hash: &str,
) -> Result<Option<Attempt>, Failure> {
    let claimed: Option<Attempt> = query(c,"UPDATE oauth_attempts SET status='processing' WHERE state_hash=$1 AND status='pending' AND expires_at>now() AND (callback IS NOT NULL OR browser_hash=$2) RETURNING to_jsonb(oauth_attempts) AS data", &[state_hash,browser_hash])?.pop();
    if claimed.is_some() {
        return Ok(claimed);
    }
    match execute(
        c,
        "UPDATE oauth_attempts SET status='failed',verifier='',nonce='' WHERE state_hash=$1 AND status='pending' AND expires_at>now() AND callback IS NULL",
        &[state_hash],
    )? {
        0 => Err(Failure::Unauthorized),
        _ => Ok(None),
    }
}
fn query_attempt(c: &mut PgConnection, sql: &str, hash: &str) -> Result<Attempt, Failure> {
    query(c, sql, &[hash])?.pop().ok_or(Failure::Unauthorized)
}
struct GoogleIdentity {
    subject: String,
    email: String,
}
async fn verify_provider(code: String, attempt: ProviderCheck) -> Result<GoogleIdentity, Failure> {
    let (id, secret, redirect) = config()?;
    let http = http()?;
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new("https://accounts.google.com".into()).unwrap(),
        &http,
    )
    .await
    .map_err(|_| Failure::Unavailable)?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(id),
        Some(ClientSecret::new(secret)),
    )
    .set_redirect_uri(RedirectUrl::new(redirect).map_err(|_| Failure::Unavailable)?);
    let tokens = client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|_| Failure::Unauthorized)?
        .set_pkce_verifier(PkceCodeVerifier::new(attempt.verifier.clone()))
        .request_async(&http)
        .await
        .map_err(|_| Failure::Unauthorized)?;
    let id_token = tokens.id_token().ok_or(Failure::Unauthorized)?;
    let verifier = client.id_token_verifier();
    verify_token(
        id_token,
        tokens.access_token(),
        &verifier,
        &attempt.nonce,
        attempt.reauthenticate,
    )
}

fn verify_token(
    id_token: &CoreIdToken,
    access_token: &AccessToken,
    verifier: &CoreIdTokenVerifier<'_>,
    expected_nonce: &str,
    reauthenticate: bool,
) -> Result<GoogleIdentity, Failure> {
    let nonce = Nonce::new(expected_nonce.to_owned());
    // Library verifies signature, issuer, audience, expiration and nonce against Google's JWKS.
    let claims = id_token
        .claims(verifier, &nonce)
        .map_err(|_| Failure::Unauthorized)?;
    if claims.email_verified() != Some(true) {
        return Err(Failure::Unauthorized);
    }
    if reauthenticate
        && !claims
            .auth_time()
            .is_some_and(|t| t > chrono::Utc::now() - chrono::Duration::minutes(5))
    {
        return Err(Failure::Unauthorized);
    }
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            access_token,
            id_token.signing_alg().map_err(|_| Failure::Unauthorized)?,
            id_token
                .signing_key(verifier)
                .map_err(|_| Failure::Unauthorized)?,
        )
        .map_err(|_| Failure::Unauthorized)?;
        if &actual != expected {
            return Err(Failure::Unauthorized);
        }
    }
    Ok(GoogleIdentity {
        subject: claims.subject().as_str().to_owned(),
        email: email(claims.email().ok_or(Failure::Unauthorized)?.as_str())?,
    })
}
/// Records the verified Google identity on the claimed attempt. Nothing is applied
/// yet: account creation, linking and reauthentication wait for a bound completion.
fn ready(
    c: &mut PgConnection,
    state_hash: &str,
    verified: Result<GoogleIdentity, Failure>,
    completion_hash: Option<&str>,
) -> Result<(), Failure> {
    let identity = verified?;
    // A login attempt remembers which account the identity belongs to now, so
    // account deletion discards it and completion detects a later change.
    match execute(
        c,
        "UPDATE oauth_attempts SET status='ready',subject=$2,email=$3,completion_hash=NULLIF($4,''),verifier='',nonce='',account_id=CASE WHEN purpose='login' THEN (SELECT account_id FROM identities WHERE provider='google' AND subject=$2) ELSE account_id END WHERE state_hash=$1 AND status='processing' AND expires_at>now()",
        &[
            state_hash,
            &identity.subject,
            &identity.email,
            completion_hash.unwrap_or(""),
        ],
    )? {
        1 => Ok(()),
        _ => Err(Failure::Unauthorized),
    }
}
/// Browser attempts were bound at the callback by their attempt cookie. A loopback
/// attempt also needs the completion secret that only its loopback received.
fn bound(attempt: &Attempt, completion: Option<&str>) -> Result<(), Failure> {
    if attempt.callback.is_none() {
        return Ok(());
    }
    let Some(completion) = completion else {
        return Err(Failure::Invalid(UPDATE_REQUIRED));
    };
    // Only the SHA-256 of a 256-bit secret is stored; comparing hashes reveals
    // nothing usable about the secret through timing.
    match &attempt.completion_hash {
        Some(expected) if completion.len() == 43 && digest(completion) == *expected => Ok(()),
        _ => Err(Failure::Unauthorized),
    }
}
/// Applies a ready attempt's identity operation and consumes the attempt.
fn apply(
    c: &mut PgConnection,
    a: &Attempt,
    mutations: &mut Vec<crate::access::Mutation>,
) -> Result<AccountResponse, Failure> {
    let (Some(subject), Some(email)) = (&a.subject, &a.email) else {
        return Err(Failure::Unauthorized);
    };
    execute(
        c,
        "DELETE FROM oauth_attempts WHERE state_hash=$1",
        &[&a.state_hash],
    )?;
    if let Some(sid) = a.session_id {
        let sessions: Vec<Session> = query(
            c,
            "SELECT to_jsonb(s) AS data FROM sessions s WHERE id=$1::uuid AND NOT revoked AND expires_at>now() AND last_seen_at>now()-interval '7 days' FOR UPDATE",
            &[&sid.to_string()],
        )?;
        let session = sessions.first().ok_or(Failure::Unauthorized)?;
        if a.purpose == "link" {
            recent(session)?;
        }
    }
    let existing: Option<AccountId> = query(c,"SELECT to_jsonb(account_id) AS data FROM identities WHERE provider='google' AND subject=$1", &[subject])?.pop();
    match a.purpose.as_str() {
        "login" => {
            // The account recorded at the callback (and locked by the caller).
            if existing != a.account_id {
                return Err(Failure::Conflict);
            }
            let id = if let Some(id) = existing {
                id
            } else {
                // Never merge by email, even if Google reports it verified.
                let id = AccountId::from_uuid(uuid::Uuid::new_v4());
                let username = format!("user_{}", &id.as_uuid().simple().to_string()[..20]);
                execute(
                    c,
                    "INSERT INTO accounts(id,username,email,email_verified,display_name) VALUES($1::uuid,$2,$3,TRUE,$2)",
                    &[&id.to_string(), &username, email],
                )?;
                execute(
                    c,
                    "INSERT INTO identities(account_id,provider,subject) VALUES($1::uuid,'google',$2)",
                    &[&id.to_string(), subject],
                )?;
                id
            };
            grant(c, id, &a.device, mutations)
        }
        "link" => {
            let id = a.account_id.ok_or(Failure::Unauthorized)?;
            if existing.is_some() {
                return Err(Failure::Conflict);
            }
            execute(
                c,
                "INSERT INTO identities(account_id,provider,subject) VALUES($1::uuid,'google',$2)",
                &[&id.to_string(), subject],
            )?;
            Ok(AccountResponse::Done {
                message: "Google identity operation completed".into(),
            })
        }
        "reauthenticate" => {
            let id = a.account_id.ok_or(Failure::Unauthorized)?;
            if existing != Some(id) {
                return Err(Failure::Unauthorized);
            }
            execute(
                c,
                "UPDATE sessions SET reauthenticated_at=now() WHERE id=$1::uuid",
                &[&a.session_id.ok_or(Failure::Unauthorized)?.to_string()],
            )?;
            Ok(AccountResponse::Done {
                message: "Google identity operation completed".into(),
            })
        }
        _ => Err(Failure::Unauthorized),
    }
}
/// Redeems a ticket. Missing or wrong binding fails without consuming the attempt,
/// so its legitimate holder can still complete it.
pub(super) fn complete(
    c: &mut PgConnection,
    ticket: &str,
    completion: Option<&str>,
) -> Result<AccountResponse, Failure> {
    if ticket.len() != 43 {
        return Err(Failure::Unauthorized);
    }
    let ticket_hash = digest(ticket);
    // Pending ticket polls do not affect voice. A completed login can evict a
    // device; keep its drain guard outside the closure so it survives commit.
    let mut mutations = Vec::new();
    let result = c.transaction(|c| {
        let initial = query_attempt(c,"SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE ticket_hash=$1 AND expires_at>now()", &ticket_hash)?;
        if initial.status=="pending" || initial.status=="processing" { return Ok(Ok(AccountResponse::Pending)); }
        if initial.status!="ready" { return Ok(Err(Failure::Invalid(FAILED))); }
        bound(&initial, completion)?;
        // Match the normal account-before-session lock order.
        if let Some(id) = initial.account_id { execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE", &[&id.to_string()])?; }
        let a = query_attempt(c,"SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE ticket_hash=$1 AND expires_at>now() FOR UPDATE", &ticket_hash)?;
        if a.status!="ready" || a.account_id!=initial.account_id { return Ok(Err(Failure::Invalid(FAILED))); }
        bound(&a, completion)?;
        // A savepoint: a refused identity operation rolls back alone and the
        // attempt is marked failed, as a failed callback would have marked it.
        match c.transaction(|c| apply(c, &a, &mut mutations)) {
            Ok(response) => Ok(Ok(response)),
            Err(_) => {
                execute(c,"UPDATE oauth_attempts SET status='failed' WHERE ticket_hash=$1", &[&ticket_hash])?;
                Ok(Err(Failure::Invalid(FAILED)))
            }
        }
    }).and_then(|outcome| outcome);
    crate::access::complete(result, &mut mutations)
}

#[cfg(test)]
mod tests {
    use super::*;

    use diesel::connection::SimpleConnection;
    struct Cleanup(PgConnection, String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self
                .0
                .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.1));
        }
    }
    /// A migrated pool in a random schema of the test database, dropped afterwards.
    fn test_pool() -> (Cleanup, DbPool) {
        dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        let mut url = url::Url::parse(&database_url).unwrap();
        assert!(url.path().ends_with("_test"));
        let mut cleanup = Cleanup(
            PgConnection::establish(&database_url).unwrap(),
            format!("oidc_test_{}", uuid::Uuid::new_v4().simple()),
        );
        cleanup
            .0
            .batch_execute(&format!("CREATE SCHEMA {}", cleanup.1))
            .unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={}", cleanup.1));
        let pool = crate::db::connect_and_migrate(url.as_str()).unwrap();
        (cleanup, pool)
    }

    #[test]
    #[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
    fn google_identities_never_merge_by_email_and_last_method_is_preserved() {
        let (_cleanup, pool) = test_pool();
        let AccountResponse::Session { session: grant1 } = dispatch(
            &pool,
            "",
            AccountRequest::Register {
                username: "owner".into(),
                email: "same@example.com".into(),
                password: "correct test password".into(),
                device: "test".into(),
            },
        )
        .unwrap() else {
            panic!("expected session")
        };
        let mut c = connection(&pool).unwrap();
        let session = authenticate(&mut c, &grant1.token).unwrap();
        // A browser attempt (bound by its cookie at the callback) made ready with
        // the given verified identity; the identity operation runs at completion.
        let create = |c: &mut PgConnection,
                      purpose: &str,
                      session: Option<&Session>,
                      subject: &str| {
            let state = secret();
            let ticket = secret();
            execute(c,"INSERT INTO oauth_attempts(state_hash,ticket_hash,verifier,nonce,purpose,session_id,account_id,device,status,browser_hash) VALUES($1,$2,'verifier','nonce',$3,NULLIF($4,'')::uuid,NULLIF($5,'')::uuid,'test','processing','test-browser')", &[&state,&digest(&ticket),purpose,&session.map(|s|s.id.to_string()).unwrap_or_default(),&session.map(|s|s.account_id.to_string()).unwrap_or_default()]).unwrap();
            ready(
                c,
                &state,
                Ok(GoogleIdentity {
                    subject: subject.into(),
                    email: "same@example.com".into(),
                }),
                None,
            )
            .unwrap();
            ticket
        };
        let failed = |c: &mut PgConnection, ticket: &str| {
            matches!(complete(c, ticket, None), Err(Failure::Invalid(FAILED)))
        };
        let ticket = create(&mut c, "login", None, "google-owner");
        assert!(failed(&mut c, &ticket));
        assert!(failed(&mut c, &ticket), "a refused attempt stays failed");
        assert_eq!(
            account(&mut c, session.account_id).unwrap().identities,
            vec![IdentityProvider::Password]
        );
        // Linking requires recent reauthentication.
        let ticket = create(&mut c, "link", Some(&session), "google-owner");
        assert!(failed(&mut c, &ticket));
        dispatch(
            &pool,
            &grant1.token,
            AccountRequest::Reauthenticate {
                password: "correct test password".into(),
            },
        )
        .unwrap();
        let ticket = create(&mut c, "link", Some(&session), "google-owner");
        complete(&mut c, &ticket, None).unwrap();
        assert!(matches!(
            complete(&mut c, &ticket, None),
            Err(Failure::Unauthorized)
        ));
        assert_eq!(
            account(&mut c, session.account_id)
                .unwrap()
                .identities
                .len(),
            2
        );
        // Identity linking does not verify the existing account's email implicitly.
        assert!(!account(&mut c, session.account_id).unwrap().email_verified);
        let ticket = create(&mut c, "reauthenticate", Some(&session), "different-owner");
        assert!(failed(&mut c, &ticket));
        let ticket = create(&mut c, "login", None, "google-owner");
        let AccountResponse::Session { session: grant2 } = complete(&mut c, &ticket, None).unwrap()
        else {
            panic!("expected session")
        };
        assert_eq!(grant2.account.id, grant1.account.id);
        assert!(matches!(
            complete(&mut c, &ticket, None),
            Err(Failure::Unauthorized)
        ));
        dispatch(
            &pool,
            &grant2.token,
            AccountRequest::Reauthenticate {
                password: "correct test password".into(),
            },
        )
        .unwrap();
        execute(
            &mut c,
            "UPDATE accounts SET email_verified=TRUE WHERE id=$1::uuid",
            &[&session.account_id.to_string()],
        )
        .unwrap();
        drop(c);
        // Two concurrent removals cannot remove both methods or use revoked sessions.
        let p = pool.clone();
        let first = std::thread::spawn(move || {
            dispatch(
                &p,
                &grant1.token,
                AccountRequest::UnlinkIdentity {
                    provider: IdentityProvider::Password,
                },
            )
        });
        let p = pool.clone();
        let second = std::thread::spawn(move || {
            dispatch(
                &p,
                &grant2.token,
                AccountRequest::UnlinkIdentity {
                    provider: IdentityProvider::Google,
                },
            )
        });
        let outcomes = [
            first.join().unwrap().is_ok(),
            second.join().unwrap().is_ok(),
        ];
        assert_eq!(outcomes.into_iter().filter(|ok| *ok).count(), 1);
        assert_eq!(
            account(&mut connection(&pool).unwrap(), session.account_id)
                .unwrap()
                .identities
                .len(),
            1
        );
    }
    use openidconnect::{
        Audience, EmptyAdditionalClaims, EndUserEmail, PrivateSigningKey, StandardClaims,
        SubjectIdentifier,
        core::{
            CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey,
        },
    };

    #[test]
    fn signed_tokens_enforce_provider_claims_and_signature() {
        // Deliberately public fixture, used only for signing local test tokens.
        let key = CoreRsaPrivateSigningKey::from_pem(
            include_str!("../../tests/fixtures/oidc-test-only.pem"),
            None,
        )
        .unwrap();
        let issuer = IssuerUrl::new("https://accounts.google.com".into()).unwrap();
        let access = AccessToken::new("test-access-token".into());
        let verifier = CoreIdTokenVerifier::new_public_client(
            ClientId::new("test-client".into()),
            issuer.clone(),
            CoreJsonWebKeySet::new(vec![key.as_verification_key()]),
        );
        let base = CoreIdTokenClaims::new(
            issuer,
            vec![Audience::new("test-client".into())],
            chrono::Utc::now() + chrono::Duration::minutes(5),
            chrono::Utc::now(),
            StandardClaims::new(SubjectIdentifier::new("google-subject".into()))
                .set_email(Some(EndUserEmail::new("test@example.com".into())))
                .set_email_verified(Some(true)),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new("expected-nonce".into())))
        .set_auth_time(Some(chrono::Utc::now()));
        let sign = |claims| {
            CoreIdToken::new(
                claims,
                &key,
                CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                Some(&access),
                None,
            )
            .unwrap()
        };
        assert!(
            verify_token(
                &sign(base.clone()),
                &access,
                &verifier,
                "expected-nonce",
                true
            )
            .is_ok()
        );
        assert!(
            verify_token(
                &sign(base.clone()),
                &access,
                &verifier,
                "wrong-nonce",
                false
            )
            .is_err()
        );
        assert!(
            verify_token(
                &sign(base.clone()),
                &AccessToken::new("substituted".into()),
                &verifier,
                "expected-nonce",
                false
            )
            .is_err()
        );
        for claims in [
            base.clone()
                .set_issuer(IssuerUrl::new("https://evil.test".into()).unwrap()),
            base.clone()
                .set_audiences(vec![Audience::new("wrong-client".into())]),
            base.clone()
                .set_expiration(chrono::Utc::now() - chrono::Duration::minutes(10)),
            base.clone().set_email_verified(Some(false)),
            base.clone()
                .set_auth_time(Some(chrono::Utc::now() - chrono::Duration::minutes(10))),
        ] {
            assert!(
                verify_token(&sign(claims), &access, &verifier, "expected-nonce", true).is_err()
            );
        }
        let wrong_keys = CoreIdTokenVerifier::new_public_client(
            ClientId::new("test-client".into()),
            IssuerUrl::new("https://accounts.google.com".into()).unwrap(),
            CoreJsonWebKeySet::new(vec![]),
        );
        let mut forged = sign(base.clone()).to_string().into_bytes();
        let signature_start = forged.iter().rposition(|b| *b == b'.').unwrap() + 1;
        forged[signature_start] = if forged[signature_start] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let forged: CoreIdToken = serde_json::from_value(serde_json::Value::String(
            String::from_utf8(forged).unwrap(),
        ))
        .unwrap();
        assert!(verify_token(&forged, &access, &verifier, "expected-nonce", false).is_err());
        assert!(verify_token(&sign(base), &access, &wrong_keys, "expected-nonce", false).is_err());
    }
    #[test]
    fn desktop_redirects_are_strictly_loopback_notifications() {
        let path = format!("/thiscord/{}", secret());
        assert!(valid_callback(&format!("http://127.0.0.1:54321{path}")));
        for host in ["localhost", "127.0.0.1.evil.test", "evil.test", "[::1]"] {
            assert!(!valid_callback(&format!("http://{host}:54321{path}")));
        }
        assert!(!valid_callback(&format!(
            "http://user@127.0.0.1:54321{path}"
        )));
        assert!(!valid_callback(&format!(
            "http://127.0.0.1:54321{path}?token=x"
        )));
    }

    /// The offline test provider: whoever completes the Google step picks the
    /// account `subject`; the ID token is signed with the public test key and
    /// verified exactly like Google's, against the attempt's nonce.
    async fn test_provider(
        subject: String,
        check: ProviderCheck,
    ) -> Result<GoogleIdentity, Failure> {
        let key = CoreRsaPrivateSigningKey::from_pem(
            include_str!("../../tests/fixtures/oidc-test-only.pem"),
            None,
        )
        .unwrap();
        let issuer = IssuerUrl::new("https://accounts.google.com".into()).unwrap();
        let access = AccessToken::new("test-access-token".into());
        let verifier = CoreIdTokenVerifier::new_public_client(
            ClientId::new("test-client".into()),
            issuer.clone(),
            CoreJsonWebKeySet::new(vec![key.as_verification_key()]),
        );
        let claims = CoreIdTokenClaims::new(
            issuer,
            vec![Audience::new("test-client".into())],
            chrono::Utc::now() + chrono::Duration::minutes(5),
            chrono::Utc::now(),
            StandardClaims::new(SubjectIdentifier::new(subject.clone()))
                .set_email(Some(EndUserEmail::new(format!("{subject}@example.com"))))
                .set_email_verified(Some(true)),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new(check.nonce.clone())))
        .set_auth_time(Some(chrono::Utc::now()));
        let token = CoreIdToken::new(
            claims,
            &key,
            CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            Some(&access),
            None,
        )
        .unwrap();
        verify_token(
            &token,
            &access,
            &verifier,
            &check.nonce,
            check.reauthenticate,
        )
    }

    /// Drives the real callback for `state` from a browser holding `cookie`, where
    /// the person at the Google step picks `subject` (or denies, without one).
    async fn hit_callback(
        pool: &DbPool,
        state: &str,
        cookie: Option<&str>,
        subject: Option<&str>,
    ) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(cookie) = cookie {
            headers.insert(
                header::COOKIE,
                format!("{}={cookie}", cookie::GOOGLE_ATTEMPT_COOKIE)
                    .parse()
                    .unwrap(),
            );
        }
        let input = Callback {
            state: state.into(),
            code: subject.map(str::to_owned),
            error: subject.is_none().then(|| "access_denied".into()),
        };
        finish_callback(
            Some(pool.clone()),
            RequestId::from_uuid(uuid::Uuid::new_v4()),
            &headers,
            Ok(input),
            test_provider,
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
    async fn google_attempts_complete_only_where_they_started() {
        let (_cleanup, pool) = test_pool();
        let mut conn = connection(&pool).unwrap();
        let c: &mut PgConnection = &mut conn;
        let exists = |c: &mut PgConnection, sql: &str, value: &str| {
            !query::<serde_json::Value>(c, sql, &[value])
                .unwrap()
                .is_empty()
        };
        let status = |c: &mut PgConnection, ticket: &str| {
            query::<String>(
                c,
                "SELECT to_jsonb(status) AS data FROM oauth_attempts WHERE ticket_hash=$1",
                &[&digest(ticket)],
            )
            .unwrap()
            .pop()
        };
        let loopback = format!("http://127.0.0.1:54321/thiscord/{}", secret());
        for (run, purpose) in [
            GooglePurpose::Login,
            GooglePurpose::Link,
            GooglePurpose::Reauthenticate,
        ]
        .into_iter()
        .enumerate()
        {
            for desktop in [true, false] {
                // The holder starts the attempt; a different person may click its link.
                let name = format!("holder{run}{desktop}");
                let subject = format!("google-{name}");
                let AccountResponse::Session { session: grant } = dispatch(
                    &pool,
                    "",
                    AccountRequest::Register {
                        username: name.clone(),
                        email: format!("{name}@example.com"),
                        password: "correct test password".into(),
                        device: "test".into(),
                    },
                )
                .unwrap() else {
                    panic!("expected session")
                };
                let session = authenticate(c, &grant.token).unwrap();
                let sid = session.id.to_string();
                let token = match purpose {
                    GooglePurpose::Login => "",
                    GooglePurpose::Link => {
                        dispatch(
                            &pool,
                            &grant.token,
                            AccountRequest::Reauthenticate {
                                password: "correct test password".into(),
                            },
                        )
                        .unwrap();
                        grant.token.as_str()
                    }
                    GooglePurpose::Reauthenticate => {
                        execute(c,"INSERT INTO identities(account_id,provider,subject) VALUES($1::uuid,'google',$2)", &[&session.account_id.to_string(),&subject]).unwrap();
                        execute(
                            c,
                            "UPDATE sessions SET reauthenticated_at=NULL WHERE id=$1::uuid",
                            &[&sid],
                        )
                        .unwrap();
                        grant.token.as_str()
                    }
                };
                let applied = |c: &mut PgConnection| match purpose {
                    GooglePurpose::Login | GooglePurpose::Link => exists(
                        c,
                        "SELECT to_jsonb(account_id) AS data FROM identities WHERE provider='google' AND subject=$1",
                        &subject,
                    ),
                    GooglePurpose::Reauthenticate => exists(
                        c,
                        "SELECT to_jsonb(id) AS data FROM sessions WHERE id=$1::uuid AND reauthenticated_at IS NOT NULL",
                        &sid,
                    ),
                };
                let start = |c: &mut PgConnection| {
                    let state = secret();
                    let (ticket, browser) = record(
                        c,
                        token,
                        purpose,
                        desktop.then_some(loopback.as_str()),
                        "test",
                        [&digest(&state), "verifier", "nonce"],
                    )
                    .unwrap();
                    // `start` never yields a completion secret; none exists yet.
                    assert!(exists(
                        c,
                        "SELECT to_jsonb(ticket_hash) AS data FROM oauth_attempts WHERE ticket_hash=$1 AND completion_hash IS NULL",
                        &digest(&ticket)
                    ));
                    assert_eq!(browser.is_some(), !desktop);
                    (state, ticket, browser)
                };
                if desktop {
                    // Denied at Google: the loopback is notified without a secret.
                    let (state, ticket, _) = start(c);
                    let response = hit_callback(&pool, &state, None, None).await;
                    assert_eq!(response.headers()[header::LOCATION], loopback.as_str());
                    assert!(matches!(
                        complete(c, &ticket, None),
                        Err(Failure::Invalid(FAILED))
                    ));
                    // Any browser may finish the Google step of a loopback attempt...
                    let (state, ticket, _) = start(c);
                    let response = hit_callback(&pool, &state, None, Some(&subject)).await;
                    assert_eq!(response.status(), StatusCode::SEE_OTHER);
                    let location = response.headers()[header::LOCATION].to_str().unwrap();
                    let completion = location
                        .strip_prefix(&format!("{loopback}?{GOOGLE_COMPLETION_PARAM}="))
                        .unwrap()
                        .to_owned();
                    assert_eq!(completion.len(), 43);
                    assert_ne!(completion, ticket);
                    // ...but the ticket alone (an attacker's, or a 0.1.63 client's)
                    // cannot redeem it, nor can a wrong secret; neither consumes it.
                    assert!(!applied(c));
                    assert!(matches!(
                        complete(c, &ticket, None),
                        Err(Failure::Invalid(UPDATE_REQUIRED))
                    ));
                    assert!(matches!(
                        complete(c, &ticket, Some(&secret())),
                        Err(Failure::Unauthorized)
                    ));
                    assert!(matches!(
                        complete(c, &ticket, Some(&completion[1..])),
                        Err(Failure::Unauthorized)
                    ));
                    assert!(!applied(c));
                    assert_eq!(status(c, &ticket).as_deref(), Some("ready"));
                    // The loopback's holder completes exactly once.
                    let response = complete(c, &ticket, Some(&completion)).unwrap();
                    assert_eq!(
                        matches!(response, AccountResponse::Session { .. }),
                        purpose == GooglePurpose::Login
                    );
                    assert!(applied(c));
                    assert!(matches!(
                        complete(c, &ticket, Some(&completion)),
                        Err(Failure::Unauthorized)
                    ));
                } else {
                    // A browser without the attempt cookie (the victim of a forwarded
                    // link) stops the attempt; nothing becomes ready or applies.
                    let (state, ticket, _) = start(c);
                    let response = hit_callback(&pool, &state, None, Some(&subject)).await;
                    assert_eq!(response.status(), StatusCode::FORBIDDEN);
                    assert!(response.headers().get(header::SET_COOKIE).is_none());
                    assert_eq!(status(c, &ticket).as_deref(), Some("failed"));
                    assert!(matches!(
                        complete(c, &ticket, None),
                        Err(Failure::Invalid(FAILED))
                    ));
                    // Another attempt's cookie does not bind this one either.
                    let (_, other_ticket, other) = start(c);
                    let (state, ticket, _) = start(c);
                    let response =
                        hit_callback(&pool, &state, other.as_deref(), Some(&subject)).await;
                    assert_eq!(response.status(), StatusCode::FORBIDDEN);
                    assert!(matches!(
                        complete(c, &ticket, None),
                        Err(Failure::Invalid(FAILED))
                    ));
                    assert_eq!(status(c, &other_ticket).as_deref(), Some("pending"));
                    assert!(!applied(c));
                    // The starting browser, with its own cookie, succeeds once and the
                    // cookie is cleared.
                    let (state, ticket, browser) = start(c);
                    let response =
                        hit_callback(&pool, &state, browser.as_deref(), Some(&subject)).await;
                    assert_eq!(response.status(), StatusCode::OK);
                    let cleared = response.headers()[header::SET_COOKIE].to_str().unwrap();
                    assert!(cleared.starts_with(&format!("{}=;", cookie::GOOGLE_ATTEMPT_COOKIE)));
                    assert!(cleared.contains("Max-Age=0"));
                    assert_eq!(status(c, &ticket).as_deref(), Some("ready"));
                    assert!(!applied(c));
                    assert!(complete(c, &ticket, None).is_ok());
                    assert!(applied(c));
                    assert!(matches!(
                        complete(c, &ticket, None),
                        Err(Failure::Unauthorized)
                    ));
                    // A replayed callback cannot reopen it.
                    let response =
                        hit_callback(&pool, &state, browser.as_deref(), Some(&subject)).await;
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                }
            }
        }
    }
}
