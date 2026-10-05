use crate::agent::AgentEvent;
use crate::config::Config;
use crate::tools::{get_tool_definitions, ToolCall};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn messages_for_api(messages: &[ChatMessage]) -> Vec<Value> {
    messages.iter().map(|message| {
        let mut value = serde_json::to_value(message).expect("ChatMessage serialization");
        value.as_object_mut().unwrap().remove("image_urls");
        if !message.image_urls.is_empty() {
            let mut content = vec![json!({"type": "text", "text": message.content.as_deref().unwrap_or("")})];
            content.extend(message.image_urls.iter().map(|url| json!({"type": "image_url", "image_url": {"url": url}})));
            value["content"] = Value::Array(content);
        }
        value
    }).collect()
}

#[cfg(test)]
mod image_message_tests {
    use super::*;

    #[test]
    fn images_use_multimodal_content_and_text_only_sessions_stay_compatible() {
        let old = json!({"role": "user", "content": "describe"});
        let mut message: ChatMessage = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(messages_for_api(&[message.clone()]), [old]);
        message.image_urls.push("data:image/png;base64,aGVsbG8=".into());
        let stored = serde_json::to_string(&message).unwrap();
        let restored: ChatMessage = serde_json::from_str(&stored).unwrap();
        let wire = messages_for_api(&[restored]);
        assert!(wire[0].get("image_urls").is_none());
        assert_eq!(wire[0]["content"][0], json!({"type": "text", "text": "describe"}));
        assert_eq!(wire[0]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}}));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: LlmFunctionCall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub enum LlmResponse {
    Message(String),
    Empty { finish_reason: Option<String> },
    ToolCalls(Vec<ToolCall>, Option<String>),
}

pub fn supports_reasoning_effort(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("o1")
        || m.contains("o3")
        || m.contains("o4")
        || m.contains("reasoning")
        || m.contains("reasoner")
        || m.contains("deepseek-r1")
        || m.contains("claude-3-7")
        || m.contains("claude-3.7")
        || m.contains("thinking")
}

#[derive(Debug, PartialEq, Eq)]
enum ThinkPhase {
    Initial,
    Thinking,
    Content,
}

fn append_tool_arguments(buffer: &mut String, chunk: &Value) -> Result<()> {
    match chunk {
        Value::Null => {},
        Value::String(fragment) => buffer.push_str(fragment),
        Value::Object(_) => {
            // Some OpenAI-compatible gateways send parsed JSON instead of a string.
            // Treat it as a complete snapshot, never discard it with as_str().
            let snapshot = chunk.to_string();
            anyhow::ensure!(buffer.is_empty() || buffer == &snapshot,
                "Provider mixed incompatible tool argument formats");
            *buffer = snapshot;
        },
        _ => anyhow::bail!("Provider returned unsupported tool argument format"),
    }
    Ok(())
}

fn checked_tool_call(id: String, name: String, arguments: String) -> Result<ToolCall> {
    validate_tool_call(id, name, arguments)
        .map_err(|error| InvalidToolCall(error.to_string()).into())
}

fn validate_tool_call(id: String, name: String, arguments: String) -> Result<ToolCall> {
    let definitions = get_tool_definitions();
    let function = definitions.as_array().unwrap().iter()
        .find(|tool| tool["function"]["name"].as_str() == Some(name.as_str()))
        .map(|tool| &tool["function"])
        .ok_or_else(|| anyhow::anyhow!("Provider requested an unknown tool"))?;
    let schema = &function["parameters"];
    let required = schema["required"].as_array();
    let arguments = if arguments.trim().is_empty() {
        anyhow::ensure!(required.is_none_or(|fields| fields.is_empty()),
            "Provider returned an incomplete {name} call: missing JSON arguments. This call was not executed; resend the request or choose another model.");
        "{}".to_string()
    } else { arguments };
    let parsed: Value = serde_json::from_str(&arguments)
        .map_err(|_| anyhow::anyhow!("Provider returned invalid JSON arguments for {name}. This call was not executed; resend the request or choose another model."))?;
    anyhow::ensure!(parsed.is_object(), "Provider returned non-object arguments for {name}");
    if let Some(required) = required {
        for field in required.iter().filter_map(Value::as_str) {
            anyhow::ensure!(parsed.get(field).is_some(), "Provider returned an incomplete {name} call: missing required argument {field}");
        }
    }
    Ok(ToolCall { id, name, arguments })
}

