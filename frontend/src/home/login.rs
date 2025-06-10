use leptos::{context, html::Input, logging::{error, log}, prelude::*, task::spawn_local};
use serde_wasm_bindgen::{from_value, to_value};
use shared::{LoginRequest, RegisterRequest};

use crate::{app::SessionCookieSignal, utils::invoke};

use stylance::import_style;

import_style!(#[allow(dead_code)] style, "login.css");

#[component]
pub fn Login() -> impl IntoView {
    let (is_login, set_is_login) = signal(true);
    let session_cookie =
        context::use_context::<SessionCookieSignal>().expect("SessionCookie context not found");

    let email_ref: NodeRef<Input> = NodeRef::new();
    let username_ref: NodeRef<Input> = NodeRef::new();
    let password_ref: NodeRef<Input> = NodeRef::new();
    let confirm_password_ref: NodeRef<Input> = NodeRef::new();

    let login = move || {
        let username = username_ref.get().expect("Failed to get username").value();
        let password = password_ref.get().expect("Failed to get password").value();
        // Send a login request to the server
        // Example: fetch(url + "/login", { method: "POST", body: JSON.stringify({ username, password }) })
        spawn_local(async move {
            let request = LoginRequest {
                username,
                password,
            };
            let response = invoke("login", to_value(&request).unwrap()).await;
            log!("Response: {:?}", response);
            if let Err(e) = response {
                // Handle login failure (e.g., show an error message)
                error!("Login failed: {}", from_value::<String>(e).unwrap_or_else(|_| "Unknown error".to_string()));
            } else {
                session_cookie.set(true);
            } 
        });
    };

    let register = move || {
        let email = email_ref.get().expect("Failed to get email").value();
        let username = username_ref.get().expect("Failed to get username").value();
        let password = password_ref.get().expect("Failed to get password").value();
        let confirm_password = confirm_password_ref.get().expect("Failed to get confirm password").value();
        if password != confirm_password {
            error!("Passwords do not match");
            return;
        }
        spawn_local(async move {
            let request = RegisterRequest {
                username,
                password,
                email,
            };
            let response = invoke("signup", to_value(&request).unwrap()).await;
            if let Err(e) = response {
                error!("Registration failed: {}", from_value::<String>(e).unwrap_or_else(|_| "Unknown error".to_string()));
            } else {
                log!("Registration successful");
            }
        });
    };

    view! {
        <div class=style::login_container>
            <h2>{move || if is_login.get() { "Login Page" } else { "Register Page" }}</h2>
            <form on:submit=move |ev| {
                ev.prevent_default();
                if is_login.get() {
                    login();
                } else {
                    register();
                }
            }>
                <Show when=move || !is_login.get()>
                    <input type="email" required placeholder="Email" node_ref=email_ref />
                </Show>

                <input type="text" required placeholder="Username" node_ref=username_ref />
                <input type="password" required placeholder="Password" node_ref=password_ref />
                <Show when=move || !is_login.get()>
                    <input type="password" required placeholder="Confirm Password" node_ref=confirm_password_ref />
                </Show>
                <button type="submit">
                    {move || if is_login.get() { "Login" } else { "Register" }}
                </button>
            </form>
            <div class=style::toggle_login>
                {move || {
                    if is_login.get() {
                        "Don't have an account? \n"
                    } else {
                        "Already have an account? \n"
                    }
                }}
                <button
                    type="button"
                    on:click=move |_| {
                        set_is_login.update(|is_login| *is_login = !*is_login);
                    }
                >
                    {move || if is_login.get() { "Register here" } else { "Login here" }}
                </button>
            </div>
        </div>
    }
}
