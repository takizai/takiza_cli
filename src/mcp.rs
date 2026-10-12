use crate::runtime::{HttpAuth, McpConfig};
use anyhow::{anyhow, Result};
use rmcp::{
    model::{CallToolRequestParams, Tool},
    service::RunningService,
    transport::{
        auth::{
            AuthClient, AuthError, AuthorizationManager, AuthorizationRequest,
            AuthorizationSession, CredentialStore, StoredCredentials,
        },
        streamable_http_client::StreamableHttpClientTransportConfig,
        StreamableHttpClientTransport, TokioChildProcess,
    },
    RoleClient, ServiceExt,
};
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

#[derive(Clone)]
struct SecureStore {
    account: String,
    memory: Arc<Mutex<Option<StoredCredentials>>>,
}
impl SecureStore {
    fn new(url: &str) -> Self {
        Self {
            account: url.to_owned(),
            memory: Default::default(),
        }
    }
    fn entry(&self) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new("takiza-mcp", &self.account)
    }
}
#[async_trait::async_trait]
impl CredentialStore for SecureStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let mut memory = self.memory.lock().await;
        if memory.is_none() {
            if let Ok(value) = self.entry().and_then(|e| e.get_password()) {
                *memory = serde_json::from_str(&value).ok();
            }
        }
        Ok(memory.clone())
    }
    async fn save(&self, value: StoredCredentials) -> Result<(), AuthError> {
        // The system keyring is the only persistent store. Failure falls back
        // to this session's memory; never write credentials into config/logs.
        if let Ok(serialized) = serde_json::to_string(&value) {
            let _ = self.entry().and_then(|e| e.set_password(&serialized));
        }
        *self.memory.lock().await = Some(value);
        Ok(())
    }
    async fn clear(&self) -> Result<(), AuthError> {
        let _ = self.entry().and_then(|e| e.delete_credential());
        *self.memory.lock().await = None;
        Ok(())
    }
}

