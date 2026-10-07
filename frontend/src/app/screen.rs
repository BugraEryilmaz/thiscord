use super::Ui;
use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::screen::{Quality, Source, Status};

#[component]
pub(super) fn ScreenControls(ui: Ui) -> impl IntoView {
    let status = RwSignal::new(Status::default());
    let sources = RwSignal::new(Vec::<Source>::new());
    let selected = RwSignal::new(0_usize);
    let picking = RwSignal::new(false);
    let audio = RwSignal::new(true);
    let quality = RwSignal::new(Quality::default());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let diagnostics = RwSignal::new(String::new());
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                let mut previous = thiscord_shared::screen::Diagnostics::default();
                loop {
                    if let Ok(value) = native::<Status>("screen_status", json!({})).await {
                        diagnostics.set(super::screen_player::describe(
                            &value.diagnostics,
                            &previous,
                        ));
                        previous = value.diagnostics.clone();
                        status.set(value);
                    }
                    gloo_timers::future::TimeoutFuture::new(300).await;
                }
            },
            registration,
        )
        .await;
    });
    view! {
        <div class="flex flex-wrap items-center gap-2" aria-label="Screen sharing">
            <Show when=move || status.get().sharing fallback=move || view! {
                <button class="rounded bg-white/10 px-3 py-2 text-sm disabled:opacity-50"
                    disabled=move || !status.get().available || ui.audio.get().deafened || busy.get()
                    on:click=move |_| {
                        busy.set(true); error.set(String::new());
                        leptos::task::spawn_local(async move {
                            let result = native::<Vec<Source>>("screen_sources", json!({})).await;
                            if busy.try_get_untracked().is_none() { return; }
                            match result {
                                Ok(value) => { sources.set(value); selected.set(0); picking.set(true); }
                                Err(message) => error.set(message),
                            }
                            busy.set(false);
                        });
                    }>"Share screen"</button>
            }>
                <span class="text-sm text-green-300">{move || status.get().message}</span>
                <span class="text-xs text-white/60">{move || status.get().encoder.unwrap_or_default()}</span>
                <button class="rounded bg-red-700 px-3 py-2 text-sm" on:click=move |_| {
                    leptos::task::spawn_local(async move {
                        if let Err(message) = native::<()>("screen_stop", json!({})).await && error.try_get_untracked().is_some() { error.set(message); }
                    });
                }>"Stop sharing"</button>
            </Show>
            <DiagnosticsPanel label="Screen pipeline diagnostics" text=diagnostics/>
            <Show when=move || picking.get()>
                <section class="w-full space-y-3 rounded-lg border border-white/20 bg-zinc-900 p-4" aria-label="Choose what to share">
                    <p class="text-sm">"Choose a screen or window. Everyone in this voice channel can watch. Sharing a screen includes notifications and other visible windows."</p>
                    <select class="max-w-full rounded bg-black/40 p-2" aria-label="Screen or window" prop:value=move || selected.get().to_string()
                        on:change=move |event| selected.set(event_target_value(&event).parse().unwrap_or(0))>
                        {move || sources.get().into_iter().enumerate().map(|(index, source)| view! { <option value=index.to_string()>{source.label}</option> }).collect_view()}
                    </select>
                    <div class="flex flex-wrap gap-3">
                        <label class="text-sm">"Resolution "<select class="rounded bg-black/40 p-2" prop:value=move || quality.get().height.to_string()
                            on:change=move |event| { if let Ok(height) = event_target_value(&event).parse() { quality.update(|q| q.height = height); } }>
                            <option value="720">"720p"</option><option value="1080">"1080p"</option><option value="1440">"1440p"</option><option value="2160">"4K (2160p)"</option>
                        </select></label>
                        <label class="text-sm">"Frame rate "<select class="rounded bg-black/40 p-2" prop:value=move || quality.get().fps.to_string()
                            on:change=move |event| { if let Ok(fps) = event_target_value(&event).parse() { quality.update(|q| q.fps = fps); } }>
                            <option value="15">"15 fps"</option><option value="30">"30 fps"</option><option value="60">"60 fps"</option>
                        </select></label>
                    </div>
                    <p class="text-xs text-white/60">"Target quality. Higher settings use more bandwidth and CPU; actual frame rate depends on your computer and connection. Smaller sources retain their original size."</p>
                    <label class="flex items-center gap-2 text-sm"><input type="checkbox" prop:checked=move || audio.get() on:change=move |event| audio.set(event_target_checked(&event))/>
                        "Share system audio (all other apps, even when sharing one window)"</label>
                    <p class="text-xs text-white/60">"Thiscord playback is excluded. Microphone mute does not mute shared audio. Stop sharing to stop both video and system audio."</p>
                    <button class="rounded bg-brand px-3 py-2 disabled:opacity-50" disabled=move || busy.get() || sources.get().is_empty()
                        on:click=move |_| {
                            let Some(source) = sources.get_untracked().get(selected.get_untracked()).cloned() else { return; };
                            let include_audio = audio.get_untracked();
                            let target_quality = quality.get_untracked();
                            busy.set(true);
                            leptos::task::spawn_local(async move {
                                let result = native::<()>("screen_start", json!({"source":source.id,"audio":include_audio,"quality":target_quality})).await;
                                if busy.try_get_untracked().is_none() { return; }
                                match result {
                                    Ok(()) => picking.set(false), Err(message) => error.set(message),
                                }
                                busy.set(false);
                            });
                        }>"Start sharing"</button>
                    <button class="ml-3 text-sm underline" on:click=move |_| picking.set(false)>"Cancel"</button>
                </section>
            </Show>
            <Show when=move || !error.get().is_empty()><p class="w-full text-sm text-red-300" role="alert">{move || error.get()}</p></Show>
            <Show when=move || !status.get().sharing && !status.get().message.is_empty()><p class="text-xs text-white/60" role="status">{move || status.get().message}</p></Show>
        </div>
    }
}

