//! Recovery must use the small active-message index without changing its results.
use aionui_db::{IConversationRepository, MessageRowUpdate, SqliteConversationRepository, init_database_memory};
use sqlx::Row;

const USER: &str = "system_default_user";
const CONVERSATION: &str = "recovery-index-conversation";
const RECOVERY_SQL: &str = "SELECT c.user_id, m.* FROM messages m \
    INNER JOIN conversations c ON c.id = m.conversation_id \
    WHERE m.position = 'left' AND m.status IN ('work', 'pending') \
      AND m.type IN ('text', 'thinking', 'tool_call', 'tool_group') \
    ORDER BY m.created_at ASC";

async fn insert_conversation(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO conversations (id,user_id,name,type,created_at,updated_at) \
         VALUES (?,?,'Recovery index','codex',1,1)",
    )
    .bind(CONVERSATION)
    .bind(USER)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_message(
    pool: &sqlx::SqlitePool,
    id: &str,
    kind: &str,
    position: Option<&str>,
    status: Option<&str>,
    created_at: i64,
) {
    sqlx::query(
        "INSERT INTO messages (id,conversation_id,type,content,position,status,created_at) \
         VALUES (?,?,?,'{\"text\":\"preserve this content\"}',?,?,?)",
    )
    .bind(id)
    .bind(CONVERSATION)
    .bind(kind)
    .bind(position)
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn snapshot(pool: &sqlx::SqlitePool) -> Vec<(String, String, Option<String>, Option<String>)> {
    sqlx::query_as("SELECT id,content,position,status FROM messages ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn recovery_ids(repo: &SqliteConversationRepository) -> Vec<String> {
    repo.list_stale_runtime_messages()
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            assert_eq!(row.user_id, USER);
            row.message.id
        })
        .collect()
}

#[tokio::test]
async fn stale_runtime_index_upgrades_existing_history_once_and_is_selected() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    // Reconstruct the immediately preceding schema on an initialized database.
    // No historical migration is edited or bypassed in production.
    sqlx::query("DROP INDEX idx_messages_stale_runtime")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 46")
        .execute(pool)
        .await
        .unwrap();
    insert_conversation(pool).await;
    for i in 0..128 {
        insert_message(pool, &format!("finished-{i}"), "text", Some("left"), Some("finish"), i).await;
    }
    insert_message(pool, "active", "tool_call", Some("left"), Some("work"), 200).await;
    let before = snapshot(pool).await;
    let repo = SqliteConversationRepository::new(pool.clone());
    let recovered_before = recovery_ids(&repo).await;

    let migrator = sqlx::migrate!();
    migrator.run(pool).await.unwrap();
    assert_eq!(snapshot(pool).await, before, "migration must not rewrite messages");
    assert_eq!(recovery_ids(&repo).await, recovered_before);
    assert_eq!(recovered_before, vec!["active"]);
    let plan = sqlx::query(&format!("EXPLAIN QUERY PLAN {RECOVERY_SQL}"))
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH m USING INDEX idx_messages_stale_runtime")),
        "the actual SQLite planner must select the recovery index: {plan:?}"
    );
    assert!(plan.iter().all(|line| !line.contains("idx_messages_type")));
    let schema_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(pool)
        .await
        .unwrap();
    migrator.run(pool).await.unwrap();
    let second_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        second_version, schema_version,
        "subsequent startup must not rebuild the index"
    );
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = 46 AND success = 1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    assert_eq!(snapshot(pool).await, before);
}

#[tokio::test]
async fn stale_runtime_index_preserves_filter_order_and_write_transitions() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    insert_conversation(pool).await;
    let mut expected = Vec::new();
    let mut n = 0;
    for kind in ["text", "thinking", "tool_call", "tool_group", "tips", "image"] {
        for position in [Some("left"), Some("right"), Some("center"), Some("pop"), None] {
            for status in [Some("work"), Some("pending"), Some("finish"), Some("error"), None] {
                let id = format!("fixture-{n:03}");
                let created_at = 1000 - n;
                insert_message(pool, &id, kind, position, status, created_at).await;
                if position == Some("left")
                    && matches!(status, Some("work" | "pending"))
                    && matches!(kind, "text" | "thinking" | "tool_call" | "tool_group")
                {
                    expected.push((created_at, id));
                }
                n += 1;
            }
        }
    }
    expected.sort();
    let expected: Vec<String> = expected.into_iter().map(|(_, id)| id).collect();
    let repo = SqliteConversationRepository::new(pool.clone());
    assert_eq!(recovery_ids(&repo).await, expected);
    assert_eq!(expected.len(), 8);

    let id = &expected[0];
    repo.update_message(
        USER,
        CONVERSATION,
        id,
        &MessageRowUpdate {
            content: Some(r#"{"text":"streamed content"}"#.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        recovery_ids(&repo).await,
        expected,
        "streaming must preserve index membership"
    );
    for status in [Some("finish"), Some("work"), None, Some("pending")] {
        repo.update_message(
            USER,
            CONVERSATION,
            id,
            &MessageRowUpdate {
                status: Some(status.map(str::to_string)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut wanted = expected.clone();
        if !matches!(status, Some("work" | "pending")) {
            wanted.remove(0);
        }
        assert_eq!(recovery_ids(&repo).await, wanted);
    }
    sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(recovery_ids(&repo).await, expected[1..]);
    let check: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(check, "ok");
}