pub struct Server {
    config: McpConfig,
    service: Option<RunningService<RoleClient, ()>>,
    manager: Option<AuthorizationManager>,
    store: Option<SecureStore>,
    login_task: Option<tokio::task::JoinHandle<Result<AuthorizationManager>>>,
    pub tools: Vec<Tool>,
    pub attempted: bool,
    pub disabled: bool,
    error: Option<String>,
}
impl Server {
    pub fn new(config: McpConfig) -> Self {
        let store = match &config {
            McpConfig::Http {
                url,
                auth: HttpAuth::Oauth { .. },
            } => Some(SecureStore::new(url)),
            _ => None,
        };
        Self {
            config,
            store,
            service: None,
            manager: None,
            login_task: None,
            tools: vec![],
            attempted: false,
            disabled: false,
            error: None,
        }
    }
    pub fn connected(&self) -> bool {
        !self.disabled && self.service.as_ref().is_some_and(|s| !s.is_closed())
    }
    pub fn status(&self) -> String {
        if self.login_task.is_some() {
            return crate::i18n::tr("Waiting for browser login").into();
        }
        if self.disabled {
            return crate::i18n::tr("Disconnected").into();
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.connected() {
            format!(
                "{} · {} tools",
                crate::i18n::tr("Connected"),
                self.tools.len()
            )
        } else {
            self.error
                .clone()
                .unwrap_or_else(|| crate::i18n::tr("Not started").into())
        }
    }
    pub async fn finish_login(&mut self) {
        if self
            .login_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let task = self.login_task.take().unwrap();
            match task.await {
                Ok(Ok(manager)) => {
                    self.manager = Some(manager);
                    self.attempted = false;
                    self.error = None;
                }
                _ => {
                    self.error = Some(crate::i18n::tr("OAuth login failed or timed out").into());
                }
            }
        }
    }
    pub async fn connect(&mut self, workspace: &Path) {
        self.finish_login().await;
        if self.login_task.is_some() {
            return;
        }
        self.attempted = true;
        let result =
            tokio::time::timeout(Duration::from_secs(20), self.connect_inner(workspace)).await;
        match result {
            Ok(Ok(service)) => {
                self.service = Some(service);
                self.error = None;
                self.refresh().await;
            }
            // SDK errors can contain untrusted bodies and URLs. Keep credentials
            // and provider messages out of chat and application logs.
            _ => {
                self.error = Some(
                    crate::i18n::tr("Connection failed. Check configuration or run login.").into(),
                );
                self.tools.clear();
            }
        }
    }
    async fn connect_inner(&mut self, workspace: &Path) -> Result<RunningService<RoleClient, ()>> {
        match &self.config {
            McpConfig::Stdio { command, args, env } => {
                let mut process = tokio::process::Command::new(command);
                process
                    .args(args)
                    .envs(env)
                    .current_dir(workspace)
                    .kill_on_drop(true);
                // The SDK builder owns stdio and otherwise overrides Command.stderr.
                let (transport, _) = TokioChildProcess::builder(process)
                    .stderr(Stdio::null())
                    .spawn()?;
                Ok(().serve(transport).await?)
            }
            McpConfig::Http { url, auth } => {
                let parsed = mcp_reqwest::Url::parse(url)?;
                anyhow::ensure!(
                    parsed.username().is_empty() && parsed.password().is_none(),
                    "Use environment token authorization"
                );
                let client = mcp_reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .redirect(mcp_reqwest::redirect::Policy::none())
                    .build()?;
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                config.reinit_on_expired_session = false;
                match auth {
                    HttpAuth::None => Ok(
                        ().serve(StreamableHttpClientTransport::with_client(client, config))
                            .await?,
                    ),
                    HttpAuth::Token { variable } => {
                        config.auth_header = Some(
                            std::env::var(variable)
                                .map_err(|_| anyhow!("Token environment variable missing"))?,
                        );
                        Ok(
                            ().serve(StreamableHttpClientTransport::with_client(client, config))
                                .await?,
                        )
                    }
                    HttpAuth::Oauth { .. } => {
                        if self.manager.is_none() {
                            let mut manager = AuthorizationManager::new(url.clone()).await?;
                            manager.set_credential_store(self.store.clone().unwrap());
                            anyhow::ensure!(
                                manager.initialize_from_store().await?,
                                "Login required"
                            );
                            self.manager = Some(manager);
                        }
                        let authorized = AuthClient::new(client, self.manager.take().unwrap());
                        Ok(()
                            .serve(StreamableHttpClientTransport::with_client(
                                authorized, config,
                            ))
                            .await?)
                    }
                }
            }
        }
    }
    pub async fn refresh(&mut self) {
        self.finish_login().await;
        let Some(service) = &self.service else {
            return;
        };
        match tokio::time::timeout(Duration::from_secs(10), service.list_all_tools()).await {
            Ok(Ok(tools)) => {
                let total = tools.len();
                self.tools = tools
                    .into_iter()
                    .filter(|tool| jsonschema::validator_for(&json!(tool.input_schema)).is_ok())
                    .collect();
                if total != self.tools.len() {
                    self.error = Some(
                        crate::i18n::tr(
                            "Server supplied invalid tool schemas; those tools were omitted",
                        )
                        .into(),
                    );
                } else {
                    self.error = None;
                }
            }
            _ => {
                self.tools.clear();
                self.error = Some(crate::i18n::tr("Tool discovery failed").into());
            }
        }
    }
    pub async fn call(&mut self, tool: &str, args: Value) -> Result<String, String> {
        let service = self
            .service
            .as_ref()
            .filter(|s| !s.is_closed())
            .ok_or("MCP disconnected; reconnect explicitly")?;
        let params = CallToolRequestParams::new(tool.to_owned())
            .with_arguments(args.as_object().ok_or("Expected object arguments")?.clone());
        let handle = service
            .peer()
            .send_cancellable_request(
                rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(
                    params,
                )),
                rmcp::service::PeerRequestOptions::with_timeout(Duration::from_secs(55)),
            )
            .await
            .map_err(|_| {
                "MCP call failed; execution may have occurred. Do not retry automatically."
                    .to_string()
            })?;
        let mut guard = McpCancelGuard {
            peer: handle.peer.clone(),
            id: Some(handle.id.clone()),
        };
        let result = handle.await_response().await.map_err(|_| {
            "MCP call failed; execution may have occurred. Do not retry automatically.".to_string()
        })?;
        guard.id = None;
        let rmcp::model::ServerResult::CallToolResult(result) = result else {
            return Err("MCP returned an unsupported tool response".into());
        };
        render_result(serde_json::to_value(result).map_err(|e| e.to_string())?)
    }
    pub async fn login(&mut self, _name: &str) -> Result<String> {
        let McpConfig::Http {
            url,
            auth: HttpAuth::Oauth { client_id },
        } = &self.config
        else {
            return Err(anyhow!("OAuth is not configured for this server"));
        };
        let url = url.clone();
        let client_id = client_id.clone();
        self.stop().await;
        self.disabled = false;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr()?.port()
        );
        let mut manager = AuthorizationManager::new(url).await?;
        manager.set_credential_store(self.store.clone().unwrap());
        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(|_| anyhow!("OAuth discovery failed"))?;
        manager.set_metadata(resolution.metadata);
        let mut request = AuthorizationRequest::new(redirect.clone()).with_client_name("Takiza");
        if let Some(id) = client_id {
            request = request.with_preregistered_client(id);
        }
        let session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|_| anyhow!("OAuth registration failed; configure clientId if required"))?;
        let authorization_url = session.get_authorization_url().to_owned();
        self.login_task = Some(tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(180), async {
                loop {
                    let (socket, _) = listener.accept().await?;
                    let mut socket = BufReader::new(socket); let mut line = String::new();
                    tokio::time::timeout(Duration::from_secs(5), socket.read_line(&mut line)).await??;
                    let target = line.split_whitespace().nth(1).unwrap_or("");
                    if !target.starts_with("/callback?") { continue; }
                    let callback = format!("{}{}", redirect.trim_end_matches("/callback"), target);
                    let result = session.handle_callback_url(&callback).await;
                    let response = if result.is_ok() { "Login complete. You can return to Takiza." } else { "Login rejected." };
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}", response.len()).as_bytes()).await?;
                    result.map_err(|_| anyhow!("OAuth callback rejected"))?;
                    return Ok(session.auth_manager);
                }
            }).await.map_err(|_| anyhow!("Login timed out"))?
        }));
        open_browser(&authorization_url);
        Ok(format!(
            "{}\n{authorization_url}",
            crate::i18n::tr("Open this link to log in:")
        ))
    }
    pub async fn logout(&mut self, _name: &str) {
        self.stop().await;
        self.manager = None;
        if let Some(store) = &self.store {
            let _ = store.clear().await;
        }
        self.disabled = true;
    }
    pub async fn stop(&mut self) {
        if let Some(task) = self.login_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(mut service) = self.service.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), service.close()).await;
        }
        self.tools.clear();
    }
}
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let (program, args) = ("xdg-open", vec![url]);
    #[cfg(target_os = "macos")]
    let (program, args) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let (program, args) = ("rundll32", vec!["url.dll,FileProtocolHandler", url]);
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let mut process = tokio::process::Command::new(program);
        process
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(mut child) = process.spawn() {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}
fn render_result(mut result: Value) -> Result<String, String> {
    if let Some(content) = result["content"].as_array_mut() {
        for block in content {
            match block["type"].as_str().unwrap_or("") {
                "text" | "resource_link" => {}
                "resource" if block["resource"].get("text").is_some() => {}
                "resource" => {
                    *block = json!({"type":"unsupported","format":"embedded_resource_blob","uri":block["resource"]["uri"],"mimeType":block["resource"]["mimeType"]});
                }
                "image" => {
                    use base64::Engine;
                    let image = block["data"]
                        .as_str()
                        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok());
                    *block = match image
                        .and_then(|bytes| crate::attachments::cache_image(&bytes).ok())
                    {
                        Some(attachment) => json!({"type":"image","attachment":attachment}),
                        None => {
                            json!({"type":"unsupported","format":"image","message":"Image format unavailable"})
                        }
                    };
                }
                other => {
                    *block = json!({"type":"unsupported","format":other});
                }
            }
        }
    }
    if result["isError"].as_bool() == Some(true) {
        Err(result.to_string())
    } else {
        Ok(result.to_string())
    }
}

