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
    Paperclip,
    Reply,
    Reaction,
    More,
    Flag,
    Close,
    Expand,
    Person,
    Shield,
    Keyboard,
    Search,
    Soundboard,
    Back,
    Download,
    Palette,
    Chevron,
    Friends,
    Server,
    Update,
    Emoji,
    Status,
    Message,
    Phone,
    Ban,
    UserMinus,
    Voice,
    Badge,
    Power,
    Globe,
    Bell,
    Lock,
    Key,
    Logout,
    Devices,
    Hash,
    Users,
    Sliders,
    Bot,
    Folder,
    Ticket,
    Gavel,
    Spotlight,
    Grid,
    ArrowLeft,
    ArrowRight,
    Eye,
    EyeOff,
    Shrink,
    First,
    Last,
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
        Glyph::Chevron => "m6 15 6-6 6 6",
        Glyph::CameraOff => "m3 3 18 18M9 5h6v6M15 10l6-4v12l-4-3M3 7v12h12v-4",
        Glyph::Signal => "M4 20v-3M9 20v-7M14 20V9M19 20V4",
        Glyph::Volume => "M3 9h4l5-4v14l-5-4H3V9ZM16 8a6 6 0 0 1 0 8M19 5a10 10 0 0 1 0 14",
        Glyph::VolumeOff => "M3 9h4l5-4v14l-5-4H3V9Zm13 0 6 6M16 15l6-6",
        Glyph::Edit => "m4 16 12-12 4 4L8 20H4v-4ZM13 7l4 4",
        Glyph::Trash => "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7",
        Glyph::Reply => "m9 5-6 6 6 6M3 11h10a7 7 0 0 1 7 7",
        Glyph::Reaction => {
            "M20 13a8 8 0 1 1-9-9M8 10h.01M14 10h.01M8 14a4 4 0 0 0 6 0M19 2v6M16 5h6"
        }
        Glyph::More => "M5 12h.01M12 12h.01M19 12h.01",
        Glyph::Flag => "M5 21V3m0 1c5-4 9 4 14 0v10c-5 4-9-4-14 0",
        Glyph::Paperclip => {
            "m21 11-9 9a6 6 0 0 1-8.5-8.5l10-10a4 4 0 0 1 5.7 5.7l-10 10a2 2 0 0 1-2.8-2.8l9-9"
        }
        Glyph::Send => "m3 3 19 9-19 9 4-9-4-9ZM7 12h15",
        Glyph::Close => "m6 6 12 12M6 18 18 6",
        Glyph::Back => "m15 5-7 7 7 7",
        Glyph::Download => "M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5",
        Glyph::Soundboard => "M4 10h3l6-5v14l-6-5H4v-4ZM17 9l2-2M17.5 12H21M17 15l2 2",
        Glyph::Expand => "M8 3H3v5M16 3h5v5M21 16v5h-5M8 21H3v-5",
        Glyph::Person => "M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM4 21v-2a8 8 0 0 1 16 0v2",
        Glyph::Shield => "m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6l8-3Zm-4 9 3 3 5-6",
        Glyph::Palette => {
            "M12 3a9 9 0 0 0 0 18 2 2 0 0 0 1.5-3.3 2 2 0 0 1 1.5-3.3h2.4A4.6 4.6 0 0 0 21 9.8C21 6 17 3 12 3ZM7.5 11.5h.01M10 7.5h.01M14.5 7.5h.01M17 11h.01"
        }
        Glyph::Friends => {
            "M14 8a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM2 21v-1a7 7 0 0 1 10.5-6M19 21l-3.3-3.2a2 2 0 0 1 3.3-2.3 2 2 0 0 1 3.3 2.3L19 21Z"
        }
        Glyph::Globe => {
            "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM3.5 9h17M3.5 15h17M12 3c-2.5 2.5-3.5 5.5-3.5 9s1 6.5 3.5 9c2.5-2.5 3.5-5.5 3.5-9s-1-6.5-3.5-9Z"
        }
        Glyph::Ban => "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM5.6 5.6l12.8 12.8",
        Glyph::UserMinus => "M14 8a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM2 21v-1a7 7 0 0 1 12-4.9M16 18h6",
        Glyph::Phone => {
            "M5 3h4l2 5-2.5 1.5a11 11 0 0 0 6 6L16 13l5 2v4a2 2 0 0 1-2 2A16 16 0 0 1 3 5a2 2 0 0 1 2-2Z"
        }
        Glyph::Voice => "M12 4v16M8 8v8M16 8v8M4 11v2M20 11v2",
        Glyph::Badge => "M4 4h9l7 7-9 9-7-7V4ZM8.5 8.5h.01",
        Glyph::Power => "M12 3v8M6.3 7a8 8 0 1 0 11.4 0",
        Glyph::Emoji => {
            "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM8 14a4 4 0 0 0 8 0M9 9.5h.01M15 9.5h.01"
        }
        Glyph::Status => "M20 14.5A8.5 8.5 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5Z",
        Glyph::Message => "M4 5h16v11H9l-5 4V5ZM8 9.5h8M8 12.5h5",
        Glyph::Update => "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM12 16V8M8.5 11.5 12 8l3.5 3.5",
        Glyph::Server => "M4 4h16v6H4V4ZM4 14h16v6H4v-6ZM8 7h.01M8 17h.01",
        Glyph::Bell => "M6 9a6 6 0 0 1 12 0c0 6 2.5 8 2.5 8h-17S6 15 6 9ZM10 21h4",
        Glyph::Lock => "M5 11h14v10H5V11ZM8 11V7a4 4 0 0 1 8 0v4M12 15v2",
        Glyph::Key => "M14.5 9.5a5 5 0 1 1-1.4 3.5L3 23M7 19l2 2M10 16l2 2M17 7h.01",
        Glyph::Logout => "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9",
        Glyph::Devices => "M2 5h14v10H2V5ZM6 19h6M9 15v4M18 9h4v11h-4V9ZM20 17h.01",
        Glyph::Hash => "M5 9h15M4 15h15M10 3 8 21M16 3l-2 18",
        Glyph::Users => {
            "M15 8a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0ZM4 20v-1a7 7 0 0 1 11-5.7M17 11a3 3 0 1 0-1.2-5.75M17.5 14.5A5 5 0 0 1 21 19.5V20"
        }
        Glyph::Sliders => "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1M15 4v4M9 10v4M17 16v4",
        Glyph::Bot => "M5 8h14v11H5V8ZM12 8V4M9 13h.01M15 13h.01M9 16h6M2 12v3M22 12v3",
        Glyph::Folder => {
            "M3 6a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6Z"
        }
        Glyph::Ticket => {
            "M3 7a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v3a2 2 0 0 0 0 4v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-3a2 2 0 0 0 0-4V7ZM14 5v14"
        }
        Glyph::Gavel => {
            "m14 13-8.5 8.5a2.1 2.1 0 0 1-3-3L11 10M15 16l6-6M8 9l6-6M9.5 7.5l7 7M13 21h8"
        }
        Glyph::Spotlight => "M3 5h12v14H3V5ZM18 5h3v4h-3V5ZM18 10h3v4h-3v-4ZM18 15h3v4h-3v-4Z",
        Glyph::Grid => "M4 4h7v7H4V4ZM13 4h7v7h-7V4ZM4 13h7v7H4v-7ZM13 13h7v7h-7v-7Z",
        Glyph::ArrowLeft => "M19 12H5m6-6-6 6 6 6",
        Glyph::ArrowRight => "M5 12h14m-6-6 6 6-6 6",
        Glyph::Eye => {
            "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12ZM15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"
        }
        Glyph::EyeOff => {
            "m3 3 18 18M10.6 5.1Q11.3 5 12 5c6 0 10 7 10 7a17 17 0 0 1-3.2 3.9M6.6 6.6C3.8 8.4 2 12 2 12s4 7 10 7a9.6 9.6 0 0 0 5.4-1.6M9.9 9.9a3 3 0 0 0 4.2 4.2"
        }
        Glyph::Shrink => "M3 8h5V3M21 8h-5V3M21 16h-5v5M3 16h5v5",
        Glyph::First => "M5 5v14M19 12H9m5-6-6 6 6 6",
        Glyph::Last => "M19 5v14M5 12h10m-5-6 6 6-6 6",
        Glyph::Keyboard => {
            "M3 6h18v13H3V6ZM7 10h.01M11 10h.01M15 10h.01M18 10h.01M7 13h.01M11 13h.01M15 13h.01M7 16h10"
        }
    };
    view! { <svg class="size-5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg> }
}
