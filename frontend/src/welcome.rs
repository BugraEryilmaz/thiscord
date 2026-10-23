use futures_util::{future::Either, pin_mut};
use leptos::prelude::*;
use thiscord_frontend::downloads::{LATEST_API, LATEST_PAGE, Platform, REPOSITORY, Release};

#[derive(Clone)]
enum Downloads {
    Loading,
    Ready(Release),
    Unavailable,
}

fn refresh(downloads: RwSignal<Downloads>) {
    downloads.set(Downloads::Loading);
    leptos::task::spawn_local(async move {
        let fetch = async {
            let response = gloo_net::http::Request::get(LATEST_API)
                .header("Accept", "application/vnd.github+json")
                .credentials(web_sys::RequestCredentials::Omit)
                .send()
                .await
                .ok()?;
            if !response.ok() {
                return None;
            }
            let release: Release = response.json().await.ok()?;
            release.version()?;
            Some(release)
        };
        let timeout = gloo_timers::future::TimeoutFuture::new(10_000);
        pin_mut!(fetch, timeout);
        let result = match futures_util::future::select(fetch, timeout).await {
            Either::Left((Some(release), _)) => Downloads::Ready(release),
            _ => Downloads::Unavailable,
        };
        downloads.set(result);
    });
}

#[component]
pub fn Welcome() -> impl IntoView {
    let downloads = RwSignal::new(Downloads::Loading);
    let suggested = web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .and_then(|ua| Platform::suggested(&ua));
    refresh(downloads);
    let primary_link = move || match (downloads.get(), suggested) {
        (Downloads::Ready(release), Some(platform)) => release
            .download(platform)
            .map(|asset| asset.browser_download_url.clone())
            .unwrap_or("#downloads".into()),
        _ => "#downloads".into(),
    };
    let primary_label = move || match (downloads.get(), suggested) {
        (Downloads::Ready(release), Some(platform)) if release.download(platform).is_some() => {
            if platform == Platform::Mac {
                "Download for Apple Silicon".into()
            } else {
                format!("Download for {}", platform.name())
            }
        }
        _ => "Get Thiscord".into(),
    };
    view! {
        <div class="welcome-page">
            <a class="welcome-skip" href="#main">"Skip to content"</a>
            <header class="welcome-nav welcome-wrap flex items-center justify-between gap-6">
                <a class="welcome-logo flex items-center gap-2" href="#" aria-label="Thiscord home"><Logo/>"thiscord"</a>
                <nav aria-label="Main navigation" class="flex items-center gap-6 sm:gap-9">
                    <a class="welcome-about-link" href="#about">"The good part"</a>
                    <a href={REPOSITORY}>"GitHub"<Icon kind="external"/></a>
                    <a class="welcome-nav-download" href="#downloads">"Download"<Icon kind="download"/></a>
                </nav>
            </header>
            <main id="main">
                <section class="welcome-hero welcome-wrap" aria-labelledby="welcome-title">
                    <div class="welcome-intro">
                        <h1 id="welcome-title">"A place for"<br/>"your people."</h1>
                        <p class="welcome-lead">"The late-night games. The one-more-story calls. The comfortable silence. Make room for it all on Thiscord."</p>
                        <div class="welcome-hero-actions flex flex-wrap items-center gap-4">
                            <a class="welcome-primary" href=primary_link><Icon kind="download"/>{primary_label}</a>
                            <a class="welcome-text-link" href="#downloads">"All downloads"</a>
                        </div>
                        <p class="welcome-platform-note">"Made for Windows, macOS & Linux."</p>
                    </div>
                    <Room/>
                </section>
                <section id="about" class="welcome-about welcome-wrap" aria-labelledby="about-title">
                    <div class="welcome-about-heading">
                        <span class="welcome-small-mark" aria-hidden="true"><Icon kind="spark"/></span>
                        <h2 id="about-title">"Good company."<br/>"Your kind of space."</h2>
                        <p>"A shared corner of the internet, with everything you need to spend time together."</p>
                    </div>
                    <div class="welcome-features">
                        <article><Icon kind="chat"/><div><h3>"Keep the conversation going"</h3><p>"A channel for every conversation. A direct message for the things that are just between you."</p></div></article>
                        <article><Icon kind="headphones"/><div><h3>"Drop in. Hang out."</h3><p>"Join a voice channel when you’re around. Talk through a game, catch up, or just keep each other company."</p></div></article>
                        <article><Icon kind="screen"/><div><h3>"Let everyone in on it"</h3><p>"Share your screen to show what you’re working on, playing, or trying to figure out."</p></div></article>
                    </div>
                </section>
                <section id="downloads" class="welcome-download-section" aria-labelledby="download-title">
                    <div class="welcome-wrap welcome-download-layout">
                        <div class="welcome-download-intro">
                            <h2 id="download-title">"See you in there."</h2>
                            <p>"Pick your desktop. Bring your people."</p>
                            <div class="welcome-release-status" role="status" aria-live="polite">
                                {move || match downloads.get() {
                                    Downloads::Loading => "Finding the latest release…".to_owned(),
                                    Downloads::Ready(release) => format!("Latest release: {}", release.version().unwrap_or_default()),
                                    Downloads::Unavailable => "Couldn’t load downloads. Try again or get the latest release on GitHub.".into(),
                                }}
                            </div>
                            <div class="flex flex-wrap items-center gap-5">
                                <Show when=move || matches!(downloads.get(), Downloads::Unavailable)>
                                    <button class="welcome-retry" on:click=move |_| refresh(downloads)>"Try again"</button>
                                </Show>
                                <a class="welcome-text-link" href=LATEST_PAGE>"Release notes on GitHub"<Icon kind="external"/></a>
                            </div>
                        </div>
                        <div class="welcome-download-list">
                            { [Platform::Windows, Platform::Mac, Platform::Linux, Platform::Debian].into_iter()
                                .map(|platform| view! { <DownloadRow platform downloads/> }).collect_view() }
                            <p class="welcome-requirements">"macOS: Apple Silicon, 11.3 or newer. Linux: built on Ubuntu 24.04; compatible system libraries required."</p>
                        </div>
                    </div>
                </section>
            </main>
            <footer class="welcome-footer welcome-wrap flex flex-wrap items-center justify-between gap-6">
                <a class="welcome-logo flex items-center gap-2" href="#"><Logo/>"thiscord"</a>
                <p>"A little closer, wherever you are."</p>
                <a href={REPOSITORY}>"Built in the open"<Icon kind="external"/></a>
            </footer>
        </div>
    }
}

