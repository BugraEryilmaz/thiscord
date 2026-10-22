use super::{Ui, run};
use leptos::prelude::*;
use thiscord_shared::{ApiError, account::AccountRequest, admin::*};

#[component]
pub(super) fn Dashboard(ui: Ui) -> impl IntoView {
    let data = RwSignal::new(None::<Diagnostics>);
    let previous = RwSignal::new(
        None::<(
            thiscord_shared::Timestamp,
            thiscord_shared::performance::BackendPerformance,
        )>,
    );
    let selected = RwSignal::new(None::<Selection>);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let filter = RwSignal::new(String::new());
    let refresh = move || {
        if busy.get_untracked() {
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let fetch = async {
                let response = gloo_net::http::Request::get(&format!(
                    "{}{DIAGNOSTICS_PATH}",
                    crate::account_client::api_base()
                ))
                .header("Authorization", &format!("Bearer {token}"))
                .send()
                .await
                .map_err(|_| "Cannot reach the server. Diagnostics are unavailable.".to_string())?;
                if !response.ok() {
                    let status = response.status();
                    let detail = response
                        .json::<ApiError>()
                        .await
                        .map(|e| format!("{} (request {})", e.message, e.request_id))
                        .unwrap_or_else(|_| format!("Server returned HTTP {status}"));
                    return Err(if status == 403 {
                        format!(
                            "Access denied. Only the instance owner and instance admins can view diagnostics. {detail}"
                        )
                    } else {
                        detail
                    });
                }
                response
                    .json::<Diagnostics>()
                    .await
                    .map_err(|_| "Invalid diagnostic response".into())
            };
            let result = match futures_util::future::select(
                Box::pin(fetch),
                Box::pin(gloo_timers::future::TimeoutFuture::new(4000)),
            )
            .await
            {
                futures_util::future::Either::Left((result, _)) => result,
                futures_util::future::Either::Right(_) => {
                    Err("The server did not respond in time. Diagnostics are unavailable.".into())
                }
            };
            // Ignore responses after logout/unmount; never restore an old user's data.
            if ui.token.try_get_untracked().flatten().as_deref() != Some(token.as_str())
                || data.is_disposed()
            {
                return;
            }
            match result {
                Ok(value) => {
                    previous.set(data.with_untracked(|d| {
                        d.as_ref()
                            .filter(|d| value.uptime_seconds >= d.uptime_seconds)
                            .map(|d| (d.sampled_at, d.performance.clone()))
                    }));
                    data.set(Some(value));
                    error.set(String::new());
                }
                Err(message) => {
                    previous.set(None);
                    data.set(None);
                    selected.set(None);
                    error.set(message);
                }
            }
            busy.set(false);
        });
    };
    refresh();
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                loop {
                    gloo_timers::future::TimeoutFuture::new(5000).await;
                    refresh();
                }
            },
            registration,
        )
        .await;
    });
    view! {
        <main class="min-h-screen bg-slate-950 px-5 py-8 text-slate-100 md:px-10">
            <div class="mx-auto max-w-7xl space-y-8">
                <header class="flex flex-wrap items-start justify-between gap-5 border-b border-slate-800 pb-6">
                    <div><p class="mb-2 text-xs font-semibold uppercase tracking-widest text-indigo-400">"Thiscord / Administration"</p><h1 class="text-3xl font-semibold tracking-tight">"Instance overview"</h1><p class="mt-2 text-sm text-slate-400">"Live infrastructure and room diagnostics"</p></div>
                    <div class="flex flex-wrap items-center gap-3"><button class="icon-button" disabled=move ||busy.get() on:click=move |_|refresh() aria-label="Refresh now" data-tip="Refresh now"><super::icons::Icon kind=super::icons::Glyph::Refresh/></button><button class="rounded-lg bg-slate-800 px-4 py-2 text-sm hover:bg-slate-700 disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::Logout)>"Sign out"</button></div>
                </header>
                <p class="text-sm text-amber-300" role="alert">{move ||error.get()}</p>
                <Show when=move ||data.get().is_some()><label class="block text-sm text-slate-400">"Filter active rooms"<input aria-label="Filter rooms or users" type="search" placeholder="Find a room, server or user…" class="mt-2 block w-full rounded-lg border border-slate-700 bg-slate-900 px-4 py-2 text-sm sm:ml-4 sm:inline-block sm:w-80" prop:value=move ||filter.get() on:input=move |e|filter.set(event_target_value(&e))/></label></Show>
                <Show when=move ||data.get().is_none() && error.get().is_empty()><p class="text-slate-400" role="status">"Checking access and loading diagnostics…"</p></Show>
                {move ||data.get().map(|d| {
                    let participants: usize = d.rooms.iter().map(|r| r.participants.len()).sum();
                    let unique = d.rooms.iter().flat_map(|r|r.participants.iter().map(|p|p.participant.account_id)).collect::<std::collections::HashSet<_>>().len();
                    let memory = d.host.memory_used_bytes.zip(d.host.memory_total_bytes).map(|(used,total)|format!("{} / {}",bytes(used),bytes(total))).unwrap_or_else(||"—".into());
                    let memory_percent = d.host.memory_used_bytes.zip(d.host.memory_total_bytes).filter(|(_,t)|*t>0).map(|(u,t)|format!("{:.1}% used",100.0*u as f64/t as f64)).unwrap_or_else(||"Unavailable on this host".into());
                    view! {
                        <div class="flex flex-wrap items-center justify-between gap-3 text-xs text-slate-400"><span class="rounded-full border border-emerald-900 bg-emerald-950 px-3 py-1 text-emerald-300">"Auto-refresh · 5 seconds"</span><span>{format!("Updated {} UTC · {:?}",d.sampled_at.format("%H:%M:%S"), d.role)}</span></div>
                        <section aria-label="Instance metrics" class="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
                            <Metric label="Host CPU" value=d.host.cpu_percent.map(|v|format!("{v:.1}%")).unwrap_or_else(||"—".into()) detail="Across all host CPU cores".into()/>
                            <Metric label="Host RAM" value=memory detail=memory_percent/>
                            <Metric label="Active voice rooms" value=d.rooms.len().to_string() detail=format!("{participants} participant connections")/>
                            <Metric label="Active voice users" value=unique.to_string() detail="Unique accounts across rooms".into()/>
                        </section>
                        <section aria-label="Backend health" class="grid gap-5 rounded-xl border border-slate-800 bg-slate-900/60 p-5 text-sm sm:grid-cols-2 lg:grid-cols-4">
                            <div><p class="text-slate-400">"Backend uptime"</p><p class="mt-2 font-mono">{format!("{}h {}m · v{}",d.uptime_seconds/3600,(d.uptime_seconds/60)%60,d.version)}</p></div>
                            <div><p class="text-slate-400">"Backend RAM (RSS)"</p><p class="mt-2 font-mono">{d.host.process_memory_bytes.map(bytes).unwrap_or_else(||"—".into())}</p></div>
                            <div><p class="text-slate-400">"Database pool"</p><p class="mt-2 font-mono">{format!("{} open / {} idle",d.database_connections,d.database_idle_connections)}</p></div>
                            <div><p class="text-slate-400">"Load average · 1 / 5 / 15 min"</p><p class="mt-2 font-mono">{d.host.load_average.map(|v|format!("{:.2} / {:.2} / {:.2}",v[0],v[1],v[2])).unwrap_or_else(||"—".into())}</p></div>
                        </section>
                        <BackendPerformance current=d.performance previous=previous.get_untracked() sampled_at=d.sampled_at/>
                        <details class="rounded-xl border border-slate-800 p-5"><summary class="cursor-pointer text-sm">"This admin browser's UI measurements"</summary><UiPerformance measurements=crate::performance::snapshot()/></details>
                        <section class="space-y-4" aria-labelledby="rooms-title">
                            <h2 id="rooms-title" class="text-xl font-semibold">"Active rooms"</h2>
                            <p class="max-w-4xl text-sm leading-relaxed text-slate-400">"Latency is the user ↔ server transport round trip. Jitter is microphone packet arrival variation at the server. Values are milliseconds; — means no recent measurement. Muted users may have no microphone samples."</p>
                            {if d.rooms.is_empty() {view!{<div class="rounded-xl border border-dashed border-slate-700 p-12 text-center"><p class="font-medium">"No active voice rooms"</p><p class="mt-2 text-sm text-slate-400">"Rooms appear here when someone connects."</p></div>}.into_any()} else {
                                let rooms=d.rooms;
                                view!{{move || {
                                    let query=filter.get().to_lowercase();
                                    let visible:Vec<_>=rooms.iter().filter(|r|format!("{} {} {}",r.guild_name,r.channel_name,r.participants.iter().map(|p|format!("{} {}",p.participant.username,p.participant.display_name)).collect::<Vec<_>>().join(" ")).to_lowercase().contains(&query)).cloned().collect();
                                    if visible.is_empty() {view!{<p class="py-8 text-slate-400">"No rooms match your search."</p>}.into_any()} else {visible.into_iter().map(|room|view!{<Room room=room sampled_at=d.sampled_at selected=selected ui=ui owner=d.role==thiscord_shared::permissions::InstanceRole::Owner/>}).collect_view().into_any()}
                                }}}.into_any()
                            }}
                        </section>
                    }
                })}
                <For each=move || {selected.get().into_iter().collect::<Vec<_>>()} key=|s| (s.connection, s.name.clone()) children=move |selection| view! {<RemoteAudioPanel ui=ui selection=selection close=Callback::new(move |_| selected.set(None))/>}/>
                <footer class="border-t border-slate-800 pt-5 text-xs leading-relaxed text-slate-500">"Instance diagnostics and remote desktop audio support. Host values describe the Linux host or VM. Room data covers this backend process. No message content or media is exposed."</footer>
            </div>
        </main>
    }
}

