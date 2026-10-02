use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{from_value, json, to_value};
use thiscord_shared::{
    ApiError, ErrorCode, HealthResponse, HealthStatus, InstanceId, RequestId, Timestamp,
    pagination::{Page, PageCursor, PageRequest, PageSize},
    validation::{ValidationCode, validate_text},
};
use uuid::Uuid;

const ID: &str = "f8f72890-fbae-4e56-9e7b-8038b5c3a094";

#[test]
fn chat_envelopes_and_bounds_are_stable() {
    use thiscord_shared::chat::*;
    let frame = ClientFrame {
        version: SOCKET_VERSION,
        event: ClientEvent::Ping {},
    };
    assert_eq!(
        to_value(frame).unwrap(),
        json!({"version":1,"event":{"type":"ping"}})
    );
    assert!(
        from_value::<ChatRequest>(
            json!({"action":"history","guild_id":ID,"channel_id":ID,"limit":101})
        )
        .is_err()
    );
    assert!(
        from_value::<ClientFrame>(
            json!({"version":1,"event":{"type":"typing","active":true,"admin":true}})
        )
        .is_err()
    );
    assert!(from_value::<ChatRequest>(json!({"action":"send","guild_id":ID,"channel_id":ID,"client_id":"invalid","content":"hello"})).is_err());
}

#[test]
fn permission_contracts_reject_unknown_names_and_preserve_scope() {
    use thiscord_shared::{GuildId, RoleId, permissions::*};
    assert_eq!(
        to_value(Permission::ManageRoles).unwrap(),
        json!("manage_roles")
    );
    assert!(from_value::<Permission>(json!("super_admin")).is_err());
    let target = OverrideTarget::Role(ID.parse::<RoleId>().unwrap());
    assert_eq!(to_value(target).unwrap(), json!({"kind":"role","id":ID}));
    assert_eq!(
        to_value(PermissionRequest::Inspect {
            guild_id: ID.parse::<GuildId>().unwrap()
        })
        .unwrap(),
        json!({"action":"inspect","guild_id":ID})
    );
    assert!(
        from_value::<PermissionRequest>(
            json!({"action":"inspect","guild_id":ID,"administrator":true})
        )
        .is_err()
    );
    assert!(from_value::<Permissions>(json!(["manage_roles", "new_unknown_permission"])).is_err());
}

#[test]
fn ids_and_health_keep_their_wire_representations() {
    let id: InstanceId = ID.parse().unwrap();
    assert_eq!(to_value(id).unwrap(), json!(ID));
    assert_eq!(from_value::<InstanceId>(json!(ID)).unwrap(), id);
    assert!(from_value::<RequestId>(json!("not-a-uuid")).is_err());
    assert_eq!(
        to_value(HealthResponse {
            status: HealthStatus::Ok
        })
        .unwrap(),
        json!({"status":"ok"})
    );
}

#[test]
fn errors_are_stable_and_do_not_require_field_details() {
    let error = ApiError {
        code: ErrorCode::ServiceUnavailable,
        message: "Database is not ready".into(),
        request_id: ID.parse().unwrap(),
        fields: vec![],
    };
    let wire =
        json!({"code":"service_unavailable","message":"Database is not ready","request_id":ID});
    assert_eq!(to_value(&error).unwrap(), wire);
    assert_eq!(from_value::<ApiError>(wire).unwrap(), error);
}

#[test]
fn timestamps_normalize_offsets_and_reject_naive_dates() {
    let value: Timestamp = from_value(json!("2026-09-27T14:00:00.123456+02:00")).unwrap();
    assert_eq!(
        to_value(value).unwrap(),
        json!("2026-09-27T12:00:00.123456Z")
    );
    assert!(from_value::<Timestamp>(json!("2026-09-27T12:00:00")).is_err());
    assert!(from_value::<Timestamp>(json!("2026-02-30T12:00:00Z")).is_err());
}

#[test]
fn validation_counts_unicode_and_rejects_blank_input() {
    assert!(validate_text("name", "Çağrı", 5).is_ok());
    assert_eq!(
        validate_text("name", " \n\t", 10).unwrap_err().code,
        ValidationCode::Required
    );
    assert_eq!(
        validate_text("name", "Çağrı!", 5).unwrap_err().code,
        ValidationCode::TooLong
    );
}

