use crate::cli_ui::HistoryItem;
use crate::llm::ChatMessage;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenUsage {
    pub total: Option<u64>,
    pub complete: bool,
}

pub fn record_tokens(usage: &mut Option<TokenUsage>, tokens: Option<u64>) {
    let usage = usage.get_or_insert(TokenUsage { total: None, complete: true });
    match tokens {
        Some(tokens) => usage.total = Some(usage.total.unwrap_or(0).saturating_add(tokens)),
        None => usage.complete = false,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub created_at: String,
    pub model: String,
    #[serde(default)]
    pub title: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default, deserialize_with = "crate::session_format::deserialize_history")]
    pub history: Vec<HistoryItem>,
    #[serde(default, flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
    /// Latest model-reported request context, separate from cumulative billing.
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub started: bool,
}

impl Session {
    pub fn new(model: String) -> Self {
        let now = Local::now();
        let id = now.format("%Y%m%d_%H%M%S_%9f").to_string();
        let created_at = now.format("%Y-%m-%d %H:%M:%S").to_string();

        Self {
            id,
            created_at,
            model,
            title: None,
            messages: Vec::new(),
            history: Vec::new(),
            extra: Default::default(),
            token_usage: None,
            context_tokens: Some(0),
            started: false,
        }
    }

    pub fn title_from_prompt(prompt: &str) -> String {
        let first_line = prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
        let mut title = first_line.chars().take(40).collect::<String>();
        if first_line.chars().count() > 40 { title.push_str("..."); }
        title
    }

    pub fn title(&self) -> String {
        if let Some(ref t) = self.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        for prompt in self.messages.iter().filter(|message| message.role == "user").filter_map(|message| message.content.as_deref())
            .chain(self.history.iter().filter_map(|item| if let HistoryItem::UserPrompt(prompt) = item { Some(prompt.as_str()) } else { None })) {
            let title = Self::title_from_prompt(prompt);
            if !title.is_empty() { return title; }
        }
        "Empty conversation".to_string()
    }

    pub fn message_count(&self) -> usize {
        self.messages.iter().filter(|m| m.role != "system").count()
    }

    pub fn is_started(&self) -> bool {
        self.started || self.messages.iter().any(|message| message.role == "user")
            || self.history.iter().any(|item| matches!(item, HistoryItem::UserPrompt(_)))
    }

