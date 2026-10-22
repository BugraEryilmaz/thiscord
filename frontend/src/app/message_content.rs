use super::Ui;
use crate::account_client;
use leptos::{either::Either, prelude::*};

pub(super) fn render(text: &str, ui: Ui) -> impl IntoView + use<> {
    thiscord_frontend::message_links::spans(text)
        .into_iter()
        .map(|span| {
            let text = span.text.to_owned();
            match span.href {
                None => Either::Left(text),
                Some(href) => {
                    let url = href.clone();
                    Either::Right(view! {
                        <a href=href target="_blank" rel="noopener noreferrer"
                            class="text-sky-400 underline decoration-sky-400/50 underline-offset-2 hover:text-sky-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
                            on:click=move |event| {
                                if account_client::desktop() {
                                    event.prevent_default();
                                    let url = url.clone();
                                    leptos::task::spawn_local(async move {
                                        if let Err(error) = account_client::native::<()>(
                                            "open_external_url", serde_json::json!({ "url": url }),
                                        ).await {
                                            ui.status.set(error);
                                        }
                                    });
                                }
                            }
                        >{text}</a>
                    })
                }
            }
        })
        .collect_view()
}
