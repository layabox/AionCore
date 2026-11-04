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
            "goal": snapshot.goal, "revision": snapshot.revision,
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
        match self
            .conversation_repo()
            .get_message_by_msg_id(user_id, id, &format!("{id}:goal"), "acp_goal")
            .await?
        {
            Some(row) => serde_json::from_str(&row.content)
                .map_err(|_| ConversationError::internal("Invalid persisted goal snapshot")),
            None => Ok(ConversationGoalSnapshot::default()),
        }
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
