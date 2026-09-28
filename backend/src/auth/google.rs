use super::{Failure, store::*};
use crate::db::DbPool;
use axum::{
    Extension,
    extract::{Query, State},
    http::StatusCode,
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
) -> Result<AccountResponse, Failure> {
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
    let ticket = secret();
    let state_hash = digest(state.secret());
    let ticket_hash = digest(&ticket);
    tokio::task::spawn_blocking(move || {
        let mut c = connection(&pool)?;
        let session = if purpose!=GooglePurpose::Login { Some(authenticate(&mut c,&token)?) } else { None };
        c.transaction::<_,Failure,_>(|c| {
            if let Some(s) = &session { lock_session(c,&token,s)?; if purpose==GooglePurpose::Link { recent(s)?; } }
            execute(c,"DELETE FROM oauth_attempts WHERE expires_at<now()", &[])?;
            execute(c,"INSERT INTO oauth_attempts(state_hash,ticket_hash,verifier,nonce,purpose,session_id,account_id,device,callback) VALUES($1,$2,$3,$4,$5,NULLIF($6,'')::uuid,NULLIF($7,'')::uuid,$8,NULLIF($9,''))", &[&state_hash,&ticket_hash,verifier.secret(),nonce.secret(),match purpose {GooglePurpose::Login=>"login",GooglePurpose::Link=>"link",GooglePurpose::Reauthenticate=>"reauthenticate"},&session.as_ref().map(|s|s.id.to_string()).unwrap_or_default(),&session.as_ref().map(|s|s.account_id.to_string()).unwrap_or_default(),&name,callback.as_deref().unwrap_or("")])?;
            Ok(())
        })
    }).await.map_err(|_| Failure::Unavailable)??;
    Ok(AccountResponse::GoogleStarted {
        authorization_url: authorization_url.to_string(),
        ticket,
    })
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
    status: String,
}
#[derive(Deserialize)]
pub(super) struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

