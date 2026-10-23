//! A single-guild music bot. The operator fixes the voice destination and an
//! optional local file; chat input can never select arbitrary local files.
use std::sync::Arc;
use thiscord_frontend::bots::voice::PlaybackEvent;
use thiscord_frontend::bots::{Bot, Credentials, Error, Voice};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("THISCORD_BOT_SERVER")?;
    let token = Credentials::from_env()?;
    let text = std::env::var("THISCORD_BOT_TEXT_CHANNEL").unwrap_or_else(|_| "general".into());
    let voice = std::env::var("THISCORD_BOT_VOICE_CHANNEL").unwrap_or_else(|_| "Music".into());
    let bot = Bot::new(&server, token)?.text_channel(&text).await?;
    let voice_channel = bot.voice_channel(&voice).await?;
    let file = std::env::var("THISCORD_BOT_AUDIO_FILE").ok();
    let player = Arc::new(Mutex::new(None::<Voice>));
    let play = player.clone();
    let local = player.clone();
    let stop = player.clone();
    let leave = player.clone();
    let status = player.clone();
    let mut bot = bot
        .command(
            "play",
            "Play one HTTPS YouTube video URL",
            move |ctx, args| {
                let player = play.clone();
                async move {
                    let mut state = player.lock().await;
                    if state.is_none() {
                        *state = Some(ctx.join_voice(voice_channel).await?);
                    }
                    state.as_ref().unwrap().play_youtube(args.trim()).await?;
                    ctx.reply("Playback requested. Use /stop to stop or /leave to disconnect.")
                        .await?;
                    Ok(())
                }
            },
        )?
        .command(
            "local",
            "Play the audio file configured by the bot operator",
            move |ctx, _| {
                let player = local.clone();
                let file = file.clone();
                async move {
                    let file =
                        file.ok_or_else(|| Error::message("Set THISCORD_BOT_AUDIO_FILE first"))?;
                    let mut state = player.lock().await;
                    if state.is_none() {
                        *state = Some(ctx.join_voice(voice_channel).await?);
                    }
                    state.as_ref().unwrap().play_file(file).await?;
                    ctx.reply("Local playback requested.").await?;
                    Ok(())
                }
            },
        )?
        .command("stop", "Stop playback and stay in voice", move |ctx, _| {
            let player = stop.clone();
            async move {
                if let Some(voice) = player.lock().await.as_ref() {
                    voice.stop().await?;
                }
                ctx.reply("Playback stopped.").await?;
                Ok(())
            }
        })?
        .command(
            "status",
            "Show the current playback state",
            move |ctx, _| {
                let player = status.clone();
                async move {
                    let description = match player.lock().await.as_ref() {
                        Some(voice) => match &*voice.status().borrow() {
                            PlaybackEvent::Error(message) => format!("Playback failed: {message}"),
                            event => format!("{event:?}"),
                        },
                        None => "Not connected".into(),
                    };
                    ctx.reply(description).await?;
                    Ok(())
                }
            },
        )?
        .command("leave", "Stop playback and leave voice", move |ctx, _| {
            let player = leave.clone();
            async move {
                if let Some(voice) = player.lock().await.take() {
                    voice.leave().await?;
                }
                ctx.reply("Left voice.").await?;
                Ok(())
            }
        })?;
    if let Ok(origin) = std::env::var("THISCORD_BOT_ORIGIN") {
        bot = bot.origin(&origin)?;
    }
    let result = tokio::select! {
        result = bot.run() => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    if let Some(voice) = player.lock().await.take() {
        let _ = voice.leave().await;
    }
    result?;
    Ok(())
}
