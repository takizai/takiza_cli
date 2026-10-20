//! Interactive design interviews. Inspired by Matt Pocock's grill-me/grilling skill.
//! https://github.com/mattpocock/skills/tree/main/skills/productivity/grilling
use crate::agent::{QuestionItem, QuestionResponse};
use serde::Deserialize;
use serde_json::{json, Value};

pub const NAME: &str = "grill-me";
pub const INSTRUCTIONS: &str = "The /grill-me [topic] command explicitly starts an interview using this tool. Use the supplied topic, or clarify the interview scope if no topic or prior plan is available. Use grill-me when the user asks to be interviewed or to stress-test a plan, idea, or decision. Resolve observable facts with tools first. Organize decisions by dependency and ask a round of up to five independent questions whose prerequisites are settled, each with a concrete recommended answer. Wait for the answers, reuse settled decisions, and ask only newly unblocked questions next. Do not repeatedly ask answered or skipped questions. On cancellation stop the interview. When no open decisions remain, summarize choices, trade-offs and unresolved assumptions; obtain the user's agreement before implementing the plan. Ordinary implementation requests do not need an interview.";

/// Route this conversational slash command through the normal prompt/queue path.
pub fn is_command(input: &str) -> bool {
    input.split_whitespace().next() == Some("/grill-me")
}

