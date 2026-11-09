use super::*;
use aionui_api_types::AgentHandshake;
use aionui_db::{SqliteAgentMetadataRepository, init_database_memory};
use aionui_session::{BackendError, Capabilities, Command, CommandReceipt, ModelInfo, SessionBackend, SessionEnvelope};
use futures_util::stream::BoxStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const USER: &str = "system_default_user";

#[derive(Default)]
struct ProbeState {
    opens: AtomicUsize,
    terminated: AtomicUsize,
    dropped: AtomicUsize,
    empty: AtomicBool,
    configs: std::sync::Mutex<Vec<SessionConfig>>,
}

struct ProbeConnection(Arc<ProbeState>);
struct ProbeBackend(Arc<ProbeState>);

fn discovered_caps() -> Capabilities {
    Capabilities {
        available_models: [
            (
                "default",
                "Default (recommended)",
                "Use the default model (currently deepseek-flash[1m])",
            ),
            ("opus", "deepseek-flash", "Custom Opus model (1M context)"),
            ("fable", "deepseek-flash", "Custom Fable model"),
            ("sonnet", "deepseek-flash", "Custom Sonnet model (1M context)"),
            ("haiku", "deepseek-flash", "Custom Haiku model"),
            ("deepseek-flash[1M]", "deepseek-flash[1M]", "Custom model"),
        ]
        .into_iter()
        .map(|(id, name, description)| ModelInfo {
            id: id.into(),
            name: name.into(),
            description: Some(description.into()),
            reasoning_efforts: vec!["low".into(), "high".into(), "xhigh".into(), "ultracode".into()],
        })
        .collect(),
        ..Default::default()
    }
}

#[async_trait::async_trait]
impl BackendConnection for ProbeConnection {
    async fn open_session(
        &self,
        spec: SessionSpec,
        config: SessionConfig,
    ) -> Result<Arc<dyn SessionBackend>, BackendError> {
        assert!(matches!(spec, SessionSpec::Fresh { .. }));
        self.0.opens.fetch_add(1, Ordering::SeqCst);
        self.0.configs.lock().unwrap().push(config);
        let backend = Arc::new(ProbeBackend(self.0.clone()));
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(backend)
    }
    async fn close_session(&self, _: &str) -> Result<(), BackendError> {
        Ok(())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
}

#[async_trait::async_trait]
impl SessionBackend for ProbeBackend {
    async fn dispatch(&self, _: Command) -> Result<CommandReceipt, BackendError> {
        panic!("catalog discovery must never dispatch a prompt or a model mutation")
    }
    fn events(&self) -> BoxStream<'static, SessionEnvelope> {
        Box::pin(futures_util::stream::pending())
    }
    fn capabilities(&self) -> Capabilities {
        if self.0.empty.load(Ordering::SeqCst) {
            Capabilities::default()
        } else {
            discovered_caps()
        }
    }
    async fn terminate(&self) {
        self.0.terminated.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for ProbeBackend {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

async fn setup() -> (Arc<AgentCatalogRefresh>, Arc<ProbeState>, String) {
    let db = init_database_memory().await.unwrap();
    let registry = AgentRegistry::new(Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone())));
    registry.hydrate().await.unwrap();
    let id = registry.find_builtin_by_backend("claude").await.unwrap().id;
    registry
        .catalog_sender()
        .send_partial_and_wait(
            USER.into(),
            id.clone(),
            AgentHandshake {
                config_options: Some(serde_json::json!([{
                    "id": "model", "category": "model", "type": "select",
                    "options": [{ "value": "old", "name": "Old model" }],
                }])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let state = Arc::new(ProbeState::default());
    let service = AgentCatalogRefresh::new(
        registry,
        Arc::new(ProbeConnection(state.clone())),
        PathBuf::from("/current-project"),
    );
    (service, state, id)
}

#[tokio::test]
async fn claude_catalog_refresh_replaces_old_models_preserves_aliases_and_confirms_persistence() {
    let (service, state, id) = setup().await;
    let row = service.refresh(USER, &id).await.unwrap();
    let options = row.config_options.unwrap();
    let models = options
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "model")
        .unwrap();
    assert_eq!(models["options"].as_array().unwrap().len(), 6);
    assert_eq!(models["options"][2]["value"], "fable");
    assert_eq!(models["options"][3]["description"], "Custom Sonnet model (1M context)");
    assert!(!models.to_string().contains("Old model"));
    let stored = service
        .registry
        .management_row_by_id_for_user(USER, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.config_options.as_ref(), Some(&options));
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    assert_eq!(state.terminated.load(Ordering::SeqCst), 1);
    assert_eq!(state.dropped.load(Ordering::SeqCst), 1);
    let configs = state.configs.lock().unwrap();
    let config = &configs[0];
    assert_eq!(config.cwd.as_deref(), Some("/current-project"));
    assert!(config.model.is_none());
    assert!(config.extra_args.iter().any(|arg| arg == "--no-session-persistence"));
    assert!(config.extra_args.iter().any(|arg| arg == "--strict-mcp-config"));
    assert!(config.init.mcp_servers.is_empty());
}

#[tokio::test]
async fn claude_catalog_refresh_coalesces_concurrent_requests_but_refreshes_a_later_visit() {
    let (service, state, id) = setup().await;
    let (first, second) = tokio::join!(service.refresh(USER, &id), service.refresh(USER, &id));
    first.unwrap();
    second.unwrap();
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    service.refresh(USER, &id).await.unwrap();
    assert_eq!(state.opens.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn claude_catalog_refresh_timeout_keeps_cache_and_drops_backend() {
    let (mut service, state, id) = setup().await;
    Arc::get_mut(&mut service).unwrap().timeout = Duration::from_millis(80);
    state.empty.store(true, Ordering::SeqCst);
    let error = service.refresh(USER, &id).await.unwrap_err();
    assert!(matches!(error, AgentError::Timeout(_)));
    assert!(error.to_string().contains("Claude model discovery timed out"));
    assert_eq!(state.dropped.load(Ordering::SeqCst), 1);
    let row = service
        .registry
        .management_row_by_id_for_user(USER, &id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.config_options.unwrap().to_string().contains("Old model"));
}

#[tokio::test]
async fn claude_catalog_refresh_rejects_missing_disabled_and_non_claude_agents_without_spawning() {
    let (service, state, id) = setup().await;
    assert!(matches!(
        service.refresh(USER, "missing").await,
        Err(AgentError::NotFound(_))
    ));
    let codex = service.registry.find_builtin_by_backend("codex").await.unwrap();
    let error = service.refresh(USER, &codex.id).await.unwrap_err();
    assert!(matches!(error, AgentError::BadRequest(_)));
    assert!(error.to_string().contains("only for the builtin Claude"));
    service.registry.repo_handle().set_enabled(&id, false).await.unwrap();
    assert!(matches!(service.refresh(USER, &id).await, Err(AgentError::Conflict(_))));
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn claude_catalog_refresh_discards_result_when_launch_configuration_changes() {
    let (service, state, id) = setup().await;
    let refresh = service.refresh(USER, &id);
    let update = async {
        while state.opens.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        service
            .registry
            .repo_handle()
            .set_agent_launch_arguments_for_user(USER, &id, "[\"--new-config\"]")
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(refresh, update);
    assert!(matches!(result, Err(AgentError::Conflict(_))));
    assert_eq!(state.terminated.load(Ordering::SeqCst), 1);
    let row = service
        .registry
        .management_row_by_id_for_user(USER, &id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.config_options.unwrap().to_string().contains("Old model"));
}
