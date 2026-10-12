//! Read-only LSP adapter. Tool columns are Unicode scalar positions, one based.
use crate::runtime::{definition, LspConfig};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::{oneshot, Mutex as AsyncMutex},
};

type Pending = Arc<Mutex<BTreeMap<u64, oneshot::Sender<Value>>>>;
type Diagnostics = Arc<Mutex<BTreeMap<String, Value>>>;
#[derive(Debug)]
struct ResponseError(Value);
impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LSP request rejected: {}", self.0)
    }
}
impl std::error::Error for ResponseError {}
pub fn definitions(servers: &[String]) -> Vec<Value> {
    ["lsp_hover", "lsp_definition", "lsp_references", "lsp_symbols", "lsp_diagnostics"].into_iter().map(|name| {
        let positioned = matches!(name, "lsp_hover" | "lsp_definition" | "lsp_references");
        let mut choices = vec![String::new()];
        choices.extend_from_slice(servers);
        let mut properties = json!({
            "path":{"type":"string","minLength":1,"description":"Workspace-relative file path."},
            "server":{"type":"string","enum":choices,"description":"Configured server name; omit or use an empty string to select by file language."}
        });
        if positioned {
            properties["line"] = json!({"type":"integer","minimum":1,"description":"1-based line."});
            properties["column"] = json!({"type":"integer","minimum":1,"description":"1-based Unicode character column."});
        }
        if name == "lsp_references" { properties["include_declaration"] = json!({"type":"boolean"}); }
        if name == "lsp_symbols" { properties["query"] = json!({"type":"string","description":"Project symbol query; provide a nonempty server name when omitting path."}); }
        let mut schema = json!({"type":"object","properties":properties,"additionalProperties":false,
            "required": if positioned { vec!["path","line","column"] } else if name == "lsp_diagnostics" { vec!["path"] } else { vec![] }});
        if name == "lsp_symbols" {
            schema["anyOf"] = json!([{"required":["path"]},{"required":["query","server"],"properties":{"server":{"minLength":1}}}]);
        }
        definition(name, &format!("Read-only LSP {}. File requests automatically select a server by language. Positions use 1-based lines and Unicode characters. For project symbols provide query and a configured server name instead of path.",name.trim_start_matches("lsp_")), schema)
    }).collect()
}
struct Document {
    text: String,
    version: i64,
}
struct Connection {
    writer: Arc<AsyncMutex<ChildStdin>>,
    pending: Pending,
    diagnostics: Diagnostics,
    reader: tokio::task::JoinHandle<()>,
    child: Child,
}
pub struct Server {
    pub config: LspConfig,
    workspace: PathBuf,
    connection: Option<Connection>,
    documents: BTreeMap<String, Document>,
    capabilities: Value,
    encoding: String,
    next_id: u64,
    error: Option<String>,
    pub disabled: bool,
}
async fn write(writer: &Arc<AsyncMutex<ChildStdin>>, message: Value) -> Result<()> {
    let body = message.to_string();
    let mut writer = writer.lock().await;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
        .await?;
    writer.flush().await?;
    Ok(())
}
struct CancelRequest {
    id: u64,
    writer: Arc<AsyncMutex<ChildStdin>>,
    pending: Pending,
}
impl Drop for CancelRequest {
    fn drop(&mut self) {
        if self.pending.lock().unwrap().remove(&self.id).is_some() {
            let writer = self.writer.clone();
            let id = self.id;
            tokio::spawn(async move {
                let _ = write(
                    &writer,
                    json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}}),
                )
                .await;
            });
        }
    }
}
impl Server {
    pub fn new(config: LspConfig, workspace: PathBuf) -> Self {
        Self {
            config,
            workspace,
            connection: None,
            documents: BTreeMap::new(),
            capabilities: Value::Null,
            encoding: "utf-16".into(),
            next_id: 0,
            error: None,
            disabled: false,
        }
    }
    pub fn supports(&self, path: &str) -> bool {
        let ext = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let lang = language(ext);
        self.config.languages.iter().any(|s| {
            s == ext
                || s == lang
                || (s == "typescript" && ext == "tsx")
                || (s == "javascript" && ext == "jsx")
        })
    }
    pub fn status(&self) -> String {
        if self.disabled {
            return crate::i18n::tr("Disconnected").into();
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self
            .connection
            .as_ref()
            .is_some_and(|c| !c.reader.is_finished())
        {
            format!(
                "{} · {} · capabilities: {}",
                crate::i18n::tr("Connected"),
                self.encoding,
                self.capabilities
            )
        } else {
            self.error
                .clone()
                .unwrap_or_else(|| crate::i18n::tr("Not started").into())
        }
    }
    fn root(&self, file: Option<&Path>) -> PathBuf {
        let mut dir = file
            .and_then(Path::parent)
            .unwrap_or(&self.workspace)
            .to_path_buf();
        while dir.starts_with(&self.workspace) {
            if self
                .config
                .root_markers
                .iter()
                .any(|marker| dir.join(marker).exists())
            {
                return dir;
            }
            if !dir.pop() {
                break;
            }
        }
        self.workspace.clone()
    }
    pub async fn start(&mut self) -> Result<()> {
        self.start_at(None).await
    }
    async fn start_at(&mut self, file: Option<&Path>) -> Result<()> {
        if self.disabled {
            bail!("Server is disabled; use /lsp connect");
        }
        if let Some(connection) = &self.connection {
            if !connection.reader.is_finished() && !self.capabilities.is_null() {
                return Ok(());
            }
            self.stop().await;
        }
        let root = self.root(file);
        let mut child = tokio::process::Command::new(&self.config.command)
            .args(&self.config.args)
            .envs(&self.config.env)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let writer = Arc::new(AsyncMutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Default::default();
        let diagnostics: Diagnostics = Default::default();
        let (p, d, w) = (pending.clone(), diagnostics.clone(), writer.clone());
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = match read_message(&mut reader).await {
                    Ok(message) => message,
                    Err(_) => break,
                };
                if let Some(method) = message["method"].as_str() {
                    if method == "textDocument/publishDiagnostics" {
                        if let Some(uri) = message["params"]["uri"].as_str() {
                            d.lock()
                                .unwrap()
                                .insert(uri.to_owned(), message["params"].clone());
                        }
                    } else if let Some(id) = message.get("id") {
                        let result = match method {
                            "workspace/configuration" => Value::Array(
                                message["params"]["items"]
                                    .as_array()
                                    .map(|items| items.iter().map(|_| Value::Null).collect())
                                    .unwrap_or_default(),
                            ),
                            "workspace/workspaceFolders" => json!([]),
                            "client/registerCapability"
                            | "client/unregisterCapability"
                            | "window/workDoneProgress/create" => Value::Null,
                            _ => {
                                let _ = write(&w,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Client method unavailable"}})).await;
                                continue;
                            }
                        };
                        let _ = write(&w, json!({"jsonrpc":"2.0","id":id,"result":result})).await;
                    }
                } else if let Some(id) = message["id"].as_u64() {
                    let responder = p.lock().unwrap().remove(&id);
                    if let Some(responder) = responder {
                        let _ = responder.send(message);
                    }
                }
            }
            p.lock().unwrap().clear();
        });
        self.connection = Some(Connection {
            writer,
            pending,
            diagnostics,
            reader,
            child,
        });
        let uri = file_uri(&root)?;
        let result = self.request("initialize",json!({"processId":std::process::id(),"rootUri":uri,
            "workspaceFolders":[{"uri":uri,"name":root.file_name().and_then(|s| s.to_str()).unwrap_or("workspace")}],
            "capabilities":{"general":{"positionEncodings":["utf-8","utf-16","utf-32"]},
                "textDocument":{"synchronization":{"dynamicRegistration":false},"publishDiagnostics":{"versionSupport":true},
                    "diagnostic":{"dynamicRegistration":false}},"workspace":{"configuration":true,"workspaceFolders":true}}
        })).await;
        match result {
            Ok(result) => {
                self.capabilities = result["capabilities"].clone();
                self.encoding = self.capabilities["positionEncoding"]
                    .as_str()
                    .unwrap_or("utf-16")
                    .to_string();
                if !["utf-8", "utf-16", "utf-32"].contains(&self.encoding.as_str()) {
                    self.stop().await;
                    bail!("Unsupported position encoding");
                }
                self.notify("initialized", json!({})).await?;
                self.error = None;
                Ok(())
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.stop().await;
                Err(error)
            }
        }
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| anyhow!("LSP not connected"))?;
        let (tx, rx) = oneshot::channel();
        connection.pending.lock().unwrap().insert(id, tx);
        let _guard = CancelRequest {
            id,
            writer: connection.writer.clone(),
            pending: connection.pending.clone(),
        };
        write(
            &connection.writer,
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .await?;
        let message = tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| anyhow!("LSP request timed out"))?
            .map_err(|_| anyhow!("LSP connection closed"))?;
        if message.get("error").is_some() {
            return Err(ResponseError(message["error"].clone()).into());
        }
        Ok(message["result"].clone())
    }
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        write(
            &self
                .connection
                .as_ref()
                .ok_or_else(|| anyhow!("LSP not connected"))?
                .writer,
            json!({"jsonrpc":"2.0","method":method,"params":params}),
        )
        .await
    }
    fn checked_path(&self, path: &str) -> Result<PathBuf> {
        anyhow::ensure!(
            !Path::new(path).is_absolute(),
            "Use a workspace-relative path"
        );
        let root = self.workspace.canonicalize()?;
        let path = root.join(path).canonicalize()?;
        anyhow::ensure!(path.starts_with(root), "Path is outside the workspace");
        Ok(path)
    }
    async fn sync(&mut self, path: Option<&Path>) -> Result<()> {
        let mut paths: Vec<_> = self.documents.keys().cloned().collect();
        if let Some(path) = path {
            let uri = file_uri(path)?;
            if !paths.contains(&uri) {
                paths.push(uri);
            }
        }
        for uri in paths {
            let disk_path = reqwest::Url::parse(&uri)?
                .to_file_path()
                .map_err(|_| anyhow!("Invalid file URI"))?;
            let text = match tokio::fs::read_to_string(&disk_path).await {
                Ok(text) => text,
                Err(_) => {
                    if self.documents.remove(&uri).is_some() {
                        self.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                            .await?;
                        self.connection
                            .as_ref()
                            .unwrap()
                            .diagnostics
                            .lock()
                            .unwrap()
                            .remove(&uri);
                    }
                    continue;
                }
            };
            if let Some(document) = self.documents.get(&uri) {
                if document.text == text {
                    continue;
                }
                let version = document.version + 1;
                let sync = &self.capabilities["textDocumentSync"];
                let change = sync
                    .as_u64()
                    .or_else(|| sync["change"].as_u64())
                    .unwrap_or(0);
                anyhow::ensure!(
                    change != 0,
                    "Server does not support document changes; restart it"
                );
                self.connection
                    .as_ref()
                    .unwrap()
                    .diagnostics
                    .lock()
                    .unwrap()
                    .remove(&uri);
                self.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]})).await?;
                self.documents.insert(uri, Document { text, version });
            } else {
                let language =
                    language(disk_path.extension().and_then(|e| e.to_str()).unwrap_or(""));
                self.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":language,"version":1,"text":text}})).await?;
                self.documents.insert(uri, Document { text, version: 1 });
            }
        }
        Ok(())
    }
    pub async fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        let result = self.call_inner(name, args).await;
        self.error = result.as_ref().err().map(ToString::to_string);
        result
    }
    async fn call_inner(&mut self, name: &str, args: &Value) -> Result<Value> {
        let path = args["path"]
            .as_str()
            .map(|p| self.checked_path(p))
            .transpose()?;
        self.start_at(path.as_deref()).await?;
        self.sync(path.as_deref()).await?;
        if name == "lsp_symbols" && args.get("query").is_some() {
            self.require("workspaceSymbolProvider")?;
            let result = self
                .request("workspace/symbol", json!({"query":args["query"]}))
                .await?;
            return Ok(self.normalize(result, None));
        }
        let path = path.ok_or_else(|| anyhow!("path is required for this request"))?;
        let uri = file_uri(&path)?;
        if name == "lsp_diagnostics" {
            return self.diagnostics(&uri).await;
        }
        let (method, capability) = match name {
            "lsp_hover" => ("textDocument/hover", "hoverProvider"),
            "lsp_definition" => ("textDocument/definition", "definitionProvider"),
            "lsp_references" => ("textDocument/references", "referencesProvider"),
            "lsp_symbols" => ("textDocument/documentSymbol", "documentSymbolProvider"),
            _ => bail!("Unknown LSP tool"),
        };
        self.require(capability)?;
        let mut params = json!({"textDocument":{"uri":uri}});
        if name != "lsp_symbols" {
            params["position"] = tool_position(
                &self.documents[&uri].text,
                args["line"].as_u64().unwrap_or(0),
                args["column"].as_u64().unwrap_or(0),
                &self.encoding,
            )?;
        }
        if name == "lsp_references" {
            params["context"] =
                json!({"includeDeclaration":args["include_declaration"].as_bool().unwrap_or(true)});
        }
        let result = self.request(method, params).await?;
        Ok(self.normalize(result, Some(&uri)))
    }
    fn require(&self, capability: &str) -> Result<()> {
        let value = &self.capabilities[capability];
        anyhow::ensure!(
            !value.is_null() && value != &Value::Bool(false),
            "Server does not support {capability}"
        );
        Ok(())
    }
    async fn diagnostics(&mut self, uri: &str) -> Result<Value> {
        let version = self.documents[uri].version;
        if self
            .capabilities
            .get("diagnosticProvider")
            .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
        {
            // ServerCancelled is part of the pull-diagnostics protocol, not a
            // broken connection. Retry only when the server explicitly asks.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut attempt = 0;
            let result = loop {
                let response = tokio::time::timeout_at(
                    deadline,
                    self.request(
                        "textDocument/diagnostic",
                        json!({"textDocument":{"uri":uri}}),
                    ),
                )
                .await;
                match response {
                    Ok(Ok(result)) => break result,
                    Ok(Err(error)) => {
                        let Some(response) = error.downcast_ref::<ResponseError>() else {
                            return Err(error);
                        };
                        if response.0["code"].as_i64() != Some(-32802) {
                            return Err(error);
                        }
                        if response.0["data"]["retriggerRequest"] != Value::Bool(true)
                            || attempt >= 5
                        {
                            return Ok(json!({"status":"pending","version":version,
                                "message":"Server cancelled diagnostics; current diagnostics are not available yet"}));
                        }
                        let delay = Duration::from_millis(100 << attempt);
                        attempt += 1;
                        if tokio::time::timeout_at(deadline, tokio::time::sleep(delay))
                            .await
                            .is_err()
                        {
                            return Ok(json!({"status":"pending","version":version,
                                "message":"Server is still preparing diagnostics; try again shortly"}));
                        }
                    }
                    Err(_) => {
                        return Ok(json!({"status":"pending","version":version,
                        "message":"Server is still preparing diagnostics; try again shortly"}))
                    }
                }
            };
            if result["kind"] == "full" {
                return Ok(
                    json!({"status":"current","version":version,"diagnostics":self.normalize(result["items"].clone(),Some(uri))}),
                );
            }
        }
        // Give asynchronous push diagnostics a bounded opportunity to arrive.
        for _ in 0..10 {
            let pushed = self
                .connection
                .as_ref()
                .unwrap()
                .diagnostics
                .lock()
                .unwrap()
                .get(uri)
                .cloned();
            if let Some(result) = pushed {
                if result["version"].as_i64() == Some(version) {
                    return Ok(
                        json!({"status":"current","version":version,"diagnostics":self.normalize(result["diagnostics"].clone(),Some(uri))}),
                    );
                }
                if result.get("version").is_none() {
                    return Ok(
                        json!({"status":"unverified","version":version,"message":"Server omitted document version; these diagnostics cannot confirm the current file","diagnostics":self.normalize(result["diagnostics"].clone(),Some(uri))}),
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(
            json!({"status":"pending","version":version,"message":"Diagnostics for this document version have not arrived"}),
        )
    }
    fn normalize(&self, mut value: Value, inherited: Option<&str>) -> Value {
        fn walk(server: &Server, value: &mut Value, inherited: Option<&str>) {
            if let Some(array) = value.as_array_mut() {
                for item in array {
                    walk(server, item, inherited);
                }
                return;
            }
            let Some(object) = value.as_object_mut() else {
                return;
            };
            let uri = object
                .get("uri")
                .or_else(|| object.get("targetUri"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| inherited.map(str::to_owned));
            if object.contains_key("line") && object.contains_key("character") {
                if let Some(uri) = &uri {
                    let text = server
                        .documents
                        .get(uri)
                        .map(|d| d.text.clone())
                        .or_else(|| {
                            reqwest::Url::parse(uri)
                                .ok()?
                                .to_file_path()
                                .ok()
                                .and_then(|p| std::fs::read_to_string(p).ok())
                        });
                    if let Some(text) = text {
                        let line = object["line"].as_u64().unwrap_or(0) as usize;
                        let units = object["character"].as_u64().unwrap_or(0) as usize;
                        let content = text.split('\n').nth(line).unwrap_or("");
                        let mut consumed = 0;
                        let mut column = 0;
                        for c in content.chars() {
                            let size = char_units(c, &server.encoding);
                            if consumed + size > units {
                                break;
                            }
                            consumed += size;
                            column += 1;
                        }
                        object.insert("line".into(), json!(line + 1));
                        object.remove("character");
                        object.insert("column".into(), json!(column + 1));
                    }
                }
            }
            for (key, item) in object.iter_mut() {
                if key == "uri" || key == "targetUri" {
                    if let Some(path) = item
                        .as_str()
                        .and_then(|u| reqwest::Url::parse(u).ok())
                        .and_then(|u| u.to_file_path().ok())
                    {
                        *item = json!(path
                            .strip_prefix(&server.workspace)
                            .unwrap_or(&path)
                            .to_string_lossy());
                    }
                } else {
                    walk(server, item, uri.as_deref());
                }
            }
        }
        walk(self, &mut value, inherited);
        value
    }
    pub async fn stop(&mut self) {
        if self.connection.is_some() {
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                self.request("shutdown", Value::Null),
            )
            .await;
            let _ = self.notify("exit", Value::Null).await;
        }
        if let Some(mut connection) = self.connection.take() {
            if tokio::time::timeout(Duration::from_millis(500), connection.child.wait())
                .await
                .is_err()
            {
                let _ = connection.child.kill().await;
            }
            connection.reader.abort();
            let _ = connection.reader.await;
        }
        self.documents.clear();
        self.capabilities = Value::Null;
    }
}
fn language(ext: &str) -> &str {
    match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        other => other,
    }
}
fn file_uri(path: &Path) -> Result<String> {
    reqwest::Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|_| anyhow!("Invalid file path"))
}
fn char_units(c: char, encoding: &str) -> usize {
    match encoding {
        "utf-8" => c.len_utf8(),
        "utf-32" => 1,
        _ => c.len_utf16(),
    }
}
fn tool_position(text: &str, line: u64, column: u64, encoding: &str) -> Result<Value> {
    anyhow::ensure!(line > 0 && column > 0, "line and column must be at least 1");
    let content = text
        .split('\n')
        .nth(line as usize - 1)
        .ok_or_else(|| anyhow!("Line is outside the file"))?
        .trim_end_matches('\r');
    anyhow::ensure!(
        column as usize <= content.chars().count() + 1,
        "Column is outside the line"
    );
    let character: usize = content
        .chars()
        .take(column as usize - 1)
        .map(|c| char_units(c, encoding))
        .sum();
    Ok(json!({"line":line-1,"character":character}))
}
async fn read_message<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value> {
    let mut size = None;
    loop {
        let mut line = String::new();
        anyhow::ensure!(reader.read_line(&mut line).await? > 0, "LSP closed stdout");
        if line == "\r\n" || line == "\n" {
            break;
        }
        anyhow::ensure!(line.len() < 8192, "LSP header too large");
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                size = Some(value.trim().parse::<usize>()?);
            }
        }
    }
    let size = size.ok_or_else(|| anyhow!("Missing Content-Length"))?;
    anyhow::ensure!(size <= 16 * 1024 * 1024, "LSP message too large");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_columns_are_converted_for_each_encoding() {
        assert_eq!(
            tool_position("a😀Жz", 1, 4, "utf-16").unwrap(),
            json!({"line":0,"character":4})
        );
        assert_eq!(
            tool_position("a😀Жz", 1, 4, "utf-8").unwrap()["character"],
            7
        );
        assert_eq!(
            tool_position("a😀Жz", 1, 4, "utf-32").unwrap()["character"],
            3
        );
        assert!(tool_position("abc", 0, 1, "utf-16").is_err());
        assert!(tool_position("abc", 1, 5, "utf-16").is_err());
    }
    #[tokio::test]
    async fn framing_handles_bytes_and_rejects_oversize() {
        let body = json!({"result":"😀"}).to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        assert_eq!(
            read_message(&mut frame.as_bytes()).await.unwrap()["result"],
            "😀"
        );
        assert!(
            read_message(&mut "Content-Length: 20000000\r\n\r\n".as_bytes())
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[tokio::test]
    async fn pull_diagnostics_retries_server_cancellation_with_bounded_wait() {
        let root = std::env::temp_dir().join(format!(
            "takiza-lsp-retry-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        for (mode, expected_calls, expected_status) in [
            ("retry", 3, "current"),
            ("always", 6, "pending"),
            ("no-retry", 1, "pending"),
            ("error", 1, "error"),
        ] {
            let log = root.join(format!("{mode}.jsonl"));
            let mut server = Server::new(
                LspConfig {
                    command: "python3".into(),
                    args: vec![
                        format!(
                            "{}/tests/fixtures/integration_server.py",
                            env!("CARGO_MANIFEST_DIR")
                        ),
                        "lsp".into(),
                        "--pull-diagnostics".into(),
                        mode.into(),
                        "--log".into(),
                        log.to_string_lossy().into(),
                    ],
                    env: Default::default(),
                    languages: vec!["rust".into()],
                    root_markers: vec![],
                },
                root.clone(),
            );
            let result = server
                .call("lsp_diagnostics", &json!({"path":"main.rs"}))
                .await;
            if expected_status == "error" {
                assert!(result.unwrap_err().to_string().contains("-32603"));
            } else {
                let result = result.unwrap();
                assert_eq!(result["status"], expected_status, "{mode}");
                if expected_status == "pending" {
                    assert!(result.get("diagnostics").is_none());
                }
                assert!(
                    server.error.is_none(),
                    "Cancellation must not mark the server unavailable"
                );
            }
            server.stop().await;
            let messages: Vec<Value> = std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let requests: Vec<_> = messages
                .iter()
                .filter(|m| m["method"] == "textDocument/diagnostic")
                .collect();
            assert_eq!(requests.len(), expected_calls, "{mode}");
            for pair in requests.windows(2) {
                assert_ne!(pair[0]["id"], pair[1]["id"]);
                assert_eq!(pair[0]["params"], pair[1]["params"]);
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn syncs_shell_edits_unicode_definitions_references_and_rewind() {
        let root = std::env::temp_dir().join(format!(
            "takiza-lsp-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        std::fs::write(&path, "a😀Жz broken").unwrap();
        let config = LspConfig {
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
            root_markers: vec!["Cargo.toml".into()],
        };
        let mut server = Server::new(config, root.clone());
        let args = json!({"path":"main.rs","line":1,"column":4});
        let hover = server.call("lsp_hover", &args).await.unwrap();
        assert_eq!(hover["range"]["start"], json!({"line":1,"column":4}));
        assert!(hover["contents"]["value"]
            .as_str()
            .unwrap()
            .starts_with("1:"));
        for tool in ["lsp_definition", "lsp_references"] {
            let result = server.call(tool, &args).await.unwrap();
            assert_eq!(result[0]["uri"], "main.rs");
            assert_eq!(result[0]["range"]["start"]["column"], 4);
        }
        let diagnostics = server.call("lsp_diagnostics", &args).await.unwrap();
        assert_eq!(diagnostics["status"], "current");
        assert_eq!(diagnostics["diagnostics"].as_array().unwrap().len(), 1);
        std::fs::write(&path, "a😀Жz fixed").unwrap();
        let hover = server.call("lsp_hover", &args).await.unwrap();
        assert!(hover["contents"]["value"]
            .as_str()
            .unwrap()
            .starts_with("2:"));
        let diagnostics = server.call("lsp_diagnostics", &args).await.unwrap();
        assert_eq!(diagnostics["version"], 2);
        assert_eq!(diagnostics["diagnostics"], json!([]));
        assert!(server.call("lsp_symbols", &args).await.unwrap().is_array());
        assert_eq!(
            server
                .call("lsp_symbols", &json!({"query":"example"}))
                .await
                .unwrap(),
            json!([])
        );
        server.stop().await;
        std::fs::write(&path, "restored").unwrap();
        let hover = server
            .call("lsp_hover", &json!({"path":"main.rs","line":1,"column":1}))
            .await
            .unwrap();
        assert_eq!(hover["contents"]["value"], "1:restored");
        assert!(server.checked_path("../outside.rs").is_err());
        server.stop().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn stale_push_diagnostics_remain_pending() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let config = LspConfig {
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
        };
        let mut server = Server::new(config, root);
        server.start().await.unwrap();
        let uri = "file:///fixture.rs";
        server.documents.insert(
            uri.into(),
            Document {
                text: "fixed".into(),
                version: 2,
            },
        );
        server
            .connection
            .as_ref()
            .unwrap()
            .diagnostics
            .lock()
            .unwrap()
            .insert(uri.into(), json!({"version":1,"diagnostics":[]}));
        assert_eq!(server.diagnostics(uri).await.unwrap()["status"], "pending");
        server
            .connection
            .as_ref()
            .unwrap()
            .diagnostics
            .lock()
            .unwrap()
            .insert(uri.into(), json!({"diagnostics":[]}));
        assert_eq!(
            server.diagnostics(uri).await.unwrap()["status"],
            "unverified"
        );
        server.stop().await;
    }
}

#[cfg(test)]
mod real_server_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires explicitly installed language servers; set TAKIZA_TEST_LSP_BIN_DIR and TAKIZA_TEST_RUST_ANALYZER"]
    async fn installed_rust_typescript_javascript_python_servers() {
        let bin = std::env::var("TAKIZA_TEST_LSP_BIN_DIR").unwrap();
        let rust = std::env::var("TAKIZA_TEST_RUST_ANALYZER").unwrap_or_default();
        let cases = [
            (
                "rust",
                rust,
                vec![],
                "main.rs",
                "fn hello() -> i32 { 42 }\nfn main() { let answer = hello(); }\n",
                2,
                26,
                "fn hello() -> i32 { \"broken\" }\nfn main() { let answer = hello(); }\n",
            ),
            (
                "typescript",
                format!("{bin}/typescript-language-server"),
                vec!["--stdio".to_string()],
                "main.ts",
                "function hello(): number { return 42; }\nconst answer = hello();\n",
                2,
                16,
                "function hello(): number { return \"broken\"; }\nconst answer = hello();\n",
            ),
            (
                "javascript",
                format!("{bin}/typescript-language-server"),
                vec!["--stdio".to_string()],
                "main.js",
                "// @ts-check\nfunction hello() { return 42; }\nconst answer = hello();\n",
                3,
                16,
                "// @ts-check\n/** @type {number} */\nconst answer = \"broken\";\n",
            ),
            (
                "python",
                format!("{bin}/pyright-langserver"),
                vec!["--stdio".to_string()],
                "main.py",
                "def hello() -> int:\n    return 42\nanswer = hello()\n",
                3,
                11,
                "def hello() -> int:\n    return \"broken\"\nanswer = hello()\n",
            ),
        ];
        for (lang, command, args, file, text, line, column, broken) in cases {
            if lang == "rust" && command.is_empty() {
                println!("SKIP rust: TAKIZA_TEST_RUST_ANALYZER missing");
                continue;
            }
            let root = std::env::temp_dir().join(format!(
                "takiza-real-lsp-{lang}-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            std::fs::create_dir_all(&root).unwrap();
            if lang == "rust" {
                std::fs::create_dir(root.join("src")).unwrap();
                std::fs::write(
                    root.join("Cargo.toml"),
                    "[package]\nname=\"lsp-fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
                )
                .unwrap();
            }
            if lang == "typescript" || lang == "javascript" {
                std::fs::write(
                    root.join("tsconfig.json"),
                    "{\"compilerOptions\":{\"allowJs\":true,\"checkJs\":true,\"noEmit\":true}}",
                )
                .unwrap();
            }
            #[cfg(unix)]
            if lang == "typescript" || lang == "javascript" {
                std::os::unix::fs::symlink(
                    Path::new(&bin).parent().unwrap(),
                    root.join("node_modules"),
                )
                .unwrap();
            }
            let path = if lang == "rust" {
                format!("src/{file}")
            } else {
                file.to_owned()
            };
            std::fs::write(root.join(&path), text).unwrap();
            let mut server = Server::new(
                LspConfig {
                    command,
                    args,
                    env: Default::default(),
                    languages: vec![lang.into()],
                    root_markers: vec!["Cargo.toml".into(), "tsconfig.json".into()],
                },
                root.clone(),
            );
            let positioned = json!({"path":path,"line":line,"column":column});
            let mut hover = Value::Null;
            for _ in 0..40 {
                hover = server.call("lsp_hover", &positioned).await.unwrap();
                if !hover.is_null() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            assert!(!hover.is_null(), "{lang}: no hover");
            let definitions = server.call("lsp_definition", &positioned).await.unwrap();
            assert!(!definitions.is_null(), "{lang}: no definition");
            let references = server.call("lsp_references", &positioned).await.unwrap();
            assert!(
                references.as_array().is_some_and(|v| !v.is_empty()),
                "{lang}: no references"
            );
            assert!(
                !server
                    .call("lsp_symbols", &positioned)
                    .await
                    .unwrap()
                    .is_null(),
                "{lang}: no symbols"
            );
            std::fs::write(root.join(&path), broken).unwrap();
            let mut diagnostics = Value::Null;
            for _ in 0..40 {
                diagnostics = server
                    .call("lsp_diagnostics", &json!({"path":path}))
                    .await
                    .unwrap();
                if diagnostics["diagnostics"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            assert!(
                diagnostics["diagnostics"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty()),
                "{lang}: no diagnostics after disk edit: {diagnostics}"
            );
            println!("PASS real LSP {lang}: hover, definition, references, symbols, diagnostics after disk edit ({})",diagnostics["status"]);
            server.stop().await;
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
