use crate::config::Config;
use crate::llm::{ChatMessage, LlmClient, LlmFunctionCall, LlmResponse, LlmToolCall};
use crate::tools::{ToolExecutor, ToolOutputEvent};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionResponse {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum AgentEvent {
    StatusUpdate(String),
    UserMessage(String),
    AssistantMessage(String),
    AssistantThought(String),
    AssistantToken(String),
    ThoughtToken(String),
    PermissionRequest {
        id: String,
        name: String,
        command: String,
        responder: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<PermissionResponse>>>>,
    },
    ToolStart { id: String, name: String, args: String },
    ToolLog(String),
    ToolEnd { id: String, name: String, args: String, result: String, is_error: bool },
    Error(String),
    Finished,
}

pub struct Agent {
    pub config: Config,
    llm: LlmClient,
    tools: Arc<ToolExecutor>,
    messages: Vec<ChatMessage>,
}

fn gather_workspace_context(workspace: &std::path::Path) -> String {
    let mut sections = Vec::new();

    // 1. Git details
    let git = crate::git::GitInfo::get(workspace);
    if let Some(ref b) = git.branch {
        sections.push(format!("Git Branch: {} ({})", b, if git.is_dirty { "modified" } else { "clean" }));
    }

    // 2. Directory structure (top-level items)
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    files.push(format!("{}/", name));
                } else {
                    files.push(name);
                }
            }
        }
    }
    files.sort();
    if !files.is_empty() {
        sections.push(format!("Files: {}", files.join(", ")));
    }

    // 3. Project overview from README.md (first 5 lines only)
    let readme = workspace.join("README.md");
    if readme.exists() {
        if let Ok(content) = std::fs::read_to_string(&readme) {
            let lines: Vec<&str> = content.lines().take(5).collect();
            if !lines.is_empty() {
                sections.push(format!("Project Overview:\n{}", lines.join("\n")));
            }
        }
    }

    sections.join("\n\n")
}

