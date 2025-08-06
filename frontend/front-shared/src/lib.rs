pub const URL: &str = "thiscord.com.tr";

mod update;
pub mod models;
#[cfg(feature = "diesel")]
pub mod schema;
use std::error::Error;

#[cfg(feature = "diesel")]
use diesel::migration::MigrationVersion;
use shared::models::AudioChannelMemberUpdate;
pub use update::{DownloadProgress, UpdateState};

mod login;
pub use login::{LoginStatus, Session};

mod status;
pub use status::*;

pub mod audio;
pub use audio::*;

use serde::Deserialize;
use wasm_bindgen::JsValue;

#[derive(Deserialize)]
struct EventParams<T> {
    payload: T,
}

#[cfg(feature = "diesel")]
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
#[cfg(feature = "diesel")]
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[cfg(feature = "diesel")]
pub fn do_migrations<DB: diesel::backend::Backend, T: MigrationHarness<DB>>(
    conn: &mut T,
) -> Result<Vec<MigrationVersion>, Box<dyn Error + Send + Sync>> {
    conn.run_pending_migrations(MIGRATIONS)
}

pub trait FromEvent {
    fn from_event_js(event: JsValue) -> Result<Self, serde_wasm_bindgen::Error>
    where
        Self: Sized + serde::de::DeserializeOwned,
    {
        let ev: EventParams<Self> = serde_wasm_bindgen::from_value(event)?;
        Ok(ev.payload)
    }
}

impl FromEvent for AudioChannelMemberUpdate {}
