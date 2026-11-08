//! Native remembered approvals. Verified against Anthropic's SDK PermissionUpdate
//! types and the official claude-agent-acp permission normalization implementation;
//! exact sources and captured frames: docs/protocol-schemas/claude-2.1.267/README.md.

use serde_json::Value;

fn safe_text(value: &Value, limit: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= limit && !text.chars().any(|ch| ch.is_control()))
}

fn valid_update(update: &Value) -> bool {
    if !matches!(
        update.get("destination").and_then(Value::as_str),
        Some("session" | "cliArg" | "userSettings" | "projectSettings" | "localSettings")
    ) {
        return false;
    }
    match update.get("type").and_then(Value::as_str) {
        // Remember this native grant; never turn an ordinary approval into a
        // permission-mode change or replacement/removal of existing policy.
        Some("addRules") if update.get("behavior").and_then(Value::as_str) == Some("allow") => {
            update.get("rules").and_then(Value::as_array).is_some_and(|rules| {
                !rules.is_empty()
                    && rules.len() <= 100
                    && rules.iter().all(|rule| {
                        rule.get("toolName").is_some_and(|value| safe_text(value, 512))
                            && rule.get("ruleContent").is_none_or(|value| safe_text(value, 10_000))
                    })
            })
        }
        Some("addDirectories") => update
            .get("directories")
            .and_then(Value::as_array)
            .is_some_and(|paths| {
                !paths.is_empty() && paths.len() <= 100 && paths.iter().all(|value| safe_text(value, 4096))
            }),
        _ => false,
    }
}

/// Snapshot only native additive allow suggestions, keeping their exact scope.
/// A malformed bundle is rejected as a whole; never infer rules from tool input.
pub(super) fn remembered_permission_updates(request: &Value) -> Option<Value> {
    // Verified raw field: @anthropic-ai/claude-agent-sdk 0.3.267 sdk.mjs.
    if request.get("matched_ask_rule").is_some_and(|value| !value.is_null()) {
        return None;
    }
    // Verified raw field: official SDK 0.3.268 sdk.mjs.
    if request.get("suppress_always_allow_rule").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if request.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion") {
        return None;
    }
    let updates = request.get("permission_suggestions")?.as_array()?;
    if updates.is_empty() || updates.len() > 100 || !updates.iter().all(valid_update) {
        return None;
    }
    let size: usize = updates
        .iter()
        .map(|update| {
            1 + update
                .get("rules")
                .or_else(|| update.get("directories"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    (size <= 4096).then(|| Value::Array(updates.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_remembered_approval_preserves_native_scope() {
        let request = json!({"tool_name":"Bash", "permission_suggestions":[{
            "type":"addRules", "rules":[{"toolName":"Bash","ruleContent":"printf:*"}],
            "behavior":"allow", "destination":"localSettings"
        }]});
        assert_eq!(
            remembered_permission_updates(&request),
            Some(request["permission_suggestions"].clone())
        );
        let directories = json!({"tool_name":"Read", "permission_suggestions":[{
            "type":"addDirectories", "directories":["/tmp/approved"], "destination":"session"
        }]});
        assert_eq!(
            remembered_permission_updates(&directories),
            Some(directories["permission_suggestions"].clone())
        );
    }

    #[test]
    fn claude_remembered_approval_rejects_invalid_or_broader_bundles() {
        let valid = json!({"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"allow","destination":"session"});
        let mut invalid = valid.clone();
        invalid["rules"][0]["toolName"] = json!("");
        for updates in [
            json!([]),
            json!({}),
            json!([invalid]),
            json!([valid.clone(), {"type":"setMode","mode":"bypassPermissions","destination":"session"}]),
            json!([{"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"deny","destination":"session"}]),
            json!([{"type":"addDirectories","directories":["bad\u{0}path"],"destination":"session"}]),
            json!([{"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"allow","destination":"unknown"}]),
            Value::Array(vec![valid.clone(); 101]),
        ] {
            assert_eq!(
                remembered_permission_updates(&json!({"tool_name":"Bash","permission_suggestions":updates})),
                None
            );
        }
        assert_eq!(remembered_permission_updates(&json!({"tool_name":"Bash"})), None);
        assert_eq!(
            remembered_permission_updates(&json!({"tool_name":"AskUserQuestion","permission_suggestions":[valid]})),
            None
        );
    }

    #[test]
    fn claude_remembered_approval_respects_forced_ask_and_suppression() {
        let mut request = json!({"tool_name":"Bash","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash"}],"behavior":"allow","destination":"session"}]});
        request["matched_ask_rule"] = json!({"source":"userSettings","tool_name":"Bash"});
        assert_eq!(remembered_permission_updates(&request), None);
        request.as_object_mut().unwrap().remove("matched_ask_rule");
        request["suppress_always_allow_rule"] = json!(true);
        assert_eq!(remembered_permission_updates(&request), None);
        request["suppress_always_allow_rule"] = json!(false);
        assert!(remembered_permission_updates(&request).is_some());
    }
}
