//! User recency is persisted with messages, never derived by list-time history reads.
use aionui_db::{
    ConversationFilters, DbError, IConversationRepository, SqliteConversationRepository, init_database_memory,
    models::MessageRow,
};
use sqlx::Row;

const USER: &str = "system_default_user";
const BASE: i64 = 1_700_000_000_000;

async fn conversation(pool: &sqlx::SqlitePool, id: &str) {
    sqlx::query("INSERT INTO conversations (id,user_id,name,type,created_at,updated_at) VALUES (?,?,'Test','acp',?,?)")
        .bind(id)
        .bind(USER)
        .bind(BASE)
        .bind(BASE)
        .execute(pool)
        .await
        .unwrap();
}

fn message(id: &str, conv_id: &str, kind: &str, position: Option<&str>, time: i64) -> MessageRow {
    MessageRow {
        id: id.into(),
        conversation_id: conv_id.into(),
        msg_id: None,
        r#type: kind.into(),
        content: "{}".into(),
        position: position.map(str::to_owned),
        status: Some("finish".into()),
        hidden: false,
        created_at: time,
        backend_turn_id: None,
    }
}

async fn recency(repo: &SqliteConversationRepository, id: &str) -> Option<i64> {
    repo.get(USER, id).await.unwrap().unwrap().last_user_message_at
}

#[tokio::test]
async fn sends_update_list_summary_but_replies_renames_and_retries_do_not() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    let repo = SqliteConversationRepository::new(pool.clone());
    conversation(pool, "chat").await;
    assert_eq!(recency(&repo, "chat").await, None);

    let mut input = message("user", "chat", "user_content", Some("right"), BASE + 100);
    repo.insert_message(USER, &input).await.unwrap();
    assert_eq!(recency(&repo, "chat").await, Some(BASE + 100));
    for kind in ["text", "thinking", "tool_call", "tool_group"] {
        repo.upsert_message(USER, &message(kind, "chat", kind, Some("left"), BASE + 900))
            .await
            .unwrap();
    }
    // A later retry updates content but the upsert preserves the original send time.
    input.created_at = BASE + 1000;
    repo.upsert_message(USER, &input).await.unwrap();
    sqlx::query("UPDATE conversations SET name = 'Renamed', updated_at = ? WHERE id = 'chat'")
        .bind(BASE + 2000)
        .execute(pool)
        .await
        .unwrap();
    let page = repo
        .list_paginated(USER, &ConversationFilters::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].last_user_message_at, Some(BASE + 100));
    assert_eq!(page.items[0].updated_at, BASE + 2000);
    assert_eq!(
        repo.get_message(USER, "chat", "user")
            .await
            .unwrap()
            .unwrap()
            .created_at,
        BASE + 100
    );

    repo.insert_message(USER, &message("older", "chat", "text", Some("right"), BASE + 50))
        .await
        .unwrap();
    assert_eq!(recency(&repo, "chat").await, Some(BASE + 100));
    repo.insert_message(USER, &message("newer", "chat", "user_content", None, BASE + 300))
        .await
        .unwrap();
    assert_eq!(recency(&repo, "chat").await, Some(BASE + 300));
}

#[tokio::test]
async fn failed_writes_and_rollback_leave_no_recency_and_cannot_change_another_owner() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    let repo = SqliteConversationRepository::new(pool.clone());
    conversation(pool, "chat").await;
    conversation(pool, "other").await;
    let input = message("shared-id", "chat", "user_content", Some("right"), BASE + 100);
    repo.insert_message(USER, &input).await.unwrap();
    let err = repo.insert_message("wrong-user", &input).await.unwrap_err();
    assert!(matches!(err, DbError::NotFound(_)));
    let err = repo.upsert_message("wrong-user", &input).await.unwrap_err();
    assert!(matches!(err, DbError::Conflict(_)));
    let err = repo
        .upsert_message(
            USER,
            &message("shared-id", "other", "user_content", Some("right"), BASE + 900),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Conflict(_)));
    assert_eq!(recency(&repo, "chat").await, Some(BASE + 100));
    assert_eq!(recency(&repo, "other").await, None);
    assert!(repo.get("wrong-user", "chat").await.unwrap().is_none());

    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO messages (id,conversation_id,type,position,created_at) VALUES ('rolled-back','other','user_content','right',?)",
    )
    .bind(BASE + 900)
    .execute(&mut *tx)
    .await
    .unwrap();
    let in_tx: Option<i64> = sqlx::query_scalar("SELECT last_user_message_at FROM conversations WHERE id = 'other'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(in_tx, Some(BASE + 900));
    tx.rollback().await.unwrap();
    assert_eq!(recency(&repo, "other").await, None);
    assert!(repo.get_message(USER, "other", "rolled-back").await.unwrap().is_none());
}

