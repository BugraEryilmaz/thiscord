use super::{Ui, client};
use crate::avatar::Avatar;
use base64::{Engine, engine::general_purpose::STANDARD};
use leptos::prelude::*;
use thiscord_shared::account::{AccountRequest, AccountResponse, MAX_AVATAR_BYTES};

async fn save(ui: Ui, token: String, image_base64: Option<String>) {
    let removing = image_base64.is_none();
    let result = client::request(&AccountRequest::SetAvatar { image_base64 }, Some(&token)).await;
    // Never apply an old session's result to a newly signed-in account.
    if ui.token.get_untracked().as_deref() == Some(&token) {
        match result {
            Ok(AccountResponse::Account { account }) => {
                ui.account.update(|current| {
                    if let Some(current) = current
                        && current.id == account.id
                    {
                        current.avatar_id = account.avatar_id;
                    }
                });
                ui.status.set(
                    if removing {
                        "Profile picture removed"
                    } else {
                        "Profile picture saved"
                    }
                    .into(),
                );
            }
            Ok(_) => ui
                .status
                .set("Server returned an invalid profile response".into()),
            Err(error) => ui.status.set(error),
        }
    }
    ui.busy.set(false);
}

#[component]
pub(super) fn ProfilePicture(ui: Ui) -> impl IntoView {
    let choose = move |event| {
        let input = event_target::<web_sys::HtmlInputElement>(&event);
        let file = input.files().and_then(|files| files.get(0));
        input.set_value(""); // Selecting the same file again should retry a failed upload.
        let Some(file) = file else {
            return;
        };
        if ui.busy.get_untracked() {
            return;
        }
        if file.size() == 0.0 || file.size() > MAX_AVATAR_BYTES as f64 {
            ui.status.set("Choose a picture up to 2 MiB".into());
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        ui.busy.set(true);
        ui.status.set("Uploading profile picture…".into());
        leptos::task::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let encoded = STANDARD.encode(js_sys::Uint8Array::new(&buffer).to_vec());
                    save(ui, token, Some(encoded)).await;
                }
                Err(_) => {
                    ui.status
                        .set("Cannot read this picture. Please choose it again".into());
                    ui.busy.set(false);
                }
            }
        });
    };
    view! {
        <section class="space-y-3 rounded-lg border border-white/15 p-4" aria-label="Profile picture">
            <h3 class="font-semibold">"Profile picture"</h3>
            <div class="flex flex-wrap items-center gap-4">
                <Avatar class="size-20 text-2xl"
                    name=Signal::derive(move || ui.account.get().map(|a| a.display_name).unwrap_or_default())
                    avatar_id=Signal::derive(move || ui.account.get().and_then(|a| a.avatar_id))/>
                <div class="space-y-2">
                    <label class="block space-y-2 text-sm">
                        <span>"Choose picture"</span>
                        <input class="block max-w-full rounded text-sm file:mr-3 file:rounded-md file:border-0 file:bg-brand file:px-3 file:py-2 file:text-white disabled:opacity-50"
                            type="file" accept="image/png,image/jpeg,image/webp" disabled=move || ui.busy.get()
                            on:change=choose/>
                    </label>
                    <button type="button" class="text-sm underline disabled:opacity-50"
                        disabled=move || ui.busy.get() || ui.account.get().is_none_or(|a| a.avatar_id.is_none())
                        on:click=move |_| {
                            if ui.busy.get_untracked() { return; }
                            if let Some(token) = ui.token.get_untracked() {
                                ui.busy.set(true);
                                ui.status.set("Removing profile picture…".into());
                                leptos::task::spawn_local(save(ui, token, None));
                            }
                        }>"Remove picture"</button>
                </div>
            </div>
            <p class="text-sm text-white/60">"PNG, JPEG or WebP, up to 2 MiB and 4096 × 4096 pixels. Choosing a picture saves it immediately, cropped to a square. It appears in voice chat and the overlay."</p>
        </section>
    }
}