pub fn definition() -> Value {
    crate::runtime::definition(
        NAME,
        INSTRUCTIONS,
        json!({
            "type":"object", "additionalProperties":false,
            "properties":{
                "topic":{"type":"string","minLength":1,"description":"Plan, idea or decision being stress-tested."},
                "questions":{"type":"array","minItems":1,"maxItems":5,"description":"Independent decisions for this round. Defer questions that depend on another unanswered question.",
                    "items":{"type":"object","additionalProperties":false,"properties":{
                        "question":{"type":"string","minLength":1,"description":"Concrete question, including the consequence or trade-off that needs a decision."},
                        "recommendation":{"type":"string","minLength":1,"description":"Proposed answer. Explain the reason briefly in the question; accepting this option accepts the recommendation."},
                        "alternatives":{"type":"array","maxItems":5,"items":{"type":"string","minLength":1},"description":"Optional alternatives to the recommended answer. Custom input is always available."},
                        "header":{"type":"string","description":"Optional short label."}
                    },"required":["question","recommendation"]}}
            },"required":["topic","questions"]
        }),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Round {
    pub topic: String,
    pub questions: Vec<Decision>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub question: String,
    pub recommendation: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    pub header: Option<String>,
}
impl Round {
    pub fn parse(arguments: &str) -> Result<Self, String> {
        let mut round: Self = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid grill-me round: {error}"))?;
        round.topic = round.topic.trim().into();
        if round.topic.is_empty() {
            return Err("grill-me requires a nonempty topic".into());
        }
        if !(1..=5).contains(&round.questions.len()) {
            return Err("grill-me requires 1 to 5 independent questions per round".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for decision in &mut round.questions {
            decision.question = decision.question.trim().into();
            decision.recommendation = decision.recommendation.trim().into();
            if decision.question.is_empty() || decision.recommendation.is_empty() {
                return Err("Each grill-me question needs a question and recommendation".into());
            }
            if !seen.insert(decision.question.to_lowercase()) {
                return Err("A grill-me round must not repeat a question".into());
            }
            if decision.alternatives.len() > 5 {
                return Err("At most 5 alternatives are allowed".into());
            }
            if decision
                .alternatives
                .iter()
                .any(|answer| answer.trim().is_empty())
            {
                return Err("Alternatives must not be empty".into());
            }
        }
        Ok(round)
    }
    pub fn questionnaire(&self) -> Vec<QuestionItem> {
        self.questions
            .iter()
            .map(|decision| {
                let mut options = vec![decision.recommendation.clone()];
                for alternative in &decision.alternatives {
                    let answer = alternative.trim().to_string();
                    if !options.contains(&answer) {
                        options.push(answer);
                    }
                }
                QuestionItem {
                    question: format!(
                        "{}\n\n{} {}",
                        decision.question,
                        crate::i18n::tr("Recommended answer:"),
                        decision.recommendation
                    ),
                    header: decision.header.clone().or_else(|| Some(self.topic.clone())),
                    options,
                    is_multi_select: false,
                    allow_custom: true,
                    placeholder: None,
                }
            })
            .collect()
    }
    pub fn result(&self, response: &QuestionResponse, answers: &str) -> String {
        let mut result: Value =
            serde_json::from_str(answers).expect("questionnaire_result produces JSON");
        result["tool"] = json!(NAME);
        result["topic"] = json!(self.topic);
        result["decisions"] = Value::Array(self.questions.iter().map(|question| json!({"question":question.question,"recommendation":question.recommendation})).collect());
        result["next_step"] = json!(if response.skipped {
            "Stop the interview. Preserve the answered decisions; do not ask another round or implement the plan."
        } else {
            "Use the answers to update the decision tree. Ask only remaining unblocked decisions. If none remain, summarize and wait for the user's agreement before implementation."
        });
        result.to_string()
    }
}

pub async fn wait_for_answer(
    receiver: tokio::sync::oneshot::Receiver<QuestionResponse>,
    cancel: &tokio_util::sync::CancellationToken,
) -> QuestionResponse {
    tokio::select! {
        biased;
        response = receiver => response.unwrap_or(QuestionResponse { answers:vec![],skipped:true }),
        _ = cancel.cancelled() => QuestionResponse { answers:vec![],skipped:true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn command_accepts_topic_or_empty_scope_and_rejects_prefix_collisions() {
        assert!(is_command("/grill-me"));
        assert!(is_command("/grill-me   архитектура редактора"));
        assert!(is_command("/grill-me\nMultiline topic"));
        assert!(!is_command("/grill-meet topic"));
        assert!(!is_command("Discuss /grill-me"));
        assert!(crate::prompt::SLASH_COMMANDS.iter().any(|command| command.name == "/grill-me"));
    }
    #[test]
    fn round_preserves_recommendations_custom_input_and_decision_order() {
        let round = Round::parse(r#"{"topic":" Offline editor ","questions":[{"question":" Which users first? ","recommendation":" Developers ","alternatives":["Designers","Developers"]},{"question":"Budget?","recommendation":"Small team"}]}"#).unwrap();
        let questions = round.questionnaire();
        assert_eq!(round.topic, "Offline editor");
        assert_eq!(questions[0].options, ["Developers", "Designers"]);
        assert!(questions[0].allow_custom && !questions[0].is_multi_select);
        assert!(questions[0].question.contains("Developers"));
        assert!(questions[1].question.starts_with("Budget?"));
    }
    #[test]
    fn malformed_rounds_never_open_an_empty_or_unbounded_questionnaire() {
        for input in [
            json!({"topic":"","questions":[]}),
            json!({"topic":"x","questions":[]}),
            json!({"topic":"x","questions":[{"question":"Q","recommendation":""}]}),
            json!({"topic":"x","questions":vec![json!({"question":"Q","recommendation":"A"});6]}),
        ] {
            assert!(Round::parse(&input.to_string()).is_err());
        }
        assert!(Round::parse(r#"{"topic":"x","questions":[{"question":"Q","recommendation":"A"},{"question":"q","recommendation":"B"}]}"#).is_err());
    }
    #[test]
    fn cancellation_preserves_answers_and_ends_the_interview() {
        let round=Round::parse(r#"{"topic":"Plan","questions":[{"question":"Audience?","recommendation":"Developers"}]}"#).unwrap();
        let response = QuestionResponse {
            answers: vec![],
            skipped: true,
        };
        let result: Value = serde_json::from_str(&round.result(
            &response,
            r#"{"status":"cancelled","answers":[{"custom_text":"Teams"}]}"#,
        ))
        .unwrap();
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["answers"][0]["custom_text"], "Teams");
        assert!(result["next_step"].as_str().unwrap().starts_with("Stop"));
    }
    #[tokio::test]
    async fn cancelled_question_wait_releases_the_agent() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            wait_for_answer(receiver, &cancel),
        )
        .await
        .unwrap();
        assert!(result.skipped);
    }
    #[test]
    fn tool_is_builtin_in_plan_mode_but_unavailable_to_readonly_workers() {
        let registry = crate::runtime::Registry::builtins();
        assert!(registry.get(NAME).is_some());
        assert!(crate::execution_mode::ExecutionMode::Plan.allows_tool(NAME));
        assert!(!crate::subagents::read_tool(NAME));
    }
}
