use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum ToolOutputEvent {
    Started { name: String, args: String },
    Log(String),
    FileDiff(String),
    Finished { name: String, result: String, is_error: bool },
}

fn file_change_diff(path: &str, before: &str, after: &str) -> Option<String> {
    if before == after { return None; }
    let old: Vec<_> = before.split_inclusive('\n').collect();
    let new: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(left, right)| left == right).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev())
        .take_while(|(left, right)| left == right).count();
    let start = prefix.saturating_sub(3);
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let context = suffix.min(3);
    let old_count = old_end + context - start;
    let new_count = new_end + context - start;
    let mut diff = format!("--- {path}\n+++ {path}\n@@ -{},{} +{},{} @@\n",
        if old_count == 0 { start } else { start + 1 }, old_count,
        if new_count == 0 { start } else { start + 1 }, new_count);
    let mut push_line = |prefix: char, line: &str| {
        diff.push(prefix);
        diff.push_str(line);
        if !line.ends_with('\n') { diff.push_str("\n\\ No newline at end of file\n"); }
    };
    for line in &old[start..prefix] { push_line(' ', line); }
    for line in &old[prefix..old_end] { push_line('-', line); }
    for line in &new[prefix..new_end] { push_line('+', line); }
    for line in &old[old_end..old_end + context] { push_line(' ', line); }
    Some(diff)
}

pub fn get_tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the internet for current or unfamiliar information. Use when the user asks to search/browse online, or your knowledge is insufficient or may be outdated. Returns source titles, URLs and snippets, not full pages.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search terms, optionally with site:domain filters." },
                        "num_results": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Results to return; default 5." }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Reads contents of a file from the workspace. Supports start_line and end_line for range inspection.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file, relative to the workspace or absolute."
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "Optional 1-based start line."
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Optional 1-based end line."
                        }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Creates a new file or overwrites an existing file with the provided content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to create or overwrite."
                        },
                        "content": {
                            "type": "string",
                            "description": "The full text content to write into the file."
                        }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Edits an existing file by replacing an exact occurrence of target_content with replacement_content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to edit."
                        },
                        "target_content": {
                            "type": "string",
                            "description": "Exact text block within the file to be replaced."
                        },
                        "replacement_content": {
                            "type": "string",
                            "description": "New text block to insert in place of target_content."
                        }
                    },
                    "required": ["path", "target_content", "replacement_content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "Lists files and subdirectories in a directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the directory (defaults to current workspace '.')."
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "find_files",
                "description": "Finds files matching a search pattern or substring across the workspace recursively.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Search pattern or filename keyword (e.g. 'main', '.rs', 'config')."
                        },
                        "path": {
                            "type": "string",
                            "description": "Subdirectory to search within (defaults to workspace root '.')."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep_search",
                "description": "Searches for a text pattern inside workspace files recursively (like ripgrep/grep).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Text query or regex to search for in files."
                        },
                        "path": {
                            "type": "string",
                            "description": "Subdirectory or file to search within (defaults to workspace root '.')."
                        }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Runs a shell command (bash) in the workspace and streams/captures its output. Use for terminal operations, bash commands (e.g. 'ls -la', 'git status', 'find'), running scripts, compiling, and testing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command line string to execute."
                        }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_skill",
                "description": "Reads the full instructions, guidelines, and rules from a named skill (such as 'claude-design', 'frontend-excellence', 'garden-skills'). Use this whenever the user task touches upon domain guidelines, UI aesthetics, or frontend standards.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Name of the skill to read (e.g. 'claude-design', 'frontend-excellence')."
                        }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask_question",
                "description": "Presents an interactive questionnaire in terminal/UI. Use when missing information materially changes the intended result or an action requires authorization beyond the user's request. Reuse prior answers and resolve routine design, stack, and implementation choices yourself; several valid approaches alone do not require a question. Ask concise questions together and continue independent work while waiting. Supports single choice, multiple choice, custom input, and open-ended questions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "List of questions to present to the user.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": {
                                        "type": "string",
                                        "description": "The question prompt to present to the user."
                                    },
                                    "header": {
                                        "type": "string",
                                        "description": "Optional category tag or short header (e.g. 'Архитектура', 'Стиль UI', 'Язык/Стек')."
                                    },
                                    "options": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "List of answer choices for the user. If omitted or empty, this is an open-ended question for free-form user text."
                                    },
                                    "is_multi_select": {
                                        "type": "boolean",
                                        "description": "If true, user can select multiple options with checkboxes. If false (default), single choice radio button."
                                    },
                                    "allow_custom": {
                                        "type": "boolean",
                                        "description": "Whether to permit user to enter their own custom answer or additional notes. Default is true."
                                    },
                                    "placeholder": {
                                        "type": "string",
                                        "description": "Optional placeholder text for the custom/free-form text input."
                                    }
                                },
                                "required": ["question"]
                            }
                        }
                    },
                    "required": ["questions"]
                }
            }
        }
    ])
}

