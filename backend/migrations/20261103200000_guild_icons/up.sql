-- One normalized icon per guild. The opaque ID changes on every replacement so
-- old image URLs stop resolving; guild deletion removes the image.
CREATE TABLE guild_icons (
    guild_id UUID PRIMARY KEY REFERENCES guilds(id) ON DELETE CASCADE,
    id UUID NOT NULL UNIQUE,
    png BYTEA NOT NULL CHECK (octet_length(png) BETWEEN 1 AND 300000)
);
