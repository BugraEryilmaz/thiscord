// @generated automatically by Diesel CLI.

diesel::table! {
    account_avatars (account_id) {
        account_id -> Uuid,
        id -> Uuid,
        png -> Bytea,
    }
}

diesel::table! {
    account_blocks (blocker_id, blocked_id) {
        blocker_id -> Uuid,
        blocked_id -> Uuid,
        created_at -> Timestamptz,
    }
}

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
        is_bot -> Bool,
    }
}

diesel::table! {
    attachments (id) {
        id -> Uuid,
        guild_id -> Uuid,
        channel_id -> Uuid,
        owner_id -> Nullable<Uuid>,
        message_id -> Nullable<Uuid>,
        filename -> Text,
        content_type -> Text,
        size_bytes -> Int8,
        data -> Bytea,
        expires_at -> Timestamptz,
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
    channel_categories (id) {
        id -> Uuid,
        guild_id -> Uuid,
        name -> Text,
        position -> Int4,
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
        category_id -> Nullable<Uuid>,
        position -> Int4,
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
    direct_attachments (id) {
        id -> Uuid,
        conversation_id -> Uuid,
        owner_id -> Uuid,
        message_id -> Nullable<Uuid>,
        filename -> Text,
        content_type -> Text,
        size_bytes -> Int8,
        data -> Bytea,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    direct_messages (id) {
        id -> Uuid,
        conversation_id -> Uuid,
        author_id -> Uuid,
        client_id -> Uuid,
        content -> Text,
        created_at -> Timestamptz,
        revision -> Int4,
        deleted -> Bool,
        edited_at -> Nullable<Timestamptz>,
        attachments -> Jsonb,
        request_hash -> Nullable<Text>,
    }
}

diesel::table! {
    friend_calls (id) {
        id -> Uuid,
        conversation_id -> Uuid,
        caller -> Uuid,
        callee -> Uuid,
        state -> Text,
        expires_at -> Timestamptz,
        caller_until -> Timestamptz,
        callee_until -> Timestamptz,
    }
}

diesel::table! {
    friendships (id) {
        id -> Uuid,
        low_id -> Uuid,
        high_id -> Uuid,
        requester -> Uuid,
        state -> Text,
        created_at -> Timestamptz,
        last_message_at -> Timestamptz,
        low_read_at -> Nullable<Timestamptz>,
        high_read_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    guild_audit_log (id) {
        id -> Uuid,
        guild_id -> Uuid,
        created_at -> Timestamptz,
        actor -> Nullable<Uuid>,
        target -> Nullable<Uuid>,
        action -> Text,
        reason -> Text,
        details -> Text,
    }
}

diesel::table! {
    guild_invites (id) {
        id -> Uuid,
        guild_id -> Uuid,
        token_hash -> Text,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
        max_uses -> Int4,
        uses -> Int4,
        revoked -> Bool,
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
        server_muted -> Bool,
        server_deafened -> Bool,
        move_source -> Nullable<Uuid>,
        move_destination -> Nullable<Uuid>,
        move_revision -> Nullable<Int8>,
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
    instance_role_assignments (account_id) {
        account_id -> Uuid,
        role_id -> Uuid,
    }
}

diesel::table! {
    instance_role_limits (id) {
        id -> Uuid,
        name -> Text,
        role -> Text,
        builtin -> Bool,
        owned_guilds -> Int4,
        text_channels -> Int4,
        voice_channels -> Int4,
        members -> Int4,
        roles -> Int4,
        revision -> Int8,
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
    message_reactions (message_id, account_id, emoji) {
        message_id -> Uuid,
        account_id -> Uuid,
        emoji -> Text,
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
        attachments -> Jsonb,
        reply_to -> Nullable<Uuid>,
        pinned -> Bool,
    }
}

diesel::table! {
    notification_preferences (account_id) {
        account_id -> Uuid,
        revision -> Int8,
        settings -> Jsonb,
    }
}

diesel::table! {
    notification_release (singleton) {
        singleton -> Bool,
        version -> Text,
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
    safety_reports (id) {
        id -> Uuid,
        reporter_id -> Nullable<Uuid>,
        subject_id -> Nullable<Uuid>,
        guild_id -> Nullable<Uuid>,
        channel_id -> Nullable<Uuid>,
        target -> Jsonb,
        reason -> Text,
        evidence -> Nullable<Jsonb>,
        state -> Text,
        resolution -> Text,
        revision -> Int4,
        created_at -> Timestamptz,
        resolved_at -> Nullable<Timestamptz>,
        reviewer_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    scripting_bots (account_id) {
        account_id -> Uuid,
        guild_id -> Uuid,
        creator_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
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

diesel::joinable!(account_avatars -> accounts (account_id));
diesel::joinable!(account_codes -> accounts (account_id));
diesel::joinable!(attachments -> accounts (owner_id));
diesel::joinable!(attachments -> messages (message_id));
diesel::joinable!(channel_categories -> guilds (guild_id));
diesel::joinable!(channels -> guilds (guild_id));
diesel::joinable!(direct_attachments -> accounts (owner_id));
diesel::joinable!(direct_attachments -> direct_messages (message_id));
diesel::joinable!(direct_attachments -> friendships (conversation_id));
diesel::joinable!(direct_messages -> accounts (author_id));
diesel::joinable!(direct_messages -> friendships (conversation_id));
diesel::joinable!(friend_calls -> friendships (conversation_id));
diesel::joinable!(guild_audit_log -> guilds (guild_id));
diesel::joinable!(guild_invites -> accounts (created_by));
diesel::joinable!(guild_invites -> guilds (guild_id));
diesel::joinable!(guild_members -> accounts (account_id));
diesel::joinable!(guild_moderation -> accounts (account_id));
diesel::joinable!(guild_moderation -> guilds (guild_id));
diesel::joinable!(guild_roles -> guilds (guild_id));
diesel::joinable!(guilds -> accounts (owner));
diesel::joinable!(identities -> accounts (account_id));
diesel::joinable!(instance -> accounts (owner_account_id));
diesel::joinable!(instance_admins -> accounts (account_id));
diesel::joinable!(instance_role_assignments -> accounts (account_id));
diesel::joinable!(instance_role_assignments -> instance_role_limits (role_id));
diesel::joinable!(mail_outbox -> accounts (account_id));
diesel::joinable!(message_events -> messages (message_id));
diesel::joinable!(message_reactions -> accounts (account_id));
diesel::joinable!(message_reactions -> messages (message_id));
diesel::joinable!(messages -> accounts (author_id));
diesel::joinable!(notification_preferences -> accounts (account_id));
diesel::joinable!(oauth_attempts -> accounts (account_id));
diesel::joinable!(oauth_attempts -> sessions (session_id));
diesel::joinable!(scripting_bots -> guilds (guild_id));
diesel::joinable!(safety_reports -> guilds (guild_id));
diesel::joinable!(session_tokens -> sessions (session_id));
diesel::joinable!(sessions -> accounts (account_id));

diesel::allow_tables_to_appear_in_same_query!(
    account_avatars,
    account_blocks,
    account_codes,
    accounts,
    attachments,
    auth_limits,
    channel_categories,
    channel_overrides,
    channel_reads,
    channels,
    chat_presence,
    direct_attachments,
    direct_messages,
    friend_calls,
    friendships,
    guild_audit_log,
    guild_invites,
    guild_member_roles,
    guild_members,
    guild_moderation,
    guild_roles,
    guilds,
    identities,
    instance,
    instance_admins,
    instance_role_assignments,
    instance_role_limits,
    mail_outbox,
    message_events,
    message_reactions,
    messages,
    notification_preferences,
    notification_release,
    oauth_attempts,
    scripting_bots,
    safety_reports,
    session_tokens,
    sessions,
);
