//! Anthropic's half of asking a model for questions.
//!
//! What a good question is, how the pages are presented and what makes an answer usable
//! are in `writing.rs`, shared with every other provider. What is here is only what is
//! actually Anthropic's: the address, the version header, and the fact that the answer
//! arrives as a forced tool call rather than as text.
//!
//! Two properties are load-bearing and are enforced in `writing.rs` rather than hoped for:
//! the model never names its own source - it is given pages numbered 1..n and answers with
//! those numbers, and the `Locator` is looked up from the page the daemon actually sent -
//! and a bad question is dropped rather than repaired or fatal.
//!
//! What leaves the machine is the text of the pages you just read. That is the trade this
//! makes, and the reason `model = ""` in the config turns it off entirely.

use mytimeoff_core::Question;
use serde_json::{Value, json};

use super::writing::{self, MAX_TOKENS, SYSTEM, brief_of, schema};
use super::{Page, QuestionSource, Questions, SourceError};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const VERSION: &str = "2023-06-01";
const TOOL: &str = "write_questions";

/// Model names this provider answers for. The config carries one model string and no
/// provider field, so the family is read off the name.
pub const PREFIX: &str = "claude-";

pub struct Claude {
    key: String,
    model: String,
    http: reqwest::Client,
}

impl Claude {
    pub fn new(key: String, model: String) -> Result<Self, SourceError> {
        Ok(Claude { key, model, http: writing::client()? })
    }

    async fn ask(&self, pages: &[Page], wanted: usize) -> Result<Vec<Question>, SourceError> {
        let request = self
            .http
            .post(ENDPOINT)
            .header("x-api-key", &self.key)
            .header("anthropic-version", VERSION)
            .json(&request(&self.model, pages, wanted));
        writing::ask(request, tool_input, pages, wanted).await
    }
}

impl QuestionSource for Claude {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a> {
        Box::pin(self.ask(pages, wanted))
    }
}

/// The whole request body. Separate from sending it so it can be read, and tested,
/// without a network.
fn request(model: &str, pages: &[Page], wanted: usize) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM,
        "messages": [{ "role": "user", "content": brief_of(pages, wanted) }],
        "tools": [{
            "name": TOOL,
            "description": "Return the comprehension questions for these pages.",
            "input_schema": schema(),
        }],
        // Not a suggestion. Without this the model may answer in prose, and prose is a
        // parsing problem rather than a quiz.
        "tool_choice": { "type": "tool", "name": TOOL },
    })
}

/// The input of the first `write_questions` block, ignoring any thinking or prose beside it.
fn tool_input(reply: &Value) -> Option<Value> {
    reply.get("content")?.as_array()?.iter().find_map(|block| {
        (block.get("type")? == "tool_use" && block.get("name")? == TOOL)
            .then(|| block.get("input").cloned())?
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::writing::{fixtures::page, harvest};
    use super::*;

    fn reply(questions: Value) -> Value {
        json!({
            "content": [
                { "type": "text", "text": "Here you go." },
                { "type": "tool_use", "name": TOOL, "input": { "questions": questions } },
            ],
        })
    }

    #[test]
    fn prose_beside_the_tool_call_is_ignored() {
        let input = tool_input(&reply(json!([]))).expect("the tool call is found past the prose");
        assert!(input.get("questions").is_some());
    }

    #[test]
    fn a_reply_with_no_tool_call_yields_nothing_rather_than_a_panic() {
        let prose = json!({ "content": [{ "type": "text", "text": "I would rather not." }] });
        assert!(tool_input(&prose).is_none());
        assert!(tool_input(&json!({})).is_none());
        assert!(tool_input(&json!({ "content": "not an array" })).is_none());
    }

    #[test]
    fn the_tool_call_carries_the_questions_through_to_checking() {
        // The seam this covers: unwrapping is this file's job, checking is writing.rs's,
        // and the two have to meet on the same object.
        let sound = super::super::writing::fixtures::sound();
        let input = tool_input(&reply(json!([sound]))).expect("tool call");
        let questions = harvest(&input, &[page(1), page(2)], 3);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].source, page(2).locator);
    }

    #[test]
    fn the_request_forces_the_tool_rather_than_suggesting_it() {
        let body = request("claude-haiku-4-5-20251001", &[page(1)], 3);
        assert_eq!(body["tool_choice"], json!({ "type": "tool", "name": TOOL }));
        assert_eq!(body["model"], "claude-haiku-4-5-20251001");
        assert_eq!(body["tools"][0]["name"], TOOL);
        // The instructions go in the field Anthropic has for them.
        assert_eq!(body["system"], SYSTEM);
    }
}
