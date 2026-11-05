//! Persistent objectives are independent from turns. Only explicit mutations ensure a runtime.
use crate::{ConversationError, service::ConversationService};
use aionui_ai_agent::GoalOperation;
use aionui_api_types::{ConversationGoalSnapshot, SetConversationGoalRequest, WebSocketMessage};
use aionui_common::now_ms;
use aionui_db::{IConversationRepository, models::MessageRow};
use std::sync::Arc;

static GOAL_PERSIST_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn publish_goal(
    repo: &Arc<dyn IConversationRepository>,
    bus: &Arc<dyn aionui_realtime::EventBroadcaster>,
    user_id: &str,
    conversation_id: &str,
    snapshot: &ConversationGoalSnapshot,
) -> Result<(), ConversationError> {
    let _guard = GOAL_PERSIST_GATE.lock().await;
    let existing = repo
        .get_message_by_msg_id(user_id, conversation_id, &format!("{conversation_id}:goal"), "acp_goal")
        .await?;
    if let Some(existing) = existing {
        let previous: ConversationGoalSnapshot = serde_json::from_str(&existing.content)
            .map_err(|_| ConversationError::internal("Invalid persisted goal snapshot"))?;
        if previous.revision >= snapshot.revision {
            return Ok(());
        }
    }
    let row = MessageRow {
        id: format!("{conversation_id}:goal"),
        conversation_id: conversation_id.to_owned(),
        msg_id: Some(format!("{conversation_id}:goal")),
        r#type: "acp_goal".into(),
        content: serde_json::to_string(snapshot).map_err(|_| ConversationError::internal("Invalid goal snapshot"))?,
        position: Some("center".into()),
        status: Some("finish".into()),
        hidden: true,
        created_at: now_ms(),
        backend_turn_id: None,
    };
    repo.upsert_message(user_id, &row).await?;
    bus.broadcast(WebSocketMessage::new(
        "conversation.goalChanged",
        serde_json::json!({
            "conversation_id": conversation_id, "user_id": user_id,
            "goal": snapshot.goal, "revision": snapshot.revision, "actions": snapshot.actions,
        }),
    ));
    Ok(())
}

impl ConversationService {
    pub async fn get_goal(&self, user_id: &str, id: &str) -> Result<ConversationGoalSnapshot, ConversationError> {
        self.ensure_owned_conversation(user_id, id).await?;
        // An existing backend exposes a cache; this performs no RPC and never wakes Codex.
        if let Ok(task) = self.task(id)
            && let Some(snapshot) = task.goal_snapshot()
        {
            publish_goal(self.conversation_repo(), self.broadcaster(), user_id, id, &snapshot).await?;
        }
        // Reopening history never creates a task.
        let mut snapshot = match self
            .conversation_repo()
            .get_message_by_msg_id(user_id, id, &format!("{id}:goal"), "acp_goal")
            .await?
        {
            Some(row) => serde_json::from_str(&row.content)
                .map_err(|_| ConversationError::internal("Invalid persisted goal snapshot"))?,
            None => ConversationGoalSnapshot::default(),
        };
        // Capability discovery is read-only and does not imply an empty Goal.
        // In particular, never clear a persisted objective just to attach actions.
        if let Ok(task) = self.task(id)
            && let Some(actions) = task.goal_actions()
        {
            snapshot.actions = Some(actions);
        }
        Ok(snapshot)
    }

