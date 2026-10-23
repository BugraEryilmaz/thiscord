use crate::{
    access::{self, Scope},
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
};
use diesel::{Connection, PgConnection};
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    ids::ReportId,
    pagination::{Page, PageCursor},
    permissions::{InstanceRole, Permission},
    safety::*,
};

pub(crate) fn blocked(c: &mut PgConnection, a: AccountId, b: AccountId) -> Result<bool, Failure> {
    Ok(!query::<bool>(c,"SELECT to_jsonb(TRUE) AS data FROM account_blocks WHERE (blocker_id=$1::uuid AND blocked_id=$2::uuid) OR (blocker_id=$2::uuid AND blocked_id=$1::uuid) LIMIT 1", &[&a.to_string(),&b.to_string()])?.is_empty())
}
fn text(value: &str) -> Result<(), Failure> {
    if !valid_report_text(value) {
        return Err(Failure::Invalid(REPORT_TEXT_ERROR));
    }
    Ok(())
}
fn json(value: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|_| Failure::Unavailable)
}
fn report(c: &mut PgConnection, id: ReportId) -> Result<Report, Failure> {
    query(
        c,
        "SELECT to_jsonb(r) AS data FROM safety_reports r WHERE id=$1::uuid",
        &[&id.to_string()],
    )?
    .pop()
    .ok_or(Failure::Forbidden)
}
fn guild(
    c: &mut PgConnection,
    actor: AccountId,
    id: GuildId,
    channel: ChannelId,
    review: bool,
    revision: Option<i64>,
) -> Result<(), Failure> {
    execute(
        c,
        if revision.is_some() {
            "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE"
        } else {
            "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE"
        },
        &[&id.to_string()],
    )?;
    let state = crate::permissions::authorization::load(c, id, &[actor], Some(channel))?;
    let grants = crate::permissions::evaluator::effective(&state, actor, Some(channel));
    if !state.members.iter().any(|m| m.account_id == actor)
        || !state.channels.iter().any(|ch| {
            ch.id == channel && ch.kind == thiscord_shared::permissions::ChannelKind::Text
        })
        || !grants.contains(&Permission::ViewChannel)
        || !grants.contains(&Permission::ReadHistory)
        || (review && !grants.contains(&Permission::ManageMessages))
    {
        return Err(Failure::Forbidden);
    }
    if revision.is_some_and(|r| r != state.guild.revision) {
        return Err(Failure::Conflict);
    }
    Ok(())
}
fn instance(c: &mut PgConnection, actor: AccountId) -> Result<(), Failure> {
    execute(c, "SELECT singleton FROM instance FOR SHARE", &[])?;
    if crate::permissions::store::instance(c, actor)?.role == InstanceRole::User {
        return Err(Failure::Forbidden);
    }
    Ok(())
}
pub(super) fn dispatch(pool: &DbPool, token: &str, request: Request) -> Result<Response, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    let actor = session.account_id;
    drop(c);
    auth::rate_limit(
        pool,
        &format!("safety:{actor}"),
        None,
        !matches!(request, Request::Blocks | Request::Reports { .. }),
    )?;
    let mut c = connection(pool)?;
    let mut guards = vec![];
    let result=c.transaction::<_,Failure,_>(|c| {
        match &request {
            Request::Block {account_id}|Request::Unblock {account_id} => crate::friends::store::lock_people(c,actor,*account_id)?,
            Request::CreateReport {..} => {execute(c,"SELECT id FROM accounts WHERE id=$1::uuid FOR NO KEY UPDATE", &[&actor.to_string()])?;},
            _=>{}
        }
        auth::read_session(c,token,&session)?;
        match request {
            Request::Blocks => {
                let people=query(c,"SELECT jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id) AS data FROM account_blocks b JOIN accounts a ON a.id=b.blocked_id LEFT JOIN account_avatars v ON v.account_id=a.id WHERE b.blocker_id=$1::uuid ORDER BY a.username,a.id LIMIT 1000", &[&actor.to_string()])?;
                Ok(Response::Blocks {people})
            }
            Request::Block {account_id:other} => {
                let exists=!query::<bool>(c,"SELECT to_jsonb(TRUE) AS data FROM account_blocks WHERE blocker_id=$1::uuid AND blocked_id=$2::uuid", &[&actor.to_string(),&other.to_string()])?.is_empty();
                if !exists && query::<bool>(c,"SELECT to_jsonb(count(*)>=1000) AS data FROM account_blocks WHERE blocker_id=$1::uuid", &[&actor.to_string()])?==[true] {return Err(Failure::Limited);}
                // Canonical account locks also serialize add/accept/call creation.
                execute(c,"UPDATE friendships SET state='removed' WHERE low_id=LEAST($1::uuid,$2::uuid) AND high_id=GREATEST($1::uuid,$2::uuid)", &[&actor.to_string(),&other.to_string()])?;
                execute(c,"UPDATE friend_calls SET state='ended' WHERE (caller=$1::uuid AND callee=$2::uuid) OR (caller=$2::uuid AND callee=$1::uuid)", &[&actor.to_string(),&other.to_string()])?;
                execute(c,"INSERT INTO account_blocks(blocker_id,blocked_id) VALUES($1::uuid,$2::uuid) ON CONFLICT DO NOTHING", &[&actor.to_string(),&other.to_string()])?;
                execute(c,"DELETE FROM direct_attachments a USING friendships f WHERE a.conversation_id=f.id AND a.message_id IS NULL AND f.low_id=LEAST($1::uuid,$2::uuid) AND f.high_id=GREATEST($1::uuid,$2::uuid)", &[&actor.to_string(),&other.to_string()])?;
                guards.push(access::pause(Scope::Account(actor)));guards.push(access::pause(Scope::Account(other)));
                Ok(Response::Done)
            }
            Request::Unblock {account_id:other} => {
                execute(c,"DELETE FROM account_blocks WHERE blocker_id=$1::uuid AND blocked_id=$2::uuid", &[&actor.to_string(),&other.to_string()])?;
                Ok(Response::Done)
            }
            Request::CreateReport {id,target,reason} => {
                text(&reason)?;
                if let Some(old)=query::<Report>(c,"SELECT to_jsonb(r) AS data FROM safety_reports r WHERE id=$1::uuid", &[&id.to_string()])?.pop() {
                    if old.reporter_id==Some(actor) && old.target==target && old.reason==reason {return Ok(Response::Report {report:Box::new(old)});}
                    return Err(Failure::Conflict);
                }
                if query::<bool>(c,"SELECT to_jsonb(count(*)>=5) AS data FROM safety_reports WHERE reporter_id=$1::uuid AND created_at>now()-interval '24 hours'", &[&actor.to_string()])?==[true] {return Err(Failure::Limited);}
                let (subject,evidence,guild_id,channel_id)=match target.clone() {
                    ReportTarget::User {account_id} => {
                        if account_id==actor || query::<bool>(c,"SELECT to_jsonb(TRUE) AS data FROM accounts WHERE id=$1::uuid", &[&account_id.to_string()])?!=[true]{return Err(Failure::Forbidden);}
                        (Some(account_id),None,None,None)
                    }
                    ReportTarget::DirectMessage {conversation_id,message_id} => {
                        crate::friends::store::pair_access(c,conversation_id,actor,false)?;
                        let m=crate::friends::store::message(c,conversation_id,message_id)?;
                        if m.deleted || m.author_id==actor {return Err(Failure::Forbidden);}
                        (Some(m.author_id),Some(Evidence {message_id:m.id,author_id:Some(m.author_id),content:m.content,revision:m.revision,created_at:m.created_at,attachments:m.attachments}),None,None)
                    }
                    ReportTarget::GuildMessage {guild_id,channel_id,message_id} => {
                        guild(c,actor,guild_id,channel_id,false,None)?;
                        execute(c,"SELECT id FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid FOR SHARE", &[&guild_id.to_string(),&channel_id.to_string()])?;
                        let e:Evidence=query(c,"SELECT jsonb_build_object('message_id',id,'author_id',author_id,'content',content,'revision',revision,'created_at',created_at,'attachments',attachments) AS data FROM messages WHERE id=$1::uuid AND guild_id=$2::uuid AND channel_id=$3::uuid AND NOT deleted", &[&message_id.to_string(),&guild_id.to_string(),&channel_id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                        if e.author_id==Some(actor){return Err(Failure::Forbidden);}
                        (e.author_id,Some(e),Some(guild_id),Some(channel_id))
                    }
                };
                execute(c,"INSERT INTO safety_reports(id,reporter_id,subject_id,guild_id,channel_id,target,reason,evidence) VALUES($1::uuid,$2::uuid,NULLIF($3,'')::uuid,NULLIF($4,'')::uuid,NULLIF($5,'')::uuid,$6::jsonb,$7,NULLIF($8,'null')::jsonb)", &[&id.to_string(),&actor.to_string(),&subject.map(|a|a.to_string()).unwrap_or_default(),&guild_id.map(|g|g.to_string()).unwrap_or_default(),&channel_id.map(|ch|ch.to_string()).unwrap_or_default(),&json(&target)?,&reason,&json(&evidence)?])?;
                Ok(Response::Report {report:Box::new(report(c,id)?)})
            }
            Request::Reports {inbox,page} => {
                let (scope,channel)=match inbox {
                    Inbox::Mine=>("mine",None),Inbox::Instance=>{instance(c,actor)?;("instance",None)},
                    Inbox::Guild {guild_id,channel_id}=>{guild(c,actor,guild_id,channel_id,true,None)?;("guild",Some((guild_id,channel_id)))},
                };
                let (at,id)=page.after.map(|p|p.position()).map(|(at,id)|(at.to_rfc3339(),id.to_string())).unwrap_or_else(||("-infinity".into(),uuid::Uuid::nil().to_string()));
                let mut reports:Vec<Report>=query(c,"SELECT to_jsonb(r) AS data FROM safety_reports r WHERE (($1='mine' AND reporter_id=$2::uuid) OR ($1='instance' AND guild_id IS NULL) OR ($1='guild' AND guild_id=NULLIF($3,'')::uuid AND channel_id=NULLIF($4,'')::uuid)) AND (created_at,id)>($5::timestamptz,$6::uuid) ORDER BY created_at,id LIMIT $7::bigint", &[scope,&actor.to_string(),&channel.map(|(g,_)|g.to_string()).unwrap_or_default(),&channel.map(|(_,ch)|ch.to_string()).unwrap_or_default(),&at,&id,&(page.limit.get()+1).to_string()])?;
                let more=reports.len()>page.limit.get() as usize;reports.truncate(page.limit.get() as usize);
                let next_cursor=if more {reports.last().map(|r|PageCursor::new(r.created_at,r.id.as_uuid()))}else{None};
                Ok(Response::Reports {page:Page {items:reports,next_cursor}})
            }
            Request::Resolve {id,revision,state,resolution,guild_revision} => {
                if state==ReportState::Open{return Err(Failure::Invalid("Choose resolved or dismissed"));}text(&resolution)?;
                // Discover scope only; no evidence is returned until fresh authority succeeds.
                let scope:Option<(GuildId,ChannelId)>=query(c,"SELECT CASE WHEN guild_id IS NULL THEN 'null'::jsonb ELSE jsonb_build_array(guild_id,channel_id) END AS data FROM safety_reports WHERE id=$1::uuid", &[&id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                if let Some((g,ch))=scope {guild(c,actor,g,ch,true,Some(guild_revision.ok_or(Failure::Invalid("Guild revision is required"))?))?;}else{instance(c,actor)?;}
                execute(c,"SELECT id FROM safety_reports WHERE id=$1::uuid FOR UPDATE", &[&id.to_string()])?;
                let old=report(c,id)?;
                if old.state==state && old.resolution==resolution && old.revision==revision.saturating_add(1){return Ok(Response::Report {report:Box::new(old)});}
                if old.state!=ReportState::Open || old.revision!=revision{return Err(Failure::Conflict);}
                execute(c,"UPDATE safety_reports SET state=$2::jsonb#>>'{}',resolution=$3,revision=revision+1,resolved_at=clock_timestamp(),reviewer_id=$4::uuid WHERE id=$1::uuid", &[&id.to_string(),&json(&state)?,&resolution,&actor.to_string()])?;
                if let Some((g,_))=scope {execute(c,"UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid", &[&g.to_string()])?;}
                Ok(Response::Report {report:Box::new(report(c,id)?)})
            }
        }
    });
    access::complete(result, &mut guards)
}
