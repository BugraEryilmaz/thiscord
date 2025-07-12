-- Your SQL goes here
CREATE TABLE IF NOT EXISTS last_used_audio_devices (
    id INTEGER PRIMARY KEY DEFAULT (1),
    mic TEXT,
    speaker TEXT
);