    pub async fn set_goal(
        &self,
        user_id: &str,
        id: &str,
        req: SetConversationGoalRequest,
    ) -> Result<ConversationGoalSnapshot, ConversationError> {
        if req
            .objective
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 100_000)
            || req.token_budget == Some(0)
            || (req.objective.is_none() && req.status.is_none() && req.token_budget.is_none())
        {
            return Err(ConversationError::BadRequest {
                reason: "Invalid goal objective, status or token budget".into(),
            });
        }
        self.ensure_owned_conversation(user_id, id).await?;
        self.ensure_runtime(user_id, id, self.task_manager()).await?;
        let status = req.status.map(|s| {
            match s {
                aionui_api_types::GoalUserStatus::Active => "active",
                aionui_api_types::GoalUserStatus::Paused => "paused",
            }
            .to_owned()
        });
        let snapshot = self
            .task(id)?
            .goal_operation(GoalOperation::Set {
                objective: req.objective,
                status,
                token_budget: req.token_budget,
            })
            .await?;
        publish_goal(self.conversation_repo(), self.broadcaster(), user_id, id, &snapshot).await?;
        Ok(snapshot)
    }

    pub async fn clear_goal(&self, user_id: &str, id: &str) -> Result<ConversationGoalSnapshot, ConversationError> {
        self.ensure_owned_conversation(user_id, id).await?;
        self.ensure_runtime(user_id, id, self.task_manager()).await?;
        let snapshot = self.task(id)?.goal_operation(GoalOperation::Clear).await?;
        publish_goal(self.conversation_repo(), self.broadcaster(), user_id, id, &snapshot).await?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod goal_tests {
    use super::*;
    use aionui_db::{SqliteConversationRepository, init_database_memory, models::ConversationRow};
    use aionui_realtime::{BroadcastEventBus, EventBroadcaster};
    use serde_json::json;

    #[tokio::test]
    async fn goal_snapshot_persists_tombstone_and_owner_event_without_regression() {
        let db = init_database_memory().await.unwrap();
        let repo: Arc<dyn IConversationRepository> = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
        let user = "system_default_user";
        let row = ConversationRow {
            id: "goal-test".into(),
            user_id: user.into(),
            name: "Goal test".into(),
            r#type: "acp".into(),
            extra: "{}".into(),
            model: None,
            status: Some("pending".into()),
            source: Some("aionui".into()),
            channel_chat_id: None,
            pinned: false,
            pinned_at: None,
            created_at: 1,
            updated_at: 1,
            project_id: None,
            folder_id: None,
            name_source: None,
        };
        repo.create(&row).await.unwrap();
        let event_bus = Arc::new(BroadcastEventBus::new(8));
        let mut rx = event_bus.subscribe();
        let bus: Arc<dyn EventBroadcaster> = event_bus;
        let snapshot: ConversationGoalSnapshot = serde_json::from_value(json!({"revision":100,"goal":{
            "objective":"Goal persistence test","status":"paused","tokenBudget":1000,"tokensUsed":250,
            "timeUsedSeconds":10,"createdAt":1793790978,"updatedAt":1793790978
        }}))
        .unwrap();
        publish_goal(&repo, &bus, user, &row.id, &snapshot).await.unwrap();
        let event = rx.recv().await.unwrap();
        assert_eq!(event.name, "conversation.goalChanged");
        assert_eq!(event.data["user_id"], user);
        assert_eq!(event.data["conversation_id"], row.id);
        assert!(event.data["goal"].get("threadId").is_none());
        let cleared = ConversationGoalSnapshot {
            actions: None,
            goal: None,
            revision: 101,
        };
        publish_goal(&repo, &bus, user, &row.id, &cleared).await.unwrap();
        assert!(rx.recv().await.unwrap().data["goal"].is_null());
        publish_goal(&repo, &bus, user, &row.id, &snapshot).await.unwrap();
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let saved = repo
            .get_message_by_msg_id(user, &row.id, "goal-test:goal", "acp_goal")
            .await
            .unwrap()
            .unwrap();
        assert!(saved.hidden);
        let restored: ConversationGoalSnapshot = serde_json::from_str(&saved.content).unwrap();
        assert!(restored.goal.is_none());
        assert_eq!(restored.revision, 101);
        assert!(
            repo.get_message_by_msg_id("different-user", &row.id, "goal-test:goal", "acp_goal")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            publish_goal(
                &repo,
                &bus,
                "different-user",
                &row.id,
                &ConversationGoalSnapshot {
                    actions: None,
                    goal: None,
                    revision: 102
                }
            )
            .await
            .is_err()
        );
    }
}
