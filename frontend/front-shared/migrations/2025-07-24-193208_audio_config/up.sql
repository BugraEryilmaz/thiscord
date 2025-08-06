-- Your SQL goes here
CREATE TABLE IF NOT EXISTS audio_config (
    id INTEGER PRIMARY KEY DEFAULT 1,
    high_pass_filter BOOLEAN NOT NULL DEFAULT 1,
    echo_cancellation BOOLEAN NOT NULL DEFAULT 1,
    -- NULL means disabled
    -- 1: Low, 2: Moderate, 3: High, 4: Very High
    noise_suppression_level INTEGER DEFAULT 2,
    gain_controller BOOLEAN NOT NULL DEFAULT 1,
    -- mode 0 is automatic, 1 is push-to-talk
    input_mode INTEGER NOT NULL DEFAULT 0,
    -- NULL means no ptt key
    ptt_key_code TEXT DEFAULT NULL,
    -- NULL means auto
    vad_threshold INTEGER DEFAULT NULL,
    -- NULL means no global attenuation
    -- Number of dB to attenuate the audio globally
    global_attenuation INTEGER DEFAULT NULL,
    -- NULL means no global attenuation
    -- 0: Self Voice, 1: Other Voice
    global_attenuation_trigger INTEGER DEFAULT NULL
);