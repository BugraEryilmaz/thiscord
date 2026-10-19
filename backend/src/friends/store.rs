use crate::{
    access::{self, Scope},
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
};
use diesel::{Connection, PgConnection};
use serde::Deserialize;
use thiscord_shared::{
    AccountId, ChannelId, ClientMessageId, MessageId,
    friends::*,
    ids::{CallId, ConversationId},
    pagination::{PageCursor, PageSize},
    voice::Participant,
};
use uuid::Uuid;

#[derive(Deserialize)]
struct Pair {
    id: ConversationId,
    low_id: AccountId,
    high_id: AccountId,
    state: String,
}
impl Pair {
    fn other(&self, actor: AccountId) -> Result<AccountId, Failure> {
        if actor == self.low_id {
            Ok(self.high_id)
        } else if actor == self.high_id {
            Ok(self.low_id)
        } else {
            Err(Failure::Forbidden)
        }
    }
}
fn pair(
    c: &mut PgConnection,
    id: ConversationId,
    actor: AccountId,
    write: bool,
) -> Result<Pair, Failure> {
    let sql = if write {
        "SELECT to_jsonb(f) AS data FROM friendships f WHERE id=$1::uuid AND state='accepted' AND $2::uuid IN (low_id,high_id) FOR UPDATE"
    } else {
        "SELECT to_jsonb(f) AS data FROM friendships f WHERE id=$1::uuid AND state='accepted' AND $2::uuid IN (low_id,high_id) FOR SHARE"
    };
    query(c, sql, &[&id.to_string(), &actor.to_string()])?
        .pop()
        .ok_or(Failure::Forbidden)
}
fn call(c: &mut PgConnection, id: CallId, actor: AccountId) -> Result<Call, Failure> {
    query(c, "SELECT to_jsonb(v) AS data FROM friend_calls v WHERE id=$1::uuid AND $2::uuid IN (caller,callee) AND state IN ('ringing','active') AND expires_at>now()", &[&id.to_string(), &actor.to_string()])?.pop().ok_or(Failure::Forbidden)
}
fn lock_people(c: &mut PgConnection, a: AccountId, b: AccountId) -> Result<(), Failure> {
    let ids: Vec<AccountId> = query(
        c,
        "SELECT to_jsonb(id) AS data FROM accounts WHERE id IN ($1::uuid,$2::uuid) ORDER BY id FOR NO KEY UPDATE",
        &[&a.to_string(), &b.to_string()],
    )?;
    if a == b || ids.len() != 2 {
        return Err(Failure::Invalid("Choose another existing account"));
    }
    Ok(())
}
pub(super) fn dispatch(pool: &DbPool, token: &str, request: Request) -> Result<Response, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    let actor = session.account_id;
    drop(c);
    let write = !matches!(request, Request::Search { .. } | Request::History { .. });
    auth::rate_limit(pool, &format!("friends:{actor}"), None, write)?;
    let mut c = connection(pool)?;
    let mut guards = vec![];
    let mut call_guards = vec![];
    let mut notification = None;
    let result = c.transaction::<_,Failure,_>(|c| {
        // Pair mutations serialize both people's capacity/call-busy checks, in UUID order.
        let other = match &request {
            Request::Add {account_id} | Request::Accept {account_id} | Request::Remove {account_id} => Some(*account_id),
            Request::StartCall {conversation_id} => {
                let p: Pair = query(c,"SELECT to_jsonb(f) AS data FROM friendships f WHERE id=$1::uuid", &[&conversation_id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                Some(p.other(actor)?)
            }
            Request::AcceptCall {call_id} | Request::EndCall {call_id} => {
                let v = call(c,*call_id,actor)?;
                Some(if v.caller==actor {v.callee} else {v.caller})
            }
            _ => None,
        };
        if let Some(other) = other { lock_people(c,actor,other)?; }
        auth::read_session(c,token,&session)?;
        match request {
            Request::Search {query: text} => {
                let text=text.trim().to_ascii_lowercase();
                if !(2..=32).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b==b'_') {
                    return Err(Failure::Invalid("Search with 2–32 username letters, numbers or underscores"));
                }
                let people=query(c,"SELECT jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id) AS data FROM accounts a LEFT JOIN account_avatars v ON v.account_id=a.id WHERE a.id<>$1::uuid AND starts_with(lower(a.username),$2) ORDER BY a.username,a.id LIMIT 20", &[&actor.to_string(), &text])?;
                Ok(Response::People {people})
            }
            Request::Add {account_id: other} => {
                if query::<bool>(c,"SELECT to_jsonb(email_verified) AS data FROM accounts WHERE id=$1::uuid", &[&actor.to_string()])? != [true] {return Err(Failure::Forbidden);}
                let existing: Option<Pair> = query(c,"SELECT to_jsonb(f) AS data FROM friendships f WHERE low_id=LEAST($1::uuid,$2::uuid) AND high_id=GREATEST($1::uuid,$2::uuid) FOR UPDATE", &[&actor.to_string(),&other.to_string()])?.pop();
                if let Some(p)=&existing && p.state!="removed" { return Ok(Response::Done); }
                let full: Vec<bool>=query(c,"SELECT to_jsonb(count(*) >= 200) AS data FROM friendships WHERE state<>'removed' AND (low_id=$1::uuid OR high_id=$1::uuid) UNION ALL SELECT to_jsonb(count(*) >= 200) AS data FROM friendships WHERE state<>'removed' AND (low_id=$2::uuid OR high_id=$2::uuid)", &[&actor.to_string(),&other.to_string()])?;
                if full.contains(&true) {return Err(Failure::Invalid("Friend and request limit reached"));}
                execute(c,"INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'pending') ON CONFLICT(low_id,high_id) DO UPDATE SET requester=EXCLUDED.requester,state='pending'", &[&Uuid::new_v4().to_string(),&actor.to_string(),&other.to_string()])?;
                Ok(Response::Done)
            }
            Request::Accept {account_id: other} => {
                if execute(c,"UPDATE friendships SET state='accepted' WHERE low_id=LEAST($1::uuid,$2::uuid) AND high_id=GREATEST($1::uuid,$2::uuid) AND requester=$2::uuid AND state='pending'", &[&actor.to_string(),&other.to_string()])?!=1 {return Err(Failure::Conflict);}
                Ok(Response::Done)
            }
            Request::Remove {account_id: other} => {
                let p: Pair=query(c,"SELECT to_jsonb(f) AS data FROM friendships f WHERE low_id=LEAST($1::uuid,$2::uuid) AND high_id=GREATEST($1::uuid,$2::uuid) FOR UPDATE", &[&actor.to_string(),&other.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                execute(c,"UPDATE friendships SET state='removed' WHERE id=$1::uuid", &[&p.id.to_string()])?;
                execute(c,"UPDATE friend_calls SET state='ended' WHERE conversation_id=$1::uuid", &[&p.id.to_string()])?;
                // Drain private snapshots and both directions of media before commit.
                guards.push(access::pause(Scope::Account(actor)));
                guards.push(access::pause(Scope::Account(other)));
                Ok(Response::Done)
            }
            Request::History {conversation_id,before,limit} => {
                pair(c,conversation_id,actor,false)?;
                Ok(Response::History {history:history(c,conversation_id,before,limit)?})
            }
            Request::Send {conversation_id,client_id,content} => {
                let p = pair(c,conversation_id,actor,true)?;
                let duplicate = !query::<bool>(c,"SELECT to_jsonb(TRUE) AS data FROM direct_messages WHERE author_id=$1::uuid AND client_id=$2::uuid", &[&actor.to_string(), &client_id.to_string()])?.is_empty();
                let message = send(c,actor,conversation_id,client_id,content)?;
                if !duplicate {
                    let mut event = crate::notifications::Candidate::new(thiscord_shared::notifications::Target::Conversation { conversation_id }, thiscord_shared::notifications::Kind::DirectMessage, "New direct message".into(), Some(actor));
                    event.id = message.id.to_string(); event.recipients = Some(vec![p.other(actor)?]); notification = Some(event);
                }
                Ok(Response::Message { message })
            }
            Request::StartCall {conversation_id} => {
                let p=pair(c,conversation_id,actor,true)?;
                let other=p.other(actor)?;
                // Every creation/acceptance holds both account locks, including calls across pairs.
                if !query::<bool>(c,"SELECT to_jsonb(TRUE) AS data FROM friend_calls WHERE state IN ('ringing','active') AND expires_at>now() AND (caller IN ($1::uuid,$2::uuid) OR callee IN ($1::uuid,$2::uuid)) LIMIT 1", &[&actor.to_string(),&other.to_string()])?.is_empty() {return Err(Failure::Conflict);}
                let id=CallId::from_uuid(Uuid::new_v4());
                execute(c,"INSERT INTO friend_calls(id,conversation_id,caller,callee,state) VALUES($1::uuid,$2::uuid,$3::uuid,$4::uuid,'ringing')", &[&id.to_string(),&conversation_id.to_string(),&actor.to_string(),&other.to_string()])?;
                call_guards.push(crate::chat::access::notification_calls(actor).pause());
                call_guards.push(crate::chat::access::notification_calls(other).pause());
                Ok(Response::Call {call:call(c,id,actor)?})
            }
            Request::AcceptCall {call_id} => {
                let v=call(c,call_id,actor)?;
                pair(c,v.conversation_id,actor,true)?;
                if v.callee!=actor {return Err(Failure::Forbidden);}
                execute(c,"UPDATE friend_calls SET state='active',expires_at=now()+interval '60 seconds',caller_until=now()+interval '60 seconds',callee_until=now()+interval '60 seconds' WHERE id=$1::uuid AND state='ringing'", &[&call_id.to_string()])?;
                call_guards.push(crate::chat::access::notification_calls(v.caller).pause());
                call_guards.push(crate::chat::access::notification_calls(v.callee).pause());
                Ok(Response::Call {call:call(c,call_id,actor)?})
            }
            Request::EndCall {call_id} => {
                let v=call(c,call_id,actor)?;
                execute(c,"UPDATE friend_calls SET state='ended' WHERE id=$1::uuid", &[&call_id.to_string()])?;
                guards.push(access::pause(Scope::Guild(v.voice_scope())));
                call_guards.push(crate::chat::access::notification_calls(v.caller).pause());
                call_guards.push(crate::chat::access::notification_calls(v.callee).pause());
                let mut event = crate::notifications::Candidate::new(thiscord_shared::notifications::Target::Conversation { conversation_id: v.conversation_id }, thiscord_shared::notifications::Kind::VoiceLeave, if v.state == CallState::Active { "Voice call ended" } else { "Call invitation ended" }.into(), Some(actor));
                event.id = format!("call-ended:{call_id}");
                event.recipients = Some(vec![if v.caller == actor { v.callee } else { v.caller }]);
                notification = Some(event);
                Ok(Response::Done)
            }
        }
    });
    let result = access::complete(result, &mut guards);
    for guard in call_guards {
        guard.finish(result.is_ok());
    }
    if result.is_ok()
        && let Some(event) = notification
    {
        crate::notifications::publish(event);
    }
    result
}
fn send(
    c: &mut PgConnection,
    actor: AccountId,
    conversation: ConversationId,
    client: ClientMessageId,
    content: String,
) -> Result<DirectMessage, Failure> {
    if content.trim().is_empty()
        || content.chars().count() > 4000
        || content
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Failure::Invalid(
            "Message must contain 1–4000 characters without control codes",
        ));
    }
    if let Some(old)=query::<DirectMessage>(c,"SELECT to_jsonb(m) AS data FROM direct_messages m WHERE author_id=$1::uuid AND client_id=$2::uuid", &[&actor.to_string(),&client.to_string()])?.pop() {
        return if old.conversation_id==conversation && old.content==content {Ok(old)} else {Err(Failure::Conflict)};
    }
    let at: thiscord_shared::Timestamp=query(c,"UPDATE friendships SET last_message_at=GREATEST(clock_timestamp(),last_message_at+interval '1 microsecond') WHERE id=$1::uuid RETURNING to_jsonb(last_message_at) AS data", &[&conversation.to_string()])?.pop().ok_or(Failure::Forbidden)?;
    let message = DirectMessage {
        id: MessageId::from_uuid(Uuid::new_v4()),
        conversation_id: conversation,
        client_id: client,
        author_id: actor,
        content,
        created_at: at,
    };
    execute(
        c,
        "INSERT INTO direct_messages(id,conversation_id,author_id,client_id,content,created_at) VALUES($1::uuid,$2::uuid,$3::uuid,$4::uuid,$5,$6::timestamptz)",
        &[
            &message.id.to_string(),
            &conversation.to_string(),
            &actor.to_string(),
            &client.to_string(),
            &message.content,
            &at.to_rfc3339(),
        ],
    )?;
    Ok(message)
}
fn history(
    c: &mut PgConnection,
    conversation: ConversationId,
    before: Option<PageCursor>,
    limit: PageSize,
) -> Result<History, Failure> {
    let (at, id) = before
        .map(|p| p.position())
        .map(|(t, id)| (t.to_rfc3339(), id.to_string()))
        .unwrap_or_else(|| ("infinity".into(), Uuid::nil().to_string()));
    let mut messages: Vec<DirectMessage> = query(
        c,
        "SELECT to_jsonb(m) AS data FROM direct_messages m WHERE conversation_id=$1::uuid AND (created_at,id)<($2::timestamptz,$3::uuid) ORDER BY created_at DESC,id DESC LIMIT $4::bigint",
        &[
            &conversation.to_string(),
            &at,
            &id,
            &(limit.get() + 1).to_string(),
        ],
    )?;
    let more = messages.len() > limit.get() as usize;
    messages.truncate(limit.get() as usize);
    messages.reverse();
    let older = if more {
        messages
            .first()
            .map(|m| PageCursor::new(m.created_at, m.id.as_uuid()))
    } else {
        None
    };
    Ok(History { messages, older })
}
pub(super) fn snapshot(
    pool: &DbPool,
    token: &str,
    selected: Option<ConversationId>,
) -> Result<Snapshot, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    c.transaction(|c| {
        auth::read_session(c,token,&session)?;
        let actor=session.account_id;
        // Lock relationships once, then load public profiles in one batch.
        execute(c,"SELECT id FROM friendships WHERE $1::uuid IN (low_id,high_id) AND state<>'removed' ORDER BY id FOR SHARE", &[&actor.to_string()])?;
        let mut friends:Vec<Friend>=query(c,"SELECT jsonb_build_object('conversation_id',f.id,'person',jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id),'relationship',CASE WHEN f.state='accepted' THEN 'friend' WHEN f.requester=$1::uuid THEN 'outgoing' ELSE 'incoming' END,'online',false) AS data FROM friendships f JOIN accounts a ON a.id=CASE WHEN f.low_id=$1::uuid THEN f.high_id ELSE f.low_id END LEFT JOIN account_avatars v ON v.account_id=a.id WHERE $1::uuid IN (f.low_id,f.high_id) AND f.state<>'removed' ORDER BY a.username,a.id", &[&actor.to_string()])?;
        for friend in &mut friends { friend.online=friend.relationship==Relationship::Friend && super::online(friend.person.id); }
        let selected=selected.filter(|id|friends.iter().any(|f|f.conversation_id==*id && f.relationship==Relationship::Friend));
        let history=selected.map(|id|history(c,id,None,Default::default())).transpose()?;
        let calls=query(c,"SELECT to_jsonb(v) AS data FROM friend_calls v JOIN friendships f ON f.id=v.conversation_id WHERE $1::uuid IN (v.caller,v.callee) AND v.state IN ('ringing','active') AND v.expires_at>now() AND f.state='accepted' ORDER BY v.id", &[&actor.to_string()])?;
        Ok(Snapshot {friends,calls,conversation_id:selected,history})
    })
}

