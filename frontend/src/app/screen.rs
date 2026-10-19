use super::Ui;
use super::icons::{Glyph, Icon};
use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::{
    ChannelId, GuildId,
    permissions::ChannelKind,
    screen::{Quality, Source, SourceId, Status},
    voice::Participant,
};

pub(super) type StreamTarget = thiscord_frontend::stream_ui::Selection;

pub(super) fn open_stream(ui: Ui, guild: GuildId, channel: ChannelId, member: Participant) {
    if !member.sharing_screen && !member.camera_active {
        return;
    }
    if !crate::account_client::desktop() {
        ui.status
            .set("Open the desktop app to watch streams".into());
        return;
    }
    if ui.voice_joining.get_untracked() {
        ui.status.set("Joining voice… Please wait".into());
        return;
    }
    let selected = StreamTarget {
        guild,
        channel,
        owner: member.account_id,
        epoch: member.screen_epoch,
    };
    leptos::task::spawn_local(async move {
        if let Err(error) = super::servers::select(ui, guild).await {
            ui.status.set(error);
            return;
        }
        let Some(home) = ui.server.get_untracked().filter(|s| s.guild.id == guild) else {
            return;
        };
        let Some(channel) = home
            .channels
            .iter()
            .find(|c| c.id == channel && c.kind == ChannelKind::Voice)
            .cloned()
        else {
            ui.status
                .set("This voice channel is no longer accessible".into());
            return;
        };
        ui.active_stream.set(Some(selected));
        ui.selected_channel.set(Some(channel.clone()));
        if ui.voice.get_untracked().channel_id != Some(channel.id) {
            ui.status.set(
                "Joining the stream’s voice channel using your current microphone settings".into(),
            );
            super::audio::join(ui, home.guild, channel);
        }
    });
}