#[tokio::test]
async fn forks_deletes_and_timestamp_corrections_follow_the_persisted_user_history() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    let repo = SqliteConversationRepository::new(pool.clone());
    for id in ["source", "fork", "moved"] {
        conversation(pool, id).await;
    }
    for (id, time) in [("first", BASE + 100), ("last", BASE + 300)] {
        repo.insert_message(USER, &message(id, "source", "text", Some("right"), time))
            .await
            .unwrap();
    }
    repo.copy_messages_up_to(USER, "source", "fork", (BASE + 100, "first"))
        .await
        .unwrap();
    assert_eq!(recency(&repo, "fork").await, Some(BASE + 100));
    assert_eq!(recency(&repo, "source").await, Some(BASE + 300));

    // A raw import/correction changes the summary without needing an application writer.
    sqlx::query("UPDATE messages SET conversation_id = 'moved' WHERE id = 'last'")
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(recency(&repo, "source").await, Some(BASE + 100));
    assert_eq!(recency(&repo, "moved").await, Some(BASE + 300));
    sqlx::query("DELETE FROM messages WHERE id = 'last'")
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(recency(&repo, "moved").await, None);
    repo.delete_messages_by_conversation(USER, "fork").await.unwrap();
    assert_eq!(recency(&repo, "fork").await, None);
    assert_eq!(recency(&repo, "source").await, Some(BASE + 100));
    repo.delete(USER, "source").await.unwrap();
    assert!(repo.get(USER, "source").await.unwrap().is_none());
}

#[tokio::test]
async fn upgrade_backfills_once_preserves_messages_and_uses_the_partial_index() {
    let db = init_database_memory().await.unwrap();
    let pool = db.pool();
    // Reconstruct schema 46. The real migrator must then apply the new migration
    // once and skip it on the next startup; no old migration is edited.
    for sql in [
        "DROP TRIGGER messages_user_activity_insert",
        "DROP TRIGGER messages_user_activity_update",
        "DROP TRIGGER messages_user_activity_delete",
        "DROP INDEX idx_messages_user_activity",
        "ALTER TABLE conversations DROP COLUMN last_user_message_at",
        "DELETE FROM _sqlx_migrations WHERE version = 47",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }
    conversation(pool, "legacy").await;
    conversation(pool, "empty").await;
    for (id, kind, position, time) in [
        ("seconds", "text", Some("right"), BASE / 1000),
        ("millis", "user_content", None, BASE + 100),
        ("reply", "text", Some("left"), BASE + 500),
        ("tool", "tool_call", Some("left"), BASE + 600),
        ("goal", "acp_goal", Some("right"), BASE + 700),
        ("invalid", "user_content", Some("right"), 0),
    ] {
        sqlx::query("INSERT INTO messages (id,conversation_id,type,position,created_at) VALUES (?,'legacy',?,?,?)")
            .bind(id)
            .bind(kind)
            .bind(position)
            .bind(time)
            .execute(pool)
            .await
            .unwrap();
    }
    let before: Vec<(String, String, i64)> = sqlx::query_as("SELECT id,content,created_at FROM messages ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();
    let migrator = sqlx::migrate!();
    migrator.run(pool).await.unwrap();
    let repo = SqliteConversationRepository::new(pool.clone());
    assert_eq!(recency(&repo, "legacy").await, Some(BASE + 100));
    assert_eq!(recency(&repo, "empty").await, None);
    let after: Vec<(String, String, i64)> = sqlx::query_as("SELECT id,content,created_at FROM messages ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    let schema_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(pool)
        .await
        .unwrap();
    migrator.run(pool).await.unwrap();
    let second_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(schema_version, second_version);
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 47 AND success = 1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN SELECT CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END \
         FROM messages INDEXED BY idx_messages_user_activity WHERE conversation_id = 'legacy' \
         AND (position = 'right' OR type = 'user_content') AND created_at > 0 \
         AND type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal') \
         ORDER BY (CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END) DESC LIMIT 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>();
    assert!(
        plan.iter().any(|line| line.contains("idx_messages_user_activity")),
        "{plan:?}"
    );
    assert!(!plan.iter().any(|line| line.contains("TEMP B-TREE")), "{plan:?}");
}
