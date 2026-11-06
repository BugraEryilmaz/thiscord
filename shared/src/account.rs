//! Account transport only. Secrets deliberately do not implement Debug.
use crate::{AccountId, AvatarId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};

pub const ACCOUNT_PATH: &str = "/api/v1/account";
/// Same-origin web clients send this header (value [`SESSION_MODE_COOKIE`]) on
/// account requests. Issued sessions then live only in an HttpOnly cookie and the
/// returned `Session::token` is empty; later requests authenticate with that cookie.
pub const SESSION_MODE_HEADER: &str = "x-thiscord-session";
pub const SESSION_MODE_COOKIE: &str = "cookie";
/// Path of the browser web client bundle served by the backend.
pub const WEB_APP_PATH: &str = "/app";
pub const AVATAR_PATH: &str = "/api/v1/avatars";
pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_AVATAR_BASE64: usize = MAX_AVATAR_BYTES.div_ceil(3) * 4;
pub const AVATAR_SIZE: u32 = 256;
pub const GOOGLE_CALLBACK_PATH: &str = "/api/v1/account/google/callback";
pub const EMAIL_VERIFICATION_PATH: &str = "/api/v1/account/verify-email";
pub const PASSWORD_RESET_PATH: &str = "/api/v1/account/reset-password";
/// Custom status text bound, in Unicode scalar values.
pub const MAX_STATUS_CHARS: usize = 128;
/// Longest accepted custom status lifetime: seven days, in seconds.
pub const MAX_STATUS_SECONDS: u32 = 7 * 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
pub struct PasswordResetQuery {
    #[serde(default)]
    pub token: String,
}

/// Browser form transport; secrets deliberately do not implement Debug.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordResetForm {
    pub token: String,
    pub password: String,
    pub confirm_password: String,
}

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
    /// Corrects the address of a pending (unverified) account and emails a new
    /// verification link; earlier links stop working. Requires recent
    /// reauthentication. Verified addresses cannot be changed in this version.
    ChangeEmail {
        email: String,
    },
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
    /// Null clears the custom status on every device.
    SetCustomStatus {
        status: Option<CustomStatusUpdate>,
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
    /// Absent from older servers and while no unexpired status is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<CustomStatus>,
}

/// Account-level custom status. Its audience matches the public profile (the
/// account itself, accepted friends and fellow guild members), except across a
/// block in either direction. Text is plain text, never Markdown or HTML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomStatus {
    /// One emoji from the shared palette ([`crate::chat::EMOJI_GROUPS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default)]
    pub text: String,
    /// Null means the status stays until cleared. Expired statuses are never sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// Longest accepted activity name, in Unicode scalar values.
pub const MAX_ACTIVITY_CHARS: usize = 64;

/// What an activity describes. Only games are detected today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Playing,
}

/// Live activity reported by a connected desktop client ("Playing Factorio").
/// It exists only while that client's friends socket is connected, is never
/// stored, and shares the online-presence audience (accepted friends and fellow
/// guild members, never across a block). The name is client-reported plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub kind: ActivityKind,
    pub name: String,
}

impl Activity {
    pub fn playing(name: impl Into<String>) -> Self {
        Self {
            kind: ActivityKind::Playing,
            name: name.into(),
        }
    }
    /// The name must already be trimmed, non-empty, single-line and bounded.
    pub fn validate(&self) -> Result<(), &'static str> {
        let name = &self.name;
        if name.is_empty()
            || name.trim() != name
            || name.chars().count() > MAX_ACTIVITY_CHARS
            || name.chars().any(char::is_control)
        {
            return Err("Activity names must be 1 to 64 characters on one line");
        }
        Ok(())
    }
}

/// Requested custom status. The server computes the expiry with its own clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomStatusUpdate {
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub text: String,
    /// Seconds from now (1 to [`MAX_STATUS_SECONDS`]); null keeps it until cleared.
    #[serde(default)]
    pub expires_in: Option<u32>,
}

