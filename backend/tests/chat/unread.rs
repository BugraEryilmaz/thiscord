use super::*;
use diesel::sql_types::{Json, Jsonb};

const RANGE_SQL: &str = include_str!("../../src/chat/unread.sql");
const ORIGINAL_SQL: &str = "SELECT jsonb_build_object('channel_id',m.channel_id,'count',count(*),'mentions',count(*) FILTER (WHERE m.mentions @> jsonb_build_array($2::uuid))) AS data FROM messages m LEFT JOIN channel_reads r ON r.guild_id=m.guild_id AND r.channel_id=m.channel_id AND r.account_id=$2::uuid WHERE m.guild_id=$1::uuid AND NOT m.deleted AND m.author_id IS DISTINCT FROM $2::uuid AND m.sequence>COALESCE(r.through,0) GROUP BY m.channel_id";

#[derive(QueryableByName)]
struct Data {
    #[diesel(sql_type = Jsonb)]
    data: Value,
}

#[derive(QueryableByName)]
struct Plan {
    #[diesel(sql_type = Json, column_name = "QUERY PLAN")]
    plan: Value,
}

fn data(c: &mut PgConnection, sql: &str, params: &[String]) -> Vec<Value> {
    let mut q = diesel::sql_query(sql).into_boxed();
    for p in params {
        q = q.bind::<Text, _>(p);
    }
    q.load::<Data>(c)
        .unwrap()
        .into_iter()
        .map(|r| r.data)
        .collect()
}

fn explain(c: &mut PgConnection, sql: &str, params: &[String]) -> Value {
    let mut q = diesel::sql_query(format!(
        "EXPLAIN (ANALYZE, BUFFERS, TIMING OFF, FORMAT JSON) {sql}"
    ))
    .into_boxed();
    for p in params {
        q = q.bind::<Text, _>(p);
    }
    q.get_result::<Plan>(c).unwrap().plan[0].clone()
}

fn message_rows(plan: &Value) -> u64 {
    let own = if plan["Relation Name"] == "messages" {
        (plan["Actual Rows"].as_u64().unwrap_or(0)
            + plan["Rows Removed by Filter"].as_u64().unwrap_or(0)
            + plan["Rows Removed by Index Recheck"].as_u64().unwrap_or(0))
            * plan["Actual Loops"].as_u64().unwrap_or(0)
    } else {
        0
    };
    own + plan["Plans"]
        .as_array()
        .map(|plans| plans.iter().map(message_rows).sum::<u64>())
        .unwrap_or(0)
}