#[component]
fn DownloadRow(platform: Platform, downloads: RwSignal<Downloads>) -> impl IntoView {
    let asset = move || match downloads.get() {
        Downloads::Ready(release) => release.download(platform).cloned(),
        _ => None,
    };
    view! {
        <div class="welcome-download-row">
            <div class="welcome-platform-icon"><Icon kind=match platform { Platform::Windows => "windows", Platform::Mac => "apple", _ => "linux" }/></div>
            <div class="welcome-download-copy"><h3>{platform.name()}</h3><p>{platform.detail()}</p></div>
            <div class="welcome-download-action">
                {move || if let Some(asset) = asset() {
                    view! { <a href=asset.browser_download_url aria-label=format!("Download {} — {}", platform.name(), platform.detail())><span>"Download"</span><Icon kind="download"/></a> }.into_any()
                } else {
                    match downloads.get() {
                        Downloads::Loading => view! { <span class="welcome-download-pending">"Checking…"</span> }.into_any(),
                        Downloads::Ready(_) => view! { <span class="welcome-download-pending">"Not in this release"</span> }.into_any(),
                        Downloads::Unavailable => view! { <a href=LATEST_PAGE aria-label=format!("Find {} downloads on GitHub", platform.name())>"View on GitHub"<Icon kind="external"/></a> }.into_any(),
                    }
                }}
            </div>
        </div>
    }
}

