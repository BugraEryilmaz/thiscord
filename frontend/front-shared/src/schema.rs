// @generated automatically by Diesel CLI.

diesel::table! {
    audio_config (id) {
        id -> Nullable<Integer>,
        high_pass_filter -> Bool,
        echo_cancellation -> Bool,
        noise_suppression_level -> Nullable<Integer>,
        gain_controller -> Bool,
        input_mode -> Integer,
        ptt_key_code -> Nullable<Text>,
        vad_threshold -> Nullable<Integer>,
        global_attenuation -> Nullable<Integer>,
        global_attenuation_trigger -> Nullable<Integer>,
    }
}

diesel::table! {
    last_used_audio_devices (id) {
        id -> Nullable<Integer>,
        mic -> Nullable<Text>,
        speaker -> Nullable<Text>,
        mic_boost -> Nullable<Integer>,
        speaker_boost -> Nullable<Integer>,
    }
}

diesel::table! {
    per_user_boost (id) {
        id -> Nullable<Integer>,
        user_id -> Text,
        boost_level -> Integer,
    }
}

diesel::table! {
    session (id) {
        id -> Integer,
        token -> Text,
        user_id -> Text,
        username -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    audio_config,
    last_used_audio_devices,
    per_user_boost,
    session,
);
