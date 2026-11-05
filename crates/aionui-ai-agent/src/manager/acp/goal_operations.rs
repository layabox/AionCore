//! ACP Goal capability/control bridge. Source contracts are recorded in
//! docs/goal-protocol-sources.md; Qwen is a separate, versioned translation.
use super::AcpAgentManager;
use crate::error::AgentError;
use crate::protocol::events::AgentStreamEvent;
use aionui_api_types::ConversationGoalSnapshot;
use aionui_session::{GoalExtension, GoalOperation, SessionGoalSnapshot};
use serde_json::json;

// Qwen's real 0.26.7 wire does NOT prefix these private methods with '_'.
// Verified: backend/qwen_goal.capture.ndjson. An underscore returns -32601.
const QWEN_READ: &str = "qwen/control/session/goal/get";
const QWEN_CONTROL: &str = "qwen/control/session/goal/control";

fn public_snapshot(snapshot: SessionGoalSnapshot) -> Result<ConversationGoalSnapshot, AgentError> {
    serde_json::from_value(serde_json::to_value(snapshot).map_err(|_| AgentError::internal("Invalid Goal state"))?)
        .map_err(|_| AgentError::internal("Invalid Goal state"))
}

impl AcpAgentManager {
    pub fn supports_goal_stream(&self) -> bool {
        self.backend() == Some("qwen") || self.goal_extension().is_some()
    }

    pub fn goal_actions(&self) -> Option<Vec<String>> {
        self.goal_extension()
            .map(|extension| extension.actions)
            .or_else(|| self.goal_snapshot().and_then(|snapshot| snapshot.actions))
    }

    fn apply_qwen_goal_activity(&self, value: &serde_json::Value, session: &str) {
        // A regular session/prompt owns its own Finish. Only autonomous Goal
        // turns outside that lock use the structured v2 activity boundary.
        let Ok(_session_guard) = self.session_lock.try_lock() else {
            return;
        };
        match value["activity"].as_str() {
            Some("running" | "verifying") => {
                self.goal_running.store(true, std::sync::atomic::Ordering::SeqCst);
                self.runtime
                    .reset_for_new_turn(aionui_common::ConversationStatus::Running);
                self.runtime.bump_activity();
            }
            Some("idle") if self.goal_running.swap(false, std::sync::atomic::Ordering::SeqCst) => {
                self.runtime.emit_finish(Some(session.to_owned()));
            }
            _ => {}
        }
    }

    fn goal_extension(&self) -> Option<GoalExtension> {
        self.protocol
            .initialize_response()
            .and_then(|v| serde_json::to_value(v).ok())
            .and_then(|v| GoalExtension::discover(&v))
    }

    pub fn goal_snapshot(&self) -> Option<ConversationGoalSnapshot> {
        self.goal_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
            .and_then(|s| public_snapshot(s).ok())
    }

    fn publish_goal_snapshot(&self, snapshot: SessionGoalSnapshot) -> Result<ConversationGoalSnapshot, AgentError> {
        let snapshot = public_snapshot(snapshot)?;
        self.runtime.bump_activity();
        self.runtime.emit(AgentStreamEvent::GoalChanged(snapshot.clone()));
        Ok(snapshot)
    }

    pub(super) async fn apply_goal_notification(
        &self,
        notification: &agent_client_protocol::schema::v1::SessionNotification,
    ) {
        let Some(session) = self.session_id().await else {
            return;
        };
        if notification.session_id.to_string() != session {
            return;
        }
        let Ok(value) = serde_json::to_value(&notification.update) else {
            return;
        };
        let Some(meta) = value.get("_meta") else {
            return;
        };
        let qwen = self.backend() == Some("qwen");
        let snapshot = if qwen {
            let Some(value) = meta.get("goalState") else {
                return;
            };
            self.goal_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .qwen(value, &session)
        } else {
            let Some(extension) = self.goal_extension() else {
                return;
            };
            let Some(value) = meta.get("goal").or_else(|| meta.pointer("/jetbrains/air/goal")) else {
                return;
            };
            self.goal_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .standard(value, &session, extension.actions)
                .map(Some)
        };
        match snapshot {
            Ok(Some(snapshot)) => {
                let _ = self.publish_goal_snapshot(snapshot);
                if qwen {
                    self.apply_qwen_goal_activity(&meta["goalState"], &session);
                }
            }
            Ok(None) => {}
            Err(_) => tracing::warn!(conversation_id = %self.params.conversation_id, "Invalid ACP Goal notification"),
        }
    }

