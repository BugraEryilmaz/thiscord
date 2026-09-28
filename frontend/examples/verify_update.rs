//! Release gate: authenticate each installer against the client's embedded key.
use base64::{Engine, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};

fn verify() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: verify_update ARTIFACT SIGNATURE VERSION".into());
    }
    let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))?;
    let key = String::from_utf8(
        STANDARD.decode(
            config["plugins"]["updater"]["pubkey"]
                .as_str()
                .ok_or("Missing public key")?,
        )?,
    )?;
    let signature = String::from_utf8(STANDARD.decode(std::fs::read_to_string(&args[1])?.trim())?)?;
    let signature = Signature::decode(&signature)?;
    PublicKey::decode(&key)?.verify(&std::fs::read(&args[0])?, &signature, true)?;
    let version = signature
        .trusted_comment()
        .split('\t')
        .find_map(|part| part.strip_prefix("version:"));
    if version != Some(args[2].as_str()) {
        return Err("Signed version does not match release version".into());
    }
    Ok(())
}
fn main() {
    if verify().is_err() {
        eprintln!("Update artifact failed signature, public-key or version verification");
        std::process::exit(1);
    }
    println!("Update signature and version verified");
}
