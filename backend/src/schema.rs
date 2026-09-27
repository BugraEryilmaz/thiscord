// @generated automatically by Diesel CLI.

diesel::table! {
    account_codes (token_hash) {
        token_hash -> Text,
        account_id -> Uuid,
        purpose -> Text,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    accounts (id) {
        id -> Uuid,
        username -> Text,
        email -> Text,
        email_verified -> Bool,
        display_name -> Text,
        bio -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    auth_limits (key) {
        key -> Text,
        window_start -> Timestamptz,
        attempts -> Int4,
    }
}

diesel::table! {
    identities (provider, subject) {
        account_id -> Uuid,
        provider -> Text,
        subject -> Text,
        password_hash -> Nullable<Text>,
    }
}

diesel::table! {
    instance (singleton) {
        singleton -> Bool,
        id -> Uuid,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mail_outbox (id) {
        id -> Uuid,
        account_id -> Uuid,
        recipient -> Text,
        body -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    oauth_attempts (state_hash) {
        state_hash -> Text,
        ticket_hash -> Text,
        verifier -> Text,
        nonce -> Text,
        purpose -> Text,
        session_id -> Nullable<Uuid>,
        account_id -> Nullable<Uuid>,
        device -> Text,
        callback -> Nullable<Text>,
        status -> Text,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    session_tokens (token_hash) {
        token_hash -> Text,
        session_id -> Uuid,
        active -> Bool,
    }
}

diesel::table! {
    sessions (id) {
        id -> Uuid,
        account_id -> Uuid,
        device -> Text,
        created_at -> Timestamptz,
        last_seen_at -> Timestamptz,
        expires_at -> Timestamptz,
        reauthenticated_at -> Nullable<Timestamptz>,
        revoked -> Bool,
    }
}

diesel::joinable!(account_codes -> accounts (account_id));
diesel::joinable!(identities -> accounts (account_id));
diesel::joinable!(mail_outbox -> accounts (account_id));
diesel::joinable!(oauth_attempts -> accounts (account_id));
diesel::joinable!(oauth_attempts -> sessions (session_id));
diesel::joinable!(session_tokens -> sessions (session_id));
diesel::joinable!(sessions -> accounts (account_id));

diesel::allow_tables_to_appear_in_same_query!(
    account_codes,
    accounts,
    auth_limits,
    identities,
    instance,
    mail_outbox,
    oauth_attempts,
    session_tokens,
    sessions,
);
