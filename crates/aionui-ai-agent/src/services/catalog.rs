//! On-demand native Claude discovery, independent of conversation creation.
//! Wire source: docs/protocol-schemas/claude-model-discovery/initialize.json.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aionui_api_types::{AgentManagementRow, AgentMetadata, AgentSource};
use aionui_session::{BackendConnection, SessionConfig, SessionSpec};
use futures_util::StreamExt;
use tokio::sync::{Mutex, watch};

use crate::error::AgentError;
use crate::registry::AgentRegistry;
use crate::session_agent::catalog_partial_from_caps;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
type RefreshResult = Result<AgentManagementRow, RefreshFailure>;
type RefreshKey = (String, String);
type PendingRefreshes = HashMap<RefreshKey, watch::Receiver<Option<RefreshResult>>>;

#[derive(Clone, Copy, Debug)]
enum RefreshFailure {
    Timeout,
    Process,
    Storage,
    Changed,
}

impl RefreshFailure {
    fn into_error(self) -> AgentError {
        match self {
            Self::Timeout => AgentError::timeout("Claude model discovery timed out. Retry the refresh."),
            Self::Process => {
                AgentError::bad_gateway("Claude model discovery failed. Check the CLI installation and configuration.")
            }
            Self::Storage => AgentError::internal("Could not save the refreshed Claude model catalog."),
            Self::Changed => {
                AgentError::conflict("Claude configuration changed during model discovery. Retry the refresh.")
            }
        }
    }
}

pub(crate) struct AgentCatalogRefresh {
    registry: Arc<AgentRegistry>,
    connection: Arc<dyn BackendConnection>,
    work_dir: PathBuf,
    pending: Mutex<PendingRefreshes>,
    timeout: Duration,
}

