//! Explicit reports disclose only the selected evidence, never a conversation.
use crate::{
    AccountId, ChannelId, GuildId, MessageId, Timestamp,
    friends::Person,
    ids::{ConversationId, ReportId},
    pagination::{Page, PageRequest},
};
use serde::{Deserialize, Serialize};
pub const SAFETY_PATH: &str = "/api/v1/safety";
pub const MAX_BLOCKS: usize = 1000;
pub const REPORT_TEXT_ERROR: &str = "Use 1–2000 characters without control codes";
pub fn valid_report_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= 2000
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReportTarget {
    User {
        account_id: AccountId,
    },
    DirectMessage {
        conversation_id: ConversationId,
        message_id: MessageId,
    },
    GuildMessage {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
    },
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inbox {
    Mine,
    Instance,
    Guild {
        guild_id: GuildId,
        channel_id: ChannelId,
    },
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Open,
    Resolved,
    Dismissed,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub message_id: MessageId,
    pub author_id: Option<AccountId>,
    pub content: String,
    pub revision: i32,
    pub created_at: Timestamp,
    /// Metadata only. Reviewers receive no private attachment capability.
    pub attachments: Vec<crate::chat::ChatAttachment>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Report {
    pub id: ReportId,
    pub reporter_id: Option<AccountId>,
    pub subject_id: Option<AccountId>,
    pub target: ReportTarget,
    pub reason: String,
    pub evidence: Option<Evidence>,
    pub state: ReportState,
    pub resolution: String,
    pub revision: i32,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Blocks,
    Block {
        account_id: AccountId,
    },
    Unblock {
        account_id: AccountId,
    },
    CreateReport {
        id: ReportId,
        target: ReportTarget,
        reason: String,
    },
    Reports {
        inbox: Inbox,
        #[serde(default)]
        page: PageRequest,
    },
    Resolve {
        id: ReportId,
        revision: i32,
        state: ReportState,
        resolution: String,
        /// Required for guild moderation writes; current guild permissions and
        /// this revision are checked under the same exclusive guild lock.
        guild_revision: Option<i64>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Blocks { people: Vec<Person> },
    Reports { page: Page<Report> },
    Report { report: Box<Report> },
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forged_evidence_and_unbounded_pages_are_rejected() {
        let id = "00000000-0000-0000-0000-000000000001";
        assert!(serde_json::from_value::<Request>(serde_json::json!({"action":"create_report","id":id,"target":{"kind":"user","account_id":id},"reason":"reason","evidence":{"content":"forged"}})).is_err());
        assert!(serde_json::from_value::<Request>(serde_json::json!({"action":"reports","inbox":{"kind":"instance"},"page":{"limit":101}})).is_err());
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"action":"block","account_id":id,"blocked_id":id})
            )
            .is_err()
        );
    }
}