#[component]
fn Metric(label: &'static str, value: String, detail: String) -> impl IntoView {
    view! {<article class="rounded-xl border border-slate-800 bg-slate-900 p-5"><h2 class="text-sm text-slate-400">{label}</h2><p class="mt-4 text-2xl font-semibold tracking-tight">{value}</p><p class="mt-3 text-xs text-slate-500">{detail}</p></article>}
}
fn bytes(value: u64) -> String {
    format!("{:.2} GiB", value as f64 / 1_073_741_824.0)
}
fn ms(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1} ms"))
        .unwrap_or_else(|| "—".into())
}

#[component]
fn Room(
    room: RoomDiagnostics,
    sampled_at: thiscord_shared::Timestamp,
    selected: RwSignal<Option<Selection>>,
    ui: Ui,
    owner: bool,
) -> impl IntoView {
    let bot_target = super::voice_bots::Target {
        guild: room.guild_id,
        channel: room.channel_id,
        name: room.channel_name.clone(),
    };
    view! {
        <article class="mb-4 overflow-hidden rounded-xl border border-slate-800 bg-slate-900/60">
            <header class="flex flex-wrap items-center justify-between gap-3 border-b border-slate-800 px-5 py-4"><div><h3 class="font-semibold">{room.channel_name}</h3><p class="mt-1 text-xs text-slate-400">{room.guild_name}</p></div><span class="rounded-full bg-indigo-500/10 px-3 py-1 text-xs text-indigo-300">{format!("{} people",room.participants.len())}</span></header>
            <div class="overflow-x-auto"><table class="w-full whitespace-nowrap text-left text-sm"><thead class="text-xs text-slate-400"><tr><th class="px-5 py-3">"Participant"</th><th class="px-5 py-3">"State"</th><th class="px-5 py-3">"Latency (RTT)"</th><th class="px-5 py-3">"Mic jitter"</th><th class="px-5 py-3">"Mic received / lost"</th><th class="px-5 py-3">"Video drops in / out"</th><th class="px-5 py-3">"Video sent / timeouts"</th></tr></thead><tbody>
            {room.participants.into_iter().map(move |p| {
                let bot_target = bot_target.clone();
                let is_bot = p.participant.test_bot;
                let selection = Selection { connection: p.audio_connection_id, name: p.participant.display_name().to_owned() };
                let fresh=p.network.sampled_at.is_some_and(|at|(sampled_at-at).num_seconds()<=15);
                let state=if p.participant.deafened {"Deafened"} else if p.participant.muted {"Muted"} else {"Listening / speaking"};
                view!{<tr class="border-t border-slate-800"><td class="px-5 py-4"><button class="font-medium text-indigo-300 underline decoration-indigo-500/40 underline-offset-4 hover:text-indigo-100" disabled=is_bot && !owner on:click=move |_| {if is_bot {ui.bot_target.set(Some(bot_target.clone()));} else {selected.set(Some(selection.clone()));}}>{p.participant.display_name().to_string()}</button><p class="mt-1 text-xs text-slate-500">{format!("@{}",p.participant.username)}</p></td><td class="px-5 py-4 text-slate-400">{state}{if p.participant.sharing_screen {" · Sharing screen"} else {""}}</td><td class="px-5 py-4 font-mono">{ms(p.network.round_trip_ms.filter(|_|fresh))}</td><td class="px-5 py-4 font-mono">{ms(p.network.microphone_jitter_ms.filter(|_|fresh))}</td><td class="px-5 py-4 font-mono">{if fresh {p.network.microphone_packets_received.zip(p.network.microphone_packets_lost).map(|(r,l)|format!("{r} / {l}")).unwrap_or_else(||"—".into())} else {"—".into()}}</td><td class="px-5 py-4 font-mono" title="Cumulative SFU queue drops for this connection">{format!("{} / {}",p.video_counters[1],p.video_counters[2])}</td><td class="px-5 py-4 font-mono" title="Cumulative SFU sent packets and send timeouts for this connection">{format!("{} / {}",p.video_counters[3],p.video_counters[4])}</td></tr>}
            }).collect_view()}
            </tbody></table></div>
        </article>
    }
}

