use leptos::prelude::*;
use thiscord_shared::notifications::*;
use wasm_bindgen::{JsCast, closure::Closure};

#[component]
pub fn Banner(
    alert: Alert,
    action: Callback<BannerAction>,
    #[prop(default = true)] can_accept: bool,
) -> impl IntoView {
    let call = alert.call_id.is_some();
    let (label, icon) = match alert.kind {
        Kind::DirectMessage => ("Direct message", "M4 4h16v12H8l-4 4V4Zm4 4h8M8 12h5"),
        Kind::GuildMessage => ("Server message", "M9 3 7 21M17 3l-2 18M4 9h17M3 15h17"),
        Kind::IncomingCall => (
            "Incoming call",
            "M3 14v-3a9 9 0 0 1 18 0v3M3 13h4v8H5a2 2 0 0 1-2-2v-6ZM21 13h-4v8h2a2 2 0 0 0 2-2v-6Z",
        ),
        Kind::VoiceJoin => (
            "Joined voice",
            "M11 5 6 9H3v6h3l5 4V5Zm4 3a6 6 0 0 1 0 8M18 5a10 10 0 0 1 0 14",
        ),
        Kind::VoiceLeave => ("Left voice", "M11 5 6 9H3v6h3l5 4V5Zm5 4 5 6M21 9l-5 6"),
        Kind::Update => ("Update available", "M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5"),
    };
    let title = if matches!(alert.target, Target::Conversation { .. }) {
        alert
            .title
            .rsplit_once(" — ")
            .map(|(name, _)| name.to_owned())
            .unwrap_or_else(|| alert.title.clone())
    } else {
        alert.title.clone()
    };
    view! {<section class="notification-card" class:notification-call=call aria-label=move || crate::i18n::t("Notification")>
        <div class="notification-heading">
            <button class="notification-open" title=alert.title on:click=move |_|action.run(BannerAction::Open)>
                <span class="notification-icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=icon/></svg></span>
                <span class="notification-copy"><span class="notification-kind">{move || crate::i18n::t(label)}<span class="notification-app">" · Thiscord"</span></span><span class="notification-title">{title}</span></span>
            </button>
            <button class="notification-dismiss" aria-label=move || crate::i18n::t("Dismiss notification") on:click=move |_|action.run(BannerAction::Dismiss)><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6"/></svg></button>
        </div>
        <div class="notification-actions">
            <Show when=move ||call fallback=move ||view!{<button class="notification-link" on:click=move |_|action.run(BannerAction::Open)>{move || if alert.kind==Kind::Update {crate::i18n::t("View update")} else {crate::i18n::t("Open")}}<span aria-hidden="true">" →"</span></button>}>
                <button class="notification-accept" disabled=!can_accept on:click=move |_|action.run(BannerAction::Accept)>{move || crate::i18n::t("Accept")}</button>
                <button class="notification-decline" on:click=move |_|action.run(BannerAction::Decline)>{move || crate::i18n::t("Decline")}</button>
            </Show>
            <select aria-label=move || crate::i18n::t("Snooze this subscription") class="notification-snooze" on:change=move |e|{let value=event_target_value(&e); if let Ok(index)=value.parse::<usize>() && let Some((duration,_))=Snooze::OPTIONS.get(index) {action.run(BannerAction::Snooze {duration:*duration});}}>
                <option value="">{move || crate::i18n::t("Snooze…")}</option>{Snooze::OPTIONS.into_iter().enumerate().skip(1).map(|(i,(_,label))|view!{<option value=i.to_string()>{move || crate::i18n::t(label)}</option>}).collect_view()}
            </select>
        </div>
        <Show when=move ||call && !can_accept><p class="notification-hint">{move || crate::i18n::t("Open the desktop app to accept this call.")}</p></Show>
    </section>}
}
#[component]
pub fn DesktopBanners() -> impl IntoView {
    let alerts = RwSignal::new(Vec::<Alert>::new());
    let revision = RwSignal::new(0_u64);
    let stack = NodeRef::<leptos::html::Div>::new();
    let measure = move || {
        let Some(node) = stack.get_untracked() else {
            return;
        };
        let current_revision = revision.get_untracked();
        let height = node.get_bounding_client_rect().height().ceil();
        leptos::task::spawn_local(async move {
            let _ = crate::account_client::native::<()>(
                "notification_resize",
                serde_json::json!({"revision":current_revision,"height":height}),
            )
            .await;
        });
    };
    // A revision can change without changing height. Always acknowledge the
    // rendered snapshot; the native side rejects measurements from older ones.
    Effect::new(move |_| {
        revision.track();
        request_animation_frame(move || {
            if !revision.is_disposed() {
                measure();
            }
        });
    });
    Effect::new(move |_| {
        let Some(node) = stack.get() else {
            return;
        };
        let callback = Closure::<dyn FnMut()>::new(measure);
        if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
            observer.observe(&node);
            let resource = StoredValue::new_local((observer, callback));
            on_cleanup(move || resource.with_value(|(observer, _)| observer.disconnect()));
        }
    });
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
    view! {<main class="notification-window" aria-live="polite"><div node_ref=stack class="notification-stack"><For each=move ||alerts.get() key=|a|a.id.clone() children=move |alert| {
        let id=StoredValue::new(alert.id.clone());
        view!{<Banner alert=alert action=Callback::new(move |action|leptos::task::spawn_local(async move {let _=crate::account_client::native::<()>("notification_action",serde_json::json!({"id":id.get_value(),"action":action})).await;}))/ >}
    }/></div></main>}
}
