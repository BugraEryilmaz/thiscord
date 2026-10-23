pub mod attachments;
#[cfg(all(not(target_arch = "wasm32"), feature = "native-audio"))]
pub mod audio;
#[cfg(all(not(target_arch = "wasm32"), feature = "scripting-bot"))]
#[path = "../../bots/src/lib.rs"]
pub mod bots;
pub mod chat_history;
pub mod downloads;
pub mod localization;
pub mod mentions;
pub mod message_links;
pub mod navigation;
pub mod notification_queue;
pub mod notification_sound;
pub mod rich_chat;
#[cfg(all(not(target_arch = "wasm32"), feature = "screen-share"))]
pub mod screen;
pub mod video_feedback;
pub mod voice_overlay;

pub mod audio_quality;
pub mod sidebar;
pub mod stream_ui;

#[cfg(all(not(target_arch = "wasm32"), feature = "voice-bot"))]
pub mod bot;

pub mod markdown;