pub struct StreamThinkParser {
    phase: ThinkPhase,
    buffer: String,
    close_tag: String,
    in_backtick: bool,
}

impl StreamThinkParser {
    pub fn new() -> Self {
        Self {
            phase: ThinkPhase::Initial,
            buffer: String::new(),
            close_tag: "</think>".to_string(),
            in_backtick: false,
        }
    }

    pub fn feed(&mut self, delta: &str) -> (Vec<String>, Vec<String>) {
        let mut thoughts = Vec::new();
        let mut content = Vec::new();

        self.buffer.push_str(delta);

        loop {
            match self.phase {
                ThinkPhase::Initial => {
                    let trimmed_start = self.buffer.trim_start();
                    if trimmed_start.is_empty() {
                        if self.buffer.len() > 100 {
                            content.push(std::mem::take(&mut self.buffer));
                            self.phase = ThinkPhase::Content;
                        }
                        break;
                    }

                    let candidates = [
                        ("<think>", "</think>"),
                        ("<thought>", "</thought>"),
                        ("<reasoning>", "</reasoning>"),
                    ];

                    let mut matched = None;
                    let mut is_prefix = false;

                    for (open, close) in &candidates {
                        if trimmed_start.starts_with(open) {
                            matched = Some((*open, *close));
                            break;
                        } else if open.starts_with(trimmed_start) || (trimmed_start.starts_with('<') && open.starts_with(&trimmed_start[..trimmed_start.len().min(open.len())])) {
                            is_prefix = true;
                        }
                    }

                    if let Some((open, close)) = matched {
                        self.close_tag = close.to_string();
                        self.phase = ThinkPhase::Thinking;
                        self.in_backtick = false;
                        let ws_len = self.buffer.len() - trimmed_start.len();
                        self.buffer.drain(..ws_len + open.len());
                    } else if is_prefix && trimmed_start.len() < 12 {
                        break;
                    } else {
                        self.phase = ThinkPhase::Content;
                        if !self.buffer.is_empty() {
                            content.push(std::mem::take(&mut self.buffer));
                        }
                        break;
                    }
                }
                ThinkPhase::Thinking => {
                    let close = &self.close_tag;
                    let close_len = close.len();

                    let mut found_pos = None;
                    let mut idx = 0;
                    let bytes = self.buffer.as_bytes();

                    while idx < bytes.len() {
                        if bytes[idx] == b'`' {
                            self.in_backtick = !self.in_backtick;
                            idx += 1;
                            continue;
                        }

                        if !self.in_backtick && self.buffer[idx..].starts_with(close) {
                            found_pos = Some(idx);
                            break;
                        }
                        idx += 1;
                    }

                    if let Some(pos) = found_pos {
                        let thought_part = self.buffer[..pos].to_string();
                        if !thought_part.is_empty() {
                            thoughts.push(thought_part);
                        }
                        self.buffer.drain(..pos + close_len);
                        self.phase = ThinkPhase::Content;
                    } else {
                        let mut safe_len = self.buffer.len();
                        if !self.in_backtick {
                            for prefix_len in (1..close_len).rev() {
                                if prefix_len <= self.buffer.len() && close.starts_with(&self.buffer[self.buffer.len() - prefix_len..]) {
                                    safe_len = self.buffer.len() - prefix_len;
                                    break;
                                }
                            }
                        }

                        if safe_len > 0 {
                            let chunk: String = self.buffer.drain(..safe_len).collect();
                            thoughts.push(chunk);
                        }
                        break;
                    }
                }
                ThinkPhase::Content => {
                    if !self.buffer.is_empty() {
                        content.push(std::mem::take(&mut self.buffer));
                    }
                    break;
                }
            }
        }

        (thoughts, content)
    }

    pub fn finish(self) -> (Option<String>, Option<String>) {
        if self.buffer.is_empty() {
            return (None, None);
        }
        match self.phase {
            ThinkPhase::Initial | ThinkPhase::Thinking => (Some(self.buffer), None),
            ThinkPhase::Content => (None, Some(self.buffer)),
        }
    }
}

pub struct LlmClient {
    client: Client,
    config: Config,
}

#[derive(Debug)]
struct StreamInterrupted(String);

impl std::fmt::Display for StreamInterrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "LLM stream interrupted: {}", self.0)
    }
}

impl std::error::Error for StreamInterrupted {}

#[derive(Debug)]
struct InvalidToolCall(String);

