use thiscord_frontend::bots::{Bot, Context, Credentials, Error};

async fn echo(ctx: Context, args: String) -> Result<(), Error> {
    ctx.reply(if args.trim().is_empty() {
        "Usage: /echo some text".into()
    } else {
        args
    })
    .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("THISCORD_BOT_SERVER")?;
    let token = Credentials::from_env()?;
    let channel = std::env::var("THISCORD_BOT_TEXT_CHANNEL")?.parse()?;
    let mut bot = Bot::new(&server, token)?
        .channel(channel)
        .command("echo", "Repeat the argument string", echo)?
        .command(
            "ping",
            "Check that the bot is online",
            |ctx, _| async move {
                ctx.reply("Pong!").await?;
                Ok(())
            },
        )?
        .on_mention(|ctx, _| async move {
            ctx.reply("Mention me with /help to see my commands.")
                .await?;
            Ok(())
        });
    if let Ok(origin) = std::env::var("THISCORD_BOT_ORIGIN") {
        bot = bot.origin(&origin)?;
    }
    tokio::select! { result = bot.run() => result?, result = tokio::signal::ctrl_c() => result? }
    Ok(())
}
