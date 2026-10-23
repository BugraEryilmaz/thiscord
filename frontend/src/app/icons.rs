use leptos::prelude::*;

#[derive(Clone, Copy)]
pub(super) enum Glyph {
    Mic,
    MicOff,
    Headphones,
    Deafen,
    Settings,
    Refresh,
    Plus,
    Link,
    Compass,
    Pin,
    BellOff,
    Join,
    Disconnect,
    Screen,
    Window,
    CameraOff,
    Camera,
    Signal,
    Volume,
    VolumeOff,
    Edit,
    Trash,
    Send,
    Close,
    Expand,
    Person,
    Shield,
    Connections,
    Keyboard,
    Search,
}

#[component]
pub(super) fn Icon(kind: Glyph) -> impl IntoView {
    let path = match kind {
        Glyph::Mic => {
            "M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0V5ZM5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"
        }
        Glyph::MicOff => {
            "m3 3 18 18M9 9v3a3 3 0 0 0 5 2M15 9V5a3 3 0 0 0-5-2M5 10v2a7 7 0 0 0 12 5M19 10v2M12 19v3M8 22h8"
        }
        Glyph::Headphones => {
            "M3 14v-3a9 9 0 0 1 18 0v3M3 13h4v8H5a2 2 0 0 1-2-2v-6ZM21 13h-4v8h2a2 2 0 0 0 2-2v-6Z"
        }
        Glyph::Deafen => {
            "m3 3 18 18M3 14v-3a9 9 0 0 1 1-4M8 3a9 9 0 0 1 13 8v3M3 13h4v8H5a2 2 0 0 1-2-2v-6ZM17 13h4v5M17 17v4h2"
        }
        Glyph::Settings => {
            "m10 2-.6 3-2 .9-2.6-1.5-2 3.4L5 10v3l-2.2 2.2 2 3.4 2.6-1.5 2 .9.6 3h4l.6-3 2-.9 2.6 1.5 2-3.4L19 13v-3l2.2-2.2-2-3.4-2.6 1.5-2-.9L14 2h-4ZM15 11.5a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"
        }
        Glyph::Refresh => "M20 7v5h-5M20 12a8 8 0 1 0-2.3 5.7",
        Glyph::Link => {
            "m10 13 4-4M8 16l-1 1a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0M16 8l1-1a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0"
        }
        Glyph::Plus => "M12 5v14M5 12h14",
        Glyph::Search => "M19 10a7 7 0 1 1-14 0 7 7 0 0 1 14 0Zm-2 5 5 6",
        Glyph::Pin => "M9 3h6l-1 7 4 4v2H6v-2l4-4-1-7ZM12 16v6",
        Glyph::BellOff => "m3 3 18 18M9 4a6 6 0 0 1 9 5v4M6 6v6l-3 5h14M10 21h4",
        Glyph::Compass => "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM16 8l-2.5 5.5L8 16l2.5-5.5L16 8Z",
        Glyph::Join => "M14 3h6v18h-6M3 12h13m-5-5 5 5-5 5",
        Glyph::Disconnect => "M3 15v-4c5-5 13-5 18 0v4l-5 1v-4a13 13 0 0 0-8 0v4l-5-1Z",
        Glyph::Screen => "M3 3h18v14H3V3ZM8 21h8M12 17v4M12 13V6m-3 3 3-3 3 3",
        Glyph::Window => "M3 4h18v16H3V4ZM3 9h18M6 6.5h.01M9 6.5h.01",
        Glyph::Camera => "M3 5h12v14H3V5ZM15 10l6-4v12l-6-4",
        Glyph::CameraOff => "m3 3 18 18M9 5h6v6M15 10l6-4v12l-4-3M3 7v12h12v-4",
        Glyph::Signal => "M4 20v-3M9 20v-7M14 20V9M19 20V4",
        Glyph::Volume => "M3 9h4l5-4v14l-5-4H3V9ZM16 8a6 6 0 0 1 0 8M19 5a10 10 0 0 1 0 14",
        Glyph::VolumeOff => "M3 9h4l5-4v14l-5-4H3V9Zm13 0 6 6M16 15l6-6",
        Glyph::Edit => "m4 16 12-12 4 4L8 20H4v-4ZM13 7l4 4",
        Glyph::Trash => "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7",
        Glyph::Send => "m3 3 19 9-19 9 4-9-4-9ZM7 12h15",
        Glyph::Close => "m6 6 12 12M6 18 18 6",
        Glyph::Expand => "M8 3H3v5M16 3h5v5M21 16v5h-5M8 21H3v-5",
        Glyph::Person => "M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM4 21v-2a8 8 0 0 1 16 0v2",
        Glyph::Shield => "m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6l8-3Zm-4 9 3 3 5-6",
        Glyph::Connections => "M3 4h18v12H3V4ZM8 21h8M12 16v5M8 9h8M8 12h4",
        Glyph::Keyboard => {
            "M3 6h18v13H3V6ZM7 10h.01M11 10h.01M15 10h.01M18 10h.01M7 13h.01M11 13h.01M15 13h.01M7 16h10"
        }
    };
    view! { <svg class="size-5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg> }
}
