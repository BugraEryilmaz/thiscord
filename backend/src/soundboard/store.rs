//! Every action authenticates, locks the guild and evaluates current permissions
//! in the same transaction. Management writes also check the viewed guild revision.
use super::{
    builtin,
    decode::{self, Clip},
};
use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{authorization::load, evaluator::effective},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use diesel::{
    Connection, PgConnection,
    prelude::*,
    sql_types::{Binary, Integer, Nullable, Text, Uuid as SqlUuid},
};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, SoundId,
    permissions::{ChannelKind, GuildState, Permission, Permissions},
    soundboard::*,
};

fn require(value: bool) -> Result<(), Failure> {
    value.then_some(()).ok_or(Failure::Forbidden)
}

fn validate(name: &str, emoji: Option<&str>) -> Result<(), Failure> {
    if !valid_name(name) || emoji.is_some_and(|e| !valid_emoji(e)) {
        return Err(Failure::Invalid(
            "Sound names need 1 to 32 characters; an emoji is optional",
        ));
    }
    Ok(())
}

/// Locks are ordered session/account -> instance -> guild, like permission writes.
fn locked<T>(
    c: &mut PgConnection,
    token: &str,
    session: &auth::Session,
    guild: GuildId,
    exclusive: bool,
    action: impl FnOnce(&mut PgConnection) -> Result<T, Failure>,
) -> Result<T, Failure> {
    session.require_guild(guild)?;
    c.transaction(|c| {
        auth::read_session(c, token, session)?;
        execute(c, "SELECT singleton FROM instance FOR SHARE", &[])?;
        execute(
            c,
            if exclusive {
                "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE"
            } else {
                "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE"
            },
            &[&guild.to_string()],
        )?;
        action(c)
    })
}

fn guild_state(
    c: &mut PgConnection,
    guild: GuildId,
    actor: AccountId,
    channel: Option<ChannelId>,
) -> Result<GuildState, Failure> {
    let state = load(c, guild, &[actor], channel)?;
    require(state.members.iter().any(|m| m.account_id == actor))?;
    Ok(state)
}

/// Guild sounds belong to voice channels only; private calls have none.
fn voice_permissions(
    state: &GuildState,
    actor: AccountId,
    channel: ChannelId,
) -> Result<Permissions, Failure> {
    require(
        state
            .channels
            .iter()
            .any(|ch| ch.id == channel && ch.kind == ChannelKind::Voice),
    )?;
    let permissions = effective(state, actor, Some(channel));
    require(
        permissions.contains(&Permission::ViewChannel)
            && permissions.contains(&Permission::JoinVoice),
    )?;
    Ok(permissions)
}

fn manages(state: &GuildState, actor: AccountId) -> bool {
    effective(state, actor, None).contains(&Permission::ManageGuild)
}

/// Uploaded sounds only; built-in sounds are added by `snapshot`.
fn sounds(c: &mut PgConnection, guild: GuildId) -> Result<Vec<Sound>, Failure> {
    query(
        c,
        "SELECT jsonb_build_object('id',id,'name',name,'emoji',emoji,'duration_ms',duration_ms,'created_by',created_by,'created_at',created_at) AS data FROM guild_sounds WHERE guild_id=$1::uuid ORDER BY created_at,id",
        &[&guild.to_string()],
    )
}

fn snapshot(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
) -> Result<SoundboardState, Failure> {
    Ok(SoundboardState {
        guild_id: state.guild.id,
        revision: state.guild.revision,
        sounds: builtin::all()
            .iter()
            .map(|b| b.sound.clone())
            .chain(sounds(c, state.guild.id)?)
            .collect(),
        can_manage: manages(state, actor),
        limit: MAX_SOUNDS as u32,
    })
}

fn guild_of(request: &SoundboardRequest) -> GuildId {
    match request {
        SoundboardRequest::List { guild_id, .. }
        | SoundboardRequest::Upload { guild_id, .. }
        | SoundboardRequest::Delete { guild_id, .. } => *guild_id,
    }
}

/// Authorizes and throttles before spending CPU on untrusted media. No database
/// connection is held while decoding; the write transaction reauthorizes.
fn prepare_upload(
    pool: &DbPool,
    token: &str,
    request: &SoundboardRequest,
) -> Result<Clip, Failure> {
    let SoundboardRequest::Upload {
        guild_id,
        name,
        emoji,
        audio_base64,
        ..
    } = request
    else {
        return Err(Failure::Unavailable);
    };
    validate(name, emoji.as_deref())?;
    if audio_base64.len() > MAX_UPLOAD_BASE64 {
        return Err(Failure::Invalid("Choose a sound file up to 1 MiB"));
    }
    let actor = {
        let mut c = connection(pool)?;
        let session = auth::authenticate(&mut c, token)?;
        locked(&mut c, token, &session, *guild_id, false, |c| {
            let state = guild_state(c, *guild_id, session.account_id, None)?;
            require(manages(&state, session.account_id))
        })?;
        session.account_id
    };
    let key = format!("soundboard:{actor}");
    auth::rate_limit(pool, &key, Some(&key), false)?;
    let bytes = STANDARD
        .decode(audio_base64)
        .map_err(|_| Failure::Invalid("Choose a sound file up to 1 MiB"))?;
    decode::normalize(bytes)
}

pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    request: SoundboardRequest,
) -> Result<SoundboardState, Failure> {
    let mut clip = match request {
        SoundboardRequest::Upload { .. } => Some(prepare_upload(pool, token, &request)?),
        _ => None,
    };
    let guild = guild_of(&request);
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    let actor = session.account_id;
    let exclusive = !matches!(request, SoundboardRequest::List { .. });
    locked(&mut c, token, &session, guild, exclusive, |c| {
        let channel = match &request {
            SoundboardRequest::List { channel_id, .. } => *channel_id,
            _ => None,
        };
        let state = guild_state(c, guild, actor, channel)?;
        let revision = match &request {
            SoundboardRequest::List {
                channel_id: Some(channel),
                ..
            } => {
                let permissions = voice_permissions(&state, actor, *channel)?;
                require(permissions.contains(&Permission::UseSoundboard))?;
                None
            }
            SoundboardRequest::List {
                channel_id: None, ..
            } => {
                require(manages(&state, actor))?;
                None
            }
            SoundboardRequest::Upload { revision, .. }
            | SoundboardRequest::Delete { revision, .. } => Some(*revision),
        };
        if let Some(revision) = revision {
            require(manages(&state, actor))?;
            if revision != state.guild.revision {
                return Err(Failure::Conflict);
            }
        }
        match request {
            SoundboardRequest::List { .. } => {}
            SoundboardRequest::Upload { name, emoji, .. } => {
                if sounds(c, guild)?.len() >= MAX_SOUNDS {
                    return Err(Failure::Capacity(
                        "Server soundboard limit reached (24 sounds)",
                    ));
                }
                let clip = clip.take().ok_or(Failure::Unavailable)?;
                diesel::sql_query("INSERT INTO guild_sounds(guild_id,name,emoji,pcm,duration_ms,created_by) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind::<SqlUuid, _>(guild.as_uuid())
                    .bind::<Text, _>(name)
                    .bind::<Nullable<Text>, _>(emoji)
                    .bind::<Binary, _>(encode_pcm(&clip.samples))
                    .bind::<Integer, _>(clip.duration_ms as i32)
                    .bind::<SqlUuid, _>(actor.as_uuid())
                    .execute(c)?;
            }
            SoundboardRequest::Delete { sound_id, .. } => {
                if builtin::find(sound_id).is_some() {
                    return Err(Failure::Invalid("Built-in sounds cannot be deleted"));
                }
                let changed = execute(
                    c,
                    "DELETE FROM guild_sounds WHERE guild_id=$1::uuid AND id=$2::uuid",
                    &[&guild.to_string(), &sound_id.to_string()],
                )?;
                if changed == 0 {
                    return Err(Failure::NotFound);
                }
            }
        }
        snapshot(c, &state, actor)
    })
}

/// Playback download for anyone currently allowed in the voice channel, so
/// listeners without UseSoundboard still hear sounds played by others.
pub(super) fn audio(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
    sound: SoundId,
) -> Result<Vec<u8>, Failure> {
    #[derive(QueryableByName)]
    struct Pcm {
        #[diesel(sql_type = Binary)]
        pcm: Vec<u8>,
    }
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    let actor = session.account_id;
    locked(&mut c, token, &session, guild, false, |c| {
        let state = guild_state(c, guild, actor, Some(channel))?;
        voice_permissions(&state, actor, channel)?;
        if let Some(builtin) = builtin::find(sound) {
            return Ok(builtin.pcm.clone());
        }
        Ok(
            diesel::sql_query("SELECT pcm FROM guild_sounds WHERE guild_id=$1 AND id=$2")
                .bind::<SqlUuid, _>(guild.as_uuid())
                .bind::<SqlUuid, _>(sound.as_uuid())
                .get_result::<Pcm>(c)
                .optional()?
                .ok_or(Failure::NotFound)?
                .pcm,
        )
    })
}

/// Called by an active voice connection with its retained credential.
pub(crate) fn authorize_play(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
    sound: SoundId,
) -> Result<(), Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate_retained(&mut c, token)?;
    let actor = session.account_id;
    locked(&mut c, token, &session, guild, false, |c| {
        let state = guild_state(c, guild, actor, Some(channel))?;
        let permissions = voice_permissions(&state, actor, channel)?;
        require(
            permissions.contains(&Permission::Speak)
                && permissions.contains(&Permission::UseSoundboard),
        )?;
        if builtin::find(sound).is_some() {
            return Ok(());
        }
        let found: Vec<SoundId> = query(
            c,
            "SELECT to_jsonb(id) AS data FROM guild_sounds WHERE guild_id=$1::uuid AND id=$2::uuid",
            &[&guild.to_string(), &sound.to_string()],
        )?;
        if found.is_empty() {
            return Err(Failure::NotFound);
        }
        Ok(())
    })
}
