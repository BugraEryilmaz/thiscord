-- Your SQL goes here
CREATE TABLE IF NOT EXISTS per_user_boost (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    boost_level INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_per_user_boost_user_id ON per_user_boost (user_id);