impl AgentCatalogRefresh {
    pub(crate) fn new(
        registry: Arc<AgentRegistry>,
        connection: Arc<dyn BackendConnection>,
        work_dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            connection,
            work_dir,
            pending: Mutex::new(HashMap::new()),
            timeout: DISCOVERY_TIMEOUT,
        })
    }

    pub(crate) async fn refresh(self: &Arc<Self>, user_id: &str, id: &str) -> Result<AgentManagementRow, AgentError> {
        // Authorize before joining a job: discovery never crosses user ownership.
        let meta = self
            .registry
            .get_for_user(user_id, id)
            .await?
            .ok_or_else(|| AgentError::not_found("Agent not found"))?;
        validate_target(&meta)?;
        let key = (user_id.to_owned(), id.to_owned());
        let mut pending = self.pending.lock().await;
        let mut receiver = if let Some(receiver) = pending.get(&key) {
            receiver.clone()
        } else {
            let (sender, receiver) = watch::channel(None);
            pending.insert(key.clone(), receiver.clone());
            let service = self.clone();
            // The bounded job owns cleanup even when an HTTP caller disconnects.
            tokio::spawn(async move {
                let result = service.discover(&key.0, &meta).await;
                if let Err(failure) = &result {
                    tracing::warn!(agent_id = %meta.id, ?failure, "Claude model catalog refresh failed");
                }
                let mut pending = service.pending.lock().await;
                let _ = sender.send(Some(result));
                pending.remove(&key);
            });
            receiver
        };
        drop(pending);
        let result = receiver
            .wait_for(|result| result.is_some())
            .await
            .map_err(|_| AgentError::internal("Claude model discovery stopped before returning a result"))?
            .as_ref()
            .expect("wait_for checked the result")
            .clone();
        result.map_err(RefreshFailure::into_error)
    }

    async fn discover(&self, user_id: &str, meta: &AgentMetadata) -> RefreshResult {
        let config = discovery_config(meta, &self.work_dir);
        let signature = configuration_signature(meta, &config);
        let connection = self.connection.clone();
        let caps = tokio::time::timeout(self.timeout, async move {
            let backend = connection.open_session(
                SessionSpec::Fresh { session_id: uuid::Uuid::new_v4().to_string() }, config,
            ).await.map_err(|_| RefreshFailure::Process)?;
            let mut events = backend.events();
            // Dropping a timed-out native backend aborts its reader and reaps its
            // own process. Success explicitly terminates before publishing anything.
            loop {
                let caps = backend.capabilities();
                if !caps.available_models.is_empty() {
                    backend.terminate().await;
                    return Ok(caps);
                }
                tokio::select! {
                    event = events.next() => {
                        if event.is_none() || event.is_some_and(|event| matches!(event.event, aionui_session::SessionEvent::Detached { .. })) {
                            backend.terminate().await;
                            return Err(RefreshFailure::Process);
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        }).await.map_err(|_| RefreshFailure::Timeout)??;
        let current = self
            .registry
            .get_for_user(user_id, &meta.id)
            .await
            .map_err(|_| RefreshFailure::Storage)?
            .ok_or(RefreshFailure::Changed)?;
        if !current.enabled
            || signature != configuration_signature(&current, &discovery_config(&current, &self.work_dir))
        {
            return Err(RefreshFailure::Changed);
        }
        let partial = catalog_partial_from_caps(&caps).ok_or(RefreshFailure::Process)?;
        self.registry
            .catalog_sender()
            .send_partial_and_wait(user_id.to_owned(), meta.id.clone(), partial)
            .await
            .map_err(|_| RefreshFailure::Storage)?;
        let row = self
            .registry
            .management_row_by_id_for_user(user_id, &meta.id)
            .await
            .map_err(|_| RefreshFailure::Storage)?
            .ok_or(RefreshFailure::Changed)?;
        tracing::info!(agent_id = %meta.id, model_count = caps.available_models.len(), "Claude model catalog refreshed without a prompt");
        Ok(row)
    }
}

fn validate_target(meta: &AgentMetadata) -> Result<(), AgentError> {
    if meta.agent_source != AgentSource::Builtin || meta.backend.as_deref() != Some("claude") {
        return Err(AgentError::bad_request(
            "Native model discovery is supported only for the builtin Claude Agent",
        ));
    }
    if !meta.enabled {
        return Err(AgentError::conflict(
            "Enable the Claude Agent before refreshing its model catalog",
        ));
    }
    Ok(())
}

fn discovery_config(meta: &AgentMetadata, work_dir: &std::path::Path) -> SessionConfig {
    let mut spawn_env: Vec<aionui_common::EnvVar> = meta
        .env
        .iter()
        .map(|entry| aionui_common::EnvVar {
            name: entry.name.clone(),
            value: entry.value.clone(),
        })
        .collect();
    // Same provider overlay as build_session_instance; values remain private.
    spawn_env.extend(
        crate::cc_switch::read_claude_provider_env()
            .into_iter()
            .map(|(name, value)| aionui_common::EnvVar { name, value }),
    );
    spawn_env.sort_by(|left, right| left.name.cmp(&right.name));
    let mut extra_args: Vec<String> = meta.args.iter().chain(&meta.additional_args).cloned().collect();
    // Verified by the captured CLI help + initialize fixture. No prompt is ever
    // dispatched, no conversation is inserted, and user MCP servers are not started.
    extra_args.extend(
        [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    SessionConfig {
        cwd: Some(work_dir.to_string_lossy().into_owned()),
        cli_program: meta
            .resolved_command
            .clone()
            .or_else(|| meta.command.as_ref().map(PathBuf::from)),
        spawn_env,
        extra_args,
        ..Default::default()
    }
}

fn configuration_signature(meta: &AgentMetadata, config: &SessionConfig) -> String {
    // Only in-memory equality; never persisted or logged (may contain credentials).
    serde_json::json!({
        "command": config.cli_program, "args": config.extra_args,
        "env": config.spawn_env.iter().map(|entry| (&entry.name, &entry.value)).collect::<Vec<_>>(),
        "backend": meta.backend, "source": meta.agent_source,
    })
    .to_string()
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
