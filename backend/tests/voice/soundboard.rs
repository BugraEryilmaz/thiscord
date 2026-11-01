use super::*;
use base64::engine::general_purpose::STANDARD;
use thiscord_shared::{SoundId, soundboard::*};

fn wav(seconds: f32) -> Vec<u8> {
    let rate = 48_000_u32;
    let data: Vec<u8> = (0..(rate as f32 * seconds) as usize)
        .flat_map(|n| {
            let value = (n as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.3;
            ((value * 32767.0) as i16).to_le_bytes()
        })
        .collect();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    for field in [16_u32, 0x0001_0001, rate, rate * 2, 0x0010_0002] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

async fn sounds(app: &Router, token: &str, request: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, SOUNDBOARD_PATH, token, request).await;
    assert_eq!(status, expected, "{body}");
    body
}

async fn download(
    app: &Router,
    token: &str,
    guild: &Value,
    channel: &Value,
    sound: &Value,
) -> (StatusCode, Vec<u8>) {
    let path = audio_path(
        serde_json::from_value(guild.clone()).unwrap(),
        serde_json::from_value(channel.clone()).unwrap(),
        serde_json::from_value(sound.clone()).unwrap(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    (
        status,
        to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
}

struct Fixture {
    db: Database,
    app: Router,
    state: Value,
    guild: Value,
    channel: Value,
    owner: String,
    member: (AccountId, String),
    listener: (AccountId, String),
}

async fn fixture(prefix: &str) -> Fixture {
    let db = database();
    let (_, owner) = user(&db, &format!("{prefix}_owner"));
    let member = user(&db, &format!("{prefix}_member"));
    let listener = user(&db, &format!("{prefix}_listener"));
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Sounds"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for name in [&format!("{prefix}_member"), &format!("{prefix}_listener")] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member","username":name}),
            StatusCode::OK,
        )
        .await;
    }
    for (name, kind) in [("lounge", "voice"), ("general", "text")] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"create_channel","name":name,"kind":kind}),
            StatusCode::OK,
        )
        .await;
    }
    let channel = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "lounge")
        .unwrap()["id"]
        .clone();
    Fixture {
        guild: state["guild"]["id"].clone(),
        db,
        app,
        state,
        channel,
        owner,
        member,
        listener,
    }
}