#[derive(Clone)]
struct Selection {
    connection: Option<thiscord_shared::RequestId>,
    name: String,
}

#[component]
fn RemoteAudioPanel(ui: Ui, selection: Selection, close: Callback<()>) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    });
    let content = selection.connection.map(|connection| {
        let ui = Ui {
            audio_control: crate::audio_control::AudioControl::remote(connection, ui.token),
            audio: RwSignal::new(Default::default()), audio_status: RwSignal::new(None),
            audio_saving: RwSignal::new(false), audio_revision: RwSignal::new(0),
            voice: RwSignal::new(Default::default()), status: RwSignal::new(String::new()),
            ..ui
        };
        let loaded = RwSignal::new(false);
        let downloading = RwSignal::new(false);
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        on_cleanup(move || abort.abort());
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(async move {
                loop {
                    if !ui.audio_saving.get_untracked() {
                        let revision = ui.audio_revision.get_untracked();
                        let settings = ui.audio_control.invoke("audio_current", serde_json::json!({})).await;
                        let status = ui.audio_control.invoke("audio_status", serde_json::json!({})).await;
                        match (settings, status) {
                            (Ok(settings), Ok(status)) => {
                                if !ui.audio_saving.get_untracked() && ui.audio_revision.get_untracked() == revision { ui.audio_control.observe(&settings); ui.audio.set(settings); }
                                ui.audio_status.set(Some(status)); loaded.set(true);
                            }
                            (Err(error), _) | (_, Err(error)) => { ui.status.set(error); loaded.set(false); ui.audio_status.set(None); }
                        }
                    }
                    gloo_timers::future::TimeoutFuture::new(3000).await;
                }
            }, registration).await;
        });
        view! {
            <p class="text-sm text-slate-400">"Changes apply and are saved on this user's connected computer. Device changes can briefly interrupt their audio. Diagnostic downloads contain technical logs, not recordings."</p>
            <button class="my-3 rounded bg-indigo-600 px-4 py-2 text-sm disabled:opacity-50" disabled=move || downloading.get() on:click=move |_| {
                downloading.set(true);
                leptos::task::spawn_local(async move {
                    let result = ui.audio_control.download().await;
                    if !downloading.is_disposed() {
                        downloading.set(false);
                        ui.status.set(result.err().unwrap_or_else(|| "Diagnostic download prepared".into()));
                    }
                });
            }>{move || if downloading.get() {"Preparing diagnostics…"} else {"Download diagnostics"}}</button>
            <p role="status" class="my-3 text-amber-300">{move || ui.status.get()}</p>
            <section class="my-4 rounded-xl border border-slate-800 p-4">
                <h3 class="font-semibold">"Desktop UI performance"</h3>
                {move || ui.audio_status.get().and_then(|s| s.ui_performance).map(|report| {
                    if report.age_ms > 30_000 {
                        view! {<p class="mt-2 text-sm text-amber-300">{format!("Snapshot is stale ({} seconds old). The WebView may be hidden, suspended or stalled.", report.age_ms/1000)}</p>}.into_any()
                    } else {
                        view! {<p class="mt-2 text-xs text-slate-400">{format!("Automatic numeric sample · {} seconds old · since this WebView loaded", report.age_ms/1000)}</p><UiPerformance measurements=report.measurements/>}.into_any()
                    }
                }).unwrap_or_else(|| view! {<p class="mt-2 text-sm text-slate-400">"Waiting for UI counters; older desktops do not report them. Allow 10 seconds after opening the app."</p>}.into_any())}
            </section>
            <Show when=move || !loaded.get()><p class="my-3 text-slate-400">"Waiting for the connected desktop…"</p></Show>
            <fieldset disabled=move || !loaded.get() || ui.audio_saving.get() class="mt-5 disabled:opacity-50"><super::audio::AudioSettingsPanel ui=ui/></fieldset>
        }.into_any()
    });
    view! {
        <dialog node_ref=dialog on:cancel=move |_: web_sys::Event| close.run(()) class="fixed inset-0 m-auto max-h-[90vh] w-full max-w-3xl overflow-auto rounded-xl border border-slate-700 bg-slate-950 p-6 text-slate-100 backdrop:bg-black/70">
            <header class="mb-5 flex items-center justify-between gap-4"><h2 class="text-xl font-semibold">{format!("Audio settings · {}", selection.name)}</h2><button autofocus class="rounded bg-slate-800 px-4 py-2" on:click=move |_| close.run(())>"Close"</button></header>
            {content.unwrap_or_else(|| view! {<p>"Remote audio is unavailable for this connection. Update the user's desktop app and reconnect to voice."</p>}.into_any())}
        </dialog>
    }
}

