DROP TABLE channel_overrides;
DROP TABLE channels;
DROP TABLE guild_member_roles;
DROP TABLE guild_roles;
ALTER TABLE guilds DROP CONSTRAINT guild_owner_member;
DROP TABLE guild_members;
DROP TABLE guilds;
DROP TABLE instance_admins;
ALTER TABLE instance DROP COLUMN owner_account_id;
