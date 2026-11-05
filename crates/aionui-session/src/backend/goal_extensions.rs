//! Goal adapters verified against the upstream Claude goal-extension.ts and
//! Qwen goal-protocol.ts/status.ts recorded in docs/goal-protocol-sources.md.
//! Only structured provider state is authoritative; command text is never state.
use super::BackendError;
use super::codex_goal::{GoalOperation, GoalStatus, SessionGoal, SessionGoalSnapshot};
use serde_json::{Value, json};

pub const GOAL_ACTIONS: [&str; 4] = ["set", "pause", "resume", "clear"];

#[derive(Debug, Clone)]
pub struct GoalExtension {
    pub method: String,
    pub actions: Vec<String>,
}

impl GoalExtension {
    /// Both the historical neutral namespace and the current opt-in AIR wire.
    pub fn discover(initialize: &Value) -> Option<Self> {
        let meta = initialize.get("_meta")?;
        let capability = meta.get("goal").or_else(|| meta.pointer("/jetbrains/air/goal"))?;
        if capability["version"].as_u64() != Some(1) {
            return None;
        }
        // Do not allow an untrusted advertisement to turn Goal into arbitrary RPC.
        let method = capability["controlMethod"].as_str()?;
        if method != "_session/goal" {
            return None;
        }
        let raw = capability["actions"].as_array()?;
        let mut actions = Vec::new();
        for action in raw {
            let action = action.as_str()?;
            if GOAL_ACTIONS.contains(&action) && !actions.iter().any(|a| a == action) {
                actions.push(action.to_owned());
            }
        }
        Some(Self {
            method: method.to_owned(),
            actions,
        })
    }

    pub fn request(&self, session: &str, operation: &GoalOperation) -> Result<Value, BackendError> {
        let (action, objective) = match operation {
            GoalOperation::Clear => ("clear", None),
            GoalOperation::Set {
                objective,
                status,
                token_budget,
            } => {
                if token_budget.is_some() {
                    return Err(unsupported());
                }
                match (objective, status.as_deref()) {
                    (Some(objective), None | Some("active")) => ("set", Some(objective)),
                    (None, Some("paused")) => ("pause", None),
                    (None, Some("active")) => ("resume", None),
                    _ => return Err(unsupported()),
                }
            }
            GoalOperation::Read => return Err(unsupported()),
        };
        if !self.actions.iter().any(|a| a == action) {
            return Err(unsupported());
        }
        let mut value = json!({"sessionId":session,"action":action});
        if let Some(objective) = objective {
            value["objective"] = json!(objective);
        }
        Ok(value)
    }
}

fn unsupported() -> BackendError {
    BackendError::CommandNotSupported { command: "goal action" }
}

#[derive(Debug, Default, Clone)]
pub struct GoalCache {
    pub snapshot: Option<SessionGoalSnapshot>,
    qwen_version: Option<(String, u64)>,
    qwen_updated_at: u64,
    qwen_cleared: bool,
    qwen_wire_sequence: u64,
}

impl GoalCache {
    pub fn store(&mut self, goal: Option<SessionGoal>, actions: Vec<String>) -> SessionGoalSnapshot {
        let clock = (aionui_common::now_ms().max(0) as u64).saturating_mul(1000);
        let revision = clock.max(self.snapshot.as_ref().map_or(0, |s| s.revision.saturating_add(1)));
        let snapshot = SessionGoalSnapshot {
            goal,
            revision,
            actions: Some(actions),
        };
        self.snapshot = Some(snapshot.clone());
        snapshot
    }

    pub fn standard(
        &mut self,
        value: &Value,
        session: &str,
        actions: Vec<String>,
    ) -> Result<SessionGoalSnapshot, BackendError> {
        let previous = self.snapshot.as_ref().and_then(|s| s.goal.as_ref());
        let goal = decode_goal(value, session, previous)?;
        Ok(self.store(goal, actions))
    }