#[component]
fn BackendPerformance(
    current: thiscord_shared::performance::BackendPerformance,
    previous: Option<(
        thiscord_shared::Timestamp,
        thiscord_shared::performance::BackendPerformance,
    )>,
    sampled_at: thiscord_shared::Timestamp,
) -> impl IntoView {
    if current.timings.is_empty() {
        return view! {<p class="text-sm text-slate-400">"Performance counters unavailable; update the backend to collect them."</p>}.into_any();
    }
    let seconds = previous
        .as_ref()
        .map(|(at, _)| (sampled_at - *at).num_milliseconds() as f64 / 1000.0)
        .filter(|s| *s > 0.0);
    let rows = current
        .timings
        .into_iter()
        .map(|m| {
            let old = previous
                .as_ref()
                .and_then(|(_, p)| p.timings.iter().find(|p| p.name == m.name));
            let recent = old
                .zip(seconds)
                .filter(|(p, _)| {
                    m.timing.calls >= p.timing.calls && m.timing.total_us >= p.timing.total_us
                })
                .map(|(p, s)| {
                    let calls = m.timing.calls - p.timing.calls;
                    let work = m.timing.total_us - p.timing.total_us;
                    format!(
                        "{:.1}/s · {:.1} ms/s · {} slow",
                        calls as f64 / s,
                        work as f64 / 1000.0 / s,
                        m.timing.over_budget.saturating_sub(p.timing.over_budget)
                    )
                })
                .unwrap_or_else(|| "Waiting for next sample".into());
            (m, recent)
        })
        .collect::<Vec<_>>();
    view! {
        <section class="space-y-4 rounded-xl border border-slate-800 bg-slate-900/60 p-5" aria-label="Performance monitors">
            <h2 class="text-xl font-semibold">"Performance monitors"</h2>
            <p class="text-sm text-slate-400">"Automatic backend measurements. Recent rates cover the last successful refresh; totals and maximums reset on backend restart. Slow means above the investigation budget, not a confirmed fault. Timings overlap; ms/s is elapsed work across concurrent operations, not CPU use."</p>
            <p class="text-sm text-slate-300">{format!("Pool checkout failures: {} · Unchanged roster polls: {} · Unchanged friends polls: {} · Friends polls skipped at capacity: {}",current.pool_failures,current.roster_unchanged,current.friends_unchanged,current.friends_admission_skipped)}</p>
            <TimingTable rows=rows/>
            <p class="text-xs text-slate-400">"SFU rows sample one in 64 ingress packets per connection. Audio DSP, capture/render deadlines and buffering faults: open a participant's Audio settings and advanced diagnostics below."</p>
        </section>
    }.into_any()
}

