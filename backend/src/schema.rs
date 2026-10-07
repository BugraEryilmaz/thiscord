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
    channel_overrides (id) {
        id -> Uuid,
        guild_id -> Uuid,
        channel_id -> Uuid,
        role_id -> Nullable<Uuid>,
        account_id -> Nullable<Uuid>,
        allow -> Jsonb,
        deny -> Jsonb,
    }
}

diesel::table! {
    channel_reads (guild_id, channel_id, account_id) {
        guild_id -> Uuid,
        channel_id -> Uuid,
        account_id -> Uuid,
        through -> Int8,
    }
}

diesel::table! {
    channels (guild_id, id) {
        guild_id -> Uuid,
        id -> Uuid,
        name -> Text,
        kind -> Text,
    }
}

diesel::table! {
    chat_presence (id) {
        id -> Uuid,
        guild_id -> Uuid,
        channel_id -> Uuid,
        account_id -> Uuid,
        expires_at -> Timestamptz,
        typing_until -> Timestamptz,
    }
}

diesel::table! {
    guild_member_roles (guild_id, account_id, role_id) {
        guild_id -> Uuid,
        account_id -> Uuid,
        role_id -> Uuid,
    }
}

diesel::table! {
    guild_members (guild_id, account_id) {
        guild_id -> Uuid,
        account_id -> Uuid,
        joined_at -> Timestamptz,
    }
}

diesel::table! {
    guild_moderation (guild_id, account_id) {
        guild_id -> Uuid,
        account_id -> Uuid,
        banned -> Bool,
        timeout_until -> Nullable<Timestamptz>,
        voice_revision -> Int8,
    }
}

diesel::table! {
    guild_roles (guild_id, id) {
        guild_id -> Uuid,
        id -> Uuid,
        name -> Text,
        position -> Int4,
        everyone -> Bool,
        permissions -> Jsonb,
    }
}

diesel::table! {
    guilds (id) {
        id -> Uuid,
        name -> Text,
        owner -> Uuid,
        revision -> Int8,
        created_at -> Timestamptz,
        password_hash -> Nullable<Text>,
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
        owner_account_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    instance_admins (account_id) {
        account_id -> Uuid,
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
    message_events (sequence) {
        sequence -> Int8,
        guild_id -> Uuid,
        channel_id -> Uuid,
        message_id -> Uuid,
    }
}

diesel::table! {
    messages (id) {
        id -> Uuid,
        guild_id -> Uuid,
        channel_id -> Uuid,
        author_id -> Nullable<Uuid>,
        client_id -> Uuid,
        request_hash -> Text,
        content -> Text,
        mentions -> Jsonb,
        created_at -> Timestamptz,
        edited_at -> Nullable<Timestamptz>,
        deleted -> Bool,
        revision -> Int4,
        sequence -> Int8,
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
diesel::joinable!(channels -> guilds (guild_id));
diesel::joinable!(guild_members -> accounts (account_id));
diesel::joinable!(guild_moderation -> accounts (account_id));
diesel::joinable!(guild_moderation -> guilds (guild_id));
diesel::joinable!(guild_roles -> guilds (guild_id));
diesel::joinable!(guilds -> accounts (owner));
diesel::joinable!(identities -> accounts (account_id));
diesel::joinable!(instance -> accounts (owner_account_id));
diesel::joinable!(instance_admins -> accounts (account_id));
diesel::joinable!(mail_outbox -> accounts (account_id));
diesel::joinable!(message_events -> messages (message_id));
diesel::joinable!(messages -> accounts (author_id));
diesel::joinable!(oauth_attempts -> accounts (account_id));
diesel::joinable!(oauth_attempts -> sessions (session_id));
diesel::joinable!(session_tokens -> sessions (session_id));
diesel::joinable!(sessions -> accounts (account_id));

diesel::allow_tables_to_appear_in_same_query!(
    account_codes,
    accounts,
    auth_limits,
    channel_overrides,
    channel_reads,
    channels,
    chat_presence,
    guild_member_roles,
    guild_members,
    guild_moderation,
    guild_roles,
    guilds,
    identities,
    instance,
    instance_admins,
    mail_outbox,
    message_events,
    messages,
    oauth_attempts,
    session_tokens,
    sessions,
);
