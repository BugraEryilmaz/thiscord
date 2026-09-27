//! Account transport only. Secrets deliberately do not implement Debug.
use crate::{AccountId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};

pub const ACCOUNT_PATH: &str = "/api/v1/account";
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
