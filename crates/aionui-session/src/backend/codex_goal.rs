//! Native Goal RPC contract, verified against codex-cli 0.161.0's own
//! `app-server generate-json-schema --experimental` (codex_goal.schema.json).
use super::BackendError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum GoalOperation {
    Read,
    Set {
        objective: Option<String>,
        status: Option<String>,
        token_budget: Option<u64>,
    },
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGoal {
    pub thread_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGoalSnapshot {
    pub revision: u64,
    pub goal: Option<SessionGoal>,
}

pub(super) fn snapshot(value: &Value, thread_id: &str, revision: u64) -> Result<SessionGoalSnapshot, BackendError> {
    let goal = match value.get("goal") {
        Some(Value::Null) | None if value.get("cleared").and_then(Value::as_bool).is_some() => None,
        Some(Value::Null) => None,
        Some(goal) => Some(
            serde_json::from_value::<SessionGoal>(goal.clone())
                .map_err(|_| BackendError::Transport("Invalid Codex goal snapshot".into()))?,
        ),
        None => return Err(BackendError::Transport("Missing Codex goal snapshot".into())),
    };
    if goal.as_ref().is_some_and(|goal| goal.thread_id != thread_id) {
        return Err(BackendError::Transport(
            "Codex goal belongs to a different thread".into(),
        ));
    }
    Ok(SessionGoalSnapshot { goal, revision })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_goal_capture_validates_receipts_and_clear() {
        let frames: Vec<Value> = include_str!("codex_goal.capture.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect();
        let replies: Vec<&Value> = frames
            .iter()
            .filter(|line| line["direction"] == "out")
            .map(|line| &line["frame"])
            .collect();
        let set = replies.iter().find(|line| line["id"] == 3).unwrap();
        let thread = set["result"]["goal"]["threadId"].as_str().unwrap();
        let parsed = snapshot(&set["result"], thread, 1).unwrap();
        assert_eq!(parsed.goal.as_ref().unwrap().tokens_used, 0);
        assert_eq!(parsed.goal.as_ref().unwrap().token_budget, Some(1000));
        assert!(matches!(parsed.goal.unwrap().status, GoalStatus::Paused));
        assert!(snapshot(&set["result"], "wrong-thread", 1).is_err());
        for id in [5, 6] {
            let reply = replies.iter().find(|line| line["id"] == id).unwrap();
            assert!(snapshot(&reply["result"], thread, id).unwrap().goal.is_none());
        }
        assert!(snapshot(&serde_json::json!({}), thread, 1).is_err());
        let mut malformed = set["result"].clone();
        malformed["goal"]["status"] = serde_json::json!("unknown");
        assert!(snapshot(&malformed, thread, 1).is_err());
    }
}