#[component]
pub(super) fn ScreenControls(ui: Ui) -> impl IntoView {
    let status = RwSignal::new(Status::default());
    let sources = RwSignal::new(Vec::<Source>::new());
    let selected = RwSignal::new(None::<usize>);
    let screens = RwSignal::new(true);
    let picking = RwSignal::new(false);
    let audio = RwSignal::new(true);
    let quality = RwSignal::new(Quality::default());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        if picking.get()
            && let Some(d) = dialog.get()
        {
            let _ = d.show_modal();
        }
    });
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
    let choose = move |_| {
        picking.set(true);
        busy.set(true);
        error.set(String::new());
        selected.set(None);
        sources.set(vec![]);
        leptos::task::spawn_local(async move {
            let result = native::<Vec<Source>>("screen_sources", json!({})).await;
            if busy.is_disposed() {
                return;
            }
            match result {
                Ok(value) => {
                    screens.set(value.iter().any(|s| matches!(s.id, SourceId::Monitor(_))));
                    sources.set(value);
                }
                Err(message) => error.set(message),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="screen-control" aria-label="Screen sharing">
            <Show when=move ||status.get().sharing fallback=move ||view!{
                <button class="icon-button voice-action" aria-label="Share screen" data-tip=move ||if ui.audio.get().deafened{"Undeafen to share"}else if !status.get().available{"Screen sharing unavailable"}else{"Share screen"} disabled=move ||!status.get().available||ui.audio.get().deafened||busy.get() on:click=choose><Icon kind=Glyph::Screen/></button>
            }>
                <button class="icon-button voice-action share-stop" aria-label="Stop sharing" data-tip="Stop sharing" disabled=move ||busy.get() on:click=move |_|{
                    busy.set(true);error.set(String::new());
                    leptos::task::spawn_local(async move {
                        let result=native::<()>("screen_stop",json!({})).await;
                        if busy.is_disposed(){return;}
                        match result{Ok(())=>status.update(|s|s.sharing=false),Err(message)=>error.set(message)}
                        busy.set(false);
                    });
                }><Icon kind=Glyph::Screen/><span class="share-stop-dot"></span></button>
            </Show>
            <Show when=move ||picking.get()>
                <dialog node_ref=dialog class="share-picker" aria-labelledby="share-picker-title" on:cancel=move |e:web_sys::Event|{if busy.get_untracked(){e.prevent_default();}} on:click=move |e|super::dialog::dismiss_outside(e,dialog,busy.get_untracked()) on:close=move |_|picking.set(false)>
                    <header class="share-picker-header"><div><span class="settings-kicker">"GO LIVE"</span><h2 id="share-picker-title">"Share your screen"</h2><p>"Choose what people in your call can see."</p></div><button class="icon-button" aria-label="Close screen picker" disabled=move ||busy.get() on:click=move |_|picking.set(false)><Icon kind=Glyph::Close/></button></header>
                    <div class="share-picker-body">
                        <div class="share-source-tabs" role="group" aria-label="Source type"><button aria-pressed=move ||screens.get() on:click=move |_|{screens.set(true);selected.set(None);}><Icon kind=Glyph::Screen/>"Screens"</button><button aria-pressed=move ||!screens.get() on:click=move |_|{screens.set(false);selected.set(None);}><Icon kind=Glyph::Window/>"Windows"</button></div>
                        <Show when=move ||!busy.get()||!sources.get().is_empty() fallback=||view!{<p class="share-empty" role="status">"Finding screens and windows…"</p>}>
                            <div class="share-source-grid" role="group" aria-label="Screens and windows">
                                {move ||sources.get().into_iter().enumerate().filter(|(_,s)|matches!(s.id,SourceId::Monitor(_))==screens.get()).map(|(index,source)|{
                                    let monitor=matches!(source.id,SourceId::Monitor(_));
                                    view!{<button class="share-source" aria-pressed=move ||selected.get()==Some(index) on:click=move |_|selected.set(Some(index))><span class="share-source-icon"><Icon kind=if monitor{Glyph::Screen}else{Glyph::Window}/></span><span class="share-source-name">{source.label}</span><span class="share-source-check" aria-hidden="true">{move ||if selected.get()==Some(index){"✓"}else{""}}</span></button>}
                                }).collect_view()}
                            </div>
                            <Show when=move ||!sources.get().iter().any(|s|matches!(s.id,SourceId::Monitor(_))==screens.get())><p class="share-empty">"No sources found. Try the other tab or open a window."</p></Show>
                        </Show>
                        <div class="share-quality"><label>"Resolution"<select aria-label="Share resolution" prop:value=move ||quality.get().height.to_string() on:change=move |e|if let Ok(height)=event_target_value(&e).parse(){quality.update(|q|q.height=height);}>
                            <option value="720">"720p"</option><option value="1080">"1080p · Recommended"</option><option value="1440">"1440p"</option><option value="2160">"4K"</option></select></label>
                            <label>"Frame rate"<select aria-label="Share frame rate" prop:value=move ||quality.get().fps.to_string() on:change=move |e|if let Ok(fps)=event_target_value(&e).parse(){quality.update(|q|q.fps=fps);}>
                            <option value="15">"15 fps · Presentations"</option><option value="30">"30 fps · Balanced"</option><option value="60">"60 fps · Smooth"</option></select></label>
                        </div>
                        <label class="share-audio-toggle"><span><strong>"Include system audio"</strong><small>"Audio from all other apps, even when sharing a single window."</small></span><input type="checkbox" prop:checked=move ||audio.get() on:change=move |e|audio.set(event_target_checked(&e))/></label>
                        <p class="share-picker-note">"Quality adapts to the connection. Microphone mute doesn’t mute shared system audio."</p>
                        <p class="share-picker-error" role="alert">{move ||error.get()}</p>
                    </div>
                    <footer class="share-picker-footer"><p>"Your selected source is visible to everyone watching in this call."</p><button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|picking.set(false)>"Cancel"</button><button class="settings-primary" disabled=move ||busy.get()||selected.get().is_none()||ui.audio.get().deafened on:click=move |_|{
                        let Some(source)=selected.get_untracked().and_then(|i|sources.get_untracked().get(i).cloned()) else{return;};
                        busy.set(true);error.set(String::new());let quality=quality.get_untracked();let audio=audio.get_untracked();
                        leptos::task::spawn_local(async move {
                            let result=native::<()>("screen_start",json!({"source":source.id,"audio":audio,"quality":quality})).await;
                            if busy.is_disposed(){return;}
                            match result{Ok(())=>{status.update(|s|s.sharing=true);picking.set(false);},Err(message)=>error.set(message)}
                            busy.set(false);
                        });
                    }>{move ||if busy.get(){"Please wait…"}else{"Go live"}}</button></footer>
                </dialog>
            </Show>
            <Show when=move ||!error.get().is_empty()&&!picking.get()><p class="screen-error text-xs text-red-300" role="alert">{move ||error.get()}</p></Show>
        </div>
    }
}

static NEXT_VIEWER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

#[component]
pub(super) fn ShareDiagnostics() -> impl IntoView {
    let diagnostics = RwSignal::new(String::new());
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                let mut previous = Default::default();
                loop {
                    if let Ok(status) = native::<Status>("screen_status", json!({})).await {
                        diagnostics.set(super::screen_player::describe(
                            &status.diagnostics,
                            &previous,
                        ));
                        previous = status.diagnostics;
                    }
                    gloo_timers::future::TimeoutFuture::new(1000).await;
                }
            },
            registration,
        )
        .await;
    });
    view! { <DiagnosticsPanel label="Screen pipeline diagnostics" text=diagnostics/> }
}

