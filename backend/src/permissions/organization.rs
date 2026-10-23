//! Guild-locked organization, invite capabilities and bounded moderation history.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub(super) fn hash(code: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(code.as_bytes()))
}
pub(super) fn invite_guild(c: &mut PgConnection, code: &str) -> Result<GuildId, Failure> {
    if code.len() != 43
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Failure::Invalid("Invalid invite code"));
    }
    query(
        c,
        "SELECT to_jsonb(guild_id) AS data FROM guild_invites WHERE token_hash=$1",
        &[&hash(code)],
    )?
    .pop()
    .ok_or(Failure::Forbidden)
}
pub(super) fn accept(
    c: &mut PgConnection,
    actor: AccountId,
    code: &str,
) -> Result<PermissionResponse, Failure> {
    verified(c, actor)?;
    let guild = invite_guild(c, code)?;
    execute(
        c,
        "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE",
        &[&guild.to_string()],
    )?;
    let state = load(c, guild)?;
    not_banned(c, guild, actor)?;
    // The code is still checked for existing members; replays do not spend a use.
    let invite: thiscord_shared::InviteId = query(c,"SELECT to_jsonb(id) AS data FROM guild_invites WHERE guild_id=$1::uuid AND token_hash=$2 AND NOT revoked AND (expires_at IS NULL OR expires_at>clock_timestamp())", &[&guild.to_string(),&hash(code)])?.pop().ok_or(Failure::Forbidden)?;
    if !state.members.iter().any(|m| m.account_id == actor) {
        require(
            query::<bool>(
                c,
                "SELECT to_jsonb(max_uses=0 OR uses<max_uses) AS data FROM guild_invites WHERE id=$1::uuid",
                &[&invite.to_string()],
            )? == [true],
        )?;
        allocation(
            state.members.len() < capacity(c, state.guild.owner)?.limits.members as usize,
            "Server member limit reached for its owner's instance role",
        )?;
        execute(
            c,
            "INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)",
            &[&guild.to_string(), &actor.to_string()],
        )?;
        execute(
            c,
            "UPDATE guild_invites SET uses=LEAST(uses::bigint+1,2147483647)::integer WHERE id=$1::uuid",
            &[&invite.to_string()],
        )?;
        execute(
            c,
            "UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid",
            &[&guild.to_string()],
        )?;
        // Existing timeout/moderator voice restrictions are deliberately retained.
    }
    Ok(PermissionResponse::Joined {
        guild: load(c, guild)?.guild,
    })
}
fn invites(c: &mut PgConnection, guild: GuildId) -> Result<Vec<GuildInvite>, Failure> {
    query(
        c,
        "SELECT jsonb_build_object('id',id,'created_by',created_by,'created_at',created_at,'expires_at',expires_at,'max_uses',max_uses,'uses',uses,'revoked',revoked) AS data FROM guild_invites WHERE guild_id=$1::uuid ORDER BY created_at,id",
        &[&guild.to_string()],
    )
}
pub(super) fn request(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
    command: PermissionRequest,
) -> Result<PermissionResponse, Failure> {
    let guild = state.guild.id.to_string();
    let permissions = effective(state, actor, None);
    match command {
        PermissionRequest::InspectOrganization { .. } => {
            require(permissions.contains(&Permission::ManageChannels))?;
            Ok(PermissionResponse::Organization {
                guild: state.guild.clone(),
                channels: state.channels.clone(),
                categories: state.categories.clone(),
            })
        }
        PermissionRequest::AuditLog { page, .. } => {
            require(can_moderate(state, actor) || permissions.contains(&Permission::ManageGuild))?;
            let (time, id) = page
                .after
                .as_ref()
                .map(|p| {
                    let (t, i) = p.position();
                    (t.to_rfc3339(), i.to_string())
                })
                .unwrap_or_default();
            let mut items: Vec<AuditEntry> = query(
                c,
                "SELECT to_jsonb(a) AS data FROM guild_audit_log a WHERE guild_id=$1::uuid AND ($2='' OR (created_at,id)>(NULLIF($2,'')::timestamptz,NULLIF($3,'')::uuid)) ORDER BY created_at,id LIMIT $4::integer",
                &[
                    &guild,
                    &time,
                    &id,
                    &(u32::from(page.limit.get()) + 1).to_string(),
                ],
            )?;
            let next_cursor = if items.len() > usize::from(page.limit.get()) {
                items.truncate(usize::from(page.limit.get()));
                items.last().map(|a| {
                    thiscord_shared::pagination::PageCursor::new(a.created_at, a.id.as_uuid())
                })
            } else {
                None
            };
            Ok(PermissionResponse::AuditLog {
                page: thiscord_shared::pagination::Page { items, next_cursor },
            })
        }
        PermissionRequest::ListInvites { .. } => {
            require(permissions.contains(&Permission::ManageInvites))?;
            Ok(PermissionResponse::Invites {
                guild: state.guild.clone(),
                invites: invites(c, state.guild.id)?,
            })
        }
        PermissionRequest::CreateInvite {
            revision,
            duration_seconds,
            max_uses,
            ..
        } => {
            require(permissions.contains(&Permission::ManageInvites))?;
            if revision != state.guild.revision {
                return Err(Failure::Conflict);
            }
            if (duration_seconds != 0 && !(60..=2_419_200).contains(&duration_seconds))
                || max_uses > 500
            {
                return Err(Failure::Invalid(
                    "Invites require 1 minute to 28 days and 1 to 500 uses, or zero for unlimited",
                ));
            }
            execute(
                c,
                "DELETE FROM guild_invites WHERE guild_id=$1::uuid AND (revoked OR expires_at<=clock_timestamp() OR (max_uses>0 AND uses>=max_uses))",
                &[&guild],
            )?;
            allocation(
                invites(c, state.guild.id)?.len() < 100,
                "Server invite limit reached (100 active invites)",
            )?;
            let mut bytes = [0u8; 32];
            OsRng.fill_bytes(&mut bytes);
            let code = URL_SAFE_NO_PAD.encode(bytes);
            let id = thiscord_shared::InviteId::from_uuid(Uuid::new_v4());
            execute(
                c,
                "INSERT INTO guild_invites(id,guild_id,token_hash,created_by,expires_at,max_uses) VALUES($1::uuid,$2::uuid,$3,$4::uuid,CASE WHEN $5::integer=0 THEN NULL ELSE clock_timestamp()+($5||' seconds')::interval END,$6::integer)",
                &[
                    &id.to_string(),
                    &guild,
                    &hash(&code),
                    &actor.to_string(),
                    &duration_seconds.to_string(),
                    &max_uses.to_string(),
                ],
            )?;
            execute(
                c,
                "UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid",
                &[&guild],
            )?;
            let invite = invites(c, state.guild.id)?
                .into_iter()
                .find(|i| i.id == id)
                .ok_or(Failure::Unavailable)?;
            Ok(PermissionResponse::InviteCreated {
                guild: load(c, state.guild.id)?.guild,
                invite,
                code,
            })
        }
        PermissionRequest::RevokeInvite {
            revision,
            invite_id,
            ..
        } => {
            require(permissions.contains(&Permission::ManageInvites))?;
            if revision != state.guild.revision {
                return Err(Failure::Conflict);
            }
            if execute(
                c,
                "UPDATE guild_invites SET revoked=TRUE WHERE guild_id=$1::uuid AND id=$2::uuid AND NOT revoked",
                &[&guild, &invite_id.to_string()],
            )? != 1
            {
                return Err(Failure::Conflict);
            }
            execute(
                c,
                "UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid",
                &[&guild],
            )?;
            Ok(PermissionResponse::Invites {
                guild: load(c, state.guild.id)?.guild,
                invites: invites(c, state.guild.id)?,
            })
        }
        _ => unreachable!(),
    }
}
pub(super) fn moderation_target(change: &GuildChange) -> Option<AccountId> {
    match change {
        GuildChange::RemoveMember { account_id }
        | GuildChange::BanMember { account_id }
        | GuildChange::UnbanMember { account_id }
        | GuildChange::TimeoutMember { account_id, .. }
        | GuildChange::DisconnectVoice { account_id }
        | GuildChange::SetServerMute { account_id, .. }
        | GuildChange::SetServerDeafen { account_id, .. }
        | GuildChange::MoveVoice { account_id, .. } => Some(*account_id),
        _ => None,
    }
}
pub(super) fn audit(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
    change: &GuildChange,
    reason: &str,
) -> Result<(), Failure> {
    let Some(target) = moderation_target(change) else {
        return Ok(());
    };
    let value = serde_json::to_value(change).map_err(|_| Failure::Unavailable)?;
    let action = value["action"].as_str().ok_or(Failure::Unavailable)?;
    let mut details = value.clone();
    if let Some(object) = details.as_object_mut() {
        object.remove("action");
        object.remove("account_id");
    }
    let guild = state.guild.id.to_string();
    execute(
        c,
        "INSERT INTO guild_audit_log(guild_id,actor,target,action,reason,details) VALUES($1::uuid,$2::uuid,$3::uuid,$4,$5,$6)",
        &[
            &guild,
            &actor.to_string(),
            &target.to_string(),
            action,
            reason.trim(),
            &details.to_string(),
        ],
    )?;
    execute(
        c,
        "DELETE FROM guild_audit_log WHERE guild_id=$1::uuid AND id IN (SELECT id FROM guild_audit_log WHERE guild_id=$1::uuid ORDER BY created_at DESC,id DESC OFFSET 1000)",
        &[&guild],
    )?;
    Ok(())
}

