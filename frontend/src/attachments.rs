//! Browser-independent attachment policy and retry payloads.
use thiscord_shared::{
    AttachmentId, ChannelId, ClientMessageId, GuildId,
    chat::{ChatRequest, MAX_ATTACHMENT_BYTES, MAX_MESSAGE_ATTACHMENTS},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Image,
    Video,
    File,
}

pub fn preview_kind(mime: &str) -> PreviewKind {
    match mime {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => PreviewKind::Image,
        "video/mp4" | "video/webm" | "video/ogg" => PreviewKind::Video,
        _ => PreviewKind::File,
    }
}

pub fn validate_file(size: u64, count: usize) -> Result<(), &'static str> {
    if count >= MAX_MESSAGE_ATTACHMENTS {
        return Err("Attach at most 4 files per message");
    }
    if size == 0 {
        return Err("Empty files cannot be attached");
    }
    if size > MAX_ATTACHMENT_BYTES as u64 {
        return Err("Each attachment must be 25 MiB or smaller");
    }
    Ok(())
}

/// Frozen once submitted: a failed message never rebuilds IDs from the composer.
#[derive(Clone, PartialEq)]
pub struct PendingSend {
    pub id: ClientMessageId,
    pub reply_to: Option<thiscord_shared::MessageId>,
    pub content: String,
    pub attachment_ids: Vec<AttachmentId>,
    pub filenames: Vec<String>,
    pub failed: bool,
}
impl PendingSend {
    pub fn request(&self, guild_id: GuildId, channel_id: ChannelId) -> ChatRequest {
        ChatRequest::Send {
            guild_id,
            channel_id,
            client_id: self.id,
            reply_to: self.reply_to,
            content: self.content.clone(),
            attachment_ids: self.attachment_ids.clone(),
        }
    }
}

pub fn can_send(content: &str, attachments: usize) -> bool {
    (!content.trim().is_empty() || attachments > 0) && attachments <= MAX_MESSAGE_ATTACHMENTS
}

/// Reserve before fetching; holds compressed Blob bytes until the last view closes.
#[derive(Default)]
pub struct FetchBudget {
    bytes: u64,
    active: usize,
}
impl FetchBudget {
    pub fn slots_full(&self) -> bool {
        self.active >= 2
    }
    pub fn reserve(&mut self, bytes: u64) -> bool {
        if bytes > MAX_ATTACHMENT_BYTES as u64
            || self.active >= 2
            || self.bytes.saturating_add(bytes) > 4 * MAX_ATTACHMENT_BYTES as u64
        {
            return false;
        }
        self.bytes += bytes;
        self.active += 1;
        true
    }
    pub fn finished(&mut self) {
        self.active = self.active.saturating_sub(1);
    }
    pub fn release(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_sub(bytes);
    }
}

/// Never let a remote filename become a path, device name, or hidden file.
pub fn download_name(filename: &str) -> String {
    let mut name: String = filename
        .chars()
        .take(180)
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    while name.len() > 180 {
        name.pop();
    }
    let name = name.trim_matches([' ', '.']);
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty() {
        return "attachment".into();
    }
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        format!("attachment-{name}")
    } else {
        name.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_content_is_download_only() {
        for mime in [
            "image/svg+xml",
            "text/html",
            "application/pdf",
            "video/quicktime",
            "image/png;anything",
            "",
        ] {
            assert!(preview_kind(mime) == PreviewKind::File);
        }
        assert!(preview_kind("image/png") == PreviewKind::Image);
        assert!(preview_kind("video/mp4") == PreviewKind::Video);
    }
    #[test]
    fn attachment_only_and_file_limits() {
        assert!(can_send("  ", 1));
        assert!(!can_send("  ", 0));
        assert!(validate_file(0, 0).is_err());
        assert!(validate_file(MAX_ATTACHMENT_BYTES as u64, 3).is_ok());
        assert!(validate_file(MAX_ATTACHMENT_BYTES as u64 + 1, 0).is_err());
        assert!(validate_file(1, MAX_MESSAGE_ATTACHMENTS).is_err());
    }
    #[test]
    fn retry_preserves_original_ids_and_order() {
        let id = "00000000-0000-0000-0000-000000000001";
        let mut pending = PendingSend {
            id: id.parse().unwrap(),
            reply_to: Some(id.parse().unwrap()),
            content: String::new(),
            attachment_ids: vec![id.parse().unwrap()],
            filenames: vec!["photo.png".into()],
            failed: true,
        };
        let original =
            serde_json::to_value(pending.request(id.parse().unwrap(), id.parse().unwrap()))
                .unwrap();
        pending.failed = false;
        let retry = serde_json::to_value(pending.request(id.parse().unwrap(), id.parse().unwrap()))
            .unwrap();
        assert_eq!(original, retry);
        assert_eq!(retry["client_id"], id);
        assert_eq!(retry["attachment_ids"][0], id);
        assert!(can_send(&pending.content, pending.attachment_ids.len()));
    }
    #[test]
    fn fetches_and_retained_bytes_are_bounded() {
        let mut budget = FetchBudget::default();
        let max = MAX_ATTACHMENT_BYTES as u64;
        assert!(!budget.reserve(max + 1));
        assert!(budget.reserve(max));
        assert!(budget.reserve(max));
        assert!(!budget.reserve(1));
        assert!(budget.slots_full());
        budget.finished();
        budget.finished();
        assert!(budget.reserve(max));
        budget.finished();
        assert!(budget.reserve(max));
        budget.finished();
        assert!(!budget.reserve(1));
        budget.release(max);
        assert!(budget.reserve(max));
    }
    #[test]
    fn filenames_cannot_escape_download_directory() {
        assert_eq!(download_name("../../secret"), "_.._secret");
        assert_eq!(download_name("CON.txt"), "attachment-CON.txt");
        assert_eq!(download_name("..."), "attachment");
        assert_eq!(download_name("clip.mp4"), "clip.mp4");
        assert!(!download_name("C:\\foo\n.txt").contains(['\\', ':', '\n']));
        assert!(download_name(&"💾".repeat(180)).len() <= 180);
    }
}
