//! Human-authenticated provisioning. New tokens go to the OS credential store,
//! never stdout, plaintext files, or command-line arguments.
use thiscord_frontend::bots::{Credentials, Error, Manager};
use thiscord_shared::bots::BotResponse;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Err("Usage: bot_manage list | create NAME | rotate BOT_ID | delete BOT_ID".into());
    }
    let server = std::env::var("THISCORD_BOT_SERVER")?;
    let guild = std::env::var("THISCORD_BOT_GUILD")?.parse()?;
    let token = match std::env::var("THISCORD_USER_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            let server = server.clone();
            tokio::task::spawn_blocking(move || {
                keyring::Entry::new("tr.com.thiscord.desktop", &server)
                    .map_err(|_| Error::message("OS credential store unavailable"))?
                    .get_password()
                    .map_err(|_| Error::message("Sign in with the desktop app using this server URL, or inject THISCORD_USER_TOKEN"))
            }).await??
        }
    };
    let manager = Manager::new(&server, token)?;
    let BotResponse::Bots { bots, revision } = manager.list(guild).await? else {
        return Err(Error::message("Unexpected list response").into());
    };
    let response = match args[0].as_str() {
        "list" if args.len() == 1 => {
            for bot in bots {
                println!("{} {}", bot.account_id, bot.name);
            }
            println!("Guild revision: {revision}");
            return Ok(());
        }
        "create" if args.len() == 2 => manager.create(guild, revision, &args[1]).await?,
        "rotate" if args.len() == 2 => manager.rotate(guild, revision, args[1].parse()?).await?,
        "delete" if args.len() == 2 => manager.delete(guild, revision, args[1].parse()?).await?,
        _ => return Err("Invalid command arguments".into()),
    };
    match response {
        BotResponse::Credential { bot, token, .. } => {
            let name = bot.name.clone();
            tokio::task::spawn_blocking(move || Credentials::store(&server, &name, &token))
                .await??;
            println!(
                "Credential saved for {} ({}). Set THISCORD_BOT_NAME={} to run it.",
                bot.name, bot.account_id, bot.name
            );
        }
        BotResponse::Done { .. } => {
            if let Some(bot) = bots
                .iter()
                .find(|bot| bot.account_id.to_string() == args[1])
            {
                let name = bot.name.clone();
                let _ =
                    tokio::task::spawn_blocking(move || Credentials::delete(&server, &name)).await;
            }
            println!("Bot removed.");
        }
        _ => return Err(Error::message("Unexpected management response").into()),
    }
    Ok(())
}