    pub fn has_conversation(&self) -> bool {
        self.messages.iter().any(|message| message.role != "system" &&
            (message.content.as_ref().is_some_and(|text| !text.trim().is_empty()) || !message.image_urls.is_empty()
                || message.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())))
            || self.history.iter().any(|item| match item {
                HistoryItem::UserPrompt(text) | HistoryItem::AssistantMessage(text) => !text.trim().is_empty(),
                HistoryItem::QuestionInteraction { questions, answers, .. } => !questions.is_empty() || !answers.is_empty(),
                _ => false,
            })
    }


    pub fn sessions_dir(workspace: &Path) -> PathBuf {
        workspace.join(".takiza").join("sessions")
    }

    pub fn save(&self, workspace: &Path) -> std::io::Result<()> {
        if !self.is_started() { return Ok(()); }
        let dir = Self::sessions_dir(workspace);
        fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{}.json", self.id));
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(e))?;
        // Readers must see either the previous or the complete new transcript,
        // including while an active response is being saved periodically.
        let temporary = file_path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&temporary, json)?;
        fs::rename(&temporary, &file_path)?;

        // Also save readable markdown transcript in .takiza/sessions/{id}.md
        let md_path = dir.join(format!("{}.md", self.id));
        let md_content = self.generate_markdown(workspace);
        let _ = fs::write(md_path, md_content);

        // A submitted prompt starts the chat, including preparation before the
        // model request and an existing chat rewound to its first prompt.
        if self.is_started() {
            let takiza_dir = workspace.join(".takiza");
            let _ = fs::create_dir_all(&takiza_dir);
            let latest_file = takiza_dir.join("latest_session");
            let _ = fs::write(latest_file, &self.id);
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn latest(workspace: &Path) -> Option<Self> {
        // 1. Scan sessions ordered by most recent activity (last modified timestamp)
        for id in Self::list_by_activity(workspace) {
            if let Some(session) = Self::load(workspace, &id) {
                // Must have actual activity (non-empty messages)
                if session.is_started() {
                    return Some(session);
                }
            }
        }

        // 2. Fallback to latest_session pointer file if present
        let latest_file = workspace.join(".takiza").join("latest_session");
        if let Ok(id) = fs::read_to_string(latest_file) {
            let id = id.trim();
            if !id.is_empty() {
                if let Some(session) = Self::load(workspace, id) {
                    if session.is_started() {
                        return Some(session);
                    }
                }
            }
        }

        None
    }

    pub fn list_by_activity(workspace: &Path) -> Vec<String> {
        let dir = Self::sessions_dir(workspace);
        let mut entries_with_mtime = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") {
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let id = name.trim_end_matches(".json").to_string();
                    entries_with_mtime.push((mtime, id));
                }
            }
        }
        // Sort descending by last modified time (most recently active first)
        entries_with_mtime.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        entries_with_mtime.into_iter().map(|(_, id)| id).collect()
    }

    pub fn list(workspace: &Path) -> Vec<String> {
        Self::list_by_activity(workspace).into_iter().filter(|id| Self::load(workspace,id).is_some_and(|session| session.has_conversation())).collect()
    }

    pub fn load(workspace: &Path, id: &str) -> Option<Self> {
        let dir = Self::sessions_dir(workspace);
        let file_path = dir.join(format!("{}.json", id));
        let content = fs::read_to_string(file_path).ok()?;
        let mut session: Self = serde_json::from_str(&content).ok()?;
        session.started = session.is_started();
        if session.token_usage.is_none() {
            for item in &session.history {
                if let HistoryItem::ResponseStats { tokens, usage_complete, .. } = item {
                    record_tokens(&mut session.token_usage, *tokens);
                    if !usage_complete { session.token_usage.as_mut().unwrap().complete = false; }
                }
            }
            if session.token_usage.is_none() && session.messages.iter().any(|message| message.role == "user") {
                record_tokens(&mut session.token_usage, None);
            }
        }
        Some(session)
    }

    pub fn delete(workspace: &Path, id: &str) -> std::io::Result<()> {
        crate::checkpoints::remove_session(workspace, id).map_err(std::io::Error::other)?;
        let dir = Self::sessions_dir(workspace);
        let file_path = dir.join(format!("{}.json", id));
        let md_path = dir.join(format!("{}.md", id));
        let _ = fs::remove_file(md_path);

        // If deleting active latest session, clean up pointer
        let latest_file = workspace.join(".takiza").join("latest_session");
        if let Ok(cur_id) = fs::read_to_string(&latest_file) {
            if cur_id.trim() == id {
                let _ = fs::remove_file(&latest_file);
            }
        }

        fs::remove_file(file_path)
    }

    pub fn build_history_from_messages(&self) -> Vec<HistoryItem> {
        if !self.history.is_empty() {
            return self.history.iter().filter(|item| !matches!(item,
                HistoryItem::ToolLog(text) if text.starts_with("Restore unavailable: No checkpoints in this chat")
                    || text.starts_with("Restore unavailable: No checkpoints yet.")
                    || text.starts_with("Workspace restored to ")))
                .cloned().collect();
        }
        let mut items = Vec::new();
        for msg in &self.messages {
            match msg.role.as_str() {
                "user" => {
                    if let Some(content) = &msg.content {
                        items.push(HistoryItem::UserPrompt(content.clone()));
                    }
                }
                "assistant" => {
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            items.push(HistoryItem::ToolStart {
                                name: tc.function.name.clone(),
                                args: tc.function.arguments.clone(),
                            });
                        }
                    }
                    if let Some(content) = &msg.content {
                        if !content.trim().is_empty() {
                            items.push(HistoryItem::AssistantMessage(content.clone()));
                        }
                    }
                }
                "tool" => {
                    if let Some(content) = &msg.content {
                        let name = msg.name.clone().unwrap_or_else(|| "tool".to_string());
                        items.push(HistoryItem::ToolEnd {
                            name,
                            args: String::new(),
                            result: content.clone(),
                            is_error: false,
                        });
                    }
                }
                _ => {}
            }
        }
        items
    }

    /// GUI restores the whole saved session. Keep CLI lifetime accounting and
    /// the started-chat marker while restoring conversation-specific state.
    pub fn restore_checkpoint(&mut self, mut restored: Self) -> anyhow::Result<()> {
        anyhow::ensure!(self.id == restored.id, "This checkpoint belongs to another chat");
        restored.history = restored.build_history_from_messages();
        restored.started |= self.is_started();
        restored.token_usage = self.token_usage.clone().or(restored.token_usage);
        *self = restored;
        Ok(())
    }

    pub fn generate_markdown(&self, workspace: &Path) -> String {
        let mut md = String::new();
        let display_title = self.title.as_deref().unwrap_or(&self.id);
        md.push_str(&format!("# Takiza Code Session: {}\n\n", display_title));
        md.push_str(&format!("- **Session ID**: `{}`\n", self.id));
        md.push_str(&format!("- **Created**: {}\n", self.created_at));
        md.push_str(&format!("- **Model**: `{}`\n", self.model));
        md.push_str(&format!("- **Workspace**: `{}`\n\n", workspace.display()));
        md.push_str("---\n\n");

        if !self.history.is_empty() {
            for item in &self.history {
                match item {
                    HistoryItem::MoaRouting { model, .. } => md.push_str(&format!("> Model: {model}\n\n")),
                    HistoryItem::QuestionInteraction { answers, skipped, .. } => {
                        md.push_str("### Questionnaire\n\n");
                        if *skipped { md.push_str("Interview ended early.\n\n"); }
                        for answer in answers { md.push_str(&format!("- {}\n  {}\n  {}\n", answer.question,answer.selected_options.join(", "),answer.custom_text.as_deref().unwrap_or(""))); }
                    },
                    HistoryItem::UserPrompt(p) => {
                        md.push_str(&format!("### 👤 User\n\n{}\n\n", p));
                    }
                    HistoryItem::Thought(t) => {
                        md.push_str(&format!("> 💭 **Thinking**:\n> {}\n\n", t.replace('\n', "\n> ")));
                    }
                    HistoryItem::ToolStart { name, args } => {
                        md.push_str(&format!("⚙️ **Tool Call**: `{}`\n```json\n{}\n```\n\n", name, args));
                    }
                    HistoryItem::FileDiff(diff) => {
                        md.push_str(&format!("```diff\n{}\n```\n\n", diff));
                    }
                    HistoryItem::ToolLog(l) => {
                        md.push_str(&format!("- `{}`\n", l));
                    }
                    HistoryItem::ToolEnd { name, result, is_error, .. } => {
                        let status = if *is_error { "❌ Error" } else { "✅ Output" };
                        md.push_str(&format!("**{} ({})**:\n```\n{}\n```\n\n", status, name, result));
                    }
                    HistoryItem::AssistantMessage(m) => {
                        md.push_str(&format!("### 🤖 Takiza\n\n{}\n\n", m));
                    }
                    HistoryItem::ResponseStats { elapsed_ms, tokens, usage_complete } => {
                        md.push_str(&format!("*{}*\n\n", crate::cli_ui::response_stats_text(*elapsed_ms, *tokens, *usage_complete)));
                    }
                    HistoryItem::Error(e) => {
                        md.push_str(&format!("> ⚠️ **Error**: {}\n\n", e));
                    }
                }
            }
        } else {
            for msg in &self.messages {
                match msg.role.as_str() {
                    "user" => {
                        if let Some(content) = &msg.content {
                            md.push_str(&format!("### 👤 User\n\n{}\n\n", content));
                        }
                    }
                    "assistant" => {
                        if let Some(content) = &msg.content {
                            md.push_str(&format!("### 🤖 Takiza\n\n{}\n\n", content));
                        }
                    }
                    "tool" => {
                        if let Some(content) = &msg.content {
                            let name = msg.name.as_deref().unwrap_or("tool");
                            md.push_str(&format!("**Tool Output ({})**:\n```\n{}\n```\n\n", name, content));
                        }
                    }
                    _ => {}
                }
            }
        }

        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_a_checkpoint_replaces_session_metadata_but_preserves_spent_tokens() {
        let mut before = Session::new("earlier-model".into());
        before.title = Some("Earlier title".into());
        before.context_tokens = Some(1200);
        before.extra.insert("gui_setting".into(), serde_json::json!({"value":"before"}));
        before.history.push(HistoryItem::UserPrompt("earlier prompt".into()));
        record_tokens(&mut before.token_usage, Some(200));
        let mut current = before.clone();
        current.title = Some("Later title".into());
        current.model = "later-model".into();
        current.context_tokens = Some(8000);
        current.extra.insert("gui_setting".into(), serde_json::json!({"value":"after"}));
        current.extra.insert("later_only".into(), serde_json::json!(true));
        current.history.push(HistoryItem::AssistantMessage("later answer".into()));
        record_tokens(&mut current.token_usage, Some(500));
        current.restore_checkpoint(before.clone()).unwrap();
        assert_eq!(current.title, before.title);
        assert_eq!(current.model, before.model);
        assert_eq!(current.context_tokens, Some(1200));
        assert_eq!(current.extra, before.extra);
        assert_eq!(current.history.len(), 1);
        assert_eq!(current.token_usage.as_ref().unwrap().total, Some(700));
        assert!(current.started);
        let mut another = Session::new("other-model".into());
        another.id = "another-chat".into();
        let saved = serde_json::to_value(&current).unwrap();
        assert!(current.restore_checkpoint(another).is_err());
        assert_eq!(serde_json::to_value(&current).unwrap(), saved);
    }

    #[test]
    fn restoring_an_empty_gui_checkpoint_keeps_the_chat_saved_and_its_context_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let before: Session = serde_json::from_value(serde_json::json!({
            "id":"gui-chat", "created_at":"2026-10-26", "model":"gui-model",
            "messages":[], "history":[], "gui_extra":{"preserved":true}
        })).unwrap();
        let mut current = before.clone();
        current.history.push(HistoryItem::UserPrompt("later prompt".into()));
        current.context_tokens = Some(9000);
        record_tokens(&mut current.token_usage, Some(450));
        current.restore_checkpoint(before).unwrap();
        assert!(current.history.is_empty() && current.messages.is_empty());
        assert!(current.started);
        assert_eq!(current.context_tokens, None);
        current.save(directory.path()).unwrap();
        let saved = Session::load(directory.path(), &current.id).unwrap();
        assert!(saved.started);
        assert_eq!(saved.extra["gui_extra"], serde_json::json!({"preserved":true}));
        assert_eq!(saved.token_usage.unwrap().total, Some(450));
    }

    #[test]
    fn unused_chat_is_not_saved_and_started_chat_survives_rewind_to_empty_history() {
        let workspace = std::env::temp_dir().join(format!("takiza-unused-chat-{}-{}",
            std::process::id(), Local::now().timestamp_nanos_opt().unwrap()));
        let mut session = Session::new("test-model".into());
        session.messages.push(ChatMessage { provider_data: None,
            role: "system".into(), content: Some("instructions".into()), image_urls: Vec::new(),
            tool_calls: None, tool_call_id: None, name: None,
        });
        session.history.push(HistoryItem::ToolLog("Configuration opened".into()));
        session.save(&workspace).unwrap();
        assert!(!workspace.exists());
        assert!(Session::latest(&workspace).is_none());

        session.history.push(HistoryItem::UserPrompt("first submitted prompt".into()));
        session.save(&workspace).unwrap();
        let mut loaded = Session::latest(&workspace).unwrap();
        assert_eq!(loaded.id, session.id);
        assert!(loaded.started);
        loaded.history.clear();
        loaded.messages.clear();
        loaded.save(&workspace).unwrap();
        assert_eq!(Session::latest(&workspace).unwrap().id, session.id);
        assert_eq!(fs::read_to_string(workspace.join(".takiza/latest_session")).unwrap(), session.id);
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn new_chats_have_distinct_ids_even_within_one_second() {
        let first = Session::new("test".into());
        let second = Session::new("test".into());
        assert_ne!(first.id, second.id);
        assert!(second.title.is_none());
        assert!(second.messages.is_empty());
    }

    #[test]
    fn test_session_save_load_and_latest() {
        let test_dir = std::env::temp_dir().join(format!("takiza_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&test_dir);
        let _ = fs::create_dir_all(&test_dir);
        let ws = &test_dir;

        let mut s1 = Session::new("llama3".to_string());
        s1.messages.push(ChatMessage { provider_data: None,
            image_urls: Vec::new(),
            role: "user".to_string(),
            content: Some("Hello Takiza!".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        s1.messages.push(ChatMessage { provider_data: None,
            image_urls: Vec::new(),
            role: "assistant".to_string(),
            content: Some("Hello! How can I help you?".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        s1.history.push(HistoryItem::UserPrompt("Hello Takiza!".to_string()));
        s1.history.push(HistoryItem::AssistantMessage("Hello! How can I help you?".to_string()));

        assert_eq!(s1.title(), "Hello Takiza!");

        s1.title = Some("AI Generated Title".to_string());
        assert_eq!(s1.title(), "AI Generated Title");

        record_tokens(&mut s1.token_usage, Some(120));
        record_tokens(&mut s1.token_usage, None);
        record_tokens(&mut s1.token_usage, Some(30));
        s1.context_tokens = Some(64_000);
        s1.save(ws).unwrap();

        // Check file existence
        let sess_dir = Session::sessions_dir(ws);
        assert!(sess_dir.join(format!("{}.json", s1.id)).exists());
        assert!(sess_dir.join(format!("{}.md", s1.id)).exists());

        // Check latest pointer
        let latest = Session::latest(ws).expect("Should find latest session");
        assert_eq!(latest.id, s1.id);
        assert_eq!(latest.title, Some("AI Generated Title".to_string()));
        assert_eq!(latest.title(), "AI Generated Title");
        assert_eq!(latest.token_usage.as_ref().unwrap().total, Some(150));
        assert!(!latest.token_usage.as_ref().unwrap().complete);
        assert_eq!(latest.context_tokens, Some(64_000));
        assert_eq!(latest.messages.len(), 2);
        assert_eq!(latest.history.len(), 2);

        // Check markdown content
        let md = latest.generate_markdown(ws);
        assert!(md.contains("# Takiza Code Session: AI Generated Title"));
        assert!(md.contains("Hello Takiza!"));
        assert!(md.contains("Hello! How can I help you?"));

        // Test delete
        Session::delete(ws, &s1.id).unwrap();
        assert!(!sess_dir.join(format!("{}.json", s1.id)).exists());
        assert!(!sess_dir.join(format!("{}.md", s1.id)).exists());
        assert!(Session::latest(ws).is_none());
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn restored_history_omits_obsolete_empty_checkpoint_notice() {
        let mut session = Session::new("test".into());
        session.history = vec![
            HistoryItem::ToolLog("Restore unavailable: No checkpoints in this chat yet. Send a prompt to create the first checkpoint.".into()),
            HistoryItem::ToolLog("Workspace restored to 2026-10-02 · safety checkpoint 123".into()),
            HistoryItem::ToolLog("Restore unavailable: missing snapshot file".into()),
            HistoryItem::UserPrompt("hello".into()),
        ];
        let restored = session.build_history_from_messages();
        assert_eq!(restored.len(), 2);
        assert!(matches!(&restored[0], HistoryItem::ToolLog(text) if text.contains("missing snapshot")));
    }

    #[test]
    fn test_build_history_from_messages_fallback() {
        let mut s = Session::new("test-model".to_string());
        s.messages.push(ChatMessage { provider_data: None,
            image_urls: Vec::new(),
            role: "user".to_string(),
            content: Some("First prompt".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        s.messages.push(ChatMessage { provider_data: None,
            image_urls: Vec::new(),
            role: "assistant".to_string(),
            content: Some("Answer".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        // history is empty, should reconstruct from messages
        let items = s.build_history_from_messages();
        assert_eq!(items.len(), 2);
        match &items[0] {
            HistoryItem::UserPrompt(p) => assert_eq!(p, "First prompt"),
            _ => panic!("Expected UserPrompt"),
        }
        match &items[1] {
            HistoryItem::AssistantMessage(m) => assert_eq!(m, "Answer"),
            _ => panic!("Expected AssistantMessage"),
        }
    }
}

#[cfg(test)]
#[path = "../tests/common/session_compat.rs"]
mod shared_format_tests;