#[test]
fn pagination_enforces_bounds_even_when_deserializing() {
    assert_eq!(
        from_value::<PageRequest>(json!({})).unwrap().limit.get(),
        50
    );
    for invalid in [
        json!(0),
        json!(101),
        json!(-1),
        json!(65536),
        json!(1.5),
        json!("10"),
    ] {
        assert!(from_value::<PageSize>(invalid).is_err());
    }
    assert_eq!(PageSize::try_from(100).unwrap().get(), 100);
    assert!(from_value::<PageRequest>(json!({"limti":10})).is_err());
    assert_eq!(
        to_value(Page::<u8> {
            items: vec![],
            next_cursor: None
        })
        .unwrap(),
        json!({"items":[],"next_cursor":null})
    );
}

#[test]
fn cursors_round_trip_timestamp_ties_and_reject_bad_tokens() {
    let time: Timestamp = "2026-09-27T12:00:00.123456789Z".parse().unwrap();
    let cursor = PageCursor::new(time, ID.parse().unwrap());
    let wire = to_value(&cursor).unwrap();
    assert_eq!(from_value::<PageCursor>(wire).unwrap(), cursor);
    assert_eq!(cursor.position().0.timestamp_subsec_nanos(), 123456000);
    assert_ne!(cursor, PageCursor::new(time, Uuid::nil()));
    for invalid in [
        "".to_owned(),
        "x".repeat(100_000),
        "!".repeat(34),
        URL_SAFE_NO_PAD.encode([2; 25]),
    ] {
        assert!(from_value::<PageCursor>(json!(invalid)).is_err());
    }
    let mut invalid_time = [0; 25];
    invalid_time[0] = 1;
    invalid_time[1..9].copy_from_slice(&i64::MAX.to_be_bytes());
    assert!(PageCursor::try_from(URL_SAFE_NO_PAD.encode(invalid_time)).is_err());
}
#[test]
fn voice_and_audio_contract_bounds() {
    use thiscord_shared::{
        audio::{AudioSettings, AudioStatus},
        voice::{ClientEvent, ClientFrame},
    };
    let frame = ClientFrame {
        version: 1,
        event: ClientEvent::State {
            muted: true,
            deafened: false,
        },
    };
    assert_eq!(
        to_value(frame).unwrap(),
        json!({"version":1,"event":{"type":"state","muted":true,"deafened":false}})
    );
    assert!(
        from_value::<ClientFrame>(json!({"version":1,"event":{"type":"ping","token":"extra"}}))
            .is_err()
    );
    let mut settings = AudioSettings::default();
    assert!(settings.validate().is_ok());
    for volume in [f32::NAN, f32::INFINITY, -0.1, 2.1] {
        settings.output_volume = volume;
        assert!(settings.validate().is_err());
    }
    assert!(from_value::<AudioSettings>(json!({"password":"must not be here"})).is_err());
    // Additive diagnostics must still accept an older native status envelope.
    let status: AudioStatus = from_value(json!({
        "running": true, "input_level": 0.25, "transmitting": false,
        "message": "Voice audio active", "streams": [],
        "dropped_samples": 0, "underrun_samples": 0
    }))
    .unwrap();
    assert_eq!(status.raw_input_level, 0.0);
    assert_eq!(status.processing_resets, 0);
    assert!(status.echo.is_none());
    assert!(status.recording.is_none());
}

#[test]
fn audio_model_selection_preserves_old_settings_and_rejects_unknown_models() {
    use thiscord_shared::audio::{AudioSettings, NoiseSuppressionModel};
    let old: AudioSettings = from_value(json!({"noise_suppression":true})).unwrap();
    assert!(old.noise_suppression);
    assert_eq!(old.noise_suppression_model, NoiseSuppressionModel::Sonora);
    assert!(!old.neural_echo);
    assert!(old.neural_echo_model.is_none());
    let mut neural: AudioSettings =
        from_value(json!({"neural_echo":true,"neural_echo_model":"/local/ree.tflite"})).unwrap();
    assert!(neural.validate().is_ok());
    assert_eq!(to_value(&neural).unwrap()["neural_echo"], true);
    neural.neural_echo_model = Some("\0invalid".into());
    assert!(neural.validate().is_err());
    let selected: AudioSettings = from_value(json!({
        "noise_suppression":false, "noise_suppression_model":"deep_filter_net3"
    }))
    .unwrap();
    assert_eq!(
        selected.noise_suppression_model,
        NoiseSuppressionModel::DeepFilterNet3
    );
    assert!(!selected.noise_suppression);
    assert_eq!(
        to_value(selected).unwrap()["noise_suppression_model"],
        "deep_filter_net3"
    );
    assert!(from_value::<AudioSettings>(json!({"noise_suppression_model":"unknown"})).is_err());
}
