use crate::cli_ui::HistoryItem;
use crate::llm::ChatMessage;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub created_at: String,
    pub model: String,
    #[serde(default)]
    pub title: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub history: Vec<HistoryItem>,
}

impl Session {
    pub fn new(model: String) -> Self {
        let now = Local::now();
        let id = now.format("%Y%m%d_%H%M%S").to_string();
        let created_at = now.format("%Y-%m-%d %H:%M:%S").to_string();

        Self {
            id,
            created_at,
            model,
            title: None,
            messages: Vec::new(),
            history: Vec::new(),
        }
    }

    pub fn title(&self) -> String {
        if let Some(ref t) = self.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        for msg in &self.messages {
            if msg.role == "user" {
                if let Some(content) = &msg.content {
                    let first_line = content.lines().next().unwrap_or("").trim();
                    if !first_line.is_empty() {
                        let mut t = first_line.chars().take(40).collect::<String>();
                        if first_line.chars().count() > 40 {
                            t.push_str("...");
                        }
                        return t;
                    }
                }
            }
        }
        "Empty conversation".to_string()
    }

    pub fn message_count(&self) -> usize {
        self.messages.iter().filter(|m| m.role != "system").count()
    }

    pub fn sessions_dir(workspace: &Path) -> PathBuf {
        workspace.join(".takiza").join("sessions")
    }

    pub fn save(&self, workspace: &Path) -> std::io::Result<()> {
        let dir = Self::sessions_dir(workspace);
        fs::create_dir_all(&dir)?;
        let file_path = dir.join(format!("{}.json", self.id));
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(e))?;
        fs::write(&file_path, json)?;

        // Also save readable markdown transcript in .takiza/sessions/{id}.md
        let md_path = dir.join(format!("{}.md", self.id));
        let md_content = self.generate_markdown(workspace);
        let _ = fs::write(md_path, md_content);

        // Update latest_session pointer if session has conversation messages
        if !self.messages.is_empty() {
            let takiza_dir = workspace.join(".takiza");
            let _ = fs::create_dir_all(&takiza_dir);
            let latest_file = takiza_dir.join("latest_session");
            let _ = fs::write(latest_file, &self.id);
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn latest(workspace: &Path) -> Option<Self> {
        let latest_file = workspace.join(".takiza").join("latest_session");
        if let Ok(id) = fs::read_to_string(latest_file) {
            let id = id.trim();
            if !id.is_empty() {
                if let Some(session) = Self::load(workspace, id) {
                    if !session.messages.is_empty() {
                        return Some(session);
                    }
                }
            }
        }
        // Fallback: search newest non-empty session from list
        for id in Self::list(workspace) {
            if let Some(session) = Self::load(workspace, &id) {
                if !session.messages.is_empty() {
                    return Some(session);
                }
            }
        }
        None
    }

    pub fn list(workspace: &Path) -> Vec<String> {
        let dir = Self::sessions_dir(workspace);
        let mut list = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") {
                    list.push(name.trim_end_matches(".json").to_string());
                }
            }
        }
        list.sort();
        list.reverse();
        list
    }

    pub fn load(workspace: &Path, id: &str) -> Option<Self> {
        let dir = Self::sessions_dir(workspace);
        let file_path = dir.join(format!("{}.json", id));
        let content = fs::read_to_string(file_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn delete(workspace: &Path, id: &str) -> std::io::Result<()> {
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
            return self.history.clone();
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
                    HistoryItem::UserPrompt(p) => {
                        md.push_str(&format!("### 👤 User\n\n{}\n\n", p));
                    }
                    HistoryItem::Thought(t) => {
                        md.push_str(&format!("> 💭 **Thinking**:\n> {}\n\n", t.replace('\n', "\n> ")));
                    }
                    HistoryItem::ToolStart { name, args } => {
                        md.push_str(&format!("⚙️ **Tool Call**: `{}`\n```json\n{}\n```\n\n", name, args));
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
    fn test_session_save_load_and_latest() {
        let test_dir = std::env::temp_dir().join(format!("takiza_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&test_dir);
        let _ = fs::create_dir_all(&test_dir);
        let ws = &test_dir;

        let mut s1 = Session::new("llama3".to_string());
        s1.messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some("Hello Takiza!".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        s1.messages.push(ChatMessage {
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
    fn test_build_history_from_messages_fallback() {
        let mut s = Session::new("test-model".to_string());
        s.messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some("First prompt".to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
        s.messages.push(ChatMessage {
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

