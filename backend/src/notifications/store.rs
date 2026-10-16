use super::Candidate;
use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{authorization::load, evaluator::effective},
};
use diesel::{Connection, PgConnection};
use thiscord_shared::{
    AccountId,
    notifications::*,
    permissions::{ChannelKind, InstanceRole, Permission},
};

pub(super) fn preferences(c: &mut PgConnection, actor: AccountId) -> Result<Preferences, Failure> {
    Ok(query(c, "SELECT settings || jsonb_build_object('revision',revision) AS data FROM notification_preferences WHERE account_id=$1::uuid", &[&actor.to_string()])?.pop().unwrap_or_default())
}
pub(super) fn authorize(
    c: &mut PgConnection,
    actor: AccountId,
    target: Target,
    kind: Option<Kind>,
) -> Result<(), Failure> {
    match target {
        Target::Channel {
            guild_id,
            channel_id,
        } => {
            execute(
                c,
                "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE",
                &[&guild_id.to_string()],
            )?;
            let state = load(c, guild_id, &[actor], Some(channel_id))?;
            let grants = effective(&state, actor, Some(channel_id));
            let channel = state
                .channels
                .iter()
                .find(|ch| ch.id == channel_id)
                .ok_or(Failure::Forbidden)?;
            if !state.members.iter().any(|m| m.account_id == actor)
                || !grants.contains(&Permission::ViewChannel)
                || (channel.kind == ChannelKind::Text && !grants.contains(&Permission::ReadHistory))
                || (kind == Some(Kind::GuildMessage) && channel.kind != ChannelKind::Text)
                || (matches!(kind, Some(Kind::VoiceJoin | Kind::VoiceLeave))
                    && (channel.kind != ChannelKind::Voice
                        || !grants.contains(&Permission::JoinVoice)))
            {
                return Err(Failure::Forbidden);
            }
        }
        Target::Conversation { conversation_id } => {
            if query::<bool>(
                c,
                "SELECT to_jsonb(TRUE) AS data FROM friendships WHERE id=$1::uuid AND $2::uuid IN (low_id,high_id) AND state='accepted' FOR SHARE",
                &[&conversation_id.to_string(), &actor.to_string()],
            )? != [true]
            {
                return Err(Failure::Forbidden);
            }
        }
        Target::Updates => {}
    }
    Ok(())
}
fn valid(delivery: Delivery) -> Result<(), Failure> {
    if delivery.volume > 100 {
        Err(Failure::Invalid(
            "Notification volume must be between 0 and 100",
        ))
    } else {
        Ok(())
    }
}
pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    command: Request,
) -> Result<Preferences, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    drop(c);
    auth::rate_limit(
        pool,
        &format!("notifications:{}", session.account_id),
        None,
        !matches!(command, Request::Get),
    )?;
    let mut c = connection(pool)?;
    let mut guard = None;
    let mut release = None;
    let result = c.transaction(|c| {
        if !matches!(command, Request::Get) {
            execute(c, "SELECT id FROM accounts WHERE id=$1::uuid FOR NO KEY UPDATE", &[&session.account_id.to_string()])?;
        }
        auth::read_session(c, token, &session)?;
        let mut prefs = preferences(c, session.account_id)?;
        match command {
            Request::Get => return Ok(prefs),
            Request::PublishUpdate { version } => {
                execute(c, "SELECT singleton FROM instance FOR SHARE", &[])?;
                if crate::permissions::store::instance(c, session.account_id)?.role == InstanceRole::User { return Err(Failure::Forbidden); }
                // The announcement carries only a release version, never an executable URL.
                if !valid_version(&version) { return Err(Failure::Invalid("Use a numeric major.minor.patch release version")); }
                let changed = execute(c, "INSERT INTO notification_release(singleton,version) VALUES(TRUE,$1) ON CONFLICT(singleton) DO UPDATE SET version=EXCLUDED.version WHERE notification_release.version<>EXCLUDED.version", &[&version])?;
                if changed > 0 { release = Some(version); }
                return Ok(prefs);
            }
            Request::SetDefault { revision, kind, delivery } => {
                valid(delivery)?;
                if revision != prefs.revision { return Err(Failure::Conflict); }
                prefs.defaults.retain(|r| r.kind != kind);
                prefs.defaults.push(Rule { kind, delivery });
            }
            Request::SetSubscription { revision, target, delivery } => {
                if let Some(delivery) = delivery { valid(delivery)?; }
                if revision != prefs.revision { return Err(Failure::Conflict); }
                authorize(c, session.account_id, target, None)?;
                subscription(&mut prefs, target)?.delivery = delivery;
            }
            Request::Snooze { target, duration } => {
                authorize(c, session.account_id, target, None)?;
                subscription(&mut prefs, target)?.mute = duration.mute(chrono::Utc::now());
            }
            Request::RemoveSubscription { revision, target } => {
                if revision != prefs.revision { return Err(Failure::Conflict); }
                // Deleting this account's preferences never grants source access.
                prefs.subscriptions.retain(|s| s.target != target);
            }
        }
        prefs.subscriptions.retain(|s| s.delivery.is_some() || s.mute.active(chrono::Utc::now()));
        prefs.revision += 1;
        execute(c, "INSERT INTO notification_preferences(account_id,revision,settings) VALUES($1::uuid,$2::bigint,$3::jsonb) ON CONFLICT(account_id) DO UPDATE SET revision=EXCLUDED.revision,settings=EXCLUDED.settings", &[&session.account_id.to_string(), &prefs.revision.to_string(), &serde_json::to_string(&prefs).map_err(|_| Failure::Unavailable)?])?;
        guard = Some(crate::chat::access::notification_preferences(session.account_id).pause());
        Ok(prefs)
    });
    if let Some(guard) = guard {
        guard.finish(result.is_ok());
    }
    if result.is_ok()
        && let Some(version) = release
    {
        super::publish(Candidate::release(version));
    }
    result
}
fn subscription(prefs: &mut Preferences, target: Target) -> Result<&mut Subscription, Failure> {
    if let Some(index) = prefs.subscriptions.iter().position(|s| s.target == target) {
        return Ok(&mut prefs.subscriptions[index]);
    }
    prefs
        .subscriptions
        .retain(|s| s.delivery.is_some() || s.mute.active(chrono::Utc::now()));
    if prefs.subscriptions.len() >= MAX_OVERRIDES {
        return Err(Failure::Invalid("Notification subscription limit reached"));
    }
    prefs.subscriptions.push(Subscription {
        target,
        delivery: None,
        mute: Mute::None,
    });
    Ok(prefs.subscriptions.last_mut().unwrap())
}
fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    version.len() <= 64
        && parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u32>().is_ok()
                && (p.len() == 1 || !p.starts_with('0'))
        })
}
pub(super) struct Initial {
    pub preferences: Preferences,
    pub release: Option<String>,
    pub calls: Vec<Invitation>,
}
#[derive(serde::Deserialize)]
pub(super) struct Invitation {
    pub id: thiscord_shared::ids::CallId,
    pub conversation_id: thiscord_shared::ids::ConversationId,
    pub caller: AccountId,
    pub expires_at: thiscord_shared::Timestamp,
}
impl Invitation {
    pub fn candidate(&self, callee: AccountId) -> Candidate {
        let mut event = Candidate::new(
            Target::Conversation {
                conversation_id: self.conversation_id,
            },
            Kind::IncomingCall,
            "Incoming voice call".into(),
            Some(self.caller),
        );
        event.id = self.id.to_string();
        event.call_id = Some(self.id);
        event.recipients = Some(vec![callee]);
        event
    }
}
pub(super) fn initial(pool: &DbPool, token: &str) -> Result<Initial, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    c.transaction(|c| {
        auth::read_session(c, token, &session)?;
        Ok(Initial {
            preferences: preferences(c, session.account_id)?,
            release: query(
                c,
                "SELECT to_jsonb(version) AS data FROM notification_release",
                &[],
            )?
            .pop(),
            calls: query(c, "SELECT to_jsonb(v) AS data FROM friend_calls v JOIN friendships f ON f.id=v.conversation_id WHERE v.callee=$1::uuid AND v.state='ringing' AND v.expires_at>now() AND f.state='accepted' ORDER BY v.expires_at LIMIT 1", &[&session.account_id.to_string()])?,
        })
    })
}
pub(super) fn alert(
    pool: &DbPool,
    token: &str,
    mut candidate: Candidate,
) -> Result<Option<Alert>, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    c.transaction(|c| {
        auth::read_session(c, token, &session)?;
        let actor = session.account_id;
        if candidate.actor == Some(actor) || candidate.recipients.as_ref().is_some_and(|ids| !ids.contains(&actor)) { return Ok(None); }
        if let Target::Channel { guild_id, channel_id } = candidate.target && guild_id.as_uuid() == channel_id.as_uuid() {
            let conversation = query(c, "SELECT to_jsonb(conversation_id) AS data FROM friend_calls WHERE id=$1::uuid AND $2::uuid IN (caller,callee) AND state='active' AND expires_at>now()", &[&channel_id.to_string(), &actor.to_string()])?.pop();
            let Some(conversation_id) = conversation else { return Ok(None); };
            candidate.target = Target::Conversation { conversation_id };
        }
        if authorize(c, actor, candidate.target, Some(candidate.kind)).is_err() { return Ok(None); }
        // A call accepted, declined or expired while queued must not ring.
        if let Some(id) = candidate.call_id
            && query::<bool>(c, "SELECT to_jsonb(TRUE) AS data FROM friend_calls WHERE id=$1::uuid AND callee=$2::uuid AND state='ringing' AND expires_at>now()", &[&id.to_string(), &actor.to_string()])? != [true] { return Ok(None); }
        let prefs = preferences(c, actor)?;
        let Some(delivery) = prefs.delivery(candidate.target, candidate.kind, chrono::Utc::now()) else { return Ok(None); };
        if !delivery.accepts(candidate.mentions.contains(&actor)) { return Ok(None); }
        Ok(Some(Alert { id: candidate.id, target: candidate.target, kind: candidate.kind, title: candidate.title, delivery, actor: candidate.actor, call_id: candidate.call_id, version: candidate.version }))
    })
}
