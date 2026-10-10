use super::evaluator::{effective, rank};
use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use diesel::{Connection, PgConnection};
use rand::rngs::OsRng;
use thiscord_shared::{AccountId, GuildId, RoleId, permissions::*};
use uuid::Uuid;

fn guild_password(password: Option<&str>) -> Result<Option<&str>, Failure> {
    let password = password.filter(|p| !p.is_empty());
    if password.is_some_and(|p| p.len() > 1024) {
        return Err(Failure::Invalid(
            "Server password must be at most 1024 bytes",
        ));
    }
    Ok(password)
}

fn name(value: &str) -> Result<&str, Failure> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err(Failure::Invalid("Name must contain 1 to 80 characters"));
    }
    Ok(value)
}
fn json(value: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|_| Failure::Unavailable)
}
fn require(value: bool) -> Result<(), Failure> {
    if value {
        Ok(())
    } else {
        Err(Failure::Forbidden)
    }
}
fn verified(c: &mut PgConnection, account: AccountId) -> Result<(), Failure> {
    require(
        query::<bool>(
            c,
            "SELECT to_jsonb(email_verified) AS data FROM accounts WHERE id=$1::uuid",
            &[&account.to_string()],
        )? == [true],
    )
}
pub(crate) fn instance(
    c: &mut PgConnection,
    account: AccountId,
) -> Result<InstanceAccess, Failure> {
    let owner: Option<AccountId> = query(
        c,
        "SELECT COALESCE(to_jsonb(owner_account_id),'null'::jsonb) AS data FROM instance",
        &[],
    )?
    .pop()
    .ok_or(Failure::Unavailable)?;
    let admin = !query::<AccountId>(
        c,
        "SELECT to_jsonb(account_id) AS data FROM instance_admins WHERE account_id=$1::uuid",
        &[&account.to_string()],
    )?
    .is_empty();
    Ok(InstanceAccess {
        owner,
        role: if owner == Some(account) {
            InstanceRole::Owner
        } else if admin {
            InstanceRole::Admin
        } else {
            InstanceRole::User
        },
    })
}
pub(super) fn bootstrap(pool: &DbPool, username: &str) -> Result<(), Failure> {
    connection(pool)?.transaction(|c| {
        execute(c, "SELECT singleton FROM instance FOR UPDATE", &[])?;
        let id: AccountId = query(
            c,
            "SELECT to_jsonb(id) AS data FROM accounts WHERE username=$1 AND email_verified",
            &[username],
        )?
        .pop()
        .ok_or(Failure::Forbidden)?;
        if execute(
            c,
            "UPDATE instance SET owner_account_id=$1::uuid WHERE owner_account_id IS NULL",
            &[&id.to_string()],
        )? != 1
        {
            return Err(Failure::Conflict);
        }
        Ok(())
    })
}
pub(crate) fn load(c: &mut PgConnection, id: GuildId) -> Result<GuildState, Failure> {
    let id = id.to_string();
    let guild = query(
        c,
        "SELECT to_jsonb(g) AS data FROM guilds g WHERE id=$1::uuid",
        &[&id],
    )?
    .pop()
    .ok_or(Failure::Forbidden)?;
    let roles = query(
        c,
        "SELECT to_jsonb(r) AS data FROM guild_roles r WHERE guild_id=$1::uuid ORDER BY position DESC,id",
        &[&id],
    )?;
    let members = query(
        c,
        "SELECT jsonb_build_object('account_id',m.account_id,'username',a.username,'display_name',a.display_name,'timeout_until',(SELECT timeout_until FROM guild_moderation x WHERE x.guild_id=m.guild_id AND x.account_id=m.account_id),'roles',COALESCE((SELECT jsonb_agg(role_id ORDER BY role_id) FROM guild_member_roles r WHERE r.guild_id=m.guild_id AND r.account_id=m.account_id),'[]'::jsonb)) AS data FROM guild_members m JOIN accounts a ON a.id=m.account_id WHERE m.guild_id=$1::uuid ORDER BY m.account_id",
        &[&id],
    )?;
    let channels = query(
        c,
        "SELECT to_jsonb(ch) AS data FROM channels ch WHERE guild_id=$1::uuid ORDER BY id",
        &[&id],
    )?;
    let overrides = query(
        c,
        "SELECT jsonb_build_object('channel_id',channel_id,'target',jsonb_build_object('kind',CASE WHEN role_id IS NULL THEN 'member' ELSE 'role' END,'id',COALESCE(role_id,account_id)),'allow',allow,'deny',deny) AS data FROM channel_overrides WHERE guild_id=$1::uuid ORDER BY id",
        &[&id],
    )?;
    Ok(GuildState {
        guild,
        roles,
        members,
        channels,
        overrides,
    })
}
fn member(state: &GuildState, id: AccountId) -> Result<(), Failure> {
    require(state.members.iter().any(|m| m.account_id == id))
}
fn role(state: &GuildState, id: RoleId) -> Result<&Role, Failure> {
    state
        .roles
        .iter()
        .find(|r| r.id == id)
        .ok_or(Failure::Forbidden)
}
fn grantable(
    state: &GuildState,
    actor: AccountId,
    permissions: &Permissions,
) -> Result<(), Failure> {
    require(permissions.is_subset(&effective(state, actor, None)))
}
fn below(state: &GuildState, actor: AccountId, target: AccountId) -> Result<(), Failure> {
    member(state, target)?;
    require(actor != target && rank(state, actor) > rank(state, target))
}
pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    command: PermissionRequest,
) -> Result<PermissionResponse, Failure> {
    let mut c = connection(pool)?;
    // Authentication runs outside the action transaction so replay revocation commits.
    let session = auth::authenticate(&mut c, token)?;
    drop(c);
    let account_key = session.account_id.to_string();
    auth::rate_limit(
        pool,
        &format!("permission:{}", session.account_id),
        if matches!(command, PermissionRequest::JoinGuild { .. }) {
            Some(&account_key)
        } else {
            None
        },
        matches!(
            command,
            PermissionRequest::JoinGuild { .. } | PermissionRequest::CreateGuild { .. }
        ),
    )?;
    let mut c = connection(pool)?;
    let _voice_change = matches!(
        &command,
        PermissionRequest::Change { .. }
            | PermissionRequest::JoinGuild { .. }
            | PermissionRequest::TransferInstance { .. }
            | PermissionRequest::SetInstanceAdmin { .. }
    )
    .then(|| crate::voice::access::global().pause());
    let scopes = match &command {
        PermissionRequest::Change { guild_id, .. }
        | PermissionRequest::JoinGuild { guild_id, .. } => {
            vec![crate::chat::access::guild(*guild_id)]
        }
        PermissionRequest::SetInstanceAdmin { account_id, .. } => {
            vec![crate::chat::access::instance_role(*account_id)]
        }
        PermissionRequest::TransferInstance { account_id } => vec![
            crate::chat::access::instance_role(session.account_id),
            crate::chat::access::instance_role(*account_id),
        ],
        _ => Vec::new(),
    };
    let mut changes = Vec::new();
    let result = c.transaction(|c| {
        let result = (|| {
        auth::read_session(c,token,&session)?;
        let actor = session.account_id;
        match command {
            PermissionRequest::Instance {} => Ok(PermissionResponse::Instance { access: instance(c,actor)? }),
            PermissionRequest::SetInstanceAdmin { account_id, admin } => {
                auth::recent(&session)?;
                execute(c,"SELECT singleton FROM instance FOR UPDATE", &[])?;
                require(instance(c,actor)?.role == InstanceRole::Owner && account_id!=actor)?;
                verified(c,account_id)?;
                if admin { execute(c,"INSERT INTO instance_admins(account_id) VALUES($1::uuid) ON CONFLICT DO NOTHING", &[&account_id.to_string()])?; }
                else { execute(c,"DELETE FROM instance_admins WHERE account_id=$1::uuid", &[&account_id.to_string()])?; }
                Ok(PermissionResponse::Instance { access: instance(c,actor)? })
            }
            PermissionRequest::TransferInstance { account_id } => {
                auth::recent(&session)?;
                execute(c,"SELECT singleton FROM instance FOR UPDATE", &[])?;
                require(instance(c,actor)?.role == InstanceRole::Owner && account_id!=actor)?;
                verified(c,account_id)?;
                execute(c,"UPDATE instance SET owner_account_id=$1::uuid", &[&account_id.to_string()])?;
                execute(c,"DELETE FROM instance_admins WHERE account_id=$1::uuid OR account_id=$2::uuid", &[&actor.to_string(),&account_id.to_string()])?;
                Ok(PermissionResponse::Instance { access: instance(c,actor)? })
            }
            PermissionRequest::ListGuilds {} => Ok(PermissionResponse::Guilds { guilds: query(c,"SELECT to_jsonb(g) AS data FROM guilds g JOIN guild_members m ON m.guild_id=g.id WHERE m.account_id=$1::uuid ORDER BY g.id", &[&actor.to_string()])? }),
            PermissionRequest::CreateGuild { name: value, password } => {
                verified(c,actor)?;
                let name=name(&value)?;
                // Lock instance to serialize capacity checks and ownership administration.
                execute(c,"SELECT singleton FROM instance FOR UPDATE", &[])?;
                require(instance(c,actor)?.role != InstanceRole::User)?;
                let count: i64 = query(c,"SELECT to_jsonb(count(*)) AS data FROM guilds", &[])?.pop().unwrap_or(0);
                require(count<100)?;
                let hash=guild_password(password.as_deref())?.map(|p|Argon2::default().hash_password(p.as_bytes(),&SaltString::generate(&mut OsRng)).map(|h|h.to_string()).map_err(|_|Failure::Unavailable)).transpose()?;
                let id = GuildId::from_uuid(Uuid::new_v4());
                execute(c,"INSERT INTO guilds(id,name,owner,password_hash) VALUES($1::uuid,$2,$3::uuid,NULLIF($4,''))", &[&id.to_string(),name,&actor.to_string(),hash.as_deref().unwrap_or("")])?;
                execute(c,"INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)", &[&id.to_string(),&actor.to_string()])?;
                let defaults: Permissions = [Permission::ViewChannel,Permission::ReadHistory,Permission::SendMessages,Permission::EditOwnMessages,Permission::DeleteOwnMessages,Permission::JoinVoice,Permission::Speak].into_iter().collect();
                execute(c,"INSERT INTO guild_roles(guild_id,name,position,everyone,permissions) VALUES($1::uuid,'Everyone',0,TRUE,$2::jsonb)", &[&id.to_string(),&json(&defaults)?])?;
                Ok(PermissionResponse::State { state: load(c,id)? })
            }
            PermissionRequest::JoinGuild { guild_id, password } => {
                verified(c,actor)?;
                let password=guild_password(password.as_deref())?.unwrap_or("");
                execute(c,"SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE", &[&guild_id.to_string()])?;
                let state=load(c,guild_id)?;
                not_banned(c,guild_id,actor)?;
                if !state.members.iter().any(|m|m.account_id==actor) {
                    let hash: Option<String>=query(c,"SELECT COALESCE(to_jsonb(password_hash),'null'::jsonb) AS data FROM guilds WHERE id=$1::uuid", &[&guild_id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                    if let Some(hash)=hash {require(PasswordHash::new(&hash).ok().is_some_and(|h|Argon2::default().verify_password(password.as_bytes(),&h).is_ok()))?;}
                    require(state.members.len()<500)?;
                    execute(c,"INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)", &[&guild_id.to_string(),&actor.to_string()])?;
                    execute(c,"UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid", &[&guild_id.to_string()])?;
                }
                let guild=query(c,"SELECT to_jsonb(g) AS data FROM guilds g WHERE id=$1::uuid", &[&guild_id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                Ok(PermissionResponse::Joined {guild})
            }
            command => {
                let guild_id=match &command { PermissionRequest::ViewGuild{guild_id} | PermissionRequest::InspectModeration{guild_id} | PermissionRequest::Inspect{guild_id} | PermissionRequest::Change{guild_id,..} | PermissionRequest::Preview{guild_id,..} => *guild_id, _=>unreachable!() };
                // Writes retain exclusive serialization and revision checks. Readers
                // protect their permission snapshot with compatible shared locks.
                let lock = if matches!(command, PermissionRequest::Change { .. }) {
                    "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE"
                } else {
                    "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE"
                };
                execute(c,lock, &[&guild_id.to_string()])?;
                let state=match &command {
                    PermissionRequest::ViewGuild { .. } => super::authorization::load(c,guild_id,&[actor],None)?,
                    PermissionRequest::Preview { account_id, channel_id, .. } => super::authorization::load(c,guild_id,&[actor,*account_id],*channel_id)?,
                    _ => load(c,guild_id)?,
                };
                member(&state,actor)?;
                match command {
                    PermissionRequest::ViewGuild {..} => {
                        let channels=state.channels.iter().filter(|ch|effective(&state,actor,Some(ch.id)).contains(&Permission::ViewChannel)).cloned().collect();
                        Ok(PermissionResponse::Home {home:GuildHome {can_moderate:can_moderate(&state,actor),can_manage_roles:effective(&state,actor,None).contains(&Permission::ManageRoles),can_manage_channels:effective(&state,actor,None).contains(&Permission::ManageChannels),guild:state.guild,channels}})
                    }
                    PermissionRequest::InspectModeration {..} => Ok(PermissionResponse::Moderation {state:moderation(c,&state,actor)?}),
                    PermissionRequest::Inspect {..} => {
                        require(effective(&state,actor,None).contains(&Permission::ManageRoles))?;
                        Ok(PermissionResponse::State {state})
                    }
                    PermissionRequest::Preview {account_id, channel_id,..} => {
                        require(account_id==actor || effective(&state,actor,None).contains(&Permission::ManageRoles))?;
                        member(&state,account_id)?;
                        if let Some(id)=channel_id { require(state.channels.iter().any(|ch|ch.id==id))?; }
                        Ok(PermissionResponse::Effective {permissions:effective(&state,account_id,channel_id),revision:state.guild.revision})
                    }
                    PermissionRequest::Change {revision, change,..} => {
                        if state.guild.revision!=revision { return Err(Failure::Conflict); }
                        if matches!(change,GuildChange::TransferOwner{..}|GuildChange::Delete {}) { auth::recent(&session)?; }
                        let moderation_change=matches!(change,GuildChange::BanMember{..}|GuildChange::UnbanMember{..}|GuildChange::TimeoutMember{..}|GuildChange::DisconnectVoice{..});
                        let gone=change_guild(c,&state,actor,change)?;
                        if gone { return Ok(PermissionResponse::Done); }
                        execute(c,"UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid", &[&guild_id.to_string()])?;
                        // Assignments can remove the actor's editor access; never return privileged state then.
                        let state=load(c,guild_id)?;
                        if moderation_change { return Ok(PermissionResponse::Moderation {state:moderation(c,&state,actor)?}); }
                        if !effective(&state,actor,None).contains(&Permission::ManageRoles) { return Ok(PermissionResponse::Done); }
                        Ok(PermissionResponse::State {state})
                    }
                    _=>unreachable!(),
                }
            }
        }
        })();
        if result.is_ok() { changes.extend(scopes.into_iter().map(|scope| scope.pause())); }
        result
    });
    for change in changes {
        change.finish(result.is_ok());
    }
    result
}

fn change_guild(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
    change: GuildChange,
) -> Result<bool, Failure> {
    let guild = state.guild.id.to_string();
    let permissions = effective(state, actor, None);
    let need = |permission| require(permissions.contains(&permission));
    let actor_rank = rank(state, actor);
    match change {
        GuildChange::Rename { name: value } => {
            need(Permission::ManageGuild)?;
            execute(
                c,
                "UPDATE guilds SET name=$2 WHERE id=$1::uuid",
                &[&guild, name(&value)?],
            )?;
        }
        GuildChange::Delete {} => {
            require(actor == state.guild.owner)?;
            execute(c, "DELETE FROM guilds WHERE id=$1::uuid", &[&guild])?;
            return Ok(true);
        }
        GuildChange::AddMember { username } => {
            need(Permission::ManageInvites)?;
            require(state.members.len() < 500)?;
            let id: AccountId = query(
                c,
                "SELECT to_jsonb(id) AS data FROM accounts WHERE username=$1 AND email_verified",
                &[&username.trim().to_ascii_lowercase()],
            )?
            .pop()
            .ok_or(Failure::Invalid("Verified account not found"))?;
            not_banned(c, state.guild.id, id)?;
            execute(
                c,
                "INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)",
                &[&guild, &id.to_string()],
            )?;
        }
        GuildChange::RemoveMember { account_id } => {
            need(Permission::KickMembers)?;
            below(state, actor, account_id)?;
            disconnect_voice(c, &guild, account_id)?;
            execute(
                c,
                "DELETE FROM guild_members WHERE guild_id=$1::uuid AND account_id=$2::uuid",
                &[&guild, &account_id.to_string()],
            )?;
        }
        GuildChange::BanMember { account_id } => {
            need(Permission::BanMembers)?;
            below(state, actor, account_id)?;
            let count: i64 = query(c,"SELECT to_jsonb(count(*)) AS data FROM guild_moderation WHERE guild_id=$1::uuid AND banned", &[&guild])?.pop().unwrap_or(0);
            if count >= 1000 {
                return Err(Failure::Invalid("Server ban limit reached (1000)"));
            }
            execute(
                c,
                "INSERT INTO guild_moderation(guild_id,account_id,banned,voice_revision) VALUES($1::uuid,$2::uuid,TRUE,1) ON CONFLICT (guild_id,account_id) DO UPDATE SET banned=TRUE,voice_revision=guild_moderation.voice_revision+1",
                &[&guild, &account_id.to_string()],
            )?;
            execute(
                c,
                "DELETE FROM guild_members WHERE guild_id=$1::uuid AND account_id=$2::uuid",
                &[&guild, &account_id.to_string()],
            )?;
        }
        GuildChange::UnbanMember { account_id } => {
            need(Permission::BanMembers)?;
            if execute(
                c,
                "UPDATE guild_moderation SET banned=FALSE WHERE guild_id=$1::uuid AND account_id=$2::uuid AND banned",
                &[&guild, &account_id.to_string()],
            )? != 1
            {
                return Err(Failure::Conflict);
            }
        }
        GuildChange::TimeoutMember {
            account_id,
            duration_seconds,
        } => {
            need(Permission::ModerateMembers)?;
            below(state, actor, account_id)?;
            if duration_seconds.is_some_and(|s| !(1..=2_419_200).contains(&s)) {
                return Err(Failure::Invalid(
                    "Timeout must be between 1 second and 28 days",
                ));
            }
            execute(
                c,
                "INSERT INTO guild_moderation(guild_id,account_id,timeout_until) VALUES($1::uuid,$2::uuid,CASE WHEN $3='' THEN NULL ELSE clock_timestamp()+($3||' seconds')::interval END) ON CONFLICT (guild_id,account_id) DO UPDATE SET timeout_until=EXCLUDED.timeout_until",
                &[
                    &guild,
                    &account_id.to_string(),
                    &duration_seconds.map(|s| s.to_string()).unwrap_or_default(),
                ],
            )?;
            if duration_seconds.is_some() {
                disconnect_voice(c, &guild, account_id)?;
            }
        }
        GuildChange::DisconnectVoice { account_id } => {
            need(Permission::MoveMembers)?;
            below(state, actor, account_id)?;
            disconnect_voice(c, &guild, account_id)?;
        }
        GuildChange::Leave {} => {
            require(actor != state.guild.owner)?;
            disconnect_voice(c, &guild, actor)?;
            execute(
                c,
                "DELETE FROM guild_members WHERE guild_id=$1::uuid AND account_id=$2::uuid",
                &[&guild, &actor.to_string()],
            )?;
        }
        GuildChange::TransferOwner { account_id } => {
            require(actor == state.guild.owner && actor != account_id)?;
            member(state, account_id)?;
            verified(c, account_id)?;
            execute(
                c,
                "UPDATE guilds SET owner=$2::uuid WHERE id=$1::uuid",
                &[&guild, &account_id.to_string()],
            )?;
        }
        GuildChange::CreateRole {
            name: value,
            position,
            permissions: grants,
        } => {
            need(Permission::ManageRoles)?;
            require(state.roles.len() < 100)?;
            require(position > 0 && position <= 10000 && position < actor_rank)?;
            grantable(state, actor, &grants)?;
            execute(
                c,
                "INSERT INTO guild_roles(guild_id,name,position,permissions) VALUES($1::uuid,$2,$3::integer,$4::jsonb)",
                &[
                    &guild,
                    name(&value)?,
                    &position.to_string(),
                    &json(&grants)?,
                ],
            )?;
        }
        GuildChange::EditRole {
            role_id,
            name: value,
            position,
            permissions: grants,
        } => {
            need(Permission::ManageRoles)?;
            let r = role(state, role_id)?;
            require(
                r.position < actor_rank
                    && position < actor_rank
                    && (0..=10000).contains(&position)
                    && r.everyone == (position == 0),
            )?;
            grantable(state, actor, &grants)?;
            grantable(state, actor, &r.permissions)?;
            execute(
                c,
                "UPDATE guild_roles SET name=$3,position=$4::integer,permissions=$5::jsonb WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[
                    &guild,
                    &role_id.to_string(),
                    name(&value)?,
                    &position.to_string(),
                    &json(&grants)?,
                ],
            )?;
        }
        GuildChange::DeleteRole { role_id } => {
            need(Permission::ManageRoles)?;
            let r = role(state, role_id)?;
            require(!r.everyone && r.position < actor_rank)?;
            grantable(state, actor, &r.permissions)?;
            execute(
                c,
                "DELETE FROM guild_roles WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[&guild, &role_id.to_string()],
            )?;
        }
        GuildChange::AssignRole {
            account_id,
            role_id,
            assigned,
        } => {
            need(Permission::ManageRoles)?;
            below(state, actor, account_id)?;
            let r = role(state, role_id)?;
            require(!r.everyone && r.position < actor_rank)?;
            grantable(state, actor, &r.permissions)?;
            for o in state
                .overrides
                .iter()
                .filter(|o| o.target == OverrideTarget::Role(role_id))
            {
                grantable(state, actor, &o.allow)?;
                grantable(state, actor, &o.deny)?;
            }
            if assigned {
                execute(
                    c,
                    "INSERT INTO guild_member_roles(guild_id,account_id,role_id) VALUES($1::uuid,$2::uuid,$3::uuid) ON CONFLICT DO NOTHING",
                    &[&guild, &account_id.to_string(), &role_id.to_string()],
                )?;
            } else {
                execute(
                    c,
                    "DELETE FROM guild_member_roles WHERE guild_id=$1::uuid AND account_id=$2::uuid AND role_id=$3::uuid",
                    &[&guild, &account_id.to_string(), &role_id.to_string()],
                )?;
            }
        }
        GuildChange::CreateChannel { name: value, kind } => {
            need(Permission::ManageChannels)?;
            require(state.channels.len() < 200)?;
            execute(
                c,
                "INSERT INTO channels(guild_id,name,kind) VALUES($1::uuid,$2,$3)",
                &[
                    &guild,
                    name(&value)?,
                    if kind == ChannelKind::Text {
                        "text"
                    } else {
                        "voice"
                    },
                ],
            )?;
        }
        GuildChange::DeleteChannel { channel_id } => {
            need(Permission::ManageChannels)?;
            require(state.channels.iter().any(|ch| ch.id == channel_id))?;
            execute(
                c,
                "DELETE FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid",
                &[&guild, &channel_id.to_string()],
            )?;
        }
        GuildChange::SetOverride {
            channel_id,
            target,
            allow,
            deny,
        } => {
            need(Permission::ManageRoles)?;
            require(
                state.overrides.len() < 1000
                    || state
                        .overrides
                        .iter()
                        .any(|o| o.channel_id == channel_id && o.target == target),
            )?;
            require(state.channels.iter().any(|ch| ch.id == channel_id))?;
            require(
                allow.is_disjoint(&deny)
                    && !allow.contains(&Permission::Administrator)
                    && !deny.contains(&Permission::Administrator),
            )?;
            grantable(state, actor, &allow)?;
            grantable(state, actor, &deny)?;
            override_target(state, actor, target)?;
            // Deleting an existing deny is also a permission grant; check its complete contents.
            if let Some(old) = state
                .overrides
                .iter()
                .find(|o| o.channel_id == channel_id && o.target == target)
            {
                grantable(state, actor, &old.allow)?;
                grantable(state, actor, &old.deny)?;
            }
            delete_override(c, &guild, &channel_id.to_string(), target)?;
            let (role_id, account_id) = match target {
                OverrideTarget::Role(id) => (id.to_string(), String::new()),
                OverrideTarget::Member(id) => (String::new(), id.to_string()),
            };
            execute(
                c,
                "INSERT INTO channel_overrides(guild_id,channel_id,role_id,account_id,allow,deny) VALUES($1::uuid,$2::uuid,NULLIF($3,'')::uuid,NULLIF($4,'')::uuid,$5::jsonb,$6::jsonb)",
                &[
                    &guild,
                    &channel_id.to_string(),
                    &role_id,
                    &account_id,
                    &json(&allow)?,
                    &json(&deny)?,
                ],
            )?;
        }
        GuildChange::DeleteOverride { channel_id, target } => {
            need(Permission::ManageRoles)?;
            require(state.channels.iter().any(|ch| ch.id == channel_id))?;
            override_target(state, actor, target)?;
            if let Some(old) = state
                .overrides
                .iter()
                .find(|o| o.channel_id == channel_id && o.target == target)
            {
                grantable(state, actor, &old.allow)?;
                grantable(state, actor, &old.deny)?;
            }
            delete_override(c, &guild, &channel_id.to_string(), target)?;
        }
    }
    Ok(false)
}
fn override_target(
    state: &GuildState,
    actor: AccountId,
    target: OverrideTarget,
) -> Result<(), Failure> {
    match target {
        OverrideTarget::Role(id) => {
            let r = role(state, id)?;
            require(r.position < rank(state, actor))?;
            grantable(state, actor, &r.permissions)
        }
        OverrideTarget::Member(id) => below(state, actor, id),
    }
}
fn delete_override(
    c: &mut PgConnection,
    guild: &str,
    channel: &str,
    target: OverrideTarget,
) -> Result<(), Failure> {
    match target {
        OverrideTarget::Role(id) => execute(
            c,
            "DELETE FROM channel_overrides WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND role_id=$3::uuid",
            &[guild, channel, &id.to_string()],
        )?,
        OverrideTarget::Member(id) => execute(
            c,
            "DELETE FROM channel_overrides WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND account_id=$3::uuid",
            &[guild, channel, &id.to_string()],
        )?,
    };
    Ok(())
}

const MODERATION_PERMISSIONS: [Permission; 4] = [
    Permission::KickMembers,
    Permission::BanMembers,
    Permission::ModerateMembers,
    Permission::MoveMembers,
];
fn disconnect_voice(c: &mut PgConnection, guild: &str, account: AccountId) -> Result<(), Failure> {
    execute(
        c,
        "INSERT INTO guild_moderation(guild_id,account_id,voice_revision) VALUES($1::uuid,$2::uuid,1) ON CONFLICT (guild_id,account_id) DO UPDATE SET voice_revision=guild_moderation.voice_revision+1",
        &[guild, &account.to_string()],
    )?;
    Ok(())
}
fn can_moderate(state: &GuildState, actor: AccountId) -> bool {
    let permissions = effective(state, actor, None);
    MODERATION_PERMISSIONS
        .iter()
        .any(|p| permissions.contains(p))
}
fn not_banned(c: &mut PgConnection, guild: GuildId, account: AccountId) -> Result<(), Failure> {
    require(query::<bool>(c,"SELECT to_jsonb(banned) AS data FROM guild_moderation WHERE guild_id=$1::uuid AND account_id=$2::uuid AND banned", &[&guild.to_string(),&account.to_string()])?.is_empty())
}
fn moderation(
    c: &mut PgConnection,
    state: &GuildState,
    actor: AccountId,
) -> Result<ModerationState, Failure> {
    require(can_moderate(state, actor))?;
    let permissions = effective(state, actor, None);
    let can_unban = permissions.contains(&Permission::BanMembers);
    let members = state
        .members
        .iter()
        .map(|m| ModerationMember {
            account_id: m.account_id,
            username: m.username.clone(),
            display_name: m.display_name.clone(),
            timeout_until: m.timeout_until,
            actions: MODERATION_PERMISSIONS
                .into_iter()
                .filter(|p| permissions.contains(p) && below(state, actor, m.account_id).is_ok())
                .collect(),
        })
        .collect();
    let bans = if can_unban {
        query(
            c,
            "SELECT jsonb_build_object('account_id',m.account_id,'username',a.username,'display_name',a.display_name) AS data FROM guild_moderation m JOIN accounts a ON a.id=m.account_id WHERE m.guild_id=$1::uuid AND m.banned ORDER BY a.username",
            &[&state.guild.id.to_string()],
        )?
    } else {
        vec![]
    };
    Ok(ModerationState {
        guild: state.guild.clone(),
        members,
        bans,
        can_unban,
    })
}
