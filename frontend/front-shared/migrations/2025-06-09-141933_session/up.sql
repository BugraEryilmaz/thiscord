-- Your SQL goes here
CREATE TABLE IF NOT EXISTS session (
    id INT PRIMARY KEY NOT NULL,
    token VARCHAR(255) NOT NULL,
    user_id TEXT NOT NULL
);