use super::{
    Field, Ui, client,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::account::*;

pub(super) fn server_page(page: &str) -> bool {
    page.starts_with("server_") || matches!(page, "permissions" | "moderation")
}
pub(super) fn navigate(ui: Ui, page: &'static str) {
    if ui.busy.get_untracked() || ui.audio_saving.get_untracked() {
        return;
    }
    if ui.page.get_untracked() == page {
        return;
    }
    if ui.settings_dirty.get_untracked() {
        ui.settings_pending.set(Some(page));
    } else {
        ui.page.set(page);
        if page == "devices" {
            super::run(ui, AccountRequest::Sessions);
        }
    }
}
fn title(page: &str) -> &'static str {
    match page {
        "audio" => "Voice & audio",
        "shortcuts" => "Shortcuts & overlay",
        "devices" => "Devices & connections",
        "security" | "password" | "delete" | "reauthenticate" => "Password & security",
        "desktop" => "App behavior",
        "updates" => "Updates",
        "administration" => "Instance administration",
        "server_roles" => "Roles",
        "server_members" => "Members",
        "server_channels" => "Channels & permissions",
        "server_danger" => "Ownership & deletion",
        "moderation" => "Moderation",
        "permissions" | "server_overview" => "Server overview",
        _ => "Your profile",
    }
}
fn subtitle(page: &str) -> &'static str {
    match page {
        "audio" => "Choose your devices and sound your best.",
        "shortcuts" => "Stay in control while other apps are focused.",
        "devices" => "Manage where you’re signed in and how you log in.",
        "security" | "password" | "delete" | "reauthenticate" => {
            "Protect your account and manage your credentials."
        }
        "desktop" => "Make Thiscord fit the way you use your computer.",
        "updates" => "Keep Thiscord up to date, on your schedule.",
        "administration" => "Manage administrators for this Thiscord installation.",
        "server_roles" => "Give people the right access with clear, named roles.",
        "server_members" => "Invite members and manage their roles.",
        "server_channels" => "Organize conversations and control who can join them.",
        "server_danger" => "Manage server ownership and permanent actions.",
        "moderation" => "Keep your community safe with permission-checked actions.",
        "permissions" | "server_overview" => "The essentials for this community.",
        _ => "How you appear to everyone on Thiscord.",
    }
}