static NEXT_VIEWER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

#[component]
pub(super) fn ScreenViewer(ui: Ui) -> impl IntoView {
    view! { <div class="grid gap-4">
        <For each=move || { ui.voice.get().participants.into_iter().filter(|m| m.sharing_screen && !ui.audio.get().deafened).collect::<Vec<_>>() } key=|m| (m.account_id, m.slot, m.screen_epoch)
            children=move |member| {
                let own = ui.account.get_untracked().is_some_and(|a| a.id == member.account_id);
                let watch = thiscord_shared::screen::Watch { slot: member.slot, owner: member.account_id, epoch: member.screen_epoch, viewer: NEXT_VIEWER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) };
                let expanded = RwSignal::new(false);
                let escape = window_event_listener(leptos::ev::keydown, move |event| {
                    if event.key() == "Escape" { expanded.set(false); }
                });
                on_cleanup(move || escape.remove());
                view! { <figure class=move || if expanded.get() {
                    "fixed inset-0 z-50 flex min-h-0 flex-col overflow-auto bg-black text-white"
                } else { "overflow-hidden rounded-lg border border-white/15 bg-black" }>
                    <figcaption class="flex shrink-0 flex-wrap items-center justify-between gap-3 p-3 text-sm">
                        <span>{member.username}" is sharing"{if own { " (you)" } else { "" }}</span>
                        {(!own).then(|| view! {
                            <button class="rounded bg-white/10 px-3 py-2" aria-pressed=move || expanded.get().to_string()
                                on:click=move |_| expanded.update(|value| *value = !*value)>
                                {move || if expanded.get() { "Exit full screen" } else { "Full screen" }}
                            </button>
                        })}
                    </figcaption>
                    {(!own).then(|| view! {
                        <div class="shrink-0 px-3 pb-3">
                            <For each=move || { ui.audio_status.get().map(|s| s.streams).unwrap_or_default().into_iter().filter(|s| s.target.is_some_and(|t| t.account_id == watch.owner && t.shared_audio)).collect::<Vec<_>>() } key=|s| (s.id.clone(), s.target)
                                children=move |stream| view! { <super::audio::StreamVolume ui=ui stream=stream/> }/>
                        </div>
                        <ScreenVideo watch=watch expanded=expanded/>
                    })}
                </figure> }
            }/>
    </div> }
}

#[component]
fn ScreenVideo(watch: thiscord_shared::screen::Watch, expanded: RwSignal<bool>) -> impl IntoView {
    let video = NodeRef::<leptos::html::Video>::new();
    let message = RwSignal::new("Connecting screen video...".to_owned());
    let diagnostics = RwSignal::new(String::new());
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    let registration = std::cell::RefCell::new(Some(registration));
    on_cleanup(move || {
        abort.abort();
        leptos::task::spawn_local(async move {
            let _ = native::<bool>(
                "screen_view_keepalive",
                json!({"watch":watch,"visible":false}),
            )
            .await;
        });
    });
    Effect::new(move |_| {
        let Some(video) = video.get() else {
            return;
        };
        let Some(registration) = registration.borrow_mut().take() else {
            return;
        };
        let video: web_sys::HtmlVideoElement = video.clone();
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                super::screen_player::run(video, watch, message, diagnostics),
                registration,
            )
            .await;
        });
    });
    view! {
        <Show when=move || !message.get().is_empty()><p class="p-4 text-sm text-white/60" role="status">{move || message.get()}</p></Show>
        <DiagnosticsPanel label="Playback diagnostics" text=diagnostics/>
        <video node_ref=video autoplay muted playsinline class=move || if expanded.get() { "min-h-0 w-full flex-1 object-contain" } else { "max-h-[65vh] w-full object-contain" } aria-label="Live shared screen"/>
    }
}

#[component]
fn DiagnosticsPanel(label: &'static str, text: RwSignal<String>) -> impl IntoView {
    let copied = RwSignal::new(String::new());
    view! {
        <details class="w-full p-3 text-xs text-white/60">
            <summary>{label}</summary>
            <button class="my-2 rounded bg-white/10 px-3 py-1" on:click=move |_| {
                let data = text.get_untracked();
                leptos::task::spawn_local(async move {
                    if let Some(window) = web_sys::window() {
                        let result = wasm_bindgen_futures::JsFuture::from(window.navigator().clipboard().write_text(&data)).await;
                        if copied.try_get_untracked().is_some() { copied.set(if result.is_ok() { "Copied" } else { "Copy failed; select the text below" }.into()); }
                    }
                });
            }>"Copy diagnostics"</button>
            <span class="ml-2" role="status">{move || copied.get()}</span>
            <pre class="max-h-72 overflow-auto whitespace-pre-wrap">{move || text.get()}</pre>
        </details>
    }
}