impl std::fmt::Display for InvalidToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidToolCall {}

fn reported_tokens(value: &Value) -> Option<u64> {
    let usage = value.get("usage")?;
    usage.get("total_tokens").and_then(Value::as_u64).or_else(|| {
        let input = usage.get("prompt_tokens").or_else(|| usage.get("input_tokens"))?.as_u64()?;
        let output = usage.get("completion_tokens").or_else(|| usage.get("output_tokens"))?.as_u64()?;
        input.checked_add(output)
    })
}

impl LlmClient {
    pub fn new(mut config: Config) -> Self {
        config.base_url = crate::config::normalize_base_url(&config.base_url);
        let mut builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(120));

        if let Some(ref proxy_str) = config.proxy {
            if let Ok(proxy) = reqwest::Proxy::all(proxy_str) {
                builder = builder.proxy(proxy);
            }
        }

        Self {
            client: builder.build().unwrap_or_default(),
            config,
        }
    }

    pub async fn generate_title(&self, prompt: &str) -> Result<String> {
        let mut last_error = anyhow::anyhow!("No title returned");
        for attempt in 1..=2 {
            match self.generate_title_once(prompt).await {
                Ok(title) => return Ok(title),
                Err(error) => {
                    crate::logger::log_warn("Chat title", &format!("Attempt {attempt}: {error:#}"));
                    last_error = error;
                }
            }
        }
        Err(last_error)
    }

    async fn generate_title_once(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let prompt_clean = prompt.replace('\n', " ").chars().take(300).collect::<String>();
        let messages = vec![
            ChatMessage {
                image_urls: Vec::new(),
                role: "system".to_string(),
                content: Some("You generate concise conversation titles. Return ONLY a 3-6 word title summarizing the user request. No quotes, no markdown, no punctuation, same language as user.".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                image_urls: Vec::new(),
                role: "user".to_string(),
                content: Some(prompt_clean),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }
        ];

        let body = json!({
            "model": self.config.model,
            "messages": messages_for_api(&messages),
            "max_tokens": 4096,
            "stream": false,
            "temperature": 0.4,
        });

        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp = req.timeout(std::time::Duration::from_secs(60)).json(&body).send().await
            .context("Title request failed")?;
        let status = resp.status();
        if !status.is_success() { anyhow::bail!("Title provider returned HTTP {status}"); }
        let json_val: Value = resp.json().await.context("Invalid title response JSON")?;
        let choice_msg = json_val["choices"].get(0).and_then(|choice| choice.get("message"))
            .context("Title response has no message")?;

        let raw = choice_msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        let mut clean = raw.trim().to_string();
        if let Some(pos) = clean.rfind("</think>") {
            clean = clean[pos + 8..].trim().to_string();
        } else if let Some(pos) = clean.rfind("</thought>") {
            clean = clean[pos + 10..].trim().to_string();
        } else if let Some(pos) = clean.rfind("</reasoning>") {
            clean = clean[pos + 12..].trim().to_string();
        }

        let first_line = clean
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim_matches('*')
            .trim_matches('`')
            .trim_end_matches('.')
            .to_string();

        if first_line.is_empty() {
            anyhow::bail!("Model returned an empty title (finish_reason: {})", json_val["choices"][0]["finish_reason"].as_str().unwrap_or("unknown"));
        }
        Ok(first_line)
    }

    #[allow(dead_code)]
    pub async fn chat_step(&self, messages: &[ChatMessage]) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let tools = get_tool_definitions();

        let mut body = json!({
            "model": self.config.model,
            "messages": messages_for_api(&messages),
            "tools": tools,
            "tool_choice": "auto",
            "max_tokens": 8192,
        });

        if supports_reasoning_effort(&self.config.model) {
            if let Some(ref effort) = self.config.effort {
                if !effort.is_empty() {
                    body["reasoning_effort"] = json!(effort.to_lowercase());
                }
            }
        }

        let mut attempts = 0;
        let (status, text) = loop {
            attempts += 1;
            let mut req = self
                .client
                .post(&url)
                .header("Content-Type", "application/json");

            if !self.config.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", self.config.api_key));
            }

            let resp = req
                .json(&body)
                .send()
                .await
                .context(format!("Failed to connect to LLM at {}", url))?;

            let status = resp.status();
            let text = resp.text().await.context("Failed to read response body")?;

            if status.as_u16() == 429 && attempts < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                continue;
            }

            break (status, text);
        };

        if !status.is_success() {
            let err_details = format!("API Error (status {status}) from {url}: {text}");
            crate::logger::log_error("LLM", &err_details);
            anyhow::bail!("API Error (status {}): {}", status, text);
        }

        let json_val: Value = serde_json::from_str(&text)
            .context(format!("Failed to parse JSON response: {}", text))?;

        let choice = json_val["choices"]
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("No choices returned in API response"))?;

        let message = &choice["message"];
        let content = message["content"].as_str().map(|s| s.to_string());
        let reasoning = message["reasoning"].as_str().map(|s| s.to_string());

        if let Some(tool_calls_val) = message["tool_calls"].as_array() {
            if !tool_calls_val.is_empty() {
                let mut tool_calls = Vec::new();
                for tc in tool_calls_val {
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let mut arguments = String::new();
                    append_tool_arguments(&mut arguments, &tc["function"]["arguments"])?;
                    tool_calls.push(checked_tool_call(id, name, arguments)?);
                }
                let thought = content.or(reasoning);
                return Ok(LlmResponse::ToolCalls(tool_calls, thought));
            }
        }

        Ok(final_message(content.unwrap_or_default(), choice["finish_reason"].as_str().map(str::to_owned)))
    }

    pub async fn chat_step_stream(
        &self,
        messages: &[ChatMessage],
        event_tx: &mpsc::Sender<AgentEvent>,
        cancel_token: &CancellationToken,
    ) -> Result<LlmResponse> {
        let mut retry_messages = messages.to_vec();
        for attempt in 1..=3 {
            match self.chat_step_stream_once(&retry_messages, event_tx, cancel_token).await {
                Err(error) if (error.is::<StreamInterrupted>() || error.is::<InvalidToolCall>())
                    && !cancel_token.is_cancelled() => {
                    crate::logger::log_warn("LLM", &format!("Stream attempt {attempt}/3 failed: {error:#}"));
                    if attempt == 3 {
                        return Err(error).context("LLM response failed after 3 attempts");
                    }
                    let repairing_tool = error.is::<InvalidToolCall>();
                    if repairing_tool {
                        retry_messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: Some(format!("Your previous tool call was rejected before execution: {error}. Continue the original task by sending a complete function call using only the declared tool names and valid JSON object arguments with all required fields. No tools from that response were executed. Do not invent missing arguments or report a successful search.")),
                            image_urls: Vec::new(), tool_calls: None, tool_call_id: None, name: None,
                        });
                    }
                    let _ = event_tx.send(AgentEvent::StreamRetry).await;
                    let _ = event_tx.send(AgentEvent::StatusUpdate(format!(
                        "{} retrying ({attempt}/2)...",
                        if repairing_tool { "Invalid tool call;" } else { "Connection interrupted;" }
                    ))).await;
                    tokio::select! {
                        _ = cancel_token.cancelled() => anyhow::bail!("Request cancelled"),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)) => {}
                    }
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn chat_step_stream_once(
        &self,
        messages: &[ChatMessage],
        event_tx: &mpsc::Sender<AgentEvent>,
        cancel_token: &CancellationToken,
    ) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let tools = get_tool_definitions();

        let mut body = json!({
            "model": self.config.model,
            "messages": messages_for_api(&messages),
            "tools": tools,
            "tool_choice": "auto",
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": 8192,
        });

        if supports_reasoning_effort(&self.config.model) {
            if let Some(ref effort) = self.config.effort {
                if !effort.is_empty() {
                    body["reasoning_effort"] = json!(effort.to_lowercase());
                }
            }
        }

        let mut attempts = 0;
        let resp = loop {
            if cancel_token.is_cancelled() {
                anyhow::bail!("Request cancelled");
            }
            attempts += 1;
            let mut req = self
                .client
                .post(&url)
                .header("Content-Type", "application/json");

            if !self.config.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", self.config.api_key));
            }

            let res = req
                .json(&body)
                .send()
                .await;

            let r = match res {
                Ok(resp) => resp,
                Err(e) => {
                    if attempts < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        continue;
                    }
                    let proxy_desc = match &self.config.proxy {
                        Some(p) => format!(" (via proxy {p})"),
                        None => " (direct connection, no proxy)".to_string(),
                    };
                    let err_details = format!("Failed to connect to LLM at {url}{proxy_desc}: {e:#}");
                    crate::logger::log_error("LLM", &err_details);
                    return Err(e).context(format!("Failed to connect to LLM at {url}{proxy_desc}"));
                }
            };

            let status = r.status();
            if status.as_u16() == 429 && attempts < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                continue;
            }

            if !status.is_success() {
                let err_text = r.text().await.unwrap_or_default();
                if matches!(status.as_u16(), 400 | 422) && body.get("stream_options").is_some()
                    && (err_text.contains("stream_options") || err_text.contains("include_usage")) {
                    body.as_object_mut().unwrap().remove("stream_options");
                    continue;
                }
                let err_details = format!("API Error (status {status}) from {url}: {err_text}");
                crate::logger::log_error("LLM", &err_details);
                anyhow::bail!("API Error (status {}): {}", status, err_text);
            }

            break r;
        };

        struct InFlightTool {
            id: String,
            name: String,
            arguments: String,
        }

        let mut think_parser = StreamThinkParser::new();
        let mut accum_tools: Vec<InFlightTool> = Vec::new();
        let mut accumulated_content = String::new();
        let mut accumulated_thought = String::new();

        let mut byte_stream = resp.bytes_stream();
        let mut sse_buffer = Vec::<u8>::new();
        let mut finish_reason = None;
        let mut done_stream = false;
        let mut token_usage = None;

        while !done_stream {
            let chunk_opt = tokio::select! {
                _ = cancel_token.cancelled() => {
                    anyhow::bail!("Request cancelled");
                }
                c = byte_stream.next() => c,
            };

            let chunk_bytes = match chunk_opt {
                Some(Ok(bytes)) => Some(bytes),
                Some(Err(e)) => {
                    // Some providers close the transport after the final choice.
                    if finish_reason.is_some() { break; }
                    let _ = event_tx.send(AgentEvent::TokenUsage(token_usage)).await;
                    return Err(StreamInterrupted(format!("Error reading stream chunk from LLM: {e:#}")).into());
                }
                None => {
                    if sse_buffer.is_empty() { break; }
                    // Process the last SSE line even if EOF arrives without a newline.
                    sse_buffer.push(b'\n');
                    None
                }
            };
            if let Some(bytes) = chunk_bytes.as_ref() { sse_buffer.extend_from_slice(bytes); }

            while let Some(newline_pos) = sse_buffer.iter().position(|byte| *byte == b'\n') {
                // Decode complete lines, so a UTF-8 character split across network chunks is preserved.
                let line = String::from_utf8_lossy(&sse_buffer[..newline_pos]).trim_end_matches('\r').to_string();
                sse_buffer.drain(..=newline_pos);

                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with(':') {
                    continue;
                }

                if let Some(data) = trimmed.strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        done_stream = true;
                        break;
                    }

                    let json_val: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if let Some(tokens) = reported_tokens(&json_val) { token_usage = Some(tokens); }

                    let choice = match json_val.get("choices").and_then(|c| c.as_array()).and_then(|arr| arr.get(0)) {
                        Some(c) => c,
                        None => continue,
                    };

                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        finish_reason = Some(reason.to_string());
                    }
                    let delta = match choice.get("delta") {
                        Some(d) => d,
                        None => continue,
                    };

                    // 1. Check reasoning / thought field (DeepSeek, OpenRouter, etc.)
                    if let Some(reasoning) = delta.get("reasoning").or_else(|| delta.get("reasoning_content")).and_then(|r| r.as_str()) {
                        if !reasoning.is_empty() {
                            accumulated_thought.push_str(reasoning);
                            let _ = event_tx.send(AgentEvent::ThoughtToken(reasoning.to_string())).await;
                        }
                    }

                    // 2. Check content field using StreamThinkParser
                    if let Some(content_str) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content_str.is_empty() {
                            let (th_tokens, as_tokens) = think_parser.feed(content_str);
                            for th in th_tokens {
                                accumulated_thought.push_str(&th);
                                let _ = event_tx.send(AgentEvent::ThoughtToken(th)).await;
                            }
                            for as_t in as_tokens {
                                accumulated_content.push_str(&as_t);
                                let _ = event_tx.send(AgentEvent::AssistantToken(as_t)).await;
                            }
                        }
                    }

                    // 3. Check tool_calls delta
                    if let Some(tool_calls_arr) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for (position, tc) in tool_calls_arr.iter().enumerate() {
                            let idx = if let Some(index) = tc.get("index").and_then(Value::as_u64) {
                                anyhow::ensure!(index < 128, "Provider returned an invalid tool-call index");
                                index as usize
                            } else if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
                                accum_tools.iter().position(|tool| tool.id == id).unwrap_or(accum_tools.len())
                            } else if tool_calls_arr.len() > 1 { position }
                            else {
                                anyhow::ensure!(accum_tools.len() <= 1, "Provider omitted the index for an ambiguous tool-call fragment");
                                0
                            };
                            while accum_tools.len() <= idx {
                                accum_tools.push(InFlightTool {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                });
                            }
                            if let Some(id) = tc.get("id").and_then(|s| s.as_str()) {
                                if !id.is_empty() {
                                    if accum_tools[idx].id.is_empty() {
                                        accum_tools[idx].id = id.to_string();
                                    } else if !accum_tools[idx].id.contains(id) {
                                        accum_tools[idx].id.push_str(id);
                                    }
                                }
                            }
                            if let Some(func) = tc.get("function") {
                                if let Some(name) = func.get("name").and_then(|s| s.as_str()) {
                                    if !name.is_empty() {
                                        if accum_tools[idx].name.is_empty() {
                                            accum_tools[idx].name = name.to_string();
                                        } else if !accum_tools[idx].name.contains(name) {
                                            accum_tools[idx].name.push_str(name);
                                        }
                                    }
                                }
                                if let Some(args) = func.get("arguments") {
                                    append_tool_arguments(&mut accum_tools[idx].arguments, args)?;
                                }
                            }
                        }
                    }
                }
            }
            if chunk_bytes.is_none() { break; }
        }

        let _ = event_tx.send(AgentEvent::TokenUsage(token_usage)).await;
        if !done_stream && finish_reason.is_none() {
            return Err(StreamInterrupted("connection closed before finish_reason or [DONE]".into()).into());
        }

        let (rem_th, rem_as) = think_parser.finish();
        if let Some(th) = rem_th {
            accumulated_thought.push_str(&th);
            let _ = event_tx.send(AgentEvent::ThoughtToken(th)).await;
        }
        if let Some(as_t) = rem_as {
            accumulated_content.push_str(&as_t);
            let _ = event_tx.send(AgentEvent::AssistantToken(as_t)).await;
        }

        // Post-processing guard: ensure no stray think envelope remains in content
        if let Some(stripped) = accumulated_content.strip_prefix("</think>") {
            accumulated_content = stripped.trim_start().to_string();
        }

        let valid_tools: Vec<ToolCall> = accum_tools
            .into_iter()
            .filter(|t| !t.name.trim().is_empty())
            .enumerate()
            .map(|(i, t)| checked_tool_call(
                if t.id.trim().is_empty() { format!("call_{}", i) } else { t.id },
                t.name, t.arguments,
            ))
            .collect::<Result<Vec<_>>>()?;

        if !valid_tools.is_empty() {
            let thought = if !accumulated_thought.trim().is_empty() {
                Some(accumulated_thought)
            } else if !accumulated_content.trim().is_empty() {
                Some(accumulated_content)
            } else {
                None
            };
            return Ok(LlmResponse::ToolCalls(valid_tools, thought));
        }

        // Reasoning is not a final answer; a reasoning-only completion is retried.
        Ok(final_message(accumulated_content, finish_reason))
    }
}