#[component]
fn NavItem(ui: Ui, page: &'static str, label: &'static str, icon: Glyph) -> impl IntoView {
    view! {<button class="settings-nav-item" aria-current=move ||if ui.page.get()==page || (page=="security" && matches!(ui.page.get(),"password"|"delete"|"reauthenticate")){Some("page")}else{None} disabled=move ||ui.busy.get() on:click=move |_|{
        navigate(ui,page);
    }><Icon kind=icon/><span>{label}</span></button>}
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let discard = NodeRef::<leptos::html::Dialog>::new();
    let content = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        let page = ui.page.get();
        if page == "permissions" {
            ui.page.set(if ui.role_target.get_untracked().is_some() {
                "server_members"
            } else if ui.permission_channel.get_untracked().is_some() {
                "server_channels"
            } else {
                "server_overview"
            });
            return;
        }
        let open = ui.account.get().is_some() && !matches!(page, "servers" | "login");
        if let Some(dialog) = dialog.get() {
            if open {
                if !dialog.open() {
                    ui.status.set(String::new());
                    let _ = dialog.show_modal();
                }
            } else {
                dialog.close();
                ui.settings_pending.set(None);
            }
        }
        if let Some(content) = content.get() {
            content.set_scroll_top(0);
        }
    });
    Effect::new(move |_| {
        if let Some(dialog) = discard.get() {
            if ui.settings_pending.get().is_some() {
                let _ = dialog.show_modal();
            } else {
                dialog.close();
            }
        }
    });
    view! {
        <dialog node_ref=dialog class="settings-dialog" aria-labelledby="settings-dialog-title" on:cancel=move |e: web_sys::Event|{e.prevent_default();navigate(ui,"servers");} on:click=move |e:leptos::ev::MouseEvent|{
            if e.target()==e.current_target() && let Some(d)=dialog.get(){let r=d.get_bounding_client_rect();let x=f64::from(e.client_x());let y=f64::from(e.client_y());if x<r.left()||x>r.right()||y<r.top()||y>r.bottom(){navigate(ui,"servers");}}
        }>
            <Show when=move ||ui.account.get().is_some() && !matches!(ui.page.get(),"servers"|"login")>
            <div class="settings-window">
                <aside class="settings-sidebar">
                    <div class="settings-brand"><span class="settings-kicker">"THISCORD"</span><h1 id="settings-dialog-title">{move ||if server_page(ui.page.get()){"Server settings"}else{"Settings"}}</h1>
                        <p class="settings-scope">{move ||if server_page(ui.page.get()){ui.server.get().map(|s|s.guild.name).unwrap_or_default()}else{ui.account.get().map(|a|format!("@{}",a.username)).unwrap_or_default()}}</p>
                    </div>
                    <Show when=move ||server_page(ui.page.get()) fallback=move ||view!{
                        <nav class="settings-nav" aria-label="General settings">
                            <p class="settings-nav-label">"Your account"</p>
                            <NavItem ui=ui page="profile" label="Profile" icon=Glyph::Person/>
                            <NavItem ui=ui page="security" label="Password & security" icon=Glyph::Shield/>
                            <NavItem ui=ui page="devices" label="Devices & connections" icon=Glyph::Connections/>
                            <p class="settings-nav-label">"Your app"</p>
                            <NavItem ui=ui page="audio" label="Voice & audio" icon=Glyph::Headphones/>
                            <Show when=client::desktop><NavItem ui=ui page="shortcuts" label="Shortcuts & overlay" icon=Glyph::Keyboard/><NavItem ui=ui page="desktop" label="App behavior" icon=Glyph::Screen/></Show>
                            <NavItem ui=ui page="updates" label="Updates" icon=Glyph::Refresh/>
                            <Show when=move ||ui.can_create_server.get()><p class="settings-nav-label">"Administration"</p><NavItem ui=ui page="administration" label="Instance access" icon=Glyph::Shield/></Show>
                        </nav>
                        <button class="settings-signout" disabled=move ||ui.busy.get() || ui.settings_dirty.get() on:click=move |_|super::run(ui,AccountRequest::Logout)><Icon kind=Glyph::Join/>"Sign out"</button>
                    }>
                        <nav class="settings-nav" aria-label="Server settings">
                            <p class="settings-nav-label">"Community"</p>
                            <Show when=move ||ui.server.get().is_some_and(|s|s.can_manage_roles)>
                                <NavItem ui=ui page="server_overview" label="Overview" icon=Glyph::Settings/>
                                <NavItem ui=ui page="server_roles" label="Roles" icon=Glyph::Shield/>
                                <NavItem ui=ui page="server_members" label="Members" icon=Glyph::Person/>
                                <NavItem ui=ui page="server_channels" label="Channels & permissions" icon=Glyph::Volume/>
                            </Show>
                            <Show when=move ||ui.server.get().is_some_and(|s|s.can_moderate)><NavItem ui=ui page="moderation" label="Moderation" icon=Glyph::Shield/></Show>
                            <Show when=move ||ui.server.get().is_some_and(|s|ui.account.get().is_some_and(|a|s.guild.owner==a.id))><p class="settings-nav-label">"Advanced"</p><NavItem ui=ui page="server_danger" label="Ownership & deletion" icon=Glyph::Trash/></Show>
                        </nav>
                    </Show>
                    <p class="settings-version">{format!("Thiscord {}",env!("CARGO_PKG_VERSION"))}</p>
                </aside>
                <div class="settings-main">
                    <header class="settings-header"><div><h2>{move ||title(ui.page.get())}</h2><p>{move ||subtitle(ui.page.get())}</p></div><button class="settings-close" aria-label="Close settings" disabled=move ||ui.busy.get()||ui.audio_saving.get() on:click=move |_|navigate(ui,"servers")><Icon kind=Glyph::Close/><span>"Esc"</span></button></header>
                    <div class="settings-content" node_ref=content>
                        <Show when=move ||ui.ticket.get().is_some()><div class="settings-card"><p>"Finish connecting Google in your browser."</p>{move ||ui.authorization_url.get().map(|url|view!{<a class="settings-link" href=url target="_blank" rel="noopener noreferrer">"Continue with Google"</a>})}<button class="settings-secondary" on:click=move |_|super::cancel_google(ui)>"Cancel"</button></div></Show>
                        <Show when=move ||ui.page.get()=="profile"><Profile ui=ui/></Show>
                        <Show when=move ||matches!(ui.page.get(),"security"|"password"|"delete"|"reauthenticate")><Security ui=ui/></Show>
                        <Show when=move ||ui.page.get()=="devices"><Devices ui=ui/></Show>
                        <Show when=move ||ui.page.get()=="audio"><super::audio::AudioSettingsPanel ui=ui/></Show>
                        <Show when=move ||ui.page.get()=="shortcuts"><super::shortcuts::ShortcutSettings control=ui.audio_control on_dirty=Callback::new(move |dirty|ui.settings_dirty.set(dirty))/><crate::overlay::OverlaySettings control=ui.audio_control/></Show>
                        <Show when=move ||ui.page.get()=="desktop"><super::window::WindowSettings/></Show>
                        <Show when=move ||ui.page.get()=="updates"><super::updates::Panel ui=ui/></Show>
                        <Show when=move ||ui.page.get()=="administration"><super::permissions::InstanceSettings ui=ui/></Show>
                        <Show when=move ||server_page(ui.page.get()) && ui.page.get()!="moderation"><super::permissions::PermissionEditor ui=ui/></Show>
                        <Show when=move ||ui.page.get()=="moderation"><super::moderation::ModerationPanel ui=ui/></Show>
                    </div>
                    <Show when=move ||!ui.status.get().is_empty()><p class="settings-feedback" role="status" aria-live="polite">{move ||ui.status.get()}</p></Show>
                </div>
            </div>
            </Show>
        </dialog>
        <dialog node_ref=discard class="context-dialog" aria-labelledby="discard-settings-title" on:cancel=move |_: web_sys::Event|ui.settings_pending.set(None) on:close=move |_|ui.settings_pending.set(None) on:click=move |e|super::dialog::dismiss_outside(e,discard,false)>
            <h2 id="discard-settings-title" class="text-xl font-semibold">"Discard unsaved changes?"</h2><p class="mt-3 text-sm text-white/60">"Your edits haven’t been saved yet."</p>
            <div class="settings-actions mt-6"><button class="settings-secondary" autofocus on:click=move |_|ui.settings_pending.set(None)>"Keep editing"</button><button class="settings-danger" on:click=move |_|{if let Some(page)=ui.settings_pending.get_untracked(){ui.settings_dirty.set(false);navigate(ui,page);}
                ui.settings_pending.set(None);}>"Discard changes"</button></div>
        </dialog>
    }
}

