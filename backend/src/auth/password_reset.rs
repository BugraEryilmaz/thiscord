//! Browser recovery uses the same one-time code, worker budget and account locks as the API.
use super::{Failure, admit, store};
use crate::db::DbPool;
use axum::{
    Extension, Form,
    extract::{
        ConnectInfo, Query, State,
        rejection::{FormRejection, QueryRejection},
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use std::net::SocketAddr;
use thiscord_shared::{
    RequestId,
    account::{AccountRequest, PASSWORD_RESET_PATH, PasswordResetForm, PasswordResetQuery},
};

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(super) async fn head() -> Response {
    secure(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn open(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    input: Result<Query<PasswordResetQuery>, QueryRejection>,
) -> Response {
    let input = match input {
        Ok(Query(input)) if valid_token(&input.token) => input,
        Ok(_) => return failure(Failure::Unauthorized, None, id),
        Err(_) => return secure(Failure::Invalid("Invalid password reset link").response(id)),
    };
    let token = input.token.clone();
    let result = work(pool, peer, move |pool| {
        store::check_reset_code(pool, &token)
    })
    .await;
    match result {
        Ok(()) => page(
            StatusCode::OK,
            "Choose a new password",
            "Use at least 12 characters. Your other devices will be signed out.",
            Some(&input.token),
            id,
        ),
        Err(error) => failure(error, None, id),
    }
}

pub(super) async fn submit(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    input: Result<Form<PasswordResetForm>, FormRejection>,
) -> Response {
    let input = match input {
        Ok(Form(input)) => input,
        // Keep extractor errors in the shared wire envelope, never Axum's text.
        Err(_) => {
            return secure(
                Failure::Invalid("Invalid password reset form. Reopen the link in your email.")
                    .response(id),
            );
        }
    };
    if !valid_token(&input.token) {
        return failure(Failure::Unauthorized, None, id);
    }
    let token = input.token.clone();
    let result = work(pool, peer, move |pool| {
        store::check_reset_code(pool, &input.token)?;
        if input.password != input.confirm_password {
            return Err(Failure::Invalid(
                "Passwords do not match. Please enter them again.",
            ));
        }
        store::dispatch(
            pool,
            "",
            AccountRequest::ResetPassword {
                code: input.token,
                password: input.password,
            },
        )?;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => page(
            StatusCode::OK,
            "Password changed",
            "Your password has been updated and your previous sessions have been signed out. Return to Thiscord and sign in with your new password. You can close this tab.",
            None,
            id,
        ),
        Err(error) => failure(error, Some(&token), id),
    }
}

async fn work(
    pool: Option<DbPool>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    action: impl FnOnce(&DbPool) -> Result<(), Failure> + Send + 'static,
) -> Result<(), Failure> {
    let pool = pool.ok_or(Failure::Unavailable)?;
    let permit = admit(&AccountRequest::ResetPassword {
        code: String::new(),
        password: String::new(),
    })?;
    let ip = peer
        .map(|p| p.0.0.ip().to_string())
        .unwrap_or_else(|| "local".into());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store::rate_limit(&pool, &ip, None, true)?;
        action(&pool)
    })
    .await
    .map_err(|_| Failure::Unavailable)?
}

fn failure(error: Failure, token: Option<&str>, id: RequestId) -> Response {
    let (status, title, message, token) = match error {
        Failure::Unauthorized => (
            StatusCode::BAD_REQUEST,
            "This reset link is no longer valid",
            "It may have expired, already been used, or been replaced by a newer email. Open Forgot password in Thiscord to request a new link.",
            None,
        ),
        Failure::Invalid(message) => (
            StatusCode::BAD_REQUEST,
            "Check your new password",
            message,
            token,
        ),
        Failure::Limited => (
            StatusCode::TOO_MANY_REQUESTS,
            "Please try again shortly",
            "Too many attempts. Wait one minute, then try again.",
            token,
        ),
        _ => {
            tracing::warn!(request_id = %id, "browser password reset unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Password reset is temporarily unavailable",
                "Please try again shortly. You can reopen the same link from your email.",
                token,
            )
        }
    };
    page(status, title, message, token, id)
}

fn page(
    status: StatusCode,
    title: &str,
    message: &str,
    token: Option<&str>,
    id: RequestId,
) -> Response {
    // Only static messages and validated URL-safe tokens enter HTML. Passwords are never echoed.
    let form = token.filter(|token| valid_token(token)).map(|token| format!(r#"
        <form method="post" action="{PASSWORD_RESET_PATH}">
            <input type="hidden" name="token" value="{token}">
            <label for="password">New password</label>
            <input id="password" name="password" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required autofocus aria-describedby="message">
            <label for="confirm-password">Confirm new password</label>
            <input id="confirm-password" name="confirm_password" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required>
            <button type="submit">Change password</button>
        </form>"#)).unwrap_or_default();
    let support = if status.is_success() {
        String::new()
    } else {
        format!("<footer>Support reference: {id}</footer>")
    };
    let html = include_str!("password_reset.html")
        .replace("{{title}}", title)
        .replace("{{message}}", message)
        .replace("{{form}}", &form)
        .replace("{{support}}", &support);
    let mut response = secure((status, Html(html)).into_response());
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert("retry-after", "60".parse().unwrap());
    }
    response
}

fn secure(mut response: Response) -> Response {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
        ("x-content-type-options", "nosniff"),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}
