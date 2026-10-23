use super::*;
use thiscord_shared::bots::BOTS_PATH;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn scripting_bot_credentials_forward_opus_and_rotate_revoke_live_media() {
    let db = database();
    let (_, owner) = user(&db, "script_owner");
    permissions::bootstrap_owner(&db.pool, "script_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Scripts"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let (status,issued)=call(&app,BOTS_PATH,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"voice_script"})).await;
    assert_eq!(status, StatusCode::OK);
    let token = issued["token"].as_str().unwrap();
    let bot = issued["bot"]["account_id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut human = Peer::new(join(addr, &owner, guild.clone(), channel.clone()).await).await;
    let mut automated = Peer::new(join(addr, token, guild.clone(), channel.clone()).await).await;
    forwarded(&mut automated, &mut human).await;
    forwarded(&mut human, &mut automated).await;
    let (status, rotated) = call(
        &app,
        BOTS_PATH,
        &owner,
        json!({"action":"rotate","guild_id":guild,"revision":issued["revision"],"account_id":bot}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    loop {
        if matches!(
            event(&mut automated.socket).await,
            ServerEvent::Revoked {} | ServerEvent::Error { .. }
        ) {
            break;
        }
    }
    automated.close().await;
    let mut rejected = join(addr, token, guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut rejected).await,
        ServerEvent::Error { .. }
    ));
    let mut rejected = join(addr, &token[4..], guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut rejected).await,
        ServerEvent::Error { .. }
    ));
    // Replaying the old bot token cannot invalidate the replacement credential.
    let replacement = rotated["token"].as_str().unwrap();
    let mut automated =
        Peer::new(join(addr, replacement, guild.clone(), channel.clone()).await).await;
    forwarded(&mut automated, &mut human).await;
    state["guild"]["revision"] = rotated["revision"].clone();
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":bot},"allow":[],"deny":["join_voice"]}),StatusCode::OK).await;
    loop {
        if matches!(
            event(&mut automated.socket).await,
            ServerEvent::Revoked {} | ServerEvent::Error { .. }
        ) {
            break;
        }
    }
    automated.close().await;
    let mut rejected = join(addr, replacement, guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut rejected).await,
        ServerEvent::Error { .. }
    ));
    human.close().await;
    serving.abort();
    let _ = serving.await;
}