async fn send(app: &Router, token: &str, guild: &Value, channel: &Value, content: &str) -> Value {
    chat(app, token, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":content}), StatusCode::OK).await["message"].clone()
}

async fn counts(app: &Router, token: &str, guild: &Value, expected: Value) {
    assert_eq!(
        chat(
            app,
            token,
            json!({"action":"unread","guild_id":guild}),
            StatusCode::OK
        )
        .await["channels"],
        expected
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn unread_preserves_authors_mentions_boundaries_and_current_permissions() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    counts(&app, &guest, &guild, json!([])).await;
    send(&app, &guest, &guild, &channel, "self @guest").await;
    counts(&app, &guest, &guild, json!([])).await;
    let first = send(&app, &owner, &guild, &channel, "hello @guest @guest").await;
    let expected =
        |count, mentions| json!([{"channel_id":channel,"count":count,"mentions":mentions}]);
    // No marker means all other authors' live messages; a repeated mention counts once.
    counts(&app, &guest, &guild, expected(1, 1)).await;
    chat(&app, &owner, json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":first["id"],"revision":0,"content":"mention removed"}), StatusCode::OK).await;
    counts(&app, &guest, &guild, expected(1, 0)).await;
    chat(&app, &owner, json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":first["id"],"revision":1,"content":"mention added @guest"}), StatusCode::OK).await;
    counts(&app, &guest, &guild, expected(1, 1)).await;
    let tombstone = send(&app, &owner, &guild, &channel, "delete @guest").await;
    chat(&app, &owner, json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":tombstone["id"],"revision":0}), StatusCode::OK).await;
    counts(&app, &guest, &guild, expected(1, 1)).await;

    let (retired_id, retired) = user(&db, "retired");
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":"retired"}),
        StatusCode::OK,
    )
    .await;
    let anonymous = send(&app, &retired, &guild, &channel, "retained @guest").await;
    diesel::sql_query("DELETE FROM accounts WHERE id=$1::uuid")
        .bind::<Text, _>(retired_id.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    counts(&app, &guest, &guild, expected(2, 2)).await;
    // Strictly greater than the marker, including gaps left by own/deleted messages.
    chat(
        &app,
        &guest,
        json!({"action":"read","guild_id":guild,"channel_id":channel,"through":first["sequence"]}),
        StatusCode::OK,
    )
    .await;
    counts(&app, &guest, &guild, expected(1, 1)).await;
    chat(
        &app,
        &guest,
        json!({"action":"read","guild_id":guild,"channel_id":channel,"through":0}),
        StatusCode::OK,
    )
    .await;
    counts(&app, &guest, &guild, expected(1, 1)).await;

    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"second","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let second = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    // Each permission is independently required. The other, empty channel stays
    // authorized, so this exercises SQL filtering as well as the no-channel case.
    for permission in ["read_history", "view_channel"] {
        change(&app, &owner, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[permission]}), StatusCode::OK).await;
        counts(&app, &guest, &guild, json!([])).await;
        change(&app, &owner, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[]}), StatusCode::OK).await;
        counts(&app, &guest, &guild, expected(1, 1)).await;
    }
    send(&app, &owner, &guild, &second, "independent marker").await;
    chat(&app, &guest, json!({"action":"read","guild_id":guild,"channel_id":channel,"through":anonymous["sequence"]}), StatusCode::OK).await;
    counts(
        &app,
        &guest,
        &guild,
        json!([{"channel_id":second,"count":1,"mentions":0}]),
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    chat(
        &app,
        &guest,
        json!({"action":"unread","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
}

// Also a reproducible benchmark: --test chat unread_ranges -- --ignored --nocapture.
// Timing is informational; row counts and result equivalence are regression gates.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn unread_ranges_scan_backlog_instead_of_history() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (_, _, _, guest_id, state, _) = setup(&db).await;
    let guild = state["guild"]["id"].as_str().unwrap().to_owned();
    let owner = state["guild"]["owner"].as_str().unwrap();
    let mut c = db.pool.get().unwrap();
    let index = diesel::sql_query("SELECT to_jsonb(indexdef) AS data FROM pg_indexes WHERE schemaname=current_schema() AND indexname='channel_unread_range'")
        .get_result::<Data>(&mut c).unwrap().data;
    assert!(
        index
            .as_str()
            .unwrap()
            .contains("INCLUDE (author_id, mentions)")
    );
    c.batch_execute(
        "SET statement_timeout = '30s'; SET jit = on; SET max_parallel_workers_per_gather = 0;",
    )
    .unwrap();
    diesel::sql_query("INSERT INTO channels(guild_id,id,name,kind) SELECT $1::uuid,md5('unread-bench-'||n)::uuid,'bench-'||n,'text' FROM generate_series(1,20) n")
        .bind::<Text, _>(&guild).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO messages(guild_id,channel_id,author_id,client_id,request_hash,content,mentions) SELECT $1::uuid,ch.id,$2::uuid,gen_random_uuid(),'fixture','fixture',jsonb_build_array($3::uuid) FROM channels ch CROSS JOIN generate_series(1,10000) n WHERE ch.guild_id=$1::uuid AND ch.name LIKE 'bench-%' ORDER BY ch.id,n")
        .bind::<Text, _>(&guild).bind::<Text, _>(owner).bind::<Text, _>(guest_id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO channel_reads(guild_id,channel_id,account_id,through) SELECT guild_id,channel_id,$2::uuid,max(sequence)-50 FROM messages WHERE guild_id=$1::uuid GROUP BY guild_id,channel_id")
        .bind::<Text, _>(&guild).bind::<Text, _>(guest_id.to_string()).execute(&mut c).unwrap();
    c.batch_execute("ANALYZE messages; ANALYZE channel_reads; ANALYZE channels;")
        .unwrap();
    let ids = diesel::sql_query(
        "SELECT to_jsonb(array_agg(id ORDER BY id)) AS data FROM channels WHERE guild_id=$1::uuid AND name LIKE 'bench-%'",
    )
    .bind::<Text, _>(&guild)
    .get_result::<Data>(&mut c)
    .unwrap()
    .data;
    let ids = format!(
        "{{{}}}",
        ids.as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(",")
    );
    let params = [guild, guest_id.to_string(), ids];
    for (scenario, marker) in [
        ("small backlog", None),
        ("large backlog", Some(0)),
        ("fully read", Some(200000)),
    ] {
        if let Some(marker) = marker {
            diesel::sql_query("UPDATE channel_reads SET through=$1::bigint")
                .bind::<Text, _>(marker.to_string())
                .execute(&mut c)
                .unwrap();
            c.batch_execute("ANALYZE channel_reads").unwrap();
        }
        let mut old = data(&mut c, ORIGINAL_SQL, &params[..2]);
        let new = data(&mut c, RANGE_SQL, &params);
        old.sort_by_key(|r| r["channel_id"].as_str().unwrap().to_owned());
        assert_eq!(old, new);
        let expected = match marker {
            None => 1000,
            Some(0) => 200000,
            _ => 0,
        };
        assert_eq!(
            new.iter()
                .map(|r| r["count"].as_u64().unwrap())
                .sum::<u64>(),
            expected
        );
        for (name, sql, args) in [
            ("original", ORIGINAL_SQL, &params[..2]),
            ("range", RANGE_SQL, &params[..]),
        ] {
            // Results above warm the queries. Include the round trip, planning,
            // and JSON decoding in elapsed time.
            let mut times = Vec::new();
            for _ in 0..5 {
                let started = std::time::Instant::now();
                data(&mut c, sql, args);
                times.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            let plan = explain(&mut c, sql, args);
            let rows = message_rows(&plan["Plan"]);
            let jit = !plan["JIT"].is_null();
            eprintln!(
                "{scenario}: {name}: median_elapsed_ms={:.3}, message_rows={}, cost={}, jit={}",
                times[2], rows, plan["Plan"]["Total Cost"], jit
            );
            if name == "range" {
                assert_eq!(rows, expected, "{plan}");
                assert!(!jit, "unexpected JIT compilation: {plan}");
            }
        }
    }
}
