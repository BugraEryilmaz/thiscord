use super::*;
use diesel::connection::SimpleConnection;
use serde_json::{Value, json};
use std::time::Instant;

struct Database {
    connection: PgConnection,
    schema: String,
    pool: Option<DbPool>,
}

impl Drop for Database {
    fn drop(&mut self) {
        self.pool.take();
        self.connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .unwrap();
    }
}

fn database() -> Database {
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"), "requires a test database");
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("direct_attachment_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    let mut db = Database {
        connection,
        schema,
        pool: None,
    };
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", db.schema));
    db.pool = Some(crate::db::connect_and_migrate(url.as_str()).unwrap());
    db
}

fn seed(c: &mut PgConnection) -> (AccountId, ConversationId, MessageId) {
    let owner = AccountId::from_uuid(Uuid::from_u128(1));
    let peer = AccountId::from_uuid(Uuid::from_u128(2));
    let conversation = ConversationId::from_uuid(Uuid::new_v4());
    let message = MessageId::from_uuid(Uuid::new_v4());
    for (id, name) in [(owner, "owner"), (peer, "peer")] {
        execute(c, "INSERT INTO accounts(id,username,email,display_name) VALUES($1::uuid,$2,$2||'@example.test',$2)", &[&id.to_string(), name]).unwrap();
    }
    execute(c, "INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,$2::uuid,$3::uuid,$2::uuid,'accepted')", &[&conversation.to_string(), &owner.to_string(), &peer.to_string()]).unwrap();
    execute(c, "INSERT INTO direct_messages(id,conversation_id,author_id,client_id,content,created_at) VALUES($1::uuid,$2::uuid,$3::uuid,gen_random_uuid(),'attachment',now())", &[&message.to_string(), &conversation.to_string(), &owner.to_string()]).unwrap();
    (owner, conversation, message)
}

fn upload(c: &mut PgConnection, owner: AccountId, conversation: ConversationId) -> AttachmentId {
    let id = AttachmentId::from_uuid(Uuid::new_v4());
    execute(c, "INSERT INTO direct_attachments(id,conversation_id,owner_id,filename,content_type,size_bytes,data) VALUES($1::uuid,$2::uuid,$3::uuid,'file.txt','application/octet-stream',3,decode('010203','hex'))", &[&id.to_string(), &conversation.to_string(), &owner.to_string()]).unwrap();
    id
}

fn scalar<T: serde::de::DeserializeOwned>(c: &mut PgConnection, sql: &str, args: &[&str]) -> T {
    query(c, sql, args).unwrap().pop().unwrap()
}

fn wait_until(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
fn cleanup_preserves_attachment_claimed_by_uncommitted_send() {
    let db = database();
    let pool = db.pool.as_ref().unwrap();
    let mut observer = pool.get().unwrap();
    let mut sender = pool.get().unwrap();
    let mut cleaner = pool.get().unwrap();
    // Bound even failure paths, but leave time to observe and release the lock.
    cleaner
        .batch_execute("SET lock_timeout = '10s'; SET statement_timeout = '15s'")
        .unwrap();
    let (owner, conversation, message) = seed(&mut observer);
    let claimed = upload(&mut observer, owner, conversation);
    let abandoned = upload(&mut observer, owner, conversation);
    let fresh = upload(&mut observer, owner, conversation);
    execute(
        &mut observer,
        "UPDATE direct_attachments SET expires_at=now()-interval '1 hour' WHERE id=$1::uuid",
        &[&abandoned.to_string()],
    )
    .unwrap();
    let sender_pid: i32 = scalar(
        &mut sender,
        "SELECT to_jsonb(pg_backend_pid()) AS data",
        &[],
    );
    let cleaner_pid: i32 = scalar(
        &mut cleaner,
        "SELECT to_jsonb(pg_backend_pid()) AS data",
        &[],
    );

    // The real send helper claims while its transaction's now() is before expiry.
    // Keep the transaction open until cleanup has seen the old unclaimed row.
    execute(&mut observer, "UPDATE direct_attachments SET expires_at=clock_timestamp()+interval '1 second' WHERE id=$1::uuid", &[&claimed.to_string()]).unwrap();
    std::thread::scope(|scope| {
        let (worker, blocked) = sender.transaction::<_, Failure, _>(|sender| {
            attach(sender, owner, conversation, message, &[claimed])?;
            assert!(wait_until(|| scalar(&mut observer, "SELECT to_jsonb(expires_at<=clock_timestamp()) AS data FROM direct_attachments WHERE id=$1::uuid", &[&claimed.to_string()])), "attachment did not expire before cleanup");
            let worker = scope.spawn(move || cleanup(&mut cleaner));
            let blocked = wait_until(|| scalar(&mut observer, "SELECT to_jsonb($1::integer=ANY(pg_blocking_pids($2::integer))) AS data", &[&sender_pid.to_string(), &cleaner_pid.to_string()]));
            // Commit before joining, including on a failed lock observation.
            Ok((worker, blocked))
        }).unwrap();
        let result = worker.join().expect("cleanup worker panicked");
        assert!(blocked, "cleanup never waited for the uncommitted claim");
        result.unwrap();
    });

    let metadata: Value = scalar(
        &mut observer,
        "SELECT attachments AS data FROM direct_messages WHERE id=$1::uuid",
        &[&message.to_string()],
    );
    assert_eq!(metadata[0]["id"], json!(claimed));
    let file = diesel::sql_query(
        "SELECT data,filename,content_type FROM direct_attachments WHERE id=$1 AND message_id=$2",
    )
    .bind::<SqlUuid, _>(claimed.as_uuid())
    .bind::<SqlUuid, _>(message.as_uuid())
    .get_result::<File>(&mut observer)
    .optional()
    .unwrap()
    .expect("cleanup deleted the published attachment bytes");
    assert_eq!(file.data, [1, 2, 3]);
    assert!(!scalar::<bool>(
        &mut observer,
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM direct_attachments WHERE id=$1::uuid)) AS data",
        &[&abandoned.to_string()]
    ));
    assert!(scalar::<bool>(
        &mut observer,
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM direct_attachments WHERE id=$1::uuid)) AS data",
        &[&fresh.to_string()]
    ));
}

#[test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
fn cleanup_expired_stages_remains_bounded() {
    let db = database();
    let mut c = db.pool.as_ref().unwrap().get().unwrap();
    let (owner, conversation, _) = seed(&mut c);
    execute(&mut c, "INSERT INTO direct_attachments(id,conversation_id,owner_id,filename,content_type,size_bytes,data,expires_at) SELECT gen_random_uuid(),$1::uuid,$2::uuid,'file.txt','application/octet-stream',1,decode('01','hex'),now()-interval '1 hour' FROM generate_series(1,65)", &[&conversation.to_string(), &owner.to_string()]).unwrap();
    cleanup(&mut c).unwrap();
    assert_eq!(
        scalar::<i64>(
            &mut c,
            "SELECT to_jsonb(count(*)) AS data FROM direct_attachments",
            &[]
        ),
        1
    );
    cleanup(&mut c).unwrap();
    assert_eq!(
        scalar::<i64>(
            &mut c,
            "SELECT to_jsonb(count(*)) AS data FROM direct_attachments",
            &[]
        ),
        0
    );
}
