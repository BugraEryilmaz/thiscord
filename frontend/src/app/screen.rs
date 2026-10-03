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
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                loop {
                    if let Ok(value) = native::<Status>("screen_status", json!({})).await {
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
                <button class="rounded bg-red-700 px-3 py-2 text-sm" on:click=move |_| {
                    leptos::task::spawn_local(async move {
                        if let Err(message) = native::<()>("screen_stop", json!({})).await && error.try_get_untracked().is_some() { error.set(message); }
                    });
                }>"Stop sharing"</button>
            </Show>
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

#[component]
pub(super) fn ScreenViewer(ui: Ui) -> impl IntoView {
    view! { <div class="grid gap-4">
        <For each=move || { ui.voice.get().participants.into_iter().filter(|m| m.sharing_screen && !ui.audio.get().deafened).collect::<Vec<_>>() } key=|m| (m.account_id, m.slot, m.screen_epoch)
            children=move |member| {
                let ready = RwSignal::new(false);
                let tick = RwSignal::new(0_u64);
                let pending = RwSignal::new(true);
                let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                on_cleanup(move || abort.abort());
                let own = ui.account.get_untracked().is_some_and(|a| a.id == member.account_id);
                let path = crate::account_client::media_url(&format!("{}-{}-{}", member.slot, member.screen_epoch, member.account_id));
                if !own {
                    leptos::task::spawn_local(async move {
                        let _ = futures_util::future::Abortable::new(async move {
                            loop {
                                gloo_timers::future::TimeoutFuture::new(16).await;
                                // One request at a time: do not cancel slow high-resolution loads.
                                if !pending.get_untracked() {
                                    pending.set(true);
                                    tick.update(|n| *n = n.wrapping_add(1));
                                }
                            }
                        }, registration).await;
                    });
                }
                view! { <figure class="overflow-hidden rounded-lg border border-white/15 bg-black">
                    <figcaption class="p-3 text-sm">{member.username}" is sharing"{if own { " (you)" } else { "" }}</figcaption>
                    {(!own).then(|| view! { <Show when=move || !ready.get()><p class="p-4 text-sm text-white/60">"Waiting for screen video..."</p></Show><img class=move || if ready.get() { "max-h-[65vh] w-full object-contain" } else { "hidden" } on:load=move |_| { ready.set(true); pending.set(false); } on:error=move |_| { ready.set(false); pending.set(false); } alt="Live shared screen" src=move || format!("{path}?frame={}", tick.get())/> })}
                </figure> }
            }/>
    </div> }
}
