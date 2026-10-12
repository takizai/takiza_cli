//! Session-scoped integrations and the model-facing tool registry.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "camelCase")]
pub enum McpConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        auth: HttpAuth,
    },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HttpAuth {
    #[default]
    None,
    Token {
        #[serde(rename = "env")]
        variable: String,
    },
    Oauth {
        #[serde(default, rename = "clientId")]
        client_id: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub languages: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
}
#[derive(Clone, Debug)]
pub enum Executor {
    Builtin,
    Mcp { server: String, tool: String },
    Lsp,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub definition: Value,
    pub executor: Executor,
}
#[derive(Clone, Debug)]
pub struct Registry(pub Vec<Entry>);
impl Registry {
    pub fn builtins() -> Self {
        Self(
            crate::tools::get_tool_definitions()
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .map(|definition| Entry {
                    definition,
                    executor: Executor::Builtin,
                })
                .collect(),
        )
    }
    pub fn definitions(&self) -> Value {
        Value::Array(self.0.iter().map(|e| e.definition.clone()).collect())
    }
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.0
            .iter()
            .find(|e| e.definition["function"]["name"] == name)
    }
}
pub fn definition(name: &str, description: &str, schema: Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":schema}})
}
// Encode every byte outside the provider alphabet, including underscores, so
// original names remain distinct. Long names use a stable FNV suffix.
pub fn model_name(server: &str, tool: &str) -> String {
    fn encode(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() {
                    (b as char).to_string()
                } else {
                    format!("_{b:02x}")
                }
            })
            .collect()
    }
    let full = format!("mcp__{}__{}", encode(server), encode(tool));
    if full.len() <= 64 {
        return full;
    }
    let hash = full.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    format!("{}_{hash:016x}", &full[..47])
}