#[component]
fn Profile(ui: Ui) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let bio = RwSignal::new(String::new());
    let saved = Memo::new(move |_| {
        ui.account
            .get()
            .map(|a| (a.display_name, a.bio))
            .unwrap_or_default()
    });
    let reset = move || {
        let (n, b) = saved.get_untracked();
        name.set(n);
        bio.set(b);
    };
    Effect::new(move |_| {
        saved.track();
        reset();
    });
    Effect::new(move |_| {
        ui.settings_dirty
            .set((name.get(), bio.get()) != saved.get())
    });
    on_cleanup(move || ui.settings_dirty.set(false));
    view! {
        <super::profile::ProfilePicture ui=ui/>
        <form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();super::run(ui,AccountRequest::UpdateProfile{display_name:name.get_untracked(),bio:bio.get_untracked()});}>
            <fieldset class="space-y-4" disabled=move ||ui.busy.get()><h3>"About you"</h3><Field label="Display name" value=name/>
            <label class="settings-field"><span>"Bio"</span><textarea rows="4" maxlength="500" placeholder="A little about you…" prop:value=move ||bio.get() on:input=move |e|bio.set(event_target_value(&e))/><small>{move ||format!("{}/500",bio.get().chars().count())}</small></label>
            <div class="settings-actions"><button class="settings-secondary" type="button" disabled=move ||ui.busy.get()||!ui.settings_dirty.get() on:click=move |_|reset()>"Reset"</button><button class="settings-primary" disabled=move ||ui.busy.get()||!ui.settings_dirty.get()>"Save profile"</button></div></fieldset>
        </form>
        <section class="settings-card"><h3>"Account details"</h3><div class="settings-info-row"><span>"Username"</span><strong>{move ||ui.account.get().map(|a|format!("@{}",a.username))}</strong></div><div class="settings-info-row"><span>"Email"</span><strong>{move ||ui.account.get().map(|a|a.email)}</strong></div><p class="settings-hint">{move ||if ui.account.get().is_some_and(|a|a.email_verified){"✓ Email verified"}else{"Check your inbox to verify your email."}}</p><Show when=move ||ui.account.get().is_some_and(|a|!a.email_verified)><button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|super::run(ui,AccountRequest::SendVerification)>"Resend verification email"</button></Show></section>
    }
}