pub(super) fn change(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
    change: GuildChange,
) -> Result<(), Failure> {
    require(effective(state, actor, None).contains(&Permission::ManageChannels))?;
    let guild = state.guild.id.to_string();
    match change {
        GuildChange::CreateCategory { name: value } => {
            allocation(
                state.categories.len() < 50,
                "Server category limit reached (50)",
            )?;
            execute(
                c,
                "INSERT INTO channel_categories(guild_id,name,position) VALUES($1::uuid,$2,COALESCE((SELECT LEAST(MAX(position)+1,10000) FROM channel_categories WHERE guild_id=$1::uuid),0))",
                &[&guild, name(&value)?],
            )?;
        }
        GuildChange::EditCategory {
            category_id,
            name: value,
            position,
        } => {
            position_valid(position)?;
            require(state.categories.iter().any(|c| c.id == category_id))?;
            execute(
                c,
                "UPDATE channel_categories SET name=$3,position=$4::integer WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[
                    &guild,
                    &category_id.to_string(),
                    name(&value)?,
                    &position.to_string(),
                ],
            )?;
        }
        GuildChange::DeleteCategory { category_id } => {
            require(state.categories.iter().any(|c| c.id == category_id))?;
            execute(
                c,
                "UPDATE channels SET category_id=NULL WHERE guild_id=$1::uuid AND category_id=$2::uuid",
                &[&guild, &category_id.to_string()],
            )?;
            execute(
                c,
                "DELETE FROM channel_categories WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[&guild, &category_id.to_string()],
            )?;
        }
        GuildChange::EditChannel {
            channel_id,
            name: value,
            category_id,
            position,
        } => {
            position_valid(position)?;
            require(state.channels.iter().any(|c| c.id == channel_id))?;
            require(category_id.is_none_or(|id| state.categories.iter().any(|c| c.id == id)))?;
            execute(
                c,
                "UPDATE channels SET name=$3,category_id=NULLIF($4,'')::uuid,position=$5::integer WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[
                    &guild,
                    &channel_id.to_string(),
                    name(&value)?,
                    &category_id.map(|i| i.to_string()).unwrap_or_default(),
                    &position.to_string(),
                ],
            )?;
        }
        _ => unreachable!(),
    }
    Ok(())
}
fn position_valid(position: i32) -> Result<(), Failure> {
    if !(0..=10000).contains(&position) {
        return Err(Failure::Invalid("Order must be between 0 and 10000"));
    }
    Ok(())
}