pub struct Runtime {
    pub workspace: PathBuf,
    pub mcp: BTreeMap<String, Arc<Mutex<crate::mcp::Server>>>,
    pub lsp: BTreeMap<String, Arc<Mutex<crate::lsp::Server>>>,
    registry: RwLock<Registry>,
    trusted: RwLock<BTreeSet<String>>,
}
impl Runtime {
    pub fn new(workspace: PathBuf) -> Arc<Self> {
        let prefs = crate::theme::UserPreferences::load();
        let mut registry = Registry::builtins();
        if !prefs.lsp_servers.is_empty() {
            registry.0.extend(
                crate::lsp::definitions(&prefs.lsp_servers.keys().cloned().collect::<Vec<_>>())
                    .into_iter()
                    .map(|definition| Entry {
                        definition,
                        executor: Executor::Lsp,
                    }),
            );
        }
        Arc::new(Self {
            mcp: prefs
                .mcp_servers
                .into_iter()
                .map(|(name, config)| (name, Arc::new(Mutex::new(crate::mcp::Server::new(config)))))
                .collect(),
            lsp: prefs
                .lsp_servers
                .into_iter()
                .map(|(name, config)| {
                    (
                        name,
                        Arc::new(Mutex::new(crate::lsp::Server::new(
                            config,
                            workspace.clone(),
                        ))),
                    )
                })
                .collect(),
            workspace,
            registry: RwLock::new(registry),
            trusted: RwLock::new(BTreeSet::new()),
        })
    }
    pub fn snapshot(&self) -> Registry {
        self.registry.read().unwrap().clone()
    }
    pub fn mcp_target(&self, name: &str) -> Option<(String, String)> {
        match &self.snapshot().get(name)?.executor {
            Executor::Mcp { server, tool } => Some((server.clone(), tool.clone())),
            _ => None,
        }
    }
    pub fn trusted(&self, server: &str) -> bool {
        self.trusted.read().unwrap().contains(server)
    }
    pub fn trust(&self, server: String) {
        self.trusted.write().unwrap().insert(server);
    }
    pub async fn prepare(&self, cancel: &CancellationToken) {
        // Each failed server is isolated; do not reconnect on every model turn.
        let connections = futures_util::future::join_all(self.mcp.values().map(|server| async {
            let mut server = server.lock().await;
            server.finish_login().await;
            if !server.attempted && !server.disabled {
                server.connect(&self.workspace).await;
            } else if server.connected() {
                server.refresh().await;
            }
        }));
        tokio::select! { _ = cancel.cancelled() => return, _ = connections => {} }
        self.rebuild().await;
    }
    async fn rebuild(&self) {
        let mut registry = self.snapshot();
        registry
            .0
            .retain(|e| !matches!(e.executor, Executor::Mcp { .. }));
        let mut names: BTreeSet<String> = registry
            .0
            .iter()
            .filter_map(|e| e.definition["function"]["name"].as_str().map(str::to_owned))
            .collect();
        for (name, server) in &self.mcp {
            let server = server.lock().await;
            if !server.connected() {
                continue;
            }
            for tool in &server.tools {
                let model = model_name(name, &tool.name);
                if !names.insert(model.clone()) {
                    continue;
                }
                registry.0.push(Entry {
                    definition: definition(
                        &model,
                        &format!(
                            "MCP server {name}, tool {}: {}",
                            tool.name,
                            tool.description.as_deref().unwrap_or("")
                        ),
                        json!(tool.input_schema),
                    ),
                    executor: Executor::Mcp {
                        server: name.clone(),
                        tool: tool.name.to_string(),
                    },
                });
            }
        }
        *self.registry.write().unwrap() = registry;
    }
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let entry = self
            .snapshot()
            .get(name)
            .cloned()
            .ok_or("Tool is no longer available")?;
        let work = async {
            match entry.executor {
                Executor::Mcp { server, tool } => {
                    self.mcp[&server].lock().await.call(&tool, args).await
                }
                Executor::Lsp => {
                    let path = args["path"].as_str();
                    let requested = args["server"]
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    for (server_name, server) in &self.lsp {
                        let mut server = server.lock().await;
                        if requested
                            .map(|s| s == server_name)
                            .unwrap_or_else(|| path.is_some_and(|p| server.supports(p)))
                        {
                            return server
                                .call(name, &args)
                                .await
                                .map(|v| v.to_string())
                                .map_err(|e| format!("LSP unavailable ({server_name}): {e}"));
                        }
                    }
                    Err("No configured LSP server matches this file; specify server for workspace symbols".into())
                }
                Executor::Builtin => Err("Builtin executor required".into()),
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => Err("Integration call cancelled; execution may have occurred. Do not retry automatically.".into()),
            result = tokio::time::timeout(std::time::Duration::from_secs(60), work) => result.unwrap_or_else(|_| Err("Integration call timed out; execution may have occurred. Do not retry automatically.".into())),
        }
    }
    pub async fn reset_documents(&self) {
        for server in self.lsp.values() {
            server.lock().await.stop().await;
        }
    }
    pub async fn command(
        &self,
        kind: &str,
        action: &str,
        name: Option<&str>,
    ) -> Result<String, String> {
        if action == "status" || action.is_empty() {
            let mut lines = vec![];
            if kind == "mcp" {
                for (name, server) in &self.mcp {
                    let mut server = server.lock().await;
                    server.finish_login().await;
                    lines.push(format!("{name}: {}", server.status()));
                }
            } else {
                for (name, server) in &self.lsp {
                    lines.push(format!("{name}: {}", server.lock().await.status()));
                }
            }
            return Ok(if lines.is_empty() {
                crate::i18n::tr("No servers configured. Edit config.json.").into()
            } else {
                lines.join("\n")
            });
        }
        let name =
            name.ok_or("Usage: /mcp|/lsp status | connect|disconnect|restart|login|logout SERVER")?;
        if kind == "mcp" {
            let server = self.mcp.get(name).ok_or("Unknown MCP server")?;
            let mut server = server.lock().await;
            match action {
                "connect" | "restart" => {
                    server.stop().await;
                    server.disabled = false;
                    server.connect(&self.workspace).await;
                }
                "disconnect" => {
                    server.stop().await;
                    server.disabled = true;
                    self.trusted.write().unwrap().remove(name);
                }
                "login" => {
                    let result = server.login(name).await.map_err(|e| e.to_string());
                    drop(server);
                    self.rebuild().await;
                    return result;
                }
                "logout" => {
                    server.logout(name).await;
                    self.trusted.write().unwrap().remove(name);
                }
                _ => return Err("Unknown MCP action".into()),
            }
            let status = server.status();
            drop(server);
            self.rebuild().await;
            Ok(status)
        } else {
            let mut server = self.lsp.get(name).ok_or("Unknown LSP server")?.lock().await;
            match action {
                "connect" | "restart" => {
                    server.stop().await;
                    server.disabled = false;
                    server.start().await.map_err(|e| e.to_string())?;
                }
                "disconnect" => {
                    server.stop().await;
                    server.disabled = true;
                }
                _ => return Err("Unknown LSP action".into()),
            }
            Ok(server.status())
        }
    }
    pub async fn shutdown(&self) {
        for server in self.mcp.values() {
            server.lock().await.stop().await;
        }
        self.reset_documents().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_stable_and_distinct() {
        assert_ne!(model_name("a-b", "c"), model_name("a_b", "c"));
        assert_ne!(model_name("a", "b__c"), model_name("a__b", "c"));
        let long = model_name(&"x".repeat(100), "tool");
        assert_eq!(long.len(), 64);
        assert_eq!(long, model_name(&"x".repeat(100), "tool"));
    }
    #[test]
    fn old_configs_and_new_transports_deserialize() {
        let prefs: crate::theme::UserPreferences = serde_json::from_value(
            json!({"agreed_to_terms":false,"terms_version":"1","theme":"Amber"}),
        )
        .unwrap();
        assert!(prefs.mcp_servers.is_empty());
        let config: McpConfig = serde_json::from_value(json!({"transport":"http","url":"https://example.com/mcp","auth":{"type":"token","env":"MCP_TOKEN"}})).unwrap();
        assert!(matches!(
            config,
            McpConfig::Http {
                auth: HttpAuth::Token { .. },
                ..
            }
        ));
    }
    #[tokio::test]
    async fn empty_lsp_server_selects_by_file_language() {
        let root = std::env::temp_dir().join(format!(
            "takiza-lsp-routing-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("example.rs"), "fn main() {}\n").unwrap();
        let names = vec!["rust".to_owned()];
        let definitions = crate::lsp::definitions(&names);
        let diagnostic = definitions
            .iter()
            .find(|d| d["function"]["name"] == "lsp_diagnostics")
            .unwrap();
        let schema = &diagnostic["function"]["parameters"];
        let validator = jsonschema::validator_for(schema).unwrap();
        assert!(validator.is_valid(&json!({"path":"example.rs","server":""})));
        assert!(!validator.is_valid(&json!({"path":"example.rs","server":"rust-analyzer"})));
        assert!(!validator.is_valid(&json!({"path":"example.rs","line":1})));
        let server = crate::lsp::Server::new(
            LspConfig {
                command: "python3".into(),
                args: vec![
                    format!(
                        "{}/tests/fixtures/integration_server.py",
                        env!("CARGO_MANIFEST_DIR")
                    ),
                    "lsp".into(),
                ],
                env: Default::default(),
                languages: vec!["rust".into()],
                root_markers: vec![],
            },
            root.clone(),
        );
        let runtime = Runtime {
            workspace: root.clone(),
            mcp: Default::default(),
            lsp: BTreeMap::from([("rust".into(), Arc::new(Mutex::new(server)))]),
            registry: RwLock::new(Registry(
                definitions
                    .into_iter()
                    .map(|definition| Entry {
                        definition,
                        executor: Executor::Lsp,
                    })
                    .collect(),
            )),
            trusted: Default::default(),
        };
        for server in [None, Some(""), Some("  "), Some("rust")] {
            let mut args = json!({"path":"example.rs"});
            if let Some(server) = server {
                args["server"] = json!(server);
            }
            let result = runtime
                .execute("lsp_diagnostics", args, CancellationToken::new())
                .await
                .unwrap();
            let result: Value = serde_json::from_str(&result).unwrap();
            assert_eq!(result["status"], "current");
        }
        runtime.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