#[component]
fn Room() -> impl IntoView {
    let room = RwSignal::new(0usize);
    let muted = RwSignal::new(false);
    let names = ["The lounge", "Game night", "Study break"];
    view! {
        <figure class="welcome-room-scene" aria-label="An interactive illustration of friends hanging out in Thiscord">
            <div class="welcome-orbit welcome-orbit-one" aria-hidden="true"></div>
            <div class="welcome-orbit welcome-orbit-two" aria-hidden="true"></div>
            <span class="welcome-spark-one" aria-hidden="true"><Icon kind="spark"/></span>
            <div class="welcome-room">
                <div class="welcome-room-header flex items-center justify-between"><span><span class="welcome-live-dot"></span>"The usual crowd"</span><Icon kind="headphones"/></div>
                <div class="welcome-room-tabs flex gap-1" role="group" aria-label="Preview a room">
                    {names.into_iter().enumerate().map(|(index, name)| view! {
                        <button aria-pressed=move || (room.get() == index).to_string() on:click=move |_| room.set(index)>{name}</button>
                    }).collect_view()}
                </div>
                <div class="welcome-room-title"><Icon kind="volume"/><h3>{move || names[room.get()]}</h3><span>"3 here"</span></div>
                <div class="welcome-friends">
                    <div class="welcome-friend"><div class="welcome-avatar welcome-avatar-peach"><Character kind=0/></div><span>"mila"</span></div>
                    <div class="welcome-friend"><div class="welcome-avatar welcome-avatar-lilac"><Character kind=1/></div><span>"deniz"<span class="welcome-speaking" aria-label="speaking">"ııı"</span></span></div>
                    <div class="welcome-friend"><div class="welcome-avatar welcome-avatar-blue"><Character kind=2/></div><span>"you"</span></div>
                </div>
                <div class="welcome-room-controls flex items-center justify-between"><span><span class="welcome-live-dot"></span>"Voice connected"</span><button aria-pressed=move || muted.get().to_string() aria-label=move || if muted.get() { "Unmute in illustration" } else { "Mute in illustration" } on:click=move |_| muted.update(|m| *m = !*m)><Icon kind="mic"/>{move || if muted.get() { "Unmute" } else { "Mute" }}</button></div>
            </div>
            <div class="welcome-message" aria-live="polite"><span class="welcome-message-avatar">"m"</span><div><span class="welcome-message-name">"mila"<span>"just now"</span></span><p>{move || match room.get() { 1 => "one more round?", 2 => "okay, five minute break ☕", _ => "same time tomorrow?" }}</p></div><span class="welcome-message-reaction" aria-label="two heart reactions">"♡ 2"</span></div>
            <figcaption>"A little preview of being together. Try a room."</figcaption>
        </figure>
    }
}

#[component]
fn Logo() -> impl IntoView {
    view! { <crate::brand::BrandMark class="welcome-brand-mark"/> }
}

#[component]
fn Character(kind: u8) -> impl IntoView {
    let body = match kind {
        0 => "#d15c37",
        1 => "#7154b6",
        _ => "#4467ce",
    };
    let shape = match kind {
        0 => "M22 89V45C22 16 77 16 77 45v44Z",
        1 => "M20 89V44l12-20 18 12 18-12 12 20v45Z",
        _ => "M17 89V56c0-43 66-43 66 0v33Z",
    };
    view! {
        <svg viewBox="0 0 100 100" fill="none" aria-hidden="true">
            <path d=shape fill=body/>
            <path d="M31 52c0-22 38-22 38 0v17c0 22-38 22-38 0Z" fill="#fff6e7"/>
            <path d="M41 56v5m18-5v5" stroke="#17264a" stroke-width="4" stroke-linecap="round"/>
            <path d="M44 71q6 6 12 0" stroke="#17264a" stroke-width="2.5" stroke-linecap="round"/>
            <path d="M23 56v-7a27 27 0 0 1 54 0v7M22 54v13m56-13v13" stroke="#17264a" stroke-width="6" stroke-linecap="round"/>
            <path d="M77 68q0 11-15 11" stroke="#17264a" stroke-width="3" stroke-linecap="round"/>
        </svg>
    }
}

#[component]
fn Icon(kind: &'static str) -> impl IntoView {
    let path = match kind {
        "download" => "M12 3v12m-5-5 5 5 5-5M4 15v6h16v-6",
        "external" => "M8 5H5v14h14v-3M12 4h8v8M20 4 10 14",
        "headphones" => {
            "M3 14v-3a9 9 0 0 1 18 0v3M3 13h4v8H5a2 2 0 0 1-2-2v-6ZM21 13h-4v8h2a2 2 0 0 0 2-2v-6Z"
        }
        "chat" => "M3 4h18v13H10l-6 4v-4H3V4Zm4 5h10M7 12h6",
        "screen" => "M3 3h18v14H3V3ZM8 21h8M12 17v4m-4-9 4-4 4 4M12 8v6",
        "spark" => "M12 2c0 7-3 10-10 10 7 0 10 3 10 10 0-7 3-10 10-10-7 0-10-3-10-10Z",
        "mic" => "M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0V5ZM5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8",
        "volume" => "M3 9h4l5-4v14l-5-4H3V9ZM16 8a6 6 0 0 1 0 8M19 5a10 10 0 0 1 0 14",
        "windows" => "M3 4h8v7H3V4Zm10-1 8-1v9h-8V3ZM3 13h8v7l-8-1v-6Zm10 0h8v9l-8-1v-8Z",
        "apple" => {
            "M14 6c3 0 4-3 4-4-3 0-4 2-4 4ZM12 7C6 3 2 9 5 16c3 8 5 3 7 4 3-1 4 4 8-4-4-2-4-6-1-8-2-3-4-2-7-1Z"
        }
        _ => "m8 8-5 4 5 4m8-8 5 4-5 4M14 4l-4 16",
    };
    view! { <svg class="welcome-icon" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg> }
}
