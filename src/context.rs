//! Compress model memory without changing the visible conversation or workspace.
use crate::{agent::AgentEvent, llm::{ChatMessage, ContextOverflow, LlmClient}};
use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage { role: role.into(), content: Some(content), image_urls: Vec::new(),
        tool_calls: None, tool_call_id: None, name: None }
}

fn retained(messages: &[ChatMessage]) -> Vec<usize> {
    let mut indices = messages.iter().enumerate().filter_map(|(i, m)| (m.role == "system").then_some(i)).collect::<Vec<_>>();
    if let Some(i) = messages.iter().rposition(|m| m.role == "user") { indices.push(i); }
    if let Some((i, last)) = messages.iter().enumerate().next_back() {
        if last.role == "assistant" && last.tool_calls.is_none()
            && last.content.as_ref().is_some_and(|text| text.chars().count() <= 6000) { indices.push(i); }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

pub async fn compact(client: &LlmClient, messages: &[ChatMessage],
    tx: &mpsc::Sender<AgentEvent>, cancel: &CancellationToken) -> Result<Option<Vec<ChatMessage>>> {
    let keep = retained(messages);
    if messages.iter().enumerate().all(|(i, _)| keep.contains(&i)) { return Ok(None); }
    let latest_user = messages.iter().rposition(|message| message.role == "user");
    let mut transcript = String::new();
    for (i, item) in messages.iter().enumerate() {
        if keep.contains(&i) && Some(i) != latest_user { continue; }
        transcript.push_str(&format!("\n{}:\n{}\n", item.role, item.content.as_deref().unwrap_or("")));
        if let Some(calls) = &item.tool_calls { transcript.push_str(&serde_json::to_string(calls)?); }
        if let Some(id) = &item.tool_call_id { transcript.push_str(&format!("\nTool call ID: {id}\n")); }
        if !item.image_urls.is_empty() { transcript.push_str("\n[Earlier image attachment; image pixels are not included in this text summary.]\n"); }
    }
    if transcript.trim().is_empty() { return Ok(None); }
    let chars = transcript.chars().collect::<Vec<_>>();
    let mut offset = 0;
    let mut chunk_size = 6000;
    let mut summary = String::new();
    while offset < chars.len() {
        anyhow::ensure!(!cancel.is_cancelled(), crate::i18n::tr("Context compaction cancelled."));
        let end = (offset + chunk_size).min(chars.len());
        let chunk = chars[offset..end].iter().collect::<String>();
        match client.summarize_context(&summary, &chunk, tx, cancel).await {
            Ok(next) => { summary = next; offset = end; }
            Err(error) if error.is::<ContextOverflow>() && chunk_size > 500 => { chunk_size = (chunk_size / 2).max(500); }
            Err(error) => return Err(error),
        }
    }
    let mut compacted = keep.iter().filter(|&&i| messages[i].role == "system")
        .map(|&i| messages[i].clone()).collect::<Vec<_>>();
    compacted.push(message("assistant", format!("[Summary of earlier context. Completed actions must not be repeated.]\n{summary}")));
    compacted.extend(keep.iter().filter(|&&i| messages[i].role != "system").map(|&i| messages[i].clone()));
    anyhow::ensure!(!cancel.is_cancelled(), crate::i18n::tr("Context compaction cancelled."));
    if serde_json::to_vec(&compacted)?.len() >= serde_json::to_vec(messages)?.len() { return Ok(None); }
    Ok(Some(compacted))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_system_instructions_latest_prompt_and_answer_without_orphan_tool_results() {
        let mut tool = message("tool", "file contents".into());
        tool.tool_call_id = Some("call".into());
        let messages = vec![message("system", "instructions".into()), message("user", "old prompt".into()),
            tool, message("user", "latest prompt".into()), message("assistant", "latest answer".into())];
        assert_eq!(retained(&messages), vec![0, 3, 4]);
    }
}