impl CustomStatusUpdate {
    /// Validates the update and returns its trimmed text. The emoji must be a
    /// single palette entry; text is bounded, single-line and control-free.
    pub fn validate(&self) -> Result<&str, &'static str> {
        let text = self.text.trim();
        if text.chars().count() > MAX_STATUS_CHARS || text.chars().any(char::is_control) {
            return Err("Custom status text must be at most 128 characters on one line");
        }
        if self
            .emoji
            .as_deref()
            .is_some_and(|emoji| !crate::chat::is_reaction_emoji(emoji))
        {
            return Err("Choose a custom status emoji from the picker");
        }
        if text.is_empty() && self.emoji.is_none() {
            return Err("Add an emoji or text to set a custom status");
        }
        if self
            .expires_in
            .is_some_and(|seconds| seconds == 0 || seconds > MAX_STATUS_SECONDS)
        {
            return Err("Custom status can expire after at most seven days");
        }
        Ok(text)
    }
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
    fn activities_are_bounded_single_line_and_stable_on_the_wire() {
        let valid = Activity::playing("Teamfight Tactics");
        assert_eq!(valid.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&valid).unwrap(),
            serde_json::json!({"kind":"playing","name":"Teamfight Tactics"})
        );
        assert!(
            Activity::playing("ç".repeat(MAX_ACTIVITY_CHARS))
                .validate()
                .is_ok()
        );
        for invalid in [
            String::new(),
            " Factorio".into(),
            "Factorio ".into(),
            "two\nlines".into(),
            "bell\u{7}".into(),
            "x".repeat(MAX_ACTIVITY_CHARS + 1),
        ] {
            assert!(Activity::playing(invalid).validate().is_err());
        }
        assert!(
            serde_json::from_value::<Activity>(serde_json::json!({"kind":"watching","name":"x"}))
                .is_err()
        );
    }

    #[test]
    fn custom_status_is_validated_and_additive() {
        let update =
            |emoji: Option<&str>, text: &str, expires_in: Option<u32>| CustomStatusUpdate {
                emoji: emoji.map(Into::into),
                text: text.into(),
                expires_in,
            };
        assert_eq!(
            update(Some("🎮"), "  Playing  ", None).validate(),
            Ok("Playing")
        );
        assert_eq!(update(Some("🎮"), "", Some(1800)).validate(), Ok(""));
        let longest = "ç".repeat(MAX_STATUS_CHARS);
        assert_eq!(
            update(None, &longest, Some(MAX_STATUS_SECONDS)).validate(),
            Ok(longest.as_str())
        );
        for invalid in [
            update(None, "", None),
            update(None, "   ", None),
            update(None, &"x".repeat(MAX_STATUS_CHARS + 1), None),
            update(None, "two\nlines", None),
            update(None, "bell\u{7}", None),
            update(Some("x"), "text", None),
            update(Some("🎮🎮"), "text", None),
            update(Some("<img>"), "", None),
            update(None, "text", Some(0)),
            update(None, "text", Some(MAX_STATUS_SECONDS + 1)),
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
        let wire = serde_json::json!({"action":"set_custom_status","status":{"emoji":"🎮","text":"Playing","expires_in":3600}});
        let request: AccountRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), wire);
        let clear: AccountRequest =
            serde_json::from_value(serde_json::json!({"action":"set_custom_status","status":null}))
                .unwrap();
        assert!(matches!(
            clear,
            AccountRequest::SetCustomStatus { status: None }
        ));
        // Clients cannot choose the server-computed expiry instant.
        assert!(
            serde_json::from_value::<AccountRequest>(serde_json::json!({
                "action":"set_custom_status",
                "status":{"text":"x","expires_at":"2026-01-01T00:00:00Z"}
            }))
            .is_err()
        );
        // Older servers omit the status; unset statuses stay off the wire.
        let account: Account = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-0000-0000-000000000001", "username":"user",
            "email":"user@example.test", "email_verified":true, "display_name":"User",
            "bio":"", "created_at":"2026-10-10T00:00:00Z", "identities":["password"],
            "custom_status":null
        }))
        .unwrap();
        assert!(account.custom_status.is_none());
        assert!(
            serde_json::to_value(&account)
                .unwrap()
                .get("custom_status")
                .is_none()
        );
        let status: CustomStatus =
            serde_json::from_value(serde_json::json!({"text":"Away"})).unwrap();
        assert_eq!((status.emoji.as_deref(), status.expires_at), (None, None));
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"text":"Away"})
        );
    }

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
