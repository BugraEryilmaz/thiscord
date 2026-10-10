//! Focused inputs to the authoritative evaluator, never an editor/API snapshot.
//! Callers hold the guild row lock for the entire authorization/action transaction.
use crate::auth::{Failure, store::query};
use diesel::PgConnection;
use thiscord_shared::{AccountId, ChannelId, GuildId, permissions::*};

pub(crate) fn load(
    c: &mut PgConnection,
    guild: GuildId,
    accounts: &[AccountId],
    channel: Option<ChannelId>,
) -> Result<GuildState, Failure> {
    let guild = guild.to_string();
    let accounts = serde_json::to_string(accounts).map_err(|_| Failure::Unavailable)?;
    let channel = channel.map(|id| id.to_string()).unwrap_or_default();
    // Every subquery is guild-scoped. Only assigned roles plus Everyone, requested
    // members, and their applicable overrides are materialized. None selects all
    // channels for unread counts/home views, without loading the whole membership.
    query(c, r#"
        WITH selected_members AS (
            SELECT m.account_id FROM guild_members m
            WHERE m.guild_id=$1::uuid
              AND m.account_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))
        ), selected_roles AS (
            SELECT r.* FROM guild_roles r WHERE r.guild_id=$1::uuid AND
            (r.everyone OR r.id IN (
                SELECT role_id FROM guild_member_roles
                WHERE guild_id=$1::uuid AND account_id IN (SELECT account_id FROM selected_members)
            ))
        )
        SELECT jsonb_build_object(
            'guild', jsonb_build_object('id',g.id,'name',g.name,'owner',g.owner,'revision',g.revision),
            'roles', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM selected_roles r), '[]'),
            'members', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                'account_id', a.id, 'username', a.username, 'display_name', a.display_name,
                'timeout_until', (SELECT timeout_until FROM guild_moderation WHERE guild_id=$1::uuid AND account_id=a.id),
                'roles', COALESCE((SELECT jsonb_agg(role_id) FROM guild_member_roles WHERE guild_id=$1::uuid AND account_id=a.id), '[]')
            )) FROM accounts a WHERE a.id IN (SELECT account_id FROM selected_members)), '[]'),
            'channels', COALESCE((SELECT jsonb_agg(to_jsonb(ch) ORDER BY ch.id) FROM channels ch
                WHERE ch.guild_id=$1::uuid AND ($3='' OR ch.id=NULLIF($3,'')::uuid)), '[]'),
            'overrides', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                'channel_id', channel_id,
                'target', jsonb_build_object('kind', CASE WHEN role_id IS NULL THEN 'member' ELSE 'role' END, 'id', COALESCE(role_id,account_id)),
                'allow', allow, 'deny', deny
            )) FROM channel_overrides WHERE guild_id=$1::uuid
                AND ($3='' OR channel_id=NULLIF($3,'')::uuid)
                AND (role_id IN (SELECT id FROM selected_roles) OR account_id IN (SELECT account_id FROM selected_members))), '[]')
        ) AS data FROM guilds g WHERE g.id=$1::uuid
    "#, &[&guild, &accounts, &channel])?
    .pop().ok_or(Failure::Forbidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::store::execute,
        db,
        permissions::{evaluator::effective, store},
    };
    use diesel::{Connection, connection::SimpleConnection};
    use diesel_migrations::MigrationHarness;
    use uuid::Uuid;

    #[test]
    #[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
    fn focused_inputs_match_full_evaluation_and_exclude_unrelated_rows() {
        dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
        assert!(url::Url::parse(&url).unwrap().path().ends_with("_test"));
        let mut c = PgConnection::establish(&url).unwrap();
        c.begin_test_transaction().unwrap();
        let schema = format!("authorization_test_{}", Uuid::new_v4().simple());
        // The schema and all fixture data roll back when this connection closes.
        c.batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET LOCAL search_path={schema}"
        ))
        .unwrap();
        c.run_pending_migrations(db::MIGRATIONS).unwrap();
        let accounts: Vec<AccountId> = (0..4)
            .map(|_| AccountId::from_uuid(Uuid::new_v4()))
            .collect();
        for (n, account) in accounts.iter().enumerate() {
            execute(&mut c, "INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)", &[&account.to_string(), &format!("user{n}")]).unwrap();
        }
        let guild = GuildId::from_uuid(Uuid::new_v4());
        execute(
            &mut c,
            "INSERT INTO guilds(id,name,owner) VALUES($1::uuid,'guild',$2::uuid)",
            &[&guild.to_string(), &accounts[0].to_string()],
        )
        .unwrap();
        for account in &accounts[..3] {
            execute(
                &mut c,
                "INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)",
                &[&guild.to_string(), &account.to_string()],
            )
            .unwrap();
        }
        execute(&mut c, r#"INSERT INTO guild_roles(guild_id,name,position,everyone,permissions)
            SELECT $1::uuid,'role'||n,n,n=0,'["view_channel","read_history","join_voice","speak"]'::jsonb FROM generate_series(0,4) n"#, &[&guild.to_string()]).unwrap();
        let roles: Vec<thiscord_shared::RoleId> = query(&mut c, "SELECT to_jsonb(id) AS data FROM guild_roles WHERE guild_id=$1::uuid ORDER BY position", &[&guild.to_string()]).unwrap();
        for role in &roles[1..3] {
            execute(&mut c, "INSERT INTO guild_member_roles(guild_id,account_id,role_id) VALUES($1::uuid,$2::uuid,$3::uuid)", &[&guild.to_string(), &accounts[1].to_string(), &role.to_string()]).unwrap();
        }
        execute(&mut c, "INSERT INTO channels(guild_id,name,kind) SELECT $1::uuid,'channel'||n,'text' FROM generate_series(1,2) n", &[&guild.to_string()]).unwrap();
        let channels: Vec<ChannelId> = query(
            &mut c,
            "SELECT to_jsonb(id) AS data FROM channels WHERE guild_id=$1::uuid ORDER BY id",
            &[&guild.to_string()],
        )
        .unwrap();
        execute(&mut c, r#"INSERT INTO channel_overrides(guild_id,channel_id,role_id,allow,deny)
            SELECT r.guild_id,ch.id,r.id,CASE WHEN r.position=2 THEN '["send_messages"]'::jsonb ELSE '[]'::jsonb END,
                CASE WHEN r.position=1 THEN '["send_messages"]'::jsonb WHEN r.position=4 THEN '["view_channel"]'::jsonb ELSE '[]'::jsonb END
            FROM guild_roles r JOIN channels ch ON ch.guild_id=r.guild_id WHERE r.guild_id=$1::uuid"#, &[&guild.to_string()]).unwrap();
        execute(&mut c, r#"INSERT INTO channel_overrides(guild_id,channel_id,account_id,deny) VALUES($1::uuid,$2::uuid,$3::uuid,'["join_voice"]')"#, &[&guild.to_string(), &channels[0].to_string(), &accounts[1].to_string()]).unwrap();
        let focused = load(&mut c, guild, &[accounts[1]], Some(channels[0])).unwrap();
        assert_eq!(focused.members.len(), 1);
        assert_eq!(focused.roles.len(), 3);
        assert_eq!(focused.channels.len(), 1);
        assert_eq!(focused.overrides.len(), 4);
        let p = effective(&focused, accounts[1], Some(channels[0]));
        assert!(p.contains(&Permission::SendMessages));
        assert!(!p.contains(&Permission::Speak));
        let compare = |c: &mut PgConnection| {
            let full = store::load(c, guild).unwrap();
            for account in &accounts {
                for channel in [
                    None,
                    Some(channels[0]),
                    Some(channels[1]),
                    Some(ChannelId::from_uuid(Uuid::new_v4())),
                ] {
                    let focused = load(c, guild, &[*account], channel).unwrap();
                    assert_eq!(
                        effective(&focused, *account, channel),
                        effective(&full, *account, channel)
                    );
                }
            }
            // Batched presence/preview inputs must not give one actor another's roles.
            let batch = load(c, guild, &accounts, Some(channels[0])).unwrap();
            for account in &accounts {
                assert_eq!(
                    effective(&batch, *account, Some(channels[0])),
                    effective(&full, *account, Some(channels[0]))
                );
            }
        };
        compare(&mut c);
        execute(&mut c, r#"UPDATE guild_roles SET permissions='["administrator"]' WHERE guild_id=$1::uuid AND id=$2::uuid"#, &[&guild.to_string(), &roles[1].to_string()]).unwrap();
        compare(&mut c);
        execute(&mut c, "INSERT INTO guild_moderation(guild_id,account_id,timeout_until) VALUES($1::uuid,$2::uuid,now()+interval '1 hour')", &[&guild.to_string(), &accounts[1].to_string()]).unwrap();
        compare(&mut c);
        execute(&mut c, "UPDATE guild_moderation SET timeout_until=now()-interval '1 hour' WHERE guild_id=$1::uuid", &[&guild.to_string()]).unwrap();
        compare(&mut c);
        execute(
            &mut c,
            "DELETE FROM guild_members WHERE guild_id=$1::uuid AND account_id=$2::uuid",
            &[&guild.to_string(), &accounts[1].to_string()],
        )
        .unwrap();
        compare(&mut c);
    }
}
