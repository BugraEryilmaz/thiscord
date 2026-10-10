//! Account transport only. Secrets deliberately do not implement Debug.
use crate::{AccountId, AvatarId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};

pub const ACCOUNT_PATH: &str = "/api/v1/account";
pub const AVATAR_PATH: &str = "/api/v1/avatars";
pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_AVATAR_BASE64: usize = MAX_AVATAR_BYTES.div_ceil(3) * 4;
pub const AVATAR_SIZE: u32 = 256;
pub const GOOGLE_CALLBACK_PATH: &str = "/api/v1/account/google/callback";
pub const EMAIL_VERIFICATION_PATH: &str = "/api/v1/account/verify-email";

#[derive(Serialize, Deserialize)]
pub struct EmailVerificationQuery {
    pub token: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountRequest {
    Register {
        username: String,
        email: String,
        password: String,
        device: String,
    },
    Login {
        login: String,
        password: String,
        device: String,
    },
    Current,
    Logout,
    LogoutAll,
    Rotate,
    Sessions,
    RevokeSession {
        session_id: SessionId,
    },
    SendVerification,
    VerifyEmail {
        code: String,
    },
    ForgotPassword {
        email: String,
    },
    ResetPassword {
        code: String,
        password: String,
    },
    Reauthenticate {
        password: String,
    },
    ChangePassword {
        password: String,
    },
    UpdateProfile {
        display_name: String,
        bio: String,
    },
    /// Standard padded base64 PNG/JPEG/WebP, or null to remove the picture.
    SetAvatar {
        image_base64: Option<String>,
    },
    UnlinkIdentity {
        provider: IdentityProvider,
    },
    DeleteAccount {
        confirmation: String,
    },
    GoogleStart {
        purpose: GooglePurpose,
        callback: Option<String>,
        device: String,
    },
    GoogleComplete {
        ticket: String,
    },
    GoogleCancel {
        ticket: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityProvider {
    Password,
    Google,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GooglePurpose {
    Login,
    Link,
    Reauthenticate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub username: String,
    pub email: String,
    pub email_verified: bool,
    pub display_name: String,
    pub bio: String,
    #[serde(default)]
    pub avatar_id: Option<AvatarId>,
    pub created_at: Timestamp,
    pub identities: Vec<IdentityProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub device: String,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub expires_at: Timestamp,
    pub current: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionGrant {
    pub token: String,
    pub expires_at: Timestamp,
    pub account: Account,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AccountResponse {
    Session {
        session: SessionGrant,
    },
    Account {
        account: Account,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Done {
        message: String,
    },
    GoogleStarted {
        authorization_url: String,
        ticket: String,
    },
    Pending,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_commands_and_older_profiles_are_wire_compatible() {
        let remove =
            serde_json::to_value(AccountRequest::SetAvatar { image_base64: None }).unwrap();
        assert_eq!(
            remove,
            serde_json::json!({"action":"set_avatar","image_base64":null})
        );
        let account: Account = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-0000-0000-000000000001", "username":"user",
            "email":"user@example.test", "email_verified":false, "display_name":"User",
            "bio":"", "created_at":"2026-10-10T00:00:00Z", "identities":["password"]
        }))
        .unwrap();
        assert!(account.avatar_id.is_none());
        assert!(
            serde_json::from_value::<AccountRequest>(serde_json::json!({
                "action":"set_avatar", "image_base64":null, "account_id":account.id
            }))
            .is_err()
        );
    }
}
