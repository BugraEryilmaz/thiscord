use diesel::prelude::*;
use reqwest::cookie::CookieStore;
use shared::{LoginRequest, LoginStatus, RegisterRequest, URL};
use tauri::{Emitter, Manager, Url};

use crate::{
    models::Session,
    schema,
    utils::{establish_connection, AppState},
};

#[tauri::command]
pub async fn login(
    username: String,
    password: String,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = handle.state::<AppState>();
    let client = &state.client;
    let _response = client
        .post(format!("{}/auth/login", URL))
        .json(&LoginRequest { username, password })
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let cookie = state.cookie_store.clone();
    if let Some(cookie) = cookie.cookies(&Url::parse(URL).unwrap()) {
        let cookie = cookie.to_str().unwrap_or_default();
        let cookie = Session::new(cookie.to_string());
        let conn = establish_connection(&handle);
        let _ = cookie.save(conn).map_err(|e| {
            tracing::error!("Failed to save session cookie: {}", e);
            e.to_string()
        })?;
    } else {
        tracing::warn!("No cookies found after login.");
    }
    tracing::info!("Cookies after login: {:?}", cookie);
    Ok(())
}

#[tauri::command]
pub async fn signup(
    username: String,
    password: String,
    email: String,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = handle.state::<AppState>();
    let client = &state.client;
    let _response = client
        .post(format!("{}/auth/signup", URL))
        .json(&RegisterRequest {
            username,
            password,
            email,
        })
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    tracing::info!("User registered successfully.");
    Ok(())
}

#[tauri::command]
pub async fn check_cookies(handle: tauri::AppHandle) -> bool {
    let state = handle.state::<AppState>();
    let cookie_store = &state.cookie_store;
    if let Some(cookie) = cookie_store.cookies(&Url::parse(URL).unwrap()) {
        tracing::info!("Cookies found: {:?}", cookie);
        return true;
    }
    tracing::warn!("No cookies found in the cookie store.");
    false
}

#[tauri::command]
pub async fn logout(handle: tauri::AppHandle) -> Result<(), String> {
    let mut conn = establish_connection(&handle);
    diesel::delete(schema::session::table)
        .execute(&mut conn)
        .map_err(|e| {
            tracing::error!("Failed to delete session cookie from database: {}", e);
            e.to_string()
        })?;
    handle.emit("login_status", LoginStatus::LoggedOut).unwrap();
    Ok(())
}