#[component]
fn Security(ui: Ui) -> impl IntoView {
    let current = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    view! {
        <section class="settings-card"><h3>"Confirm your identity"</h3><p class="settings-hint">"Required before changing a password, login method, or ownership."</p>
            <form class="space-y-4" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();let value=current.get_untracked();current.set(String::new());super::run(ui,AccountRequest::Reauthenticate{password:value});}><Field label="Current password" value=current kind="password" autocomplete="current-password"/><button class="settings-primary" disabled=move ||ui.busy.get()||current.get().is_empty()>"Confirm identity"</button></form>
            <Show when=move ||ui.account.get().is_some_and(|a|a.identities.contains(&IdentityProvider::Google))><button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|super::google(ui,GooglePurpose::Reauthenticate)>"Confirm with Google"</button></Show>
            <Show when=move ||ui.server.get().is_some_and(|s|ui.account.get().is_some_and(|a|a.id==s.guild.owner))><button class="settings-link" on:click=move |_|navigate(ui,"server_danger")>"Return to server ownership settings"</button></Show>
        </section>
        <section class="settings-card"><h3>"Change password"</h3><p class="settings-hint">"Use at least 12 characters. Saving signs you out on every device."</p><form class="space-y-4" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();let value=password.get_untracked();password.set(String::new());super::run(ui,AccountRequest::ChangePassword{password:value});}><Field label="New password" value=password kind="password" autocomplete="new-password"/><button class="settings-primary" disabled=move ||ui.busy.get()||password.get().chars().count()<12>"Save new password"</button></form></section>
        <details class="settings-card settings-danger-zone"><summary>"Delete account"</summary><p class="settings-hint">"Permanently deletes your profile, credentials and sessions. Transfer or delete any servers you own first. Confirm your identity above before continuing."</p><Field label="Type your username to confirm" value=confirmation/><ConfirmButton ui=ui label="Delete my account" title="Permanently delete your account?" detail="This cannot be undone. Your profile, credentials and sessions will be deleted." disabled=Signal::derive(move ||ui.account.get().is_none_or(|a|confirmation.get()!=a.username)) on_confirm=Callback::new(move |()|super::run(ui,AccountRequest::DeleteAccount{confirmation:confirmation.get_untracked()}))/></details>
    }
}

#[component]
fn Devices(ui: Ui) -> impl IntoView {
    view! {
        <section class="settings-card"><h3>"Signed-in devices"</h3><p class="settings-hint">"Remove access for devices you no longer use."</p><Show when=move ||ui.sessions.get().is_empty()><p class="settings-hint">{move ||if ui.busy.get(){"Loading devices…"}else{"No devices to display."}}</p></Show>
            <ul class="settings-device-list">{move ||ui.sessions.get().into_iter().map(|s|{let id=s.id;view!{<li><div class="settings-device-icon"><Icon kind=Glyph::Screen/></div><div class="min-w-0 flex-1"><strong>{s.device}</strong><Show when=move ||s.current><span class="settings-badge">"This device"</span></Show><p>{format!("Last active {}",s.last_seen_at.format("%d %b %Y · %H:%M UTC"))}</p></div><ConfirmButton ui=ui label="Sign out" title="Sign out this device?" detail="Its current session will end." on_confirm=Callback::new(move |()|super::run(ui,AccountRequest::RevokeSession{session_id:id}))/></li>}}).collect_view()}</ul>
            <ConfirmButton ui=ui label="Sign out all devices" title="Sign out everywhere?" detail="You’ll need to sign in again on every device, including this one." on_confirm=Callback::new(move |()|super::run(ui,AccountRequest::LogoutAll))/>
        </section>
        <section class="settings-card"><h3>"Login methods"</h3><p class="settings-hint">"Confirm your identity in Password & security before changing login methods. Keep at least one method."</p>{move ||ui.account.get().map(|a|a.identities.into_iter().map(|provider|view!{<div class="settings-info-row"><strong>{match provider{IdentityProvider::Google=>"Google",IdentityProvider::Password=>"Password"}}</strong><ConfirmButton ui=ui label="Remove" title="Remove this login method?" detail="Make sure you can sign in with another method before continuing." on_confirm=Callback::new(move |()|super::run(ui,AccountRequest::UnlinkIdentity{provider}))/></div>}).collect_view())}<button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|super::google(ui,GooglePurpose::Link)>"Connect Google"</button><button class="settings-link" on:click=move |_|navigate(ui,"security")>"Password & security"</button></section>
    }
}

#[component]
pub(super) fn ConfirmButton(
    ui: Ui,
    label: &'static str,
    title: &'static str,
    detail: &'static str,
    on_confirm: Callback<()>,
    #[prop(default = Signal::derive(||false))] disabled: Signal<bool>,
) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    view! {<button type="button" class="settings-danger" disabled=move ||disabled.get()||ui.busy.get() on:click=move |_|if let Some(d)=dialog.get(){let _=d.show_modal();}>{label}</button>
        <dialog node_ref=dialog class="context-dialog" aria-label=title on:click=move |e|super::dialog::dismiss_outside(e,dialog,false)><h2 class="text-xl font-semibold">{title}</h2><p class="mt-3 text-sm text-white/60">{detail}</p><div class="settings-actions mt-6"><button type="button" class="settings-secondary" autofocus on:click=move |_|if let Some(d)=dialog.get(){d.close();}>"Cancel"</button><button type="button" class="settings-danger" disabled=move ||disabled.get()||ui.busy.get() on:click=move |_|{on_confirm.run(());
                if let Some(d)=dialog.get(){d.close();}}>{label}</button></div></dialog>
    }
}
