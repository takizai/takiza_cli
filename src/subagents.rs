//! Session-local, read-only workers with isolated conversations and bounded lifetimes.
use crate::{
    agent::{Agent, AgentEvent},
    config::Config,
    tools::ToolExecutor,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

pub fn is_control(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "wait_agent" | "list_agents" | "cancel_agent"
    )
}
pub fn read_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_dir"
            | "find_files"
            | "grep_search"
            | "read_skill"
            | "web_search"
            | "lsp_hover"
            | "lsp_definition"
            | "lsp_references"
            | "lsp_symbols"
            | "lsp_diagnostics"
    )
}

pub fn definitions() -> Vec<Value> {
    vec![
        crate::runtime::definition("spawn_agent", "Launch a read-only Explore or Reviewer helper with its own context. At most two run simultaneously. Returns an id immediately; use wait_agent to collect its report. Pass all required context explicitly; helpers do not inherit the conversation.", json!({
            "type":"object", "properties": {
                "profile":{"type":"string","enum":["explore","reviewer"]},
                "task":{"type":"string","minLength":1,"maxLength":8000},
                "context":{"type":"string","maxLength":16000}}, "required":["profile","task"], "additionalProperties":false })),
        crate::runtime::definition("wait_agent", "Wait up to 30 seconds for a helper. Returns running or a terminal status with its report/error. A running status is not completion; continue independent work or wait again.", json!({
            "type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false})),
        crate::runtime::definition("list_agents", "List this session's helpers, their tasks, status, model and reported token usage.", json!({"type":"object","properties":{},"additionalProperties":false})),
        crate::runtime::definition("cancel_agent", "Cancel one of this session's helpers by id.", json!({
            "type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false})),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Explore,
    Reviewer,
}
impl Profile {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "explore" => Some(Self::Explore),
            "reviewer" => Some(Self::Reviewer),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Reviewer => "reviewer",
        }
    }
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Explore => "Investigate the assigned code paths. Return a concise explanation, concrete file paths and line references, relevant dependencies, and any uncertainties. Do not implement changes.",
            Self::Reviewer => "Review the supplied changes and relevant source for concrete bugs and regressions. Return findings ordered by severity, with file/line references and a failure scenario. Report what you actually checked; if no concrete findings, say so and list limitations. Do not implement changes or invent a diff that was not supplied.",
        }
    }
}

#[derive(Clone, Serialize)]
pub struct View {
    pub id: String,
    pub profile: String,
    pub task: String,
    pub model: String,
    pub status: String,
    pub activity: String,
    pub tokens: u64,
    pub usage_complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
struct Job {
    view: Mutex<View>,
    cancel: CancellationToken,
    done: Notify,
}
pub struct Manager {
    config: Mutex<Config>,
    tools: Arc<ToolExecutor>,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    next: AtomicU64,
}
impl Manager {
    pub fn new(config: Config, tools: Arc<ToolExecutor>) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            tools,
            jobs: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        })
    }
    pub fn reset(&self, config: Config) {
        self.cancel_all();
        self.jobs.lock().unwrap().clear();
        *self.config.lock().unwrap() = config;
    }
    pub fn views(&self) -> Vec<View> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .map(|job| job.view.lock().unwrap().clone())
            .collect()
    }
    pub fn view(&self, id: &str) -> Result<View, String> {
        Ok(self.job(id)?.view.lock().unwrap().clone())
    }
    fn job(&self, id: &str) -> Result<Arc<Job>, String> {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| crate::i18n::tf!("Unknown subagent: {id}", id = id))
    }
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let job = self.job(id)?;
        if job.view.lock().unwrap().status == "running" {
            job.cancel.cancel();
        }
        Ok(())
    }
    pub fn cancel_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.cancel.cancel();
        }
    }
    pub async fn finish_turn(&self) {
        self.cancel_all();
        let jobs = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            for job in jobs {
                let notified = job.done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if job.view.lock().unwrap().status == "running" {
                    notified.await;
                }
            }
        })
        .await;
    }
    pub async fn execute(
        &self,
        name: &str,
        args: &Value,
        parent: &CancellationToken,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<String, String> {
        let id = || {
            args["id"]
                .as_str()
                .ok_or_else(|| "Missing subagent id".to_string())
        };
        match name {
            "spawn_agent" => self.spawn(args, parent, events),
            "list_agents" => Ok(json!(self.views()).to_string()),
            "cancel_agent" => {
                let id = id()?;
                self.cancel(id)?;
                Ok(json!({"id":id,"cancel_requested":true}).to_string())
            }
            "wait_agent" => {
                let job = self.job(id()?)?;
                let notified = job.done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if job.view.lock().unwrap().status == "running" {
                    tokio::select! {
                        _ = parent.cancelled() => return Err("Subagent wait cancelled".into()),
                        _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                        _ = &mut notified => {},
                    }
                }
                let view = job.view.lock().unwrap().clone();
                Ok(json!(view).to_string())
            }
            _ => Err("Unknown subagent control".into()),
        }
    }
    fn spawn(
        &self,
        args: &Value,
        parent: &CancellationToken,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<String, String> {
        if parent.is_cancelled() {
            return Err("Parent task cancelled".into());
        }
        let profile = args["profile"]
            .as_str()
            .and_then(Profile::parse)
            .ok_or("Expected explore or reviewer profile")?;
        let task = args["task"]
            .as_str()
            .filter(|task| !task.trim().is_empty() && task.chars().count() <= 8000)
            .ok_or("Expected a nonempty task of at most 8000 characters")?;
        let context = args["context"].as_str().unwrap_or("");
        if context.chars().count() > 16000 {
            return Err("Subagent context is limited to 16000 characters".into());
        }
        let mut config = self.config.lock().unwrap().clone();
        config.max_steps = if config.max_steps == 0 {
            16
        } else {
            config.max_steps.min(16)
        };
        let mut jobs = self.jobs.lock().unwrap();
        if jobs
            .values()
            .filter(|job| job.view.lock().unwrap().status == "running")
            .count()
            >= 2
        {
            return Err(
                "Two subagents are already running. Wait for a result before spawning another."
                    .into(),
            );
        }
        if jobs.len() >= 128 {
            return Err("Session subagent limit reached (128). Start a new chat.".into());
        }
        let id = format!("agent-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let job = Arc::new(Job {
            cancel: parent.child_token(),
            done: Notify::new(),
            view: Mutex::new(View {
                id: id.clone(),
                profile: profile.name().into(),
                task: task.into(),
                model: config.model.clone(),
                status: "running".into(),
                activity: "starting".into(),
                tokens: 0,
                usage_complete: true,
                report: None,
                error: None,
            }),
        });
        jobs.insert(id.clone(), Arc::clone(&job));
        drop(jobs);
        let task_prompt =
            format!("Assigned task:\n{task}\n\nExplicit context from the parent:\n{context}");
        let tools = Arc::new(
            self.tools
                .as_ref()
                .clone()
                .read_only(config.execution_mode.clone()),
        );
        tokio::spawn(run_worker(job, config, tools, profile, task_prompt, events));
        Ok(json!({"id":id,"profile":profile.name(),"status":"running"}).to_string())
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let mut out = text.chars().take(limit).collect::<String>();
    if text.chars().count() > limit {
        out.push_str("\n[truncated]");
    }
    out
}

async fn run_worker(
    job: Arc<Job>,
    config: Config,
    tools: Arc<ToolExecutor>,
    profile: Profile,
    task: String,
    parent_events: mpsc::Sender<AgentEvent>,
) {
    let id = job.view.lock().unwrap().id.clone();
    let _ = parent_events
        .send(AgentEvent::ToolLog(crate::i18n::tf!(
            "[{id} {}] started",
            crate::i18n::tr(profile.name()), id = id
        )))
        .await;
    let mut agent = Agent::new_worker(config, tools, profile);
    let (tx, mut rx) = mpsc::channel(100);
    let run = agent.handle_user_input(task, tx, job.cancel.clone());
    let watch = async {
        while let Some(event) = rx.recv().await {
            if let AgentEvent::TokenUsage(tokens) = &event {
                let _ = parent_events.send(AgentEvent::TokenUsage(*tokens)).await;
            }
            let mut view = job.view.lock().unwrap();
            match event {
                AgentEvent::AssistantMessage(text) => view.report = Some(bounded(&text, 16000)),
                AgentEvent::Error(error) => view.error = Some(bounded(&error, 4000)),
                AgentEvent::StatusUpdate(status) => view.activity = bounded(&status, 100),
                AgentEvent::ToolStart { name, .. } => view.activity = name,
                AgentEvent::TokenUsage(Some(tokens)) => {
                    view.tokens = view.tokens.saturating_add(tokens)
                }
                AgentEvent::TokenUsage(None) => view.usage_complete = false,
                // Helpers never own user approval/question dialogs. Fail closed if one slips through.
                AgentEvent::PermissionRequest { responder, .. } => {
                    if let Some(tx) = responder.lock().unwrap().take() {
                        let _ = tx.send(crate::agent::PermissionResponse::Deny);
                    }
                }
                AgentEvent::QuestionRequest { responder, .. } => {
                    if let Some(tx) = responder.lock().unwrap().take() {
                        let _ = tx.send(crate::agent::QuestionResponse {
                            answers: vec![],
                            skipped: true,
                        });
                    }
                }
                _ => {}
            }
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(180), async {
        tokio::join!(run, watch);
    })
    .await;
    let status = {
        let mut view = job.view.lock().unwrap();
        view.status = if job.cancel.is_cancelled() {
            "cancelled"
        } else if outcome.is_err() {
            view.error = Some(crate::i18n::tr("Subagent exceeded the 180-second limit").into());
            "failed"
        } else if view.error.is_some() {
            "failed"
        } else if view.report.is_none() {
            view.error = Some(crate::i18n::tr("Subagent finished without a report").into());
            "failed"
        } else {
            "completed"
        }
        .into();
        view.activity = view.status.clone();
        view.status.clone()
    };
    if status != "completed" {
        job.view.lock().unwrap().usage_complete = false;
        let _ = parent_events.send(AgentEvent::TokenUsage(None)).await;
    }
    job.cancel.cancel();
    let _ = parent_events
        .send(AgentEvent::ToolLog(format!(
            "[{id} {}] {}",
            crate::i18n::tr(profile.name()), crate::i18n::tr(&status)
        )))
        .await;
    job.done.notify_waiters();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readonly_worker_cannot_write_run_commands_or_delegate_even_in_allow_all() {
        let dir =
            std::env::temp_dir().join(format!("takiza-readonly-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.txt"), "evidence").unwrap();
        let mode =
            crate::execution_mode::ModeControl::new(crate::execution_mode::ExecutionMode::AllowAll);
        let tools = ToolExecutor::new(dir.clone()).read_only(mode);
        for (name, args) in [
            (
                "write_file",
                json!({"path":"input.txt","content":"modified"}),
            ),
            (
                "edit_file",
                json!({"path":"input.txt","target_content":"evidence","replacement_content":"modified"}),
            ),
            ("run_command", json!({"command":"touch command-ran"})),
            (
                "spawn_agent",
                json!({"profile":"explore","task":"recursive"}),
            ),
            ("ask_question", json!({"questions":[]})),
            ("mcp__fake__mutate", json!({})),
        ] {
            assert!(tools
                .execute(name, &args.to_string(), None, None)
                .await
                .unwrap_err()
                .contains("Read-only"));
        }
        assert!(tools
            .execute("read_file", r#"{"path":"input.txt"}"#, None, None)
            .await
            .unwrap()
            .contains("evidence"));
        assert_eq!(
            std::fs::read_to_string(dir.join("input.txt")).unwrap(),
            "evidence"
        );
        assert!(!dir.join("command-ran").exists());
        assert!(tools.subagents().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