fn build_system_prompt(config: &Config) -> String {
    let ws_context = gather_workspace_context(&config.workspace_dir);
    format!(
        "You are Takiza, an autonomous AI programming agent running in the user's terminal.\n\
        Workspace: {}\n\
        OS: {} ({})\n\n\
        {}\n\n\
        RULES:\n\
        1. CONVERSATION CONTEXT: You are in an ONGOING conversation session. Maintain full awareness of all earlier user queries, your prior answers, and tool outputs. Never forget what was previously discussed.\n\
        2. NO REPEATED GREETINGS: Do NOT greet the user repeatedly (e.g. 'Привет!') once the conversation is underway. Answer directly and concisely.\n\
        3. INVESTIGATION & TERMINAL OPERATIONS: Use tools (`read_file`, `find_files`, `grep_search`, `list_dir`) to inspect files. Feel free to use `run_command` for any bash operations (e.g. `ls -la`, `git status`, inspecting environment, or running utilities) whenever shell commands or detailed output are appropriate.\n\
        4. AUTONOMOUS ACTION & PERSISTENCE:\n\
           - When asked to create, write, modify, or fix code (such as 'write a server', 'create a file', 'add a feature'):\n\
           - DO NOT stop after merely inspecting or planning. You MUST carry out the full implementation.\n\
           - DO NOT output messages narrating what you will inspect or do (e.g. 'Let's inspect src' or 'Now I will create...'). Instead, call the tools directly!\n\
           - Write or update the files using `write_file` or `edit_file`.\n\
           - Verify the implementation using `run_command` (e.g. `cargo check`, `cargo test`, `python ...`).\n\
           - Only provide a concise final summary when the code has been written and verified.\n\
        5. LANGUAGE: Always respond in the user's language (Russian if user speaks Russian).",
        config.workspace_dir.display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        ws_context
    )
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let tools = Arc::new(ToolExecutor::new(config.workspace_dir.clone()));
        let llm = LlmClient::new(config.clone());
        let mut messages = Vec::new();

        let system_prompt = build_system_prompt(&config);

        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(system_prompt),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        Self {
            config,
            llm,
            tools,
            messages,
        }
    }

    pub fn reset(&mut self, config: Config) {
        *self = Self::new(config);
    }

    pub fn message_count(&self) -> usize {
        self.messages.iter().filter(|m| m.role != "system").count()
    }

    pub fn get_messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn set_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
    }

    pub async fn handle_user_input(
        &mut self,
        input: String,
        event_tx: mpsc::Sender<AgentEvent>,
        cancel_token: CancellationToken,
    ) {
        let _ = event_tx.send(AgentEvent::UserMessage(input.clone())).await;

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(input),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        let max_steps = 15;
        let mut step = 0;

        loop {
            if cancel_token.is_cancelled() {
                let _ = event_tx
                    .send(AgentEvent::Error("Interrupted (Ctrl+C)".to_string()))
                    .await;
                break;
            }

            step += 1;
            if step > max_steps {
                let _ = event_tx
                    .send(AgentEvent::Error(
                        "Reached maximum step limit (15 steps). Stopping harness loop.".to_string(),
                    ))
                    .await;
                break;
            }

            let _ = event_tx
                .send(AgentEvent::StatusUpdate("Thinking...".to_string()))
                .await;

            let chat_fut = self.llm.chat_step_stream(&self.messages, &event_tx, &cancel_token);
            let response = tokio::select! {
                _ = cancel_token.cancelled() => {
                    let _ = event_tx
                        .send(AgentEvent::Error("Interrupted (Ctrl+C)".to_string()))
                        .await;
                    break;
                }
                res = chat_fut => {
                    match res {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = event_tx
                                .send(AgentEvent::Error(format!("LLM error: {e}")))
                                .await;
                            break;
                        }
                    }
                }
            };

            match response {
                LlmResponse::Message(msg) => {
                    let trimmed = msg.trim();
                    let lower = trimmed.to_lowercase();
                    // Detect if the model outputted intermediate chain-of-thought/narration instead of completing the task.
                    let is_intermediate = step < max_steps && (
                        lower.starts_with("user wants")
                        || lower.starts_with("the user wants")
                        || lower.starts_with("let's inspect")
                        || lower.starts_with("let's check")
                        || lower.ends_with("let's inspect src.")
                        || lower.ends_with("let's inspect src")
                        || (step > 1 && trimmed.lines().count() <= 3 && (
                            lower.contains("let's inspect")
                            || lower.contains("now let's")
                            || lower.contains("i will inspect")
                            || lower.contains("i'll inspect")
                            || lower.contains("давайте проверим")
                            || lower.contains("давай проверим")
                            || lower.contains("давайте посмотрим")
                        ))
                    );

                    if is_intermediate {
                        self.messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: Some(msg),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                        self.messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: Some("Continue. Call the appropriate tools (such as list_dir, read_file, write_file, edit_file, run_command) to complete the implementation autonomously.".to_string()),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                        continue;
                    }

                    self.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(msg.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                    let _ = event_tx.send(AgentEvent::AssistantMessage(msg)).await;
                    break;
                }
                LlmResponse::ToolCalls(tool_calls, assistant_text) => {
                    if let Some(ref text) = assistant_text {
                        if !text.trim().is_empty() {
                            let _ = event_tx
                                .send(AgentEvent::AssistantThought(text.clone()))
                                .await;
                        }
                    }

                    let llm_tc_vec: Vec<LlmToolCall> = tool_calls
                        .iter()
                        .map(|tc| LlmToolCall {
                            id: tc.id.clone(),
                            call_type: "function".to_string(),
                            function: LlmFunctionCall {
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect();

                    self.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: assistant_text,
                        tool_calls: Some(llm_tc_vec),
                        tool_call_id: None,
                        name: None,
                    });

                    for tc in tool_calls {
                        if cancel_token.is_cancelled() {
                            let _ = event_tx
                                .send(AgentEvent::Error("Interrupted (Ctrl+C)".to_string()))
                                .await;
                            break;
                        }

                        // Check permission for command execution if auto_approve is disabled
                        if tc.name == "run_command" && !self.config.auto_approve {
                            let cmd_str = match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                                Ok(v) => v.get("command").and_then(|c| c.as_str()).unwrap_or(&tc.arguments).to_string(),
                                Err(_) => tc.arguments.clone(),
                            };

                            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                            let responder = Arc::new(std::sync::Mutex::new(Some(resp_tx)));

                            let _ = event_tx
                                .send(AgentEvent::PermissionRequest {
                                    id: tc.id.clone(),
                                    name: tc.name.clone(),
                                    command: cmd_str.clone(),
                                    responder,
                                })
                                .await;

                            let response = match resp_rx.await {
                                Ok(r) => r,
                                Err(_) => PermissionResponse::Deny,
                            };

                            match response {
                                PermissionResponse::AllowOnce => {}
                                PermissionResponse::AllowAlways => {
                                    self.config.auto_approve = true;
                                }
                                PermissionResponse::Deny => {
                                    let denied_msg = format!("Command execution denied by user: permission rejected for command: {}", cmd_str);
                                    let _ = event_tx
                                        .send(AgentEvent::ToolEnd {
                                            id: tc.id.clone(),
                                            name: tc.name.clone(),
                                            args: tc.arguments.clone(),
                                            result: denied_msg.clone(),
                                            is_error: true,
                                        })
                                        .await;

                                    self.messages.push(ChatMessage {
                                        role: "tool".to_string(),
                                        content: Some(denied_msg),
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id),
                                        name: Some(tc.name),
                                    });
                                    continue;
                                }
                            }
                        }

                        let _ = event_tx
                            .send(AgentEvent::StatusUpdate(format!("Running tool: {}", tc.name)))
                            .await;

                        let _ = event_tx
                            .send(AgentEvent::ToolStart {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                args: tc.arguments.clone(),
                            })
                            .await;

                        let (sub_tx, mut sub_rx) = mpsc::channel::<ToolOutputEvent>(100);
                        let event_tx_clone = event_tx.clone();

                        let forward_handle = tokio::spawn(async move {
                            while let Some(evt) = sub_rx.recv().await {
                                if let ToolOutputEvent::Log(line) = evt {
                                    let _ = event_tx_clone.send(AgentEvent::ToolLog(line)).await;
                                }
                            }
                        });

                        let exec_res = self
                            .tools
                            .execute(
                                &tc.name,
                                &tc.arguments,
                                Some(sub_tx),
                                Some(cancel_token.clone()),
                            )
                            .await;

                        let _ = forward_handle.await;

                        let (is_err, result_content) = match exec_res {
                            Ok(res) => (false, res),
                            Err(err) => (true, err),
                        };

                        let _ = event_tx
                            .send(AgentEvent::ToolEnd {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                args: tc.arguments.clone(),
                                result: result_content.clone(),
                                is_error: is_err,
                            })
                            .await;

                        self.messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: Some(result_content),
                            tool_calls: None,
                            tool_call_id: Some(tc.id),
                            name: Some(tc.name),
                        });
                    }
                }
            }
        }

        let _ = event_tx
            .send(AgentEvent::StatusUpdate("Ready".to_string()))
            .await;
        let _ = event_tx.send(AgentEvent::Finished).await;
    }
}
