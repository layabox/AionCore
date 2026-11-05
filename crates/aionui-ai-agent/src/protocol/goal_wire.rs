//! Stamp Goal-only state at the ordered transport boundary. SDK notifications
//! and RPC receipts are applied by different tasks; their processing order is
//! not the provider's wire order. This private field never leaves the codec.
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn stamp(line: String, sequence: &AtomicU64) -> String {
    if !line.contains("\"goalState\"") && !line.contains("\"snapshot\"") {
        return line;
    }
    let Ok(mut frame) = serde_json::from_str::<Value>(&line) else {
        return line;
    };
    let state = if frame["method"] == "session/update" {
        frame.pointer_mut("/params/update/_meta/goalState")
    } else {
        frame.pointer_mut("/result/snapshot")
    };
    if let Some(state) = state
        && state["v"].as_u64() == Some(2)
        && state.is_object()
    {
        state["_aioncoreWireSequence"] = json!(sequence.fetch_add(1, Ordering::SeqCst) + 1);
        serde_json::to_string(&frame).unwrap_or(line)
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn goal_wire_order_survives_delayed_rpc_application() {
        let sequence = AtomicU64::new(0);
        let mut cache = aionui_session::GoalCache::default();
        let first = json!({"v":2,"goal":{"goalId":"g","revision":1,"objective":"test","status":"active","createdAt":1000,"updatedAt":2000},"activity":"running"});
        let early: Value = serde_json::from_str(&stamp(
            json!({"id":1,"result":{"snapshot":first}}).to_string(),
            &sequence,
        ))
        .unwrap();
        let late:Value = serde_json::from_str(&stamp(json!({"method":"session/update","params":{"sessionId":"s","update":{"_meta":{"goalState":{"v":2,"goal":null,"activity":"idle","clearedGoal":{"goalId":"g","revision":1,"updatedAt":2000}}}}}}).to_string(), &sequence)).unwrap();
        cache
            .qwen(&late["params"]["update"]["_meta"]["goalState"], "s")
            .unwrap();
        assert!(cache.qwen(&early["result"]["snapshot"], "s").unwrap().is_none());
        assert!(cache.snapshot.unwrap().goal.is_none());
        assert_eq!(stamp("not-json".into(), &sequence), "not-json");
    }
}
