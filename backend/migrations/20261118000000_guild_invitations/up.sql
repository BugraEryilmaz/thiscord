-- Adding a member by username now creates a pending invitation that only the
-- addressed account can accept. At most one per (server, account); it grants
-- nothing until accepted. Server or invited-account deletion removes it; the
-- inviter's deletion keeps it, like invite codes keep working without their creator.
CREATE TABLE guild_invitations (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    invited_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (guild_id, account_id)
);
CREATE INDEX guild_invitations_account ON guild_invitations(account_id, created_at, guild_id);
CREATE INDEX guild_invitations_inviter ON guild_invitations(invited_by);