#[component]
pub(super) fn ScreenViewer(ui: Ui, guild: GuildId, channel: ChannelId) -> impl IntoView {
    use thiscord_shared::screen::Watch;
    use thiscord_shared::voice::MediaKind;
    let hidden = RwSignal::new(Vec::<(thiscord_shared::AccountId, MediaKind, u32)>::new());
    let menu = RwSignal::new(None::<(thiscord_shared::AccountId, MediaKind, u32)>);
    let expanded = RwSignal::new(None::<(thiscord_shared::AccountId, MediaKind, u32)>);
    let tiles = Memo::new(move |_| {
        let voice = ui.voice.get();
        if !voice.connected || voice.channel_id != Some(channel) {
            return vec![];
        }
        voice
            .participants
            .into_iter()
            .flat_map(|m| {
                let mut tiles = vec![];
                if m.camera_active {
                    tiles.push((m.clone(), MediaKind::Camera, m.camera_epoch));
                }
                if m.sharing_screen {
                    tiles.push((m.clone(), MediaKind::ScreenVideo, m.screen_epoch));
                }
                tiles
            })
            .collect::<Vec<_>>()
    });
    let visible = Memo::new(move |_| {
        tiles
            .get()
            .into_iter()
            .filter(|(m, kind, epoch)| !hidden.get().contains(&(m.account_id, *kind, *epoch)))
            .collect::<Vec<_>>()
    });
    let escape = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            menu.set(None);
            expanded.set(None);
        }
    });
    let fullscreen = window_event_listener(leptos::ev::fullscreenchange, move |_| {
        if document().fullscreen_element().is_none() {
            expanded.set(None);
        }
    });
    let outside = window_event_listener(leptos::ev::pointerdown, move |_| menu.set(None));
    on_cleanup(move || {
        escape.remove();
        fullscreen.remove();
        outside.remove();
    });
    Effect::new(move |_| {
        let current = tiles.get();
        hidden.update(|h| {
            h.retain(|key| {
                current
                    .iter()
                    .any(|(m, k, e)| *key == (m.account_id, *k, *e))
            })
        });
        if expanded.get_untracked().is_some_and(|key| {
            !current
                .iter()
                .any(|(m, k, e)| key == (m.account_id, *k, *e))
        }) {
            expanded.set(None);
        }
    });
    view! {<div class="channel-streams">
        <Show when=move ||!hidden.get().is_empty()><button class="stream-choice" on:click=move |_|hidden.set(vec![])>"Show hidden items"</button></Show>
        <Show when=move ||visible.get().is_empty()><p class="voice-channel-empty">{move || {
            let voice = ui.voice.get();
            if !voice.connected || voice.channel_id != Some(channel) { "Join this call to see cameras and screen shares." }
            else if !tiles.get().is_empty() { "All media is hidden. Choose Show hidden items to restore it." }
            else { "No cameras or screen shares are active." }
        }}</p></Show>
        <div class="media-grid" style:--media-columns=move ||match visible.get().len() {0|1=>"1",2..=4=>"2",5..=9=>"3",_=>"4"}>
        <For each=move ||visible.get() key=|(m,k,e)|(m.account_id,m.slot,*k,*e) children=move |(member,kind,epoch)| {
            let owner=member.account_id;
            let key=(owner,kind,epoch);
            let own=ui.account.get_untracked().is_some_and(|a|a.id==owner);
            let watch=Watch{kind,slot:member.slot,owner,epoch,viewer:0};
            let figure=NodeRef::<leptos::html::Figure>::new();
            let full=move || {
                if expanded.get_untracked()==Some(key) {expanded.set(None);document().exit_fullscreen();}
                else {expanded.set(Some(key));if let Some(node)=figure.get_untracked(){let _=node.request_fullscreen();}}
            };
            view!{<figure node_ref=figure class="stream-view media-tile" class:expanded=move ||expanded.get()==Some(key)
                on:contextmenu=move |e|{e.prevent_default();e.stop_propagation();ui.context_menu.set(None);menu.set(Some(key));}>
                <figcaption class="stream-toolbar"><button class="stream-title" on:click=move |_|full()>{member.display_name().to_owned()}<span class="stream-live-badge">{if kind==MediaKind::Camera {"CAMERA"} else {"SCREEN"}}</span></button>
                    <button class="icon-button" aria-label="Toggle fullscreen" on:click=move |_|full()><Icon kind=Glyph::Expand/></button>
                </figcaption>
                <div class="media-tile-content" role="button" tabindex="0" aria-label="View fullscreen" on:click=move |_|full() on:keydown=move |e|{if e.key()=="Enter" || e.key()==" "{e.prevent_default();full();}}>
                    <Show when=move ||!ui.audio.get().deafened fallback=||view!{<div class="stream-placeholder">"Undeafen to view media"</div>}>
                        {if own && kind==MediaKind::Camera {view!{<super::camera::LocalCamera/>}.into_any()}

                        else {view!{<ScreenVideo watch=watch/>}.into_any()}}
                    </Show>
                </div>
                <StreamWatchers ui=ui owner=owner kind=kind epoch=epoch/>
                <Show when=move ||menu.get()==Some(key)>
                    <div class="media-item-menu" role="menu" aria-label="Media options" on:pointerdown=move |e|e.stop_propagation() on:click=move |e|e.stop_propagation()>
                        <button role="menuitem" class="stream-choice" on:click=move |_|{hidden.update(|h|h.push(key));menu.set(None);if expanded.get_untracked()==Some(key){expanded.set(None);document().exit_fullscreen();}}>"Hide this item"</button>
                        {(!own).then(||view!{<super::context_menu::ParticipantVolume ui=ui guild=guild channel=channel member=owner/>})}
                    </div>
                </Show>
            </figure>}
        }/>
        </div>
    </div>}
}