pub(super) async fn callback(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    query: Result<Query<Callback>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let result = async {
        let Query(input) = query.map_err(|_| Failure::Unauthorized)?;
        if input.state.len()>128 || input.code.as_ref().is_some_and(|s|s.len()>4096) { return Err(Failure::Unauthorized); }
        let pool = pool.ok_or(Failure::Unavailable)?;
        let p = pool.clone();
        let hash = digest(&input.state);
        let attempt: Attempt = tokio::task::spawn_blocking(move || {
            query_attempt(&mut *connection(&p)?,"UPDATE oauth_attempts SET status='processing' WHERE state_hash=$1 AND status='pending' AND expires_at>now() RETURNING to_jsonb(oauth_attempts) AS data", &hash)
        }).await.map_err(|_| Failure::Unavailable)??;
        let verified = if input.error.is_some() { Err(Failure::Unauthorized) } else {
            verify_provider(input.code.unwrap_or_default(), &attempt).await
        };
        let callback = attempt.callback.clone();
        let p = pool.clone();
        let hash = attempt.state_hash.clone();
        let finished = tokio::task::spawn_blocking(move || finish(&mut *connection(&p)?,attempt,verified)).await.map_err(|_| Failure::Unavailable)?;
        if finished.is_err() {
            tokio::task::spawn_blocking(move || execute(&mut *connection(&pool)?,"UPDATE oauth_attempts SET status='failed',verifier='',nonce='' WHERE state_hash=$1", &[&hash])).await.map_err(|_| Failure::Unavailable)??;
        }
        // Only a random notification path travels to the desktop; never a session or provider token.
        if let Some(callback) = callback { Ok(Redirect::to(&callback).into_response()) }
        else { Ok((StatusCode::OK,"Google sign-in finished. Return to Thiscord to see the result. You can close this tab.").into_response()) }
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
fn query_attempt(c: &mut PgConnection, sql: &str, hash: &str) -> Result<Attempt, Failure> {
    query(c, sql, &[hash])?.pop().ok_or(Failure::Unauthorized)
}
struct GoogleIdentity {
    subject: String,
    email: String,
}
async fn verify_provider(code: String, attempt: &Attempt) -> Result<GoogleIdentity, Failure> {
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
        attempt.purpose == "reauthenticate",
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
fn finish(
    c: &mut PgConnection,
    attempt: Attempt,
    verified: Result<GoogleIdentity, Failure>,
) -> Result<(), Failure> {
    let identity = verified?;
    c.transaction(|c| {
        // Match the normal account-before-session lock order.
        if let Some(id) = attempt.account_id { execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE", &[&id.to_string()])?; }
        let current = query_attempt(c,"SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE state_hash=$1 AND status='processing' AND expires_at>now() FOR UPDATE", &attempt.state_hash)?;
        if let Some(sid) = current.session_id {
            let sessions: Vec<Session> = query(c,"SELECT to_jsonb(s) AS data FROM sessions s WHERE id=$1::uuid AND NOT revoked AND expires_at>now() AND last_seen_at>now()-interval '7 days' FOR UPDATE", &[&sid.to_string()])?;
            let session = sessions.first().ok_or(Failure::Unauthorized)?;
            if current.purpose=="link" { recent(session)?; }
        }
        let existing: Option<AccountId> = query(c,"SELECT to_jsonb(account_id) AS data FROM identities WHERE provider='google' AND subject=$1", &[&identity.subject])?.pop();
        let id = match current.purpose.as_str() {
            "login" => if let Some(id) = existing { id } else {
                // Never merge by email, even if Google reports it verified.
                let id = AccountId::from_uuid(uuid::Uuid::new_v4());
                let username = format!("user_{}", &id.as_uuid().simple().to_string()[..20]);
                execute(c,"INSERT INTO accounts(id,username,email,email_verified,display_name) VALUES($1::uuid,$2,$3,TRUE,$2)", &[&id.to_string(),&username,&identity.email])?;
                execute(c,"INSERT INTO identities(account_id,provider,subject) VALUES($1::uuid,'google',$2)", &[&id.to_string(),&identity.subject])?;
                id
            },
            "link" => {
                let id = current.account_id.ok_or(Failure::Unauthorized)?;
                if existing.is_some() { return Err(Failure::Conflict); }
                execute(c,"INSERT INTO identities(account_id,provider,subject) VALUES($1::uuid,'google',$2)", &[&id.to_string(),&identity.subject])?;
                id
            },
            "reauthenticate" => {
                let id = current.account_id.ok_or(Failure::Unauthorized)?;
                if existing!=Some(id) { return Err(Failure::Unauthorized); }
                execute(c,"UPDATE sessions SET reauthenticated_at=now() WHERE id=$1::uuid", &[&current.session_id.ok_or(Failure::Unauthorized)?.to_string()])?;
                id
            },
            _ => return Err(Failure::Unauthorized),
        };
        execute(c,"UPDATE oauth_attempts SET account_id=$2::uuid,status='ready',verifier='',nonce='' WHERE state_hash=$1", &[&current.state_hash,&id.to_string()])?;
        Ok(())
    })
}
pub(super) fn complete(c: &mut PgConnection, ticket: &str) -> Result<AccountResponse, Failure> {
    if ticket.len() != 43 {
        return Err(Failure::Unauthorized);
    }
    c.transaction(|c| {
        let initial = query_attempt(c,"SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE ticket_hash=$1 AND expires_at>now()", &digest(ticket))?;
        if initial.status=="pending" || initial.status=="processing" { return Ok(AccountResponse::Pending); }
        if let Some(id) = initial.account_id { execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR UPDATE", &[&id.to_string()])?; }
        let a = query_attempt(c,"SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE ticket_hash=$1 AND expires_at>now() FOR UPDATE", &digest(ticket))?;
        if a.status=="pending" || a.status=="processing" { return Ok(AccountResponse::Pending); }
        if a.status!="ready" { return Err(Failure::Invalid("Google sign-in failed. If the email already has an account, sign in there first and link Google")); }
        execute(c,"DELETE FROM oauth_attempts WHERE ticket_hash=$1", &[&digest(ticket)])?;
        if a.purpose=="login" { grant(c,a.account_id.ok_or(Failure::Unauthorized)?,&a.device) }
        else { Ok(AccountResponse::Done {message:"Google identity operation completed".into()}) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
    fn google_identities_never_merge_by_email_and_last_method_is_preserved() {
        use diesel::connection::SimpleConnection;
        dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        let mut url = url::Url::parse(&database_url).unwrap();
        assert!(url.path().ends_with("_test"));
        struct Cleanup(PgConnection, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self
                    .0
                    .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.1));
            }
        }
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
        let create = |c: &mut PgConnection, purpose: &str, session: Option<&Session>| {
            let state = secret();
            let ticket = secret();
            execute(c,"INSERT INTO oauth_attempts(state_hash,ticket_hash,verifier,nonce,purpose,session_id,account_id,device,status) VALUES($1,$2,'verifier','nonce',$3,NULLIF($4,'')::uuid,NULLIF($5,'')::uuid,'test','processing')", &[&state,&digest(&ticket),purpose,&session.map(|s|s.id.to_string()).unwrap_or_default(),&session.map(|s|s.account_id.to_string()).unwrap_or_default()]).unwrap();
            (
                query_attempt(
                    c,
                    "SELECT to_jsonb(o) AS data FROM oauth_attempts o WHERE state_hash=$1",
                    &state,
                )
                .unwrap(),
                ticket,
            )
        };
        let identity = || {
            Ok(GoogleIdentity {
                subject: "google-owner".into(),
                email: "same@example.com".into(),
            })
        };
        let (attempt, _) = create(&mut c, "login", None);
        assert!(matches!(
            finish(&mut c, attempt, identity()),
            Err(Failure::Conflict)
        ));
        assert_eq!(
            account(&mut c, session.account_id).unwrap().identities,
            vec![IdentityProvider::Password]
        );
        let (attempt, _) = create(&mut c, "link", Some(&session));
        assert!(matches!(
            finish(&mut c, attempt, identity()),
            Err(Failure::Forbidden)
        ));
        dispatch(
            &pool,
            &grant1.token,
            AccountRequest::Reauthenticate {
                password: "correct test password".into(),
            },
        )
        .unwrap();
        let (attempt, ticket) = create(&mut c, "link", Some(&session));
        finish(&mut c, attempt, identity()).unwrap();
        complete(&mut c, &ticket).unwrap();
        assert!(matches!(
            complete(&mut c, &ticket),
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
        let (attempt, _) = create(&mut c, "reauthenticate", Some(&session));
        assert!(matches!(
            finish(
                &mut c,
                attempt,
                Ok(GoogleIdentity {
                    subject: "different-owner".into(),
                    email: "same@example.com".into()
                })
            ),
            Err(Failure::Unauthorized)
        ));
        let (attempt, ticket) = create(&mut c, "login", None);
        finish(&mut c, attempt, identity()).unwrap();
        let AccountResponse::Session { session: grant2 } = complete(&mut c, &ticket).unwrap()
        else {
            panic!("expected session")
        };
        assert_eq!(grant2.account.id, grant1.account.id);
        assert!(matches!(
            complete(&mut c, &ticket),
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
}
