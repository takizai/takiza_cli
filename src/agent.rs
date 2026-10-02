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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QuestionItem {
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub is_multi_select: bool,
    #[serde(default = "default_true")]
    pub allow_custom: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
}

pub fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QuestionAnswer {
    pub question_index: usize,
    pub question: String,
    #[serde(default)]
    pub selected_options: Vec<String>,
    #[serde(default)]
    pub custom_text: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QuestionResponse {
    pub answers: Vec<QuestionAnswer>,
    pub skipped: bool,
}

fn questionnaire_result(response: &QuestionResponse) -> String {
    let answers: Vec<serde_json::Value> = response.answers.iter().map(|answer| {
        let has_answer = !answer.selected_options.is_empty()
            || answer.custom_text.as_ref().is_some_and(|text| !text.trim().is_empty());
        serde_json::json!({
            "question_index": answer.question_index,
            "question": answer.question,
            "status": if has_answer { "answered" } else { "skipped" },
            "selected_options": answer.selected_options,
            "custom_text": answer.custom_text,
        })
    }).collect();
    serde_json::json!({
        "status": if response.skipped { "cancelled" } else { "completed" },
        "answers": answers,
    }).to_string()
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
    QuestionRequest {
        id: String,
        questions: Vec<QuestionItem>,
        responder: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<QuestionResponse>>>>,
    },
    ToolStart { id: String, name: String, args: String },
    ToolLog(String),
    ToolEnd { id: String, name: String, args: String, result: String, is_error: bool },
    Error(String),
    Interrupted,
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
    let skills = crate::skills::discover_skills(&config.workspace_dir);
    let skills_section = crate::skills::format_skills_for_prompt(&skills);

    format!(
        "You are Takiza, an autonomous AI programming agent running in the user's terminal.\n\
        Workspace: {}\n\
        OS: {} ({})\n\n\
        {}\n\n\
        {}\n\n\
        AVAILABLE TOOLS:\n\
        You have direct access to the following built-in tools. NEVER say you lack any of these tools. Always call them directly:\n\
        - `read_file(path, start_line, end_line)`: Inspect files in workspace.\n\
        - `write_file(path, content)`: Create or overwrite files.\n\
        - `edit_file(path, target_content, replacement_content)`: Replace exact text blocks.\n\
        - `list_dir(path)`: List directory entries.\n\
        - `find_files(pattern, path)`: Search files by pattern.\n\
        - `grep_search(query, path)`: Regex/text search across workspace files.\n\
        - `run_command(command)`: Execute bash/shell commands in workspace.\n\
        - `read_skill(name)`: Read full instructions and rules for a specialized skill (e.g. 'claude-design', 'frontend-excellence').\n\
        - `ask_question(questions)`: Present an interactive questionnaire to the user in the terminal (single/multi-choice, custom answers, open-ended questions).\n\n\
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
        5. LANGUAGE: Always respond in the user's language (Russian if user speaks Russian).\n\
        6. SPECIALIZED SKILLS:\n\
           - Check the <skills> section. If the user's request touches design/UI styling, frontend engineering standards, or specialized domains (e.g. `claude-design`, `frontend-excellence`), inspect the guidelines first with `read_skill` or `read_file` and adhere to them.\n\
        7. INTERACTIVE QUESTIONS & PREFERENCES (`ask_question`):\n\
           - You HAVE the `ask_question` tool! NEVER say you do not have it or ask clarifying questions in plain text.\n\
           - When user intent has multiple valid architectural paths, UI style choices (tones, colors, layouts), framework/library decisions, or ambiguous requirements, CALL the `ask_question` tool!\n\
           - Present clear options (single or multi-choice) and allow custom answers. Wait for user feedback before proceeding with code changes.",
        config.workspace_dir.display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        ws_context,
        skills_section
    )
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let tools = Arc::new(ToolExecutor::new(config.workspace_dir.clone()));
        let llm = LlmClient::new(config.clone());
        let mut messages = Vec::new();

        let system_prompt = build_system_prompt(&config);

        messages.push(ChatMessage {
            image_urls: Vec::new(),
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

    pub fn ensure_latest_system_prompt(&mut self) {
        let latest = build_system_prompt(&self.config);
        if let Some(first) = self.messages.first_mut() {
            if first.role == "system" {
                first.content = Some(latest);
                return;
            }
        }
        self.messages.insert(0, ChatMessage {
            image_urls: Vec::new(),
            role: "system".to_string(),
            content: Some(latest),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
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
        self.ensure_latest_system_prompt();
        let skills = crate::skills::discover_skills(&self.config.workspace_dir);
        let requested = crate::skills::requested_skills_for_prompt(&input, &skills);
        if let Some(content) = self.messages.first_mut().and_then(|message| message.content.as_mut()) {
            content.push_str(&requested);
        }
        let _ = event_tx.send(AgentEvent::UserMessage(input.clone())).await;

        let images = match crate::attachments::prompt_images(&input) {
            Ok(images) => images,
            Err(error) => {
                let _ = event_tx.send(AgentEvent::Error(error)).await;
                let _ = event_tx.send(AgentEvent::Finished).await;
                return;
            }
        };

        self.messages.push(ChatMessage {
            image_urls: images,
            role: "user".to_string(),
            content: Some(input),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        let max_steps = self.config.max_steps;
        let mut step = 0;
        let mut empty_attempts = 0;

        loop {
            if cancel_token.is_cancelled() {
                break;
            }

            step += 1;
            if max_steps != 0 && step > max_steps {
                let _ = event_tx
                    .send(AgentEvent::Error(
                        format!("Reached maximum step limit ({max_steps} steps). Set TAKIZA_MAX_STEPS to raise the limit, or 0 to disable it."),
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
                    break;
                }
                res = chat_fut => {
                    match res {
                        Ok(r) => r,
                        Err(e) => {
                            if !cancel_token.is_cancelled() {
                                let _ = event_tx.send(AgentEvent::Error(format!("LLM error: {e}"))).await;
                            }
                            break;
                        }
                    }
                }
            };

            match response {
                LlmResponse::Empty { finish_reason } => {
                    empty_attempts += 1;
                    if empty_attempts >= 3 {
                        let reason = finish_reason.map(|reason| format!(" (finish_reason: {reason})")).unwrap_or_default();
                        let _ = event_tx.send(AgentEvent::Error(format!(
                            "The model returned no answer after 3 attempts{reason}. Try another model or resend your prompt."
                        ))).await;
                        break;
                    }
                    crate::logger::log_warn("LLM", &format!("Empty response; retrying ({empty_attempts}/2)"));
                    // Retries do not consume tool-loop steps or add empty assistant messages.
                    step = step.saturating_sub(1);
                    tokio::select! {
                        _ = cancel_token.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
                    }
                    continue;
                }
                LlmResponse::Message(msg) => {
                    empty_attempts = 0;
                    let trimmed = msg.trim();
                    let lower = trimmed.to_lowercase();
                    // Detect if the model outputted intermediate chain-of-thought/narration instead of completing the task.
                    let is_intermediate = (max_steps == 0 || step < max_steps) && (
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
                            image_urls: Vec::new(),
                            role: "assistant".to_string(),
                            content: Some(msg),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                        self.messages.push(ChatMessage {
                            image_urls: Vec::new(),
                            role: "user".to_string(),
                            content: Some("Continue. Call the appropriate tools (such as list_dir, read_file, write_file, edit_file, run_command) to complete the implementation autonomously.".to_string()),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                        continue;
                    }

                    self.messages.push(ChatMessage {
                        image_urls: Vec::new(),
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
                    empty_attempts = 0;
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
                        image_urls: Vec::new(),
                        role: "assistant".to_string(),
                        content: assistant_text,
                        tool_calls: Some(llm_tc_vec),
                        tool_call_id: None,
                        name: None,
                    });

                    for tc in tool_calls {
                        if cancel_token.is_cancelled() {
                            break;
                        }

                        if tc.name == "ask_question" {
                            let parsed_questions: Result<Vec<QuestionItem>, String> = (|| {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                                    if let Some(arr) = val.get("questions").and_then(|q| q.as_array()) {
                                        let items: Result<Vec<QuestionItem>, _> = serde_json::from_value(serde_json::Value::Array(arr.clone()));
                                        if let Ok(items) = items {
                                            return Ok(items);
                                        }
                                    }
                                    if val.is_array() {
                                        if let Ok(items) = serde_json::from_value::<Vec<QuestionItem>>(val.clone()) {
                                            return Ok(items);
                                        }
                                    }
                                    if val.is_object() && val.get("question").is_some() {
                                        if let Ok(item) = serde_json::from_value::<QuestionItem>(val) {
                                            return Ok(vec![item]);
                                        }
                                    }
                                }
                                Err("Invalid arguments format for ask_question. Expected JSON with 'questions' array.".to_string())
                            })();

                            let questions = match parsed_questions {
                                Ok(q) => q,
                                Err(err_msg) => {
                                    let _ = event_tx.send(AgentEvent::ToolEnd {
                                        id: tc.id.clone(),
                                        name: tc.name.clone(),
                                        args: tc.arguments.clone(),
                                        result: err_msg.clone(),
                                        is_error: true,
                                    }).await;
                                    self.messages.push(ChatMessage {
                                        image_urls: Vec::new(),
                                        role: "tool".to_string(),
                                        content: Some(err_msg),
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id.clone()),
                                        name: Some(tc.name.clone()),
                                    });
                                    continue;
                                }
                            };

                            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                            let responder = Arc::new(std::sync::Mutex::new(Some(resp_tx)));

                            let _ = event_tx.send(AgentEvent::ToolStart {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                args: tc.arguments.clone(),
                            }).await;

                            let _ = event_tx.send(AgentEvent::QuestionRequest {
                                id: tc.id.clone(),
                                questions,
                                responder,
                            }).await;

                            let response = match resp_rx.await {
                                Ok(r) => r,
                                Err(_) => QuestionResponse { answers: Vec::new(), skipped: true },
                            };

                            let result_content = questionnaire_result(&response);

                            let _ = event_tx.send(AgentEvent::ToolEnd {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                args: tc.arguments.clone(),
                                result: result_content.clone(),
                                is_error: false,
                            }).await;

                            self.messages.push(ChatMessage {
                                image_urls: Vec::new(),
                                role: "tool".to_string(),
                                content: Some(result_content),
                                tool_calls: None,
                                tool_call_id: Some(tc.id),
                                name: Some(tc.name),
                            });
                            continue;
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
                                        image_urls: Vec::new(),
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
                            Err(_) if cancel_token.is_cancelled() => (false, "Command stopped by user".to_string()),
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
                            image_urls: Vec::new(),
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

        if cancel_token.is_cancelled() {
            let _ = event_tx.send(AgentEvent::Interrupted).await;
        }
        let _ = event_tx
            .send(AgentEvent::StatusUpdate("Ready".to_string()))
            .await;
        let _ = event_tx.send(AgentEvent::Finished).await;
    }
}

#[cfg(test)]
mod questionnaire_tests {
    use super::*;

    #[test]
    fn tool_result_preserves_questions_choices_and_custom_text() {
        let response = QuestionResponse {
            answers: vec![QuestionAnswer {
                question_index: 0,
                question: "Для кого продукт?".into(),
                selected_options: vec!["Малый бизнес".into(), "Агентства".into()],
                custom_text: Some("Команды до 20 человек".into()),
            }],
            skipped: false,
        };
        let result: serde_json::Value = serde_json::from_str(&questionnaire_result(&response)).unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["answers"][0]["status"], "answered");
        assert_eq!(result["answers"][0]["question"], response.answers[0].question);
        assert_eq!(result["answers"][0]["selected_options"], serde_json::json!(response.answers[0].selected_options));
        assert_eq!(result["answers"][0]["custom_text"], "Команды до 20 человек");
    }

    #[test]
    fn cancellation_keeps_completed_answers_and_explicit_skips() {
        let response = QuestionResponse {
            answers: vec![
                QuestionAnswer {
                    question_index: 0, question: "Кому?".into(),
                    selected_options: vec!["Бизнесу".into()], custom_text: None,
                },
                QuestionAnswer {
                    question_index: 1, question: "Когда?".into(),
                    selected_options: vec![], custom_text: None,
                },
            ],
            skipped: true,
        };
        let result: serde_json::Value = serde_json::from_str(&questionnaire_result(&response)).unwrap();
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["answers"][0]["status"], "answered");
        assert_eq!(result["answers"][0]["selected_options"][0], "Бизнесу");
        assert_eq!(result["answers"][1]["status"], "skipped");
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_prompt_finishes_with_interruption_instead_of_error() {
        let config = Config {
            api_key: String::new(), base_url: "http://127.0.0.1:1/v1".into(),
            model: "test".into(), workspace_dir: std::env::temp_dir(),
            auto_approve: false, continue_session: false, proxy: None,
            mode: crate::theme::AppMode::Manual, effort: None, max_steps: 100,
        };
        let mut agent = Agent::new(config);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (tx, mut rx) = mpsc::channel(10);
        agent.handle_user_input("test".into(), tx, cancellation).await;
        let mut interrupted = 0;
        let mut finished = false;
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::Error(error) => panic!("Cancellation became an error: {error}"),
                AgentEvent::Interrupted => interrupted += 1,
                AgentEvent::Finished => finished = true,
                _ => {}
            }
        }
        assert_eq!(interrupted, 1);
        assert!(finished);
    }
}