    /// Qwen revisions are per Goal. Pair identity and revision when mutating,
    /// and reject older state (including a clear tombstone) after an RPC race.
    pub fn qwen(&mut self, value: &Value, session: &str) -> Result<Option<SessionGoalSnapshot>, BackendError> {
        if value["v"].as_u64() != Some(2) {
            return Err(invalid());
        }
        let wire_sequence = value["_aioncoreWireSequence"].as_u64().unwrap_or(0);
        if wire_sequence > 0 && wire_sequence < self.qwen_wire_sequence {
            return Ok(None);
        }
        let raw = value.get("goal").ok_or_else(invalid)?;
        let order = if raw.is_null() {
            value.get("clearedGoal")
        } else {
            Some(raw)
        };
        let next_order = if let Some(order) = order {
            let id = order["goalId"].as_str().ok_or_else(invalid)?;
            let revision = order["revision"].as_u64().ok_or_else(invalid)?;
            let updated_at = order["updatedAt"].as_u64().ok_or_else(invalid)?;
            if updated_at < self.qwen_updated_at
                || self.qwen_version.as_ref().is_some_and(|(old_id, old_rev)| {
                    old_id == id && (revision < *old_rev || self.qwen_cleared && !raw.is_null() && revision <= *old_rev)
                })
            {
                return Ok(None);
            }
            Some((id.to_owned(), revision, updated_at))
        } else if self.qwen_version.is_some() {
            // An unversioned empty response cannot overwrite a known Goal.
            return Ok(None);
        } else {
            None
        };
        let mut normalized = raw.clone();
        if !raw.is_null()
            && let Some(ms) = raw.get("activeTimeMs")
        {
            normalized["timeUsedSeconds"] = json!(ms.as_u64().ok_or_else(invalid)? / 1000);
        }
        let previous = self.snapshot.as_ref().and_then(|s| s.goal.as_ref());
        let goal = decode_goal(&normalized, session, previous)?;
        self.qwen_wire_sequence = wire_sequence.max(self.qwen_wire_sequence);
        if let Some((id, revision, updated_at)) = next_order {
            self.qwen_updated_at = updated_at;
            self.qwen_version = Some((id, revision));
            self.qwen_cleared = raw.is_null();
        }
        Ok(Some(self.store(goal, GOAL_ACTIONS.map(str::to_owned).to_vec())))
    }

    pub fn qwen_request(&self, session: &str, operation: &GoalOperation) -> Result<Value, BackendError> {
        let mut request = match operation {
            GoalOperation::Clear => json!({"action":"clear"}),
            GoalOperation::Set {
                objective,
                status,
                token_budget,
            } => {
                if token_budget.is_some() {
                    return Err(unsupported());
                }
                match (objective, status.as_deref()) {
                    (Some(objective), None | Some("active")) => {
                        json!({"action":if self.snapshot.as_ref().is_some_and(|s| s.goal.is_some()) {"replace"} else {"create"},"objective":objective})
                    }
                    (None, Some("paused")) => json!({"action":"pause"}),
                    (None, Some("active")) => json!({"action":"resume"}),
                    _ => return Err(unsupported()),
                }
            }
            GoalOperation::Read => return Err(unsupported()),
        };
        if request["action"] != "create" {
            let (id, revision) = self.qwen_version.as_ref().ok_or_else(unsupported)?;
            request["expectedGoalId"] = json!(id);
            request["expectedRevision"] = json!(revision);
        }
        Ok(json!({"sessionId":session,"request":request}))
    }
}

fn invalid() -> BackendError {
    BackendError::Transport("Invalid structured goal state".into())
}

