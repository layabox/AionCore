//! Native feedback missing from ACP. Verified: OpenCode v1.3.13
//! cli/cmd/acp.ts, cli/network.ts and session/status.ts; pi-acp a393c743
//! src/acp/agent.ts and Pi v1.5.1 core/extensions/types.ts.
use crate::error::AgentError;
use crate::factory::acp_assembler::AcpSessionParams;
use crate::protocol::events::{TipType, TipsEventData};
use aionui_common::{CommandSpec, EnvVar};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
pub(super) struct NativeFeedback {
    opencode: Option<OpenCodeStatus>,
    _pi: Option<PiFeedbackFiles>,
}
struct OpenCodeStatus {
    client: reqwest::Client,
    url: String,
    directory: String,
    username: String,
    password: Option<String>,
}
struct PiFeedbackFiles(PathBuf);
impl Drop for PiFeedbackFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command_env(command: &CommandSpec, key: &str) -> Option<String> {
    command
        .env
        .iter()
        .rev()
        .find(|entry| entry.name == key)
        .map(|entry| entry.value.clone())
        .or_else(|| std::env::var(key).ok())
}
impl NativeFeedback {
    pub(super) fn prepare(params: &AcpSessionParams) -> Result<(CommandSpec, Self), AgentError> {
        let mut command = params.command_spec.clone();
        let mut feedback = Self::default();
        match params.metadata.backend.as_deref() {
            Some("opencode") => {
                // Never query a shared/default port. Explicit operator network flags
                // are preserved; unsupported/custom launches keep generic feedback.
                if command.args.iter().any(|arg| {
                    ["--port", "--hostname", "--mdns"]
                        .iter()
                        .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
                }) {
                    tracing::warn!(conversation_id = %params.conversation_id, "Native OpenCode feedback unavailable with explicit network arguments");
                    return Ok((command, feedback));
                }
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .map_err(|_| AgentError::internal("Cannot allocate the OpenCode feedback listener"))?;
                let port = listener
                    .local_addr()
                    .map_err(|_| AgentError::internal("Cannot inspect the OpenCode feedback listener"))?
                    .port();
                command.args.extend([
                    "--hostname".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                    "--mdns".into(),
                    "false".into(),
                ]);
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_millis(400))
                    .timeout(Duration::from_millis(900))
                    .build()
                    .map_err(|_| AgentError::internal("Cannot initialize native Agent feedback"))?;
                feedback.opencode = Some(OpenCodeStatus {
                    client,
                    url: format!("http://127.0.0.1:{port}/session/status"),
                    directory: params.workspace.path.clone(),
                    username: command_env(&command, "OPENCODE_SERVER_USERNAME").unwrap_or_else(|| "opencode".into()),
                    password: command_env(&command, "OPENCODE_SERVER_PASSWORD"),
                });
                // Bun binds this exact port; failure uses the normal startup error
                // path rather than querying a different server.
                drop(listener);
            }
            Some("pi")
                if command.args.iter().any(|arg| {
                    arg == "@jhillyerd/pi-acp@0.1.0"
                        || arg.replace('\\', "/").ends_with("/@jhillyerd/pi-acp/dist/index.js")
                }) =>
            {
                let native = command_env(&command, "PI_ACP_PI_COMMAND")
                    .unwrap_or_else(|| if cfg!(windows) { "pi.cmd".into() } else { "pi".into() });
                if let Some(native) = aionui_runtime::resolve_command_path(&native) {
                    let directory = params
                        .data_dir
                        .join("runtime")
                        .join("pi-feedback")
                        .join(uuid::Uuid::new_v4().to_string());
                    std::fs::create_dir_all(&directory)
                        .map_err(|_| AgentError::internal("Cannot prepare Pi feedback extension"))?;
                    let files = PiFeedbackFiles(directory);
                    let extension = files.0.join("request-feedback.mjs");
                    std::fs::write(&extension, include_str!("pi_request_feedback.mjs"))
                        .map_err(|_| AgentError::internal("Cannot write Pi feedback extension"))?;
                    let launcher = files.0.join(if cfg!(windows) { "pi.cmd" } else { "pi" });
                    std::fs::write(
                        &launcher,
                        pi_launcher(&native.to_string_lossy(), &extension.to_string_lossy()),
                    )
                    .map_err(|_| AgentError::internal("Cannot write Pi feedback launcher"))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
                            .map_err(|_| AgentError::internal("Cannot enable Pi feedback launcher"))?;
                    }
                    command.env.retain(|entry| entry.name != "PI_ACP_PI_COMMAND");
                    command.env.push(EnvVar {
                        name: "PI_ACP_PI_COMMAND".into(),
                        value: launcher.to_string_lossy().into_owned(),
                    });
                    feedback._pi = Some(files);
                }
            }
            _ => {}
        }
        if feedback.opencode.is_some() || feedback._pi.is_some() {
            tracing::info!(conversation_id = %params.conversation_id, backend = ?params.metadata.backend, "Native Agent feedback enabled");
        }
        Ok((command, feedback))
    }
    pub(super) async fn retry_notice(&self, session_id: &str, key: &str) -> Option<Option<TipsEventData>> {
        let source = self.opencode.as_ref()?;
        let mut request = source
            .client
            .get(&source.url)
            .query(&[("directory", &source.directory)]);
        if let Some(password) = &source.password {
            request = request.basic_auth(&source.username, Some(password));
        }
        let mut response = request.send().await.ok()?.error_for_status().ok()?;
        if response.content_length().is_some_and(|size| size > 65536) {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len() + chunk.len() > 65536 {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&body).ok()?;
        let status = value.get(session_id)?;
        match status.get("type")?.as_str()? {
            "retry" => retry_notice_from_status(status, key).map(Some),
            "busy" | "idle" => Some(None),
            _ => None,
        }
    }
}
fn pi_launcher(native: &str, extension: &str) -> String {
    if cfg!(windows) {
        format!(
            "@echo off\r\n\"{}\" --extension \"{}\" %*\r\n",
            native.replace('%', "%%"),
            extension.replace('%', "%%")
        )
    } else {
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        format!(
            "#!/bin/sh\nexec {} --extension {} \"$@\"\n",
            quote(native),
            quote(extension)
        )
    }
}
fn retry_notice_from_status(status: &Value, key: &str) -> Option<TipsEventData> {
    if status.get("type")?.as_str()? != "retry" {
        return None;
    }
    let attempt = status.get("attempt")?.as_u64()?;
    let next = status.get("next")?.as_u64()?;
    if attempt == 0 || next > 8_640_000_000_000_000 {
        return None;
    }
    let raw = status.get("message")?.as_str()?.to_ascii_lowercase();
    // Raw provider errors can contain request bodies/credentials. Only safe
    // public categories leave this native boundary or enter persisted notices.
    let (reason_code, reason) = if raw.contains("free usage exceeded") {
        ("free_quota", "The model's free usage quota has been exceeded")
    } else if ["quota", "credit", "billing", "balance"]
        .iter()
        .any(|part| raw.contains(part))
    {
        ("quota", "The model provider reports insufficient quota or credits")
    } else if raw.contains("rate limit") || raw.contains("429") {
        ("rate_limit", "The model provider is rate limiting this request")
    } else if raw.contains("overload") || raw.contains("503") {
        ("unavailable", "The model provider is temporarily unavailable")
    } else {
        ("unknown", "The model request failed and the Agent scheduled a retry")
    };
    Some(TipsEventData {
        content: format!(
            "{reason}. Retry attempt {attempt}; next retry at Unix timestamp {next} ms. You can wait or cancel the request."
        ),
        tip_type: TipType::Warning,
        code: Some("ACP_NATIVE_RETRY".into()),
        params: Some(json!({"reason":reason,"reason_code":reason_code,"attempt":attempt,"next_retry_at_ms":next})),
        supersedes_key: Some(key.to_owned()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_retry_feedback_preserves_captured_quota_deadline() {
        // verified: development/opencode-current-retry-diagnosis.json (live GET).
        let notice=retry_notice_from_status(&json!({"type":"retry","attempt":1,"message":"Free usage exceeded, add credits https://opencode.ai/zen","next":1794355200687_u64}),"turn-1").unwrap();
        assert_eq!(notice.code.as_deref(), Some("ACP_NATIVE_RETRY"));
        let params = notice.params.unwrap();
        assert_eq!(params["reason_code"], "free_quota");
        assert_eq!(params["next_retry_at_ms"], 1794355200687_u64);
    }
    #[test]
    fn native_retry_feedback_ignores_idle_and_hides_unknown_provider_payloads() {
        assert!(retry_notice_from_status(&json!({"type":"idle"}), "turn").is_none());
        let notice = retry_notice_from_status(
            &json!({"type":"retry","attempt":2,"message":"secret provider body sk-private","next":100}),
            "turn",
        )
        .unwrap();
        assert!(!notice.content.contains("sk-private"));
        assert_eq!(notice.params.unwrap()["reason_code"], "unknown");
    }
}