struct McpCancelGuard {
    peer: rmcp::Peer<RoleClient>,
    id: Option<rmcp::model::RequestId>,
}
impl Drop for McpCancelGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            tokio::spawn(async move {
                let _ = peer
                    .notify_cancelled(rmcp::model::CancelledNotificationParam::new(
                        Some(id),
                        Some("Takiza request cancelled".into()),
                    ))
                    .await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(mode: &str) -> (String, Vec<String>) {
        (
            "python3".into(),
            vec![
                format!(
                    "{}/tests/fixtures/integration_server.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
                mode.into(),
            ],
        )
    }
    #[tokio::test]
    async fn stdio_discovery_pagination_results_and_disconnect_do_not_retry() {
        let (command, args) = fixture("mcp");
        let mut server = Server::new(McpConfig::Stdio {
            command,
            args,
            env: Default::default(),
        });
        server.connect(Path::new(env!("CARGO_MANIFEST_DIR"))).await;
        assert!(server.connected(), "{}", server.status());
        assert_eq!(server.tools.len(), 2);
        let result: Value =
            serde_json::from_str(&server.call("echo", json!({"text":"hello"})).await.unwrap())
                .unwrap();
        assert_eq!(result["content"][0]["text"], "hello");
        assert_eq!(result["structuredContent"]["calls"], 1);
        assert!(server.call("echo", json!({"text":"error"})).await.is_err());
        assert!(server
            .call("echo", json!({"text":"disconnect"}))
            .await
            .unwrap_err()
            .contains("Do not retry"));
        assert!(server.attempted);
        server.stop().await;
    }
    async fn http_fixture() -> (tokio::process::Child, String) {
        let (command, args) = fixture("http");
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut port = String::new();
        stdout.read_line(&mut port).await.unwrap();
        (child, format!("http://127.0.0.1:{}", port.trim()))
    }
    #[tokio::test]
    async fn streamable_http_lists_and_calls_tools() {
        let (mut child, url) = http_fixture().await;
        let mut server = Server::new(McpConfig::Http {
            url: format!("{url}/mcp"),
            auth: HttpAuth::None,
        });
        server.connect(Path::new(env!("CARGO_MANIFEST_DIR"))).await;
        assert!(server.connected(), "{}", server.status());
        assert_eq!(server.tools.len(), 2);
        assert!(server
            .call("echo", json!({"text":"HTTP works"}))
            .await
            .unwrap()
            .contains("HTTP works"));
        server.stop().await;
        let _ = child.kill().await;
    }
    #[tokio::test]
    async fn oauth_discovery_pkce_state_validation_and_refresh() {
        let (mut child, url) = http_fixture().await;
        let mut manager = AuthorizationManager::new(format!("{url}/mcp"))
            .await
            .unwrap();
        let resolution = manager.resolve_metadata().await.unwrap();
        manager.set_metadata(resolution.metadata);
        let session = AuthorizationSession::new(
            manager,
            AuthorizationRequest::new("http://127.0.0.1:9999/callback")
                .with_client_name("Takiza test"),
        )
        .await
        .map_err(|(_, e)| e)
        .unwrap();
        let authorization = mcp_reqwest::Url::parse(session.get_authorization_url()).unwrap();
        let query: std::collections::BTreeMap<_, _> =
            authorization.query_pairs().into_owned().collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(!query["code_challenge"].is_empty());
        assert!(session
            .handle_callback_url("http://127.0.0.1:9999/callback?code=fixture&state=wrong")
            .await
            .is_err());
        session
            .handle_callback_url(&format!(
                "http://127.0.0.1:9999/callback?code=fixture&state={}",
                query["state"]
            ))
            .await
            .unwrap();
        assert_eq!(
            session.auth_manager.get_access_token().await.unwrap(),
            "fixture-access"
        );
        session.auth_manager.refresh_token().await.unwrap();
        let authorized = AuthClient::new(mcp_reqwest::Client::new(), session.auth_manager);
        let mut config = StreamableHttpClientTransportConfig::with_uri(format!("{url}/mcp"));
        config.reinit_on_expired_session = false;
        let mut service = ()
            .serve(StreamableHttpClientTransport::with_client(
                authorized, config,
            ))
            .await
            .unwrap();
        assert_eq!(service.list_all_tools().await.unwrap().len(), 2);
        let _ = service.close().await;
        let _ = child.kill().await;
    }
    #[test]
    fn unsupported_content_is_explicit_and_links_are_preserved() {
        let result: Value = serde_json::from_str(&render_result(json!({"content":[{"type":"audio","data":"secret"},{"type":"resource_link","uri":"https://example.com"}],"structuredContent":{"ok":true}})).unwrap()).unwrap();
        assert_eq!(
            result["content"][0],
            json!({"type":"unsupported","format":"audio"})
        );
        assert_eq!(result["content"][1]["uri"], "https://example.com");
        assert_eq!(result["structuredContent"]["ok"], true);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn dropping_a_call_notifies_server_and_never_replays_it() {
        let log = std::env::temp_dir().join(format!(
            "takiza-mcp-cancel-{}.jsonl",
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut server = Server::new(McpConfig::Stdio {
            command: "python3".into(),
            args: vec![
                format!(
                    "{}/tests/fixtures/integration_server.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
                "mcp".into(),
                "--log".into(),
                log.to_string_lossy().into_owned(),
            ],
            env: Default::default(),
        });
        server.connect(Path::new(env!("CARGO_MANIFEST_DIR"))).await;
        assert!(server.connected());
        assert!(tokio::time::timeout(
            Duration::from_millis(150),
            server.call("echo", json!({"text":"wait"}))
        )
        .await
        .is_err());
        tokio::time::sleep(Duration::from_millis(150)).await;
        let messages: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["method"] == "tools/call")
                .count(),
            1
        );
        assert!(messages
            .iter()
            .any(|m| m["method"] == "notifications/cancelled"));
        assert!(server
            .call("echo", json!({"text":"next"}))
            .await
            .unwrap()
            .contains("next"));
        server.stop().await;
        std::fs::remove_file(log).unwrap();
    }
}