/// ACP extension timestamps are milliseconds. The existing Core Goal API uses
/// seconds, preserving the verified Codex contract at that public boundary.
fn decode_goal(
    value: &Value,
    session: &str,
    previous: Option<&SessionGoal>,
) -> Result<Option<SessionGoal>, BackendError> {
    if value.is_null() {
        return Ok(None);
    }
    let objective = value["objective"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid)?;
    let status = match value["status"].as_str() {
        Some("active") => GoalStatus::Active,
        Some("paused") => GoalStatus::Paused,
        Some("blocked") => GoalStatus::Blocked,
        Some("limited") => GoalStatus::Limited,
        Some("usage_limited") => GoalStatus::UsageLimited,
        Some("budget_limited") => GoalStatus::BudgetLimited,
        Some("complete") => GoalStatus::Complete,
        _ => return Err(invalid()),
    };
    let now = aionui_common::now_ms() / 1000;
    let timestamp = |key: &str| -> Result<Option<i64>, BackendError> {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.as_i64().filter(|v| *v >= 0).map(|ms| ms / 1000).ok_or_else(invalid))
            .transpose()
    };
    let count = |key: &str| -> Result<Option<u64>, BackendError> {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.as_u64().ok_or_else(invalid))
            .transpose()
    };
    let created_at = timestamp("createdAt")?
        .or_else(|| previous.filter(|p| p.objective == objective).map(|p| p.created_at))
        .unwrap_or(now);
    Ok(Some(SessionGoal {
        thread_id: session.to_owned(),
        objective: objective.to_owned(),
        status,
        token_budget: count("tokenBudget")?,
        tokens_used: count("tokensUsed")?,
        time_used_seconds: count("timeUsedSeconds")?,
        created_at,
        updated_at: timestamp("updatedAt")?.unwrap_or(now),
    }))
}