#[component]
fn UiPerformance(measurements: thiscord_shared::performance::UiPerformance) -> impl IntoView {
    let rows = thiscord_shared::performance::UI_TIMINGS
        .into_iter()
        .zip(measurements.timings)
        .map(|((name, budget_us), timing)| {
            (
                thiscord_shared::performance::Measurement {
                    name: name.into(),
                    budget_us,
                    timing,
                },
                "—".into(),
            )
        })
        .collect();
    view! {<p class="my-3 text-xs text-slate-400">"Cumulative numeric counters; no content collected. Timer lateness excludes hidden/visibility-transition intervals; scheduling, OS sleep and main-thread work can all cause lateness. Layout measures synchronous geometry work, not paint time."</p><TimingTable rows=rows/>}
}

#[component]
fn TimingTable(rows: Vec<(thiscord_shared::performance::Measurement, String)>) -> impl IntoView {
    view! {<div class="mt-3 overflow-x-auto"><table class="w-full whitespace-nowrap text-left text-xs"><thead class="text-slate-400"><tr><th class="p-2">"Operation"</th><th class="p-2">"Calls"</th><th class="p-2">"Total ms"</th><th class="p-2">"Mean / max ms"</th><th class="p-2">"Slow / budget ms"</th><th class="p-2">"Since last refresh"</th></tr></thead><tbody>
        {rows.into_iter().map(|(m,recent)|view! {<tr class="border-t border-slate-800"><td class="p-2">{m.name}</td><td class="p-2 font-mono">{m.timing.calls}</td><td class="p-2 font-mono">{format!("{:.1}",m.timing.total_us as f64/1000.0)}</td><td class="p-2 font-mono">{if m.timing.calls==0 {"—".into()} else {format!("{:.3} / {:.3}",m.timing.total_us as f64/m.timing.calls as f64/1000.0,m.timing.max_us as f64/1000.0)}}</td><td class="p-2 font-mono">{format!("{} / {:.1}",m.timing.over_budget,m.budget_us as f64/1000.0)}</td><td class="p-2">{recent}</td></tr>}).collect_view()}
    </tbody></table></div>}
}