    async fn read_qwen_goal(&self, session: &str) -> Result<ConversationGoalSnapshot, AgentError> {
        let response = self.protocol.goal_rpc(QWEN_READ, json!({"sessionId":session})).await?;
        let value = response
            .get("snapshot")
            .ok_or_else(|| AgentError::bad_request("Qwen does not expose structured Goal state"))?;
        let snapshot = self
            .goal_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .qwen(value, session)
            .map_err(|e| AgentError::bad_request(e.to_string()))?;
        match snapshot {
            Some(snapshot) => self.publish_goal_snapshot(snapshot),
            None => self
                .goal_snapshot()
                .ok_or_else(|| AgentError::bad_request("Missing Qwen Goal state")),
        }
    }

    pub async fn goal_operation(&self, operation: GoalOperation) -> Result<ConversationGoalSnapshot, AgentError> {
        // No process/session creation, extension RPC, or wake on a historical read.
        if matches!(operation, GoalOperation::Read) {
            return self
                .goal_snapshot()
                .ok_or_else(|| AgentError::bad_request("Goal state is unavailable"));
        }
        let _guard = self.goal_operation_lock.lock().await;
        let qwen = self.backend() == Some("qwen");
        // Kiro and unknown providers are never guessed into a Goal implementation.
        let extension = self.goal_extension();
        if !qwen && extension.is_none() {
            return Err(AgentError::bad_request("Goals are not supported by this agent"));
        }
        let session = self.ensure_session_opened().await?;
        if qwen {
            self.read_qwen_goal(&session).await?;
            let request = self
                .goal_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .qwen_request(&session, &operation)
                .map_err(|e| AgentError::bad_request(e.to_string()))?;
            let response = self.protocol.goal_rpc(QWEN_CONTROL, request).await?;
            let value = response
                .get("snapshot")
                .ok_or_else(|| AgentError::bad_request("Invalid Qwen Goal receipt"))?;
            let snapshot = self
                .goal_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .qwen(value, &session)
                .map_err(|e| AgentError::bad_request(e.to_string()))?;
            return match snapshot {
                Some(snapshot) => self.publish_goal_snapshot(snapshot),
                None => self
                    .goal_snapshot()
                    .ok_or_else(|| AgentError::bad_request("Missing Qwen Goal receipt")),
            };
        }
        let extension = extension.ok_or_else(|| AgentError::bad_request("Missing Goal capability"))?;
        let request = extension
            .request(&session, &operation)
            .map_err(|e| AgentError::bad_request(e.to_string()))?;
        let revision = self.goal_snapshot().map_or(0, |s| s.revision);
        // Some adapters keep the RPC open for the native Goal turn. Do not cancel
        // it when the structured state confirms the mutation ahead of the reply.
        let mut receipt = self.protocol.start_goal_rpc(&extension.method, request)?;
        let mut acknowledged = false;
        for _ in 0..600 {
            if !acknowledged {
                match receipt.try_recv() {
                    Ok(Ok(_)) => acknowledged = true,
                    Ok(Err(error)) => return Err(error.into()),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        return Err(AgentError::bad_request("Goal operation disconnected"));
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                }
            }
            if let Some(snapshot) = self.goal_snapshot()
                && snapshot.revision > revision
            {
                let matches = match (&operation, &snapshot.goal) {
                    (GoalOperation::Clear, None) => true,
                    (GoalOperation::Set { objective, status, .. }, Some(goal)) => {
                        objective.as_ref().is_none_or(|o| goal.objective.trim() == o.trim())
                            && status.as_deref().is_none_or(|s| match s {
                                "active" => matches!(goal.status, aionui_api_types::ConversationGoalStatus::Active),
                                "paused" => matches!(goal.status, aionui_api_types::ConversationGoalStatus::Paused),
                                _ => false,
                            })
                    }
                    _ => false,
                };
                if matches {
                    return Ok(snapshot);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Err(AgentError::bad_request("Agent did not confirm the Goal operation"))
    }
}
