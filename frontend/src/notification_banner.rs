use leptos::prelude::*;
use thiscord_shared::notifications::*;

#[component]
pub fn Banner(
    alert: Alert,
    action: Callback<BannerAction>,
    #[prop(default = true)] can_accept: bool,
) -> impl IntoView {
    let call = alert.call_id.is_some();
    view! {<section class="rounded-xl border border-white/20 bg-surface p-4 text-white shadow-2xl" aria-label="Notification">
        <div class="flex items-start justify-between gap-3"><button class="text-left font-semibold hover:underline" on:click=move |_|action.run(BannerAction::Open)>{alert.title}</button><button aria-label="Dismiss notification" on:click=move |_|action.run(BannerAction::Dismiss)>"×"</button></div>
        <Show when=move ||call><div class="my-3 flex gap-2"><button class="rounded bg-emerald-600 px-4 py-2 disabled:opacity-50" disabled=!can_accept on:click=move |_|action.run(BannerAction::Accept)>"Accept"</button><button class="rounded bg-red-500/20 px-4 py-2" on:click=move |_|action.run(BannerAction::Decline)>"Decline"</button></div><Show when=move ||!can_accept><p class="text-sm text-white/60">"Open the desktop app to accept this call."</p></Show></Show>
        <label class="mt-3 flex items-center gap-2 text-sm text-white/70">"Snooze"
            <select aria-label="Snooze this subscription" class="min-w-0 rounded bg-surface px-2 py-1" on:change=move |e|{let value=event_target_value(&e); if let Ok(index)=value.parse::<usize>() && let Some((duration,_))=Snooze::OPTIONS.get(index) {action.run(BannerAction::Snooze {duration:*duration});}}>
                <option value="">"Choose duration…"</option>{Snooze::OPTIONS.into_iter().enumerate().skip(1).map(|(i,(_,label))|view!{<option value=i.to_string()>{label}</option>}).collect_view()}
            </select>
        </label>
    </section>}
}
#[component]
pub fn DesktopBanners() -> impl IntoView {
    let alerts = RwSignal::new(Vec::<Alert>::new());
    let revision = RwSignal::new(0_u64);
    let apply = move |snapshot: BannerSnapshot| {
        if !alerts.is_disposed() && snapshot.revision >= revision.get_untracked() {
            revision.set(snapshot.revision);
            alerts.set(snapshot.alerts);
        }
    };
    let listener = StoredValue::new_local(None::<crate::account_client::EventListener>);
    leptos::task::spawn_local(async move {
        if let Ok(handle) = crate::account_client::listen("notification-banners", apply).await {
            if listener.is_disposed() {
                return;
            }
            listener.set_value(Some(handle));
            if let Ok(items) =
                crate::account_client::native("notification_snapshot", serde_json::json!({})).await
                && !alerts.is_disposed()
            {
                apply(items);
            }
        }
    });
    on_cleanup(move || listener.set_value(None));
    view! {<main class="min-h-screen space-y-2 overflow-y-auto bg-surface p-2" aria-live="polite"><For each=move ||alerts.get() key=|a|a.id.clone() children=move |alert| {
        let id=StoredValue::new(alert.id.clone());
        view!{<Banner alert=alert action=Callback::new(move |action|leptos::task::spawn_local(async move {let _=crate::account_client::native::<()>("notification_action",serde_json::json!({"id":id.get_value(),"action":action})).await;}))/ >}
    }/></main>}
}