#[component]
fn StreamWatchers(
    ui: Ui,
    owner: thiscord_shared::AccountId,
    kind: thiscord_shared::voice::MediaKind,
    epoch: u32,
) -> impl IntoView {
    let viewers = Memo::new(move |_| {
        let voice = ui.voice.get();
        if !voice.connected {
            return vec![];
        }
        let Some(source) = voice.participants.iter().find(|p| {
            p.account_id == owner && p.video_active(kind) && p.video_epoch(kind) == epoch
        }) else {
            return vec![];
        };
        voice
            .participants
            .iter()
            .filter(|p| source.video_viewers(kind).contains(&p.account_id))
            .cloned()
            .collect::<Vec<_>>()
    });
    view! {
        <Show when=move ||!viewers.get().is_empty()>
            <div class="stream-watchers" role="group" aria-label="Watching this stream">
                <For each=move ||viewers.get() key=|p|p.account_id children=move |initial| {
                    let id=initial.account_id;
                    let person=Signal::derive(move ||ui.voice.get().participants.into_iter().find(|p|p.account_id==id).unwrap_or_else(||initial.clone()));
                    view! {<span class="stream-watcher" tabindex="0" role="img"
                        aria-label=move ||format!("Watching: {}",person.get().display_name())
                        data-tip=move ||person.get().display_name().to_owned()>
                        <crate::avatar::Avatar class="size-6 text-[10px]" name=Signal::derive(move ||person.get().display_name().to_owned()) avatar_id=Signal::derive(move ||person.get().avatar_id)/>
                    </span>}
                }/>
            </div>
        </Show>
    }
}

#[component]
fn ScreenVideo(watch: thiscord_shared::screen::Watch) -> impl IntoView {
    // Each mount owns a separate lease, including after undeafening the same share.
    let watch = thiscord_shared::screen::Watch {
        viewer: NEXT_VIEWER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ..watch
    };
    let video = NodeRef::<leptos::html::Video>::new();
    let message = RwSignal::new("Connecting video...".to_owned());
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
        <div class="stream-player"><video node_ref=video autoplay muted playsinline aria-label="Live video"/>
        <Show when=move ||!message.get().is_empty()><p class="stream-player-status" role="status">{move ||message.get()}</p></Show></div>

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