/// Native Claude's SDKActiveGoalMessage, verified: upstream goal-extension.ts.
pub fn claude_goal(value: &Value) -> Result<Value, BackendError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let objective = value["condition"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid)?;
    let mut normalized = json!({"objective":objective.trim(),"status":"active"});
    if let Some(created_at) = value.get("set_at") {
        normalized["createdAt"] = created_at.clone();
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_gate_each_action_and_reject_arbitrary_methods() {
        for meta in [
            json!({"goal":{"version":1,"controlMethod":"_session/goal","actions":["set","clear"]}}),
            json!({"jetbrains":{"air":{"goal":{"version":1,"controlMethod":"_session/goal","actions":["set","clear"]}}}}),
        ] {
            let extension = GoalExtension::discover(&json!({"_meta":meta})).unwrap();
            assert_eq!(
                extension.request("s", &GoalOperation::Clear).unwrap()["action"],
                "clear"
            );
            assert!(
                extension
                    .request(
                        "s",
                        &GoalOperation::Set {
                            objective: None,
                            status: Some("paused".into()),
                            token_budget: None
                        }
                    )
                    .is_err()
            );
            assert!(
                extension
                    .request(
                        "s",
                        &GoalOperation::Set {
                            objective: None,
                            status: Some("active".into()),
                            token_budget: None
                        }
                    )
                    .is_err()
            );
            assert!(
                extension
                    .request(
                        "s",
                        &GoalOperation::Set {
                            objective: Some("test".into()),
                            status: None,
                            token_budget: Some(10)
                        }
                    )
                    .is_err()
            );
        }
        assert!(
            GoalExtension::discover(
                &json!({"_meta":{"goal":{"version":1,"controlMethod":"_shell/exec","actions":["set"]}}})
            )
            .is_none()
        );
        assert!(
            GoalExtension::discover(
                &json!({"_meta":{"goal":{"version":2,"controlMethod":"_session/goal","actions":[]}}})
            )
            .is_none()
        );
        assert!(GoalExtension::discover(&json!({})).is_none());
    }

    #[test]
    fn real_claude_capture_has_authoritative_set_and_clear_without_fake_metrics() {
        let mut cache = GoalCache::default();
        let actions = vec!["set".into(), "clear".into()];
        let mut objectives = Vec::new();
        for line in include_str!("claude_goal.capture.ndjson").lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            if line["direction"] != "out" || line["frame"]["type"] != "active_goal" {
                continue;
            }
            let value = claude_goal(&line["frame"]["value"]).unwrap();
            let state = cache.standard(&value, "session", actions.clone()).unwrap();
            if let Some(goal) = state.goal {
                assert_eq!(goal.tokens_used, None);
                assert_eq!(goal.time_used_seconds, None);
                assert_eq!(
                    goal.created_at,
                    line["frame"]["value"]["set_at"].as_i64().unwrap() / 1000
                );
                objectives.push(goal.objective);
            }
        }
        assert_eq!(objectives.len(), 1);
        assert!(cache.snapshot.unwrap().goal.is_none());
        assert!(claude_goal(&json!({"condition":42})).is_err());
    }

    #[test]
    fn qwen_version_guards_preserve_newer_events_and_clear_tombstones() {
        // Verified structure: Qwen goal-protocol.ts GoalSnapshotV2/GoalRecord.
        let mut cache = GoalCache::default();
        let state = json!({"v":2,"goal":{"goalId":"g","revision":1,"objective":"test","status":"active","tokensUsed":42,"activeTimeMs":79000,"createdAt":1793790978000u64,"updatedAt":1793790979000u64},"activity":"running"});
        let snapshot = cache.qwen(&state, "s").unwrap().unwrap();
        assert_eq!(snapshot.goal.as_ref().unwrap().time_used_seconds, Some(79));
        let request = cache
            .qwen_request(
                "s",
                &GoalOperation::Set {
                    objective: None,
                    status: Some("paused".into()),
                    token_budget: None,
                },
            )
            .unwrap();
        assert_eq!(
            request["request"],
            json!({"action":"pause","expectedGoalId":"g","expectedRevision":1})
        );
        let clear = json!({"v":2,"goal":null,"clearedGoal":{"goalId":"g","revision":2,"updatedAt":1793790980000u64},"activity":"idle"});
        let cleared = cache.qwen(&clear, "s").unwrap().unwrap();
        assert!(cleared.goal.is_none());
        assert!(cleared.revision > snapshot.revision);
        assert!(cache.qwen(&state, "s").unwrap().is_none());
        assert!(cache.snapshot.as_ref().unwrap().goal.is_none());
        assert_eq!(
            cache
                .qwen_request(
                    "s",
                    &GoalOperation::Set {
                        objective: Some("next".into()),
                        status: Some("active".into()),
                        token_budget: None
                    }
                )
                .unwrap()["request"],
            json!({"action":"create","objective":"next"})
        );
        assert!(cache.qwen(&json!({"v":3,"goal":null}), "s").is_err());
        let mut unknown = state;
        unknown["goal"]["status"] = json!("unknown");
        let mut fresh = GoalCache::default();
        assert!(fresh.qwen(&unknown, "s").is_err());
    }

    #[test]
    fn real_qwen_capture_controls_and_same_revision_clear_remain_authoritative() {
        let mut cache = GoalCache::default();
        let mut last_active = None;
        let mut requests = Vec::new();
        for line in include_str!("qwen_goal.capture.ndjson").lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            let frame = &line["frame"];
            if line["direction"] == "in" && frame["method"] == "qwen/control/session/goal/control" {
                requests.push(frame["params"]["request"]["action"].as_str().unwrap().to_owned());
            }
            if line["direction"] != "out" {
                continue;
            }
            let value = frame
                .pointer("/result/snapshot")
                .or_else(|| frame.pointer("/params/update/_meta/goalState"));
            if let Some(value) = value {
                cache.qwen(value, "s").unwrap();
                if value["goal"]["status"] == "active" {
                    last_active = Some(value.clone());
                }
            }
        }
        assert_eq!(requests, ["create", "pause", "resume", "clear"]);
        assert!(cache.snapshot.as_ref().unwrap().goal.is_none());
        let last_active = last_active.unwrap();
        // The real clear keeps the SAME revision and updatedAt as this snapshot.
        assert!(cache.qwen(&last_active, "s").unwrap().is_none());
        assert!(cache.snapshot.unwrap().goal.is_none());
    }
}