pub struct ToolExecutor {
    pub workspace_root: PathBuf,
    web_proxy: Option<String>,
}

impl ToolExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root, web_proxy: None }
    }

    pub fn with_web_proxy(mut self, proxy: Option<String>) -> Self {
        self.web_proxy = proxy;
        self
    }

    pub fn resolve_path(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments_json: &str,
        event_tx: Option<mpsc::Sender<ToolOutputEvent>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<String, String> {
        let args: Value = serde_json::from_str(arguments_json)
            .map_err(|e| format!("Invalid JSON arguments: {e}"))?;

        if let Some(ref tx) = event_tx {
            let _ = tx
                .send(ToolOutputEvent::Started {
                    name: name.to_string(),
                    args: arguments_json.to_string(),
                })
                .await;
        }

        let previous_text = if matches!(name, "write_file" | "edit_file") {
            args["path"].as_str().and_then(|path| {
                match fs::read_to_string(self.resolve_path(path)) {
                    Ok(text) => Some(text),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
                    Err(_) => None,
                }
            })
        } else { None };

        let result = match name {
            "web_search" => crate::web_search::search(&args, self.web_proxy.as_deref(), cancel_token).await,
            "read_file" => self.exec_read_file(&args),
            "write_file" => self.exec_write_file(&args),
            "edit_file" => self.exec_edit_file(&args),
            "list_dir" => self.exec_list_dir(&args),
            "find_files" => self.exec_find_files_with_events(&args, event_tx.as_ref()).await,
            "grep_search" => self.exec_grep_search_with_events(&args, event_tx.as_ref()).await,
            "read_skill" => self.exec_read_skill(&args),
            "run_command" => {
                self.exec_run_command(&args, event_tx.as_ref(), cancel_token)
                    .await
            }
            "ask_question" => {
                Ok("Interactive questionnaire presented to user.".to_string())
            }
            other => Err(format!("Unknown tool: {other}")),
        };

        if let Some(ref tx) = event_tx {
            let (is_err, out) = match &result {
                Ok(msg) => (false, msg.clone()),
                Err(err) => (true, err.clone()),
            };
            if !is_err {
                if let (Some(before), Some(path)) = (previous_text, args["path"].as_str()) {
                    if let Ok(after) = fs::read_to_string(self.resolve_path(path)) {
                        if let Some(diff) = file_change_diff(path, &before, &after) {
                            let _ = tx.send(ToolOutputEvent::FileDiff(diff)).await;
                        }
                    }
                }
            }
            let _ = tx
                .send(ToolOutputEvent::Finished {
                    name: name.to_string(),
                    result: out,
                    is_error: is_err,
                })
                .await;
        }

        result
    }

    fn exec_read_skill(&self, args: &Value) -> Result<String, String> {
        let name = args["name"].as_str().ok_or("Missing 'name' argument")?;
        crate::skills::get_skill_content(&self.workspace_root, name)
            .ok_or_else(|| format!("Skill '{}' not found in workspace (.agents/skills) or system skills directories", name))
    }

    fn exec_read_file(&self, args: &Value) -> Result<String, String> {
        let p_str = args["path"].as_str().ok_or("Missing 'path' argument")?;
        let full_path = self.resolve_path(p_str);

        let content = fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read file '{}': {e}", full_path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start_line = args["start_line"].as_u64().map(|v| v as usize).unwrap_or(1);
        let end_line = args["end_line"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or(total_lines);

        let start_idx = if start_line > 0 { start_line - 1 } else { 0 };
        let end_idx = end_line.min(total_lines);

        if start_idx >= total_lines && total_lines > 0 {
            return Ok(format!(
                "File '{}' has {} lines. start_line {} is out of range.",
                full_path.display(),
                total_lines,
                start_line
            ));
        }

        let mut output = format!(
            "File: {} (lines {}-{} of {})\n",
            full_path.display(),
            if total_lines == 0 { 0 } else { start_idx + 1 },
            end_idx,
            total_lines
        );

        for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
            output.push_str(&format!("{:4}: {}\n", start_idx + 1 + i, line));
        }

        Ok(output)
    }

    fn exec_write_file(&self, args: &Value) -> Result<String, String> {
        let p_str = args["path"].as_str().ok_or("Missing 'path' argument")?;
        let content = args["content"].as_str().ok_or("Missing 'content' argument")?;
        let full_path = self.resolve_path(p_str);

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!("Failed to create directory '{}': {e}", parent.display())
            })?;
        }

        fs::write(&full_path, content)
            .map_err(|e| format!("Failed to write file '{}': {e}", full_path.display()))?;

        let line_count = content.lines().count();
        Ok(format!(
            "Successfully wrote {} lines to '{}'",
            line_count,
            full_path.display()
        ))
    }

    fn exec_edit_file(&self, args: &Value) -> Result<String, String> {
        let p_str = args["path"].as_str().ok_or("Missing 'path' argument")?;
        let target = args["target_content"]
            .as_str()
            .ok_or("Missing 'target_content' argument")?;
        let replacement = args["replacement_content"]
            .as_str()
            .ok_or("Missing 'replacement_content' argument")?;
        let full_path = self.resolve_path(p_str);

        let content = fs::read_to_string(&full_path)
            .map_err(|e| format!("Failed to read file '{}': {e}", full_path.display()))?;

        if !content.contains(target) {
            return Err(format!(
                "Target content not found in '{}'. Ensure whitespace matches exactly.",
                full_path.display()
            ));
        }

        let count = content.matches(target).count();
        if count > 1 {
            return Err(format!(
                "Target content matched {} times in '{}'. Provide a larger unique context block.",
                count,
                full_path.display()
            ));
        }

        let new_content = content.replacen(target, replacement, 1);
        fs::write(&full_path, new_content)
            .map_err(|e| format!("Failed to save modified file '{}': {e}", full_path.display()))?;

        // Format a unified diff-like summary
        let mut diff_summary = format!("Successfully edited '{}':\n", full_path.display());
        for l in target.lines() {
            diff_summary.push_str(&format!("- {}\n", l));
        }
        for l in replacement.lines() {
            diff_summary.push_str(&format!("+ {}\n", l));
        }

        Ok(diff_summary)
    }

    fn exec_list_dir(&self, args: &Value) -> Result<String, String> {
        let p_str = args["path"].as_str().unwrap_or(".");
        let full_path = self.resolve_path(p_str);

        let read_dir = fs::read_dir(&full_path)
            .map_err(|e| format!("Failed to list directory '{}': {e}", full_path.display()))?;

        let mut entries = Vec::new();
        for entry in read_dir.flatten() {
            let file_type = entry.file_type().ok();
            let is_dir = file_type.map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().to_string();
            let size_str = if is_dir {
                "DIR".to_string()
            } else if let Ok(meta) = entry.metadata() {
                format!("{} B", meta.len())
            } else {
                "-".to_string()
            };

            entries.push(format!("  {:10} {}", size_str, name));
        }

        entries.sort();
        let mut out = format!("Contents of '{}':\n", full_path.display());
        if entries.is_empty() {
            out.push_str("  (empty directory)\n");
        } else {
            out.push_str(&entries.join("\n"));
            out.push('\n');
        }

        Ok(out)
    }

    async fn exec_find_files_with_events(
        &self,
        args: &Value,
        event_tx: Option<&mpsc::Sender<ToolOutputEvent>>,
    ) -> Result<String, String> {
        let pattern = args["pattern"].as_str().ok_or("Missing 'pattern' argument")?;
        let sub = args["path"].as_str().unwrap_or(".");
        let root = self.resolve_path(sub);

        let mut matched = Vec::new();
        let mut stack = vec![root];

        while let Some(dir) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Skip noisy / VCS directories
                    if name == ".git" || name == "target" || name == "node_modules" || name == ".takiza" {
                        continue;
                    }

                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                        let rel_str = rel.to_string_lossy();
                        if rel_str.contains(pattern) || name.contains(pattern) {
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            let formatted = format!("  {:>8} B  {}", size, rel_str);
                            if let Some(tx) = event_tx {
                                let _ = tx.send(ToolOutputEvent::Log(rel_str.to_string())).await;
                            }
                            matched.push(formatted);
                        }
                    }
                }
            }
        }

        matched.sort();
        if matched.is_empty() {
            Ok(format!("No files matching '{}' found in workspace.", pattern))
        } else {
            Ok(format!("Found {} matching files:\n{}", matched.len(), matched.join("\n")))
        }
    }

    #[allow(dead_code)]
    fn exec_find_files(&self, args: &Value) -> Result<String, String> {
        let pattern = args["pattern"].as_str().ok_or("Missing 'pattern' argument")?;
        let sub = args["path"].as_str().unwrap_or(".");
        let root = self.resolve_path(sub);

        let mut matched = Vec::new();
        let mut stack = vec![root];

        while let Some(dir) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    if name == ".git" || name == "target" || name == "node_modules" || name == ".takiza" {
                        continue;
                    }

                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                        let rel_str = rel.to_string_lossy();
                        if rel_str.contains(pattern) || name.contains(pattern) {
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            matched.push(format!("  {:>8} B  {}", size, rel_str));
                        }
                    }
                }
            }
        }

        matched.sort();
        if matched.is_empty() {
            Ok(format!("No files matching '{}' found in workspace.", pattern))
        } else {
            Ok(format!("Found {} matching files:\n{}", matched.len(), matched.join("\n")))
        }
    }

    async fn exec_grep_search_with_events(
        &self,
        args: &Value,
        event_tx: Option<&mpsc::Sender<ToolOutputEvent>>,
    ) -> Result<String, String> {
        let query = args["query"].as_str().ok_or("Missing 'query' argument")?;
        let sub = args["path"].as_str().unwrap_or(".");
        let root = self.resolve_path(sub);

        let mut matches = Vec::new();
        let mut stack = vec![root];
        let max_matches = 50;

        while let Some(dir) = stack.pop() {
            if matches.len() >= max_matches {
                break;
            }

            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    if name == ".git" || name == "target" || name == "node_modules" || name == ".takiza" {
                        continue;
                    }

                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        if let Ok(content) = fs::read_to_string(&path) {
                            let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                            let rel_str = rel.to_string_lossy();

                            for (idx, line) in content.lines().enumerate() {
                                if line.contains(query) {
                                    let match_line = format!("{}:{}: {}", rel_str, idx + 1, line.trim());
                                    if let Some(tx) = event_tx {
                                        let _ = tx.send(ToolOutputEvent::Log(match_line.clone())).await;
                                    }
                                    matches.push(match_line);
                                    if matches.len() >= max_matches {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if matches.is_empty() {
            Ok(format!("No matches found for query '{}'", query))
        } else {
            let count = matches.len();
            let mut out = format!("Found {} matches for '{}':\n", count, query);
            out.push_str(&matches.join("\n"));
            if count >= max_matches {
                out.push_str("\n(Results capped at 50 matches)");
            }
            Ok(out)
        }
    }

    #[allow(dead_code)]
    fn exec_grep_search(&self, args: &Value) -> Result<String, String> {
        let query = args["query"].as_str().ok_or("Missing 'query' argument")?;
        let sub = args["path"].as_str().unwrap_or(".");
        let root = self.resolve_path(sub);

        let mut matches = Vec::new();
        let mut stack = vec![root];
        let max_matches = 50;

        while let Some(dir) = stack.pop() {
            if matches.len() >= max_matches {
                break;
            }

            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    if name == ".git" || name == "target" || name == "node_modules" || name == ".takiza" {
                        continue;
                    }

                    if path.is_dir() {
                        stack.push(path);
                    } else if path.is_file() {
                        if let Ok(content) = fs::read_to_string(&path) {
                            let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                            let rel_str = rel.to_string_lossy();

                            for (idx, line) in content.lines().enumerate() {
                                if line.contains(query) {
                                    matches.push(format!("{}:{}: {}", rel_str, idx + 1, line.trim()));
                                    if matches.len() >= max_matches {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if matches.is_empty() {
            Ok(format!("No matches found for query '{}'", query))
        } else {
            let count = matches.len();
            let mut out = format!("Found {} matches for '{}':\n", count, query);
            out.push_str(&matches.join("\n"));
            if count >= max_matches {
                out.push_str("\n(Results capped at 50 matches)");
            }
            Ok(out)
        }
    }

    async fn exec_run_command(
        &self,
        args: &Value,
        event_tx: Option<&mpsc::Sender<ToolOutputEvent>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<String, String> {
        let cmd_str = args["command"]
            .as_str()
            .ok_or("Missing 'command' argument")?;

        if let Some(tx) = event_tx {
            let _ = tx.send(ToolOutputEvent::Log(format!("$ {cmd_str}"))).await;
        }

        #[cfg(windows)]
        let (shell, flag) = {
            if std::path::Path::new(r"C:\Program Files\Git\bin\bash.exe").exists() {
                (r"C:\Program Files\Git\bin\bash.exe", "-c")
            } else if std::path::Path::new(r"C:\Program Files (x86)\Git\bin\bash.exe").exists() {
                (r"C:\Program Files (x86)\Git\bin\bash.exe", "-c")
            } else if let Ok(path_var) = std::env::var("PATH") {
                if std::env::split_paths(&path_var).any(|p| p.join("bash.exe").is_file()) {
                    ("bash.exe", "-c")
                } else {
                    ("cmd.exe", "/C")
                }
            } else {
                ("cmd.exe", "/C")
            }
        };

        #[cfg(not(windows))]
        let (shell, flag) = ("bash", "-c");

        let mut cmd = Command::new(shell);
        cmd.arg(flag)
            .arg(cmd_str)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn command '{cmd_str}': {e}"))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let tx_out = event_tx.cloned();
        let stdout_handle = tokio::spawn(async move {
            let mut out_lines = Vec::new();
            if let Some(stream) = stdout {
                let mut reader = BufReader::new(stream).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    if let Some(ref tx) = tx_out {
                        let _ = tx.send(ToolOutputEvent::Log(line.clone())).await;
                    }
                    out_lines.push(line);
                }
            }
            out_lines
        });

        let tx_err = event_tx.cloned();
        let stderr_handle = tokio::spawn(async move {
            let mut err_lines = Vec::new();
            if let Some(stream) = stderr {
                let mut reader = BufReader::new(stream).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let formatted = format!("[stderr] {line}");
                    if let Some(ref tx) = tx_err {
                        let _ = tx.send(ToolOutputEvent::Log(formatted.clone())).await;
                    }
                    err_lines.push(formatted);
                }
            }
            err_lines
        });

        let wait_result = tokio::select! {
            _ = async {
                if let Some(ref token) = cancel_token {
                    token.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                let _ = child.kill().await;
                Err("Command interrupted by user (Ctrl+C)".to_string())
            }
            res = child.wait() => {
                res.map_err(|e| format!("Error waiting for command: {e}"))
            }
        };

        let (stdout_res, stderr_res) = tokio::join!(stdout_handle, stderr_handle);
        let stdout_lines = stdout_res.unwrap_or_default();
        let stderr_lines = stderr_res.unwrap_or_default();

        let mut full_output = String::new();
        if !stdout_lines.is_empty() {
            full_output.push_str(&stdout_lines.join("\n"));
            full_output.push('\n');
        }
        if !stderr_lines.is_empty() {
            if !full_output.is_empty() {
                full_output.push_str("--- stderr ---\n");
            }
            full_output.push_str(&stderr_lines.join("\n"));
            full_output.push('\n');
        }

        let status = match wait_result {
            Ok(s) => s,
            Err(e) => return Err(e),
        };

        let exit_code = status.code().unwrap_or(-1);
        full_output.push_str(&format!("[Exit code: {exit_code}]"));

        if status.success() {
            Ok(full_output)
        } else {
            Err(format!("Command failed with exit code {exit_code}:\n{full_output}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn web_search_is_registered_and_uses_normal_tool_events() {
        let definitions = get_tool_definitions();
        let search = definitions.as_array().unwrap().iter().find(|tool| tool["function"]["name"] == "web_search").unwrap();
        assert_eq!(search["function"]["parameters"]["required"], json!(["query"]));
        let executor = ToolExecutor::new(std::env::temp_dir());
        let (tx, mut rx) = mpsc::channel(10);
        assert!(executor.execute("web_search", r#"{"query":" "}"#, Some(tx), None).await.is_err());
        assert!(matches!(rx.recv().await, Some(ToolOutputEvent::Started { name, .. }) if name == "web_search"));
        assert!(matches!(rx.recv().await, Some(ToolOutputEvent::Finished { name, is_error: true, .. }) if name == "web_search"));
    }

    #[test]
    fn test_find_files_and_grep() {
        let root = PathBuf::from(".");
        let exec = ToolExecutor::new(root);

        let find_res = exec.exec_find_files(&serde_json::json!({
            "pattern": "Cargo"
        }));
        assert!(find_res.is_ok());
        let find_out = find_res.unwrap();
        assert!(find_out.contains("Cargo.toml"));

        let grep_res = exec.exec_grep_search(&serde_json::json!({
            "query": "takiza-harness"
        }));
        assert!(grep_res.is_ok());
        let grep_out = grep_res.unwrap();
        assert!(grep_out.contains("Cargo.toml"));
    }
}
