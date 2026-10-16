use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionMode {
    #[default]
    Auto = 0,
    Plan = 1,
    AllowAll = 2,
}

impl ExecutionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Plan,
            Self::Plan => Self::AllowAll,
            Self::AllowAll => Self::Auto,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => crate::i18n::tr("Auto"),
            Self::Plan => crate::i18n::tr("Plan"),
            Self::AllowAll => crate::i18n::tr("Allow-all"),
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "plan" => Some(Self::Plan),
            "allow-all" => Some(Self::AllowAll),
            _ => None,
        }
    }
    pub fn allows_tool(self, name: &str) -> bool {
        self != Self::Plan || crate::subagents::is_control(name)
            || matches!(
                name,
                "read_file"
                    | "list_dir"
                    | "find_files"
                    | "grep_search"
                    | "read_skill"
                    | "web_search"
                    | "ask_question"
                    | "lsp_hover"
                    | "lsp_definition"
                    | "lsp_references"
                    | "lsp_symbols"
                    | "lsp_diagnostics"
            )
    }
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Plan => "PLAN MODE: Investigate and produce an actionable plan. Do not implement changes. Only read-only built-in tools, read-only LSP, web search, skills and questions are allowed. File writes, shell commands and MCP calls are blocked by the runtime. Ask the user to switch modes before implementation.",
            Self::Auto => "AUTO MODE: Implement the requested work. Workspace file edits are automatic; shell commands, external file writes and untrusted MCP calls require approval. Respect denied actions.",
            Self::AllowAll => "ALLOW-ALL MODE: Runtime approval prompts are disabled for tools. Implement only the user's authorized task; existing OS permissions still apply. This mode is not authorization for unrelated or destructive actions.",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModeControl(Arc<AtomicU8>);

impl Default for ModeControl {
    fn default() -> Self {
        Self::new(ExecutionMode::Auto)
    }
}
impl ModeControl {
    pub fn new(mode: ExecutionMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode as u8)))
    }
    pub fn current(&self) -> ExecutionMode {
        decode(self.0.load(Ordering::Acquire))
    }
    pub fn set(&self, mode: ExecutionMode) {
        self.0.store(mode as u8, Ordering::Release);
    }
    pub fn check_tool(&self, name: &str) -> Result<(), String> {
        if self.current().allows_tool(name) {
            Ok(())
        } else {
            Err(crate::i18n::tf!("Plan mode blocked '{name}'; no action was executed. Switch to Auto or Allow-all to implement changes.", name = name))
        }
    }
}
fn decode(mode: u8) -> ExecutionMode {
    match mode {
        1 => ExecutionMode::Plan,
        2 => ExecutionMode::AllowAll,
        _ => ExecutionMode::Auto,
    }
}

static DISPLAY: AtomicU8 = AtomicU8::new(0);
pub fn set_display(mode: ExecutionMode) {
    DISPLAY.store(mode as u8, Ordering::Relaxed);
}
pub fn display() -> ExecutionMode {
    decode(DISPLAY.load(Ordering::Relaxed))
}

pub fn is_cycle_key(key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_policy_is_shared_and_plan_is_an_explicit_tool_allowlist() {
        let mode = ModeControl::default();
        let worker = mode.clone();
        mode.set(ExecutionMode::Plan);
        for tool in [
            "read_file",
            "grep_search",
            "lsp_diagnostics",
            "ask_question",
        ] {
            assert!(worker.check_tool(tool).is_ok());
        }
        for tool in [
            "write_file",
            "edit_file",
            "run_command",
            "mcp__test__read",
            "unknown",
        ] {
            assert!(worker.check_tool(tool).is_err());
        }
        mode.set(ExecutionMode::AllowAll);
        assert!(worker.check_tool("run_command").is_ok());
        assert_eq!(
            ExecutionMode::Auto.next().next().next(),
            ExecutionMode::Auto
        );
    }

    #[tokio::test]
    async fn plan_enforcement_blocks_writes_shell_and_external_calls_in_the_executor() {
        let root = std::env::temp_dir().join(format!("takiza-mode-test-{}", std::process::id()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("input.txt"), "original").unwrap();
        let mode = ModeControl::default();
        let tools =
            crate::tools::ToolExecutor::new(workspace.clone()).with_execution_mode(mode.clone());
        let review = tools
            .prepare_external_write(
                "write_file",
                &serde_json::json!({
            "path": root.join("external.txt"), "content": "changed" })
                .to_string(),
            )
            .unwrap()
            .unwrap();
        mode.set(ExecutionMode::Plan);
        for (tool, args) in [
            (
                "write_file",
                serde_json::json!({"path":"output.txt","content":"changed"}),
            ),
            (
                "edit_file",
                serde_json::json!({"path":"input.txt","target_content":"original","replacement_content":"changed"}),
            ),
            (
                "run_command",
                serde_json::json!({"command":"touch command-ran"}),
            ),
            ("mcp__fake__mutate", serde_json::json!({})),
        ] {
            assert!(tools
                .execute(tool, &args.to_string(), None, None)
                .await
                .unwrap_err()
                .contains("Plan mode"));
        }
        assert!(tools
            .execute_external_write("write_file", &review, None, None)
            .await
            .is_err());
        assert!(tools
            .execute("read_file", r#"{"path":"input.txt"}"#, None, None)
            .await
            .unwrap()
            .contains("original"));
        assert_eq!(
            std::fs::read_to_string(workspace.join("input.txt")).unwrap(),
            "original"
        );
        assert!(!workspace.join("output.txt").exists());
        assert!(!workspace.join("command-ran").exists());
        assert!(!root.join("external.txt").exists());
        mode.set(ExecutionMode::Auto);
        tools
            .execute(
                "write_file",
                r#"{"path":"output.txt","content":"allowed"}"#,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("output.txt")).unwrap(),
            "allowed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shift_tab_is_distinct_from_completion_and_modes_roundtrip() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert!(is_cycle_key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::NONE
        )));
        assert!(is_cycle_key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::SHIFT
        )));
        assert!(!is_cycle_key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE
        )));
        for mode in [
            ExecutionMode::Auto,
            ExecutionMode::Plan,
            ExecutionMode::AllowAll,
        ] {
            let encoded = serde_json::to_string(&mode).unwrap();
            assert_eq!(
                serde_json::from_str::<ExecutionMode>(&encoded).unwrap(),
                mode
            );
        }
    }
}