async fn upload(f: &Fixture, name: &str, revision: &Value) -> Value {
    sounds(&f.app, &f.owner, json!({"action":"upload","guild_id":f.guild,"revision":revision,"name":name,"emoji":"🦆","audio_base64":STANDARD.encode(wav(1.0))}), StatusCode::OK).await
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn soundboard_management_permissions_overrides_and_limits() {
    let mut f = fixture("sound_manage").await;
    let (member, mt) = f.member.clone();
    let list = json!({"action":"list","guild_id":f.guild,"channel_id":f.channel});
    // New guilds grant Use soundboard to Everyone; management needs Manage server.
    let state = sounds(&f.app, &mt, list.clone(), StatusCode::OK).await;
    // Every server starts with the built-in sounds, which are not uploads.
    let builtins: Vec<_> = state["sounds"].as_array().unwrap().clone();
    let names: Vec<_> = builtins
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["cricket", "clap", "sad horn", "ba dum tss"]);
    assert!(builtins.iter().all(|s| s["builtin"] == true));
    assert_eq!(state["can_manage"], false);
    assert_eq!(state["limit"], MAX_SOUNDS);
    sounds(
        &f.app,
        &mt,
        json!({"action":"list","guild_id":f.guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let revision = f.state["guild"]["revision"].clone();
    sounds(&f.app, &mt, json!({"action":"upload","guild_id":f.guild,"revision":revision,"name":"nope","audio_base64":STANDARD.encode(wav(1.0))}), StatusCode::FORBIDDEN).await;
    sounds(&f.app, &f.owner, json!({"action":"upload","guild_id":f.guild,"revision":revision.as_i64().unwrap() - 1,"name":"stale","audio_base64":STANDARD.encode(wav(1.0))}), StatusCode::CONFLICT).await;
    sounds(&f.app, &f.owner, json!({"action":"upload","guild_id":f.guild,"revision":revision,"name":"bad","audio_base64":STANDARD.encode(b"not audio")}), StatusCode::BAD_REQUEST).await;
    sounds(&f.app, &f.owner, json!({"action":"upload","guild_id":f.guild,"revision":revision,"name":" padded","audio_base64":STANDARD.encode(wav(1.0))}), StatusCode::BAD_REQUEST).await;
    let state = upload(&f, "quack", &revision).await;
    assert_eq!(state["can_manage"], true);
    let sound = state["sounds"][BUILTINS].clone();
    assert!(sound.get("builtin").is_none());
    assert_eq!(sound["name"], "quack");
    assert_eq!(sound["emoji"], "🦆");
    assert!(
        (990..=1000).contains(&sound["duration_ms"].as_i64().unwrap()),
        "{sound}"
    );

    // Anyone allowed in the voice channel can download for playback.
    let (status, pcm) = download(&f.app, &mt, &f.guild, &f.channel, &sound["id"]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(decode_pcm(&pcm).is_some_and(|samples| samples.len() > 47_000));
    // A cold-cache play makes a whole call download at once; none may be rejected.
    let burst = (0..12).map(|_| download(&f.app, &mt, &f.guild, &f.channel, &sound["id"]));
    for (status, pcm) in futures_util::future::join_all(burst).await {
        assert_eq!(status, StatusCode::OK);
        assert!(decode_pcm(&pcm).is_some());
    }
    let text = f.state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "general")
        .unwrap()["id"]
        .clone();
    assert_eq!(
        download(&f.app, &mt, &f.guild, &text, &sound["id"]).await.0,
        StatusCode::FORBIDDEN
    );
    let (outsider_id, outsider) = user(&f.db, "sound_manage_outsider");
    let _ = outsider_id;
    sounds(&f.app, &outsider, list.clone(), StatusCode::FORBIDDEN).await;
    assert_eq!(
        download(&f.app, &outsider, &f.guild, &f.channel, &sound["id"])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let missing = json!(SoundId::from_uuid(Uuid::new_v4()));
    assert_eq!(
        download(&f.app, &mt, &f.guild, &f.channel, &missing)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // A channel override hides the board but still lets listeners hear sounds.
    let channel = f.channel.clone();
    change(&f.app, &f.owner, &mut f.state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["use_soundboard"]}), StatusCode::OK).await;
    sounds(&f.app, &mt, list.clone(), StatusCode::FORBIDDEN).await;
    assert_eq!(
        download(&f.app, &mt, &f.guild, &f.channel, &sound["id"])
            .await
            .0,
        StatusCode::OK
    );
    change(&f.app, &f.owner, &mut f.state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["join_voice"]}), StatusCode::OK).await;
    assert_eq!(
        download(&f.app, &mt, &f.guild, &f.channel, &sound["id"])
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    // Writes check the viewed revision and the fixed per-guild allowance.
    let revision = sounds(
        &f.app,
        &f.owner,
        json!({"action":"list","guild_id":f.guild}),
        StatusCode::OK,
    )
    .await["revision"]
        .clone();
    let mut c = f.db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO guild_sounds(guild_id,name,pcm,duration_ms) SELECT $1::uuid,'filler'||n,'\\x0000'::bytea,0 FROM generate_series(2,$2) n")
        .bind::<Text, _>(f.guild.as_str().unwrap())
        .bind::<diesel::sql_types::Integer, _>(MAX_SOUNDS as i32)
        .execute(&mut c)
        .unwrap();
    drop(c);
    let (status, body) = call(&f.app, SOUNDBOARD_PATH, &f.owner, json!({"action":"upload","guild_id":f.guild,"revision":revision,"name":"overflow","audio_base64":STANDARD.encode(wav(1.0))})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("limit_exceeded")),
        "{body}"
    );
    sounds(
        &f.app,
        &mt,
        json!({"action":"delete","guild_id":f.guild,"revision":revision,"sound_id":sound["id"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let state = sounds(
        &f.app,
        &f.owner,
        json!({"action":"delete","guild_id":f.guild,"revision":revision,"sound_id":sound["id"]}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        state["sounds"].as_array().unwrap().len(),
        BUILTINS + MAX_SOUNDS - 1
    );
    // Built-in sounds stay available: they can be downloaded but never deleted.
    let builtin = &builtins[0]["id"];
    let (status, pcm) = download(&f.app, &f.owner, &f.guild, &f.channel, builtin).await;
    assert_eq!(status, StatusCode::OK);
    assert!(decode_pcm(&pcm).is_some());
    sounds(
        &f.app,
        &f.owner,
        json!({"action":"delete","guild_id":f.guild,"revision":revision,"sound_id":builtin}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    sounds(
        &f.app,
        &f.owner,
        json!({"action":"delete","guild_id":f.guild,"revision":revision,"sound_id":sound["id"]}),
        StatusCode::NOT_FOUND,
    )
    .await;
}

async fn played(socket: &mut Socket) -> (Value, AccountId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event(socket).await {
                ServerEvent::Soundboard {
                    sound_id,
                    account_id,
                } => return (json!(sound_id), account_id),
                ServerEvent::Revoked {} | ServerEvent::Error { .. } => {
                    panic!("soundboard must not end the call")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

async fn silent(socket: &mut Socket) {
    let quiet = tokio::time::timeout(Duration::from_millis(600), async {
        loop {
            if let ServerEvent::Soundboard { .. } = event(socket).await {
                break;
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "unexpected soundboard play");
}

fn play(sound: &Value) -> ClientEvent {
    ClientEvent::Soundboard {
        sound_id: serde_json::from_value(sound.clone()).unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn soundboard_plays_reach_opted_in_participants_and_follow_permissions() {
    let mut f = fixture("sound_play").await;
    let (member, mt) = f.member.clone();
    let (_, lt) = f.listener.clone();
    let revision = f.state["guild"]["revision"].clone();
    let uploaded = upload(&f, "airhorn", &revision).await;
    let sound = uploaded["sounds"][BUILTINS]["id"].clone();
    let builtin = uploaded["sounds"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, f.app.clone()).into_future());
    let mut owner =
        Peer::with_soundboard(join(addr, &f.owner, f.guild.clone(), f.channel.clone()).await).await;
    let mut player =
        Peer::with_soundboard(join(addr, &mt, f.guild.clone(), f.channel.clone()).await).await;
    // An older client that did not opt in never receives the unknown event.
    let mut legacy = Peer::new(join(addr, &lt, f.guild.clone(), f.channel.clone()).await).await;

    send(&mut player.socket, play(&sound)).await;
    assert_eq!(played(&mut owner.socket).await, (sound.clone(), member));
    assert_eq!(played(&mut player.socket).await, (sound.clone(), member));
    still_connected(&mut legacy.socket).await;

    // Rapid repeats and unknown sounds are dropped without ending the call.
    send(&mut player.socket, play(&sound)).await;
    silent(&mut owner.socket).await;
    tokio::time::sleep(SOUNDBOARD_GAP).await;
    send(
        &mut player.socket,
        play(&json!(SoundId::from_uuid(Uuid::new_v4()))),
    )
    .await;
    silent(&mut owner.socket).await;
    still_connected(&mut player.socket).await;

    // Current permissions are evaluated for every play.
    let channel = f.channel.clone();
    change(&f.app, &f.owner, &mut f.state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["use_soundboard"]}), StatusCode::OK).await;
    tokio::time::sleep(SOUNDBOARD_GAP).await;
    send(&mut player.socket, play(&sound)).await;
    silent(&mut owner.socket).await;
    still_connected(&mut player.socket).await;
    send(&mut owner.socket, play(&sound)).await;
    assert_eq!(played(&mut player.socket).await.0, sound);
    assert_eq!(played(&mut owner.socket).await.0, sound);
    // Built-in sounds play like uploads, under the same permission.
    tokio::time::sleep(SOUNDBOARD_GAP).await;
    send(&mut owner.socket, play(&builtin)).await;
    assert_eq!(played(&mut player.socket).await.0, builtin);
    assert_eq!(played(&mut owner.socket).await.0, builtin);
    send(&mut player.socket, play(&builtin)).await;
    silent(&mut owner.socket).await;

    // A play request without a negotiated soundboard is a protocol violation.
    send(&mut legacy.socket, play(&sound)).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match legacy.socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok());
    for peer in [owner, player] {
        peer.close().await;
    }
    legacy.pc.close().await.unwrap();
    serving.abort();
}

const BUILTINS: usize = 4;
const SOUNDBOARD_GAP: Duration = Duration::from_millis(1100);