fn final_message(content: String, finish_reason: Option<String>) -> LlmResponse {
    if content.trim().is_empty() { LlmResponse::Empty { finish_reason } }
    else { LlmResponse::Message(content) }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn stream_fixture(responses: Vec<(String, bool)>) -> (LlmClient, tokio::task::JoinHandle<usize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = 0;
            for (body, truncated) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests += 1;
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0u8; 4096];
                    let count = socket.read(&mut bytes).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&bytes[..count]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") { break end + 4; }
                };
                let length = String::from_utf8_lossy(&request[..header_end]).lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap();
                while request.len() < header_end + length {
                    let mut bytes = [0u8; 4096];
                    let count = socket.read(&mut bytes).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&bytes[..count]);
                }
                let length = body.len() + if truncated { 50 } else { 0 };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
            requests
        });
        let config = Config {
            api_key: String::new(), base_url: format!("http://{address}/v1"),
            model: "test".into(), workspace_dir: std::env::temp_dir(),
            auto_approve: false, continue_session: false, proxy: None,
            mode: crate::theme::AppMode::Manual, effort: None, max_steps: 100,
        };
        (LlmClient::new(config), server)
    }

    fn stream_event(delta: Value, finish: Option<&str>) -> String {
        format!("data: {}\n\n", json!({"choices": [{"delta": delta, "finish_reason": finish}]}))
    }

    #[tokio::test]
    async fn stream_usage_accepts_empty_choices_and_reports_only_final_total() {
        let body = format!("{}data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            stream_event(json!({"content": "answer"}), Some("stop")),
            json!({"choices": [], "usage": {"total_tokens": 120}}),
            json!({"choices": [], "usage": {"prompt_tokens": 200, "completion_tokens": 56}}));
        let (client, server) = stream_fixture(vec![(body, false)]).await;
        let (tx, mut rx) = mpsc::channel(32);
        let result = client.chat_step_stream(&[], &tx, &CancellationToken::new()).await.unwrap();
        assert!(matches!(result, LlmResponse::Message(text) if text == "answer"));
        assert_eq!(server.await.unwrap(), 1);
        let mut usages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::TokenUsage(usage) = event { usages.push(usage); }
        }
        assert_eq!(usages, [Some(256)]);
        assert_eq!(reported_tokens(&json!({"usage": {"input_tokens": 4, "output_tokens": 5}})), Some(9));
        assert_eq!(reported_tokens(&json!({"usage": {"completion_tokens": 5}})), None);
        assert_eq!(reported_tokens(&json!({"usage": null})), None);
    }

    #[tokio::test]
    async fn interrupted_stream_retries_and_discards_partial_tool_arguments() {
        let broken = stream_event(json!({"content": "partial answer", "reasoning": "partial thought",
            "tool_calls": [{"index": 0, "id": "broken", "function": {
                "name": "read_file", "arguments": "{\"path\":\"incomplete"}}]}), None);
        let complete = stream_event(json!({"tool_calls": [{"index": 0, "id": "complete", "function": {
            "name": "read_file", "arguments": "{\"path\":\"README.md\"}"}}]}), Some("tool_calls"));
        let (client, server) = stream_fixture(vec![(broken, true), (complete, false)]).await;
        let (tx, mut rx) = mpsc::channel(32);
        let result = client.chat_step_stream(&[], &tx, &CancellationToken::new()).await.unwrap();
        let LlmResponse::ToolCalls(tools, thought) = result else { panic!("Expected complete tool call") };
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, "complete");
        assert_eq!(tools[0].arguments, "{\"path\":\"README.md\"}");
        assert!(thought.is_none());
        assert_eq!(server.await.unwrap(), 2);
        let mut resets = 0;
        while let Ok(event) = rx.try_recv() { if matches!(event, AgentEvent::StreamRetry) { resets += 1; } }
        assert_eq!(resets, 1);
    }

    #[tokio::test]
    async fn premature_clean_eof_is_retried_and_retry_count_is_bounded() {
        let incomplete = stream_event(json!({"content": "unfinished"}), None);
        let (client, server) = stream_fixture(vec![(incomplete, false); 3]).await;
        let (tx, mut rx) = mpsc::channel(32);
        let error = client.chat_step_stream(&[], &tx, &CancellationToken::new()).await.err().unwrap();
        assert!(format!("{error:#}").contains("before finish_reason or [DONE]"));
        assert_eq!(server.await.unwrap(), 3);
        let mut resets = 0;
        while let Ok(event) = rx.try_recv() { if matches!(event, AgentEvent::StreamRetry) { resets += 1; } }
        assert_eq!(resets, 2);
    }

    #[tokio::test]
    async fn final_choice_survives_transport_error_after_completion() {
        let complete = stream_event(json!({"content": "Полный ответ"}), Some("stop"));
        let (client, server) = stream_fixture(vec![(complete, true)]).await;
        let (tx, _) = mpsc::channel(32);
        let result = client.chat_step_stream(&[], &tx, &CancellationToken::new()).await.unwrap();
        assert!(matches!(result, LlmResponse::Message(text) if text == "Полный ответ"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn cancellation_interrupts_stream_retry_delay() {
        let (client, server) = stream_fixture(vec![(String::new(), true)]).await;
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move { client.chat_step_stream(&[], &tx, &task_cancel).await });
        while !matches!(rx.recv().await, Some(AgentEvent::StreamRetry)) {}
        cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_millis(200), task).await.unwrap().unwrap();
        assert!(result.err().unwrap().to_string().contains("cancelled"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[test]
    fn parsed_object_arguments_are_preserved_and_required_arguments_are_checked() {
        let mut arguments = String::new();
        append_tool_arguments(&mut arguments, &json!({"query":"Сибирь новости"})).unwrap();
        let tool = checked_tool_call("call".into(), "web_search".into(), arguments).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&tool.arguments).unwrap()["query"], "Сибирь новости");
        assert!(checked_tool_call("call".into(), "web_search".into(), "".into()).unwrap_err().to_string().contains("missing JSON arguments"));
        assert!(checked_tool_call("call".into(), "web_search".into(), "{}".into()).is_err());
        assert!(checked_tool_call("call".into(), "web_search".into(), "{\"query\":".into()).is_err());
        assert!(checked_tool_call("call".into(), "web_search".into(), "[]".into()).is_err());
        assert_eq!(checked_tool_call("call".into(), "list_dir".into(), "".into()).unwrap().arguments, "{}");
    }

    #[test]
    fn fragmented_string_arguments_still_assemble_without_data_loss() {
        let mut arguments = String::new();
        for fragment in ["{\"query\":", "\"news", " news\"}"] {
            append_tool_arguments(&mut arguments, &Value::String(fragment.into())).unwrap();
        }
        let tool = checked_tool_call("call".into(), "web_search".into(), arguments).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&tool.arguments).unwrap()["query"], "news news");
        assert!(append_tool_arguments(&mut String::new(), &json!(["invalid"])).is_err());
    }

    #[test]
    fn empty_and_whitespace_completions_are_not_assistant_messages() {
        for content in ["", " \n\t"] {
            assert!(matches!(final_message(content.into(), Some("stop".into())), LlmResponse::Empty { .. }));
        }
        assert!(matches!(final_message("answer".into(), None), LlmResponse::Message(_)));
    }

    #[test]
    fn test_stream_think_parser_normal() {
        let mut parser = StreamThinkParser::new();
        let (th1, as1) = parser.feed("<think>Let me analyze the problem");
        assert_eq!(th1.concat(), "Let me analyze the problem");
        assert!(as1.is_empty());

        let (th2, as2) = parser.feed(" deeply</think>Here is the answer.");
        assert_eq!(th2.concat(), " deeply");
        assert_eq!(as2.concat(), "Here is the answer.");

        let (rem_th, rem_as) = parser.finish();
        assert!(rem_th.is_none());
        assert!(rem_as.is_none());
    }

    #[test]
    fn test_stream_think_parser_chunk_split_tags() {
        let mut parser = StreamThinkParser::new();
        let (th1, as1) = parser.feed("<thi");
        assert!(th1.is_empty());
        assert!(as1.is_empty());

        let (th2, as2) = parser.feed("nk>My thought </thi");
        assert_eq!(th2.concat(), "My thought ");
        assert!(as2.is_empty());

        let (th3, as3) = parser.feed("nk>Final answer");
        assert!(th3.is_empty());
        assert_eq!(as3.concat(), "Final answer");
    }

    #[test]
    fn test_stream_think_parser_backticks_inside_thought() {
        let mut parser = StreamThinkParser::new();
        let (th1, as1) = parser.feed("<think>Reviewing `handles <think></think>` in code");
        assert_eq!(th1.concat(), "Reviewing `handles <think></think>` in code");
        assert!(as1.is_empty());

        // Now true closing tag
        let (th2, as2) = parser.feed("</think>Real response");
        assert!(th2.is_empty());
        assert_eq!(as2.concat(), "Real response");
    }

    #[test]
    fn test_stream_think_parser_no_think() {
        let mut parser = StreamThinkParser::new();
        let (th1, as1) = parser.feed("Hello directly without thinking");
        assert!(th1.is_empty());
        assert_eq!(as1.concat(), "Hello directly without thinking");

        let (th2, as2) = parser.feed(" and more");
        assert!(th2.is_empty());
        assert_eq!(as2.concat(), " and more");
    }

    #[test]
    fn test_stream_think_parser_preserves_code_mentions_in_content() {
        let mut parser = StreamThinkParser::new();
        let (th1, as1) = parser.feed("<think>plan</think>Here is how to use `<think>` tag");
        assert_eq!(th1.concat(), "plan");
        assert_eq!(as1.concat(), "Here is how to use `<think>` tag");

        let (th2, as2) = parser.feed(" and `</think>` in Rust.");
        assert!(th2.is_empty());
        assert_eq!(as2.concat(), " and `</think>` in Rust.");
    }
}
