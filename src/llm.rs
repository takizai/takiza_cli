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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    ToolCalls(Vec<ToolCall>, Option<String>),
}

#[derive(Debug, PartialEq, Eq)]
enum ThinkPhase {
    Initial,
    Thinking,
    Content,
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

impl LlmClient {
    pub fn new(config: Config) -> Self {
        let mut builder = Client::builder()
            .timeout(std::time::Duration::from_secs(120));

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

    pub async fn generate_title(&self, prompt: &str) -> Option<String> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let prompt_clean = prompt.replace('\n', " ").chars().take(300).collect::<String>();
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("You generate concise conversation titles. Return ONLY a 3-6 word title summarizing the user request. No quotes, no markdown, no punctuation, same language as user.".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(prompt_clean),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }
        ];

        let body = json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": 200,
            "temperature": 0.4,
        });

        let mut req = self.client.post(&url)
            .header("Content-Type", "application/json");

        if !self.config.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let resp = req.json(&body).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        let json_val: Value = resp.json().await.ok()?;
        let choice_msg = json_val["choices"].get(0)?.get("message")?;

        let content_str = choice_msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let raw = if !content_str.trim().is_empty() {
            content_str
        } else {
            choice_msg.get("reasoning").and_then(|r| r.as_str()).unwrap_or("")
        };

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
            None
        } else {
            Some(first_line)
        }
    }

    #[allow(dead_code)]
    pub async fn chat_step(&self, messages: &[ChatMessage]) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let tools = get_tool_definitions();

        let body = json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "max_tokens": 8192,
        });

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
                    let arguments = tc["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string();
                    tool_calls.push(ToolCall { id, name, arguments });
                }
                let thought = content.or(reasoning);
                return Ok(LlmResponse::ToolCalls(tool_calls, thought));
            }
        }

        Ok(LlmResponse::Message(
            content.unwrap_or_else(|| "(Empty response)".to_string()),
        ))
    }

    pub async fn chat_step_stream(
        &self,
        messages: &[ChatMessage],
        event_tx: &mpsc::Sender<AgentEvent>,
        cancel_token: &CancellationToken,
    ) -> Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let tools = get_tool_definitions();

        let body = json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "stream": true,
            "max_tokens": 8192,
        });

        let mut attempts = 0;
        let resp = loop {
            if cancel_token.is_cancelled() {
                anyhow::bail!("Interrupted (Ctrl+C)");
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
        let mut sse_buffer = String::new();
        let mut done_stream = false;

        while !done_stream {
            let chunk_opt = tokio::select! {
                _ = cancel_token.cancelled() => {
                    anyhow::bail!("Interrupted (Ctrl+C)");
                }
                c = byte_stream.next() => c,
            };

            let chunk_bytes = match chunk_opt {
                Some(Ok(bytes)) => bytes,
                Some(Err(e)) => {
                    return Err(e).context("Error reading stream chunk from LLM");
                }
                None => break, // EOF
            };

            let text = String::from_utf8_lossy(&chunk_bytes);
            sse_buffer.push_str(&text);

            while let Some(newline_pos) = sse_buffer.find('\n') {
                let line = sse_buffer[..newline_pos].trim_end_matches('\r').to_string();
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

                    let choice = match json_val.get("choices").and_then(|c| c.as_array()).and_then(|arr| arr.get(0)) {
                        Some(c) => c,
                        None => continue,
                    };

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
                        for tc in tool_calls_arr {
                            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            while accum_tools.len() <= idx {
                                accum_tools.push(InFlightTool {
                                    id: String::new(),
                                    name: String::new(),
                                    arguments: String::new(),
                                });
                            }
                            if let Some(id) = tc.get("id").and_then(|s| s.as_str()) {
                                accum_tools[idx].id.push_str(id);
                            }
                            if let Some(func) = tc.get("function") {
                                if let Some(name) = func.get("name").and_then(|s| s.as_str()) {
                                    accum_tools[idx].name.push_str(name);
                                }
                                if let Some(args) = func.get("arguments").and_then(|s| s.as_str()) {
                                    accum_tools[idx].arguments.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
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
            .map(|(i, t)| ToolCall {
                id: if t.id.trim().is_empty() { format!("call_{}", i) } else { t.id },
                name: t.name,
                arguments: t.arguments,
            })
            .collect();

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

        if accumulated_content.trim().is_empty() && !accumulated_thought.trim().is_empty() {
            accumulated_content = accumulated_thought;
        }

        if accumulated_content.trim().is_empty() {
            accumulated_content = "(Empty response)".to_string();
        }

        Ok(LlmResponse::Message(accumulated_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