pub(crate) fn voice(
    c: &mut PgConnection,
    actor: AccountId,
    channel: ChannelId,
) -> Result<(Participant, i64), Failure> {
    let id = CallId::from_uuid(channel.as_uuid());
    let v = call(c, id, actor)?;
    pair(c, v.conversation_id, actor, false)?;
    if v.state != CallState::Active {
        return Err(Failure::Forbidden);
    }
    // Signaling's periodic authoritative checks renew the room lease. A backend
    // restart or abandoned call cannot leave participants permanently busy.
    if execute(
        c,
        "UPDATE friend_calls SET caller_until=CASE WHEN caller=$2::uuid THEN now()+interval '60 seconds' ELSE caller_until END,callee_until=CASE WHEN callee=$2::uuid THEN now()+interval '60 seconds' ELSE callee_until END,expires_at=LEAST(CASE WHEN caller=$2::uuid THEN now()+interval '60 seconds' ELSE caller_until END,CASE WHEN callee=$2::uuid THEN now()+interval '60 seconds' ELSE callee_until END) WHERE id=$1::uuid AND state='active' AND expires_at>now()",
        &[&id.to_string(), &actor.to_string()],
    )? != 1
    {
        return Err(Failure::Forbidden);
    }
    let p:Person=query(c,"SELECT jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id) AS data FROM accounts a LEFT JOIN account_avatars v ON v.account_id=a.id WHERE a.id=$1::uuid", &[&actor.to_string()])?.pop().ok_or(Failure::Forbidden)?;
    Ok((
        Participant {
            account_id: actor,
            username: p.username,
            display_name: p.display_name,
            avatar_id: p.avatar_id,
            slot: 0,
            muted: false,
            deafened: false,
            can_speak: true,
            sharing_screen: false,
            sharing_audio: false,
            screen_epoch: 0,
            camera_active: false,
            camera_epoch: 0,
            screen_viewers: vec![],
            camera_viewers: vec![],
        },
        0,
    ))
}
