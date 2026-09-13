//! Google's half of asking a model for questions.
//!
//! Everything about what a good question is lives in `writing.rs`. What is here is what is
//! actually Google's: the Interactions endpoint, the `x-goog-api-key` header, and the fact
//! that a structured answer comes back as JSON *text*, buried a few levels down, rather
//! than as a tool call.
//!
//! One deliberate deviation from the obvious reading of the docs, because a wrong field
//! here does not fail loudly - it 400s on every gate and quietly falls back to the offline
//! stub forever:
//!
//! **The reply is read leniently.** The documented path is
//! `steps[] → model_output → content[] → text`, and that is what is tried first; if the
//! shape has moved, any text part in the reply is taken rather than nothing. Reading a
//! reply loosely is safe in a way that writing a request loosely is not: whatever comes
//! out still has to survive `writing::check` before it can become a question.
//!
//! What was checked, and how:
//!
//! - The endpoint is real. `v1beta/interactions` answers 400; `v1beta2/interactions` and
//!   every deliberately misspelt variant answer 404. That settles a genuine conflict
//!   between two of Google's own pages, which disagree about the version.
//! - The header is the right channel for the key: the 400 is `API_KEY_INVALID`, which is
//!   a key being read and rejected rather than a request being misunderstood.
//! - Every field name below is from the CreateInteraction reference, which is where the
//!   `system_instruction` question above was finally answered: it takes a plain string.
//!   They have since been confirmed against the live API with a real key: this request
//!   body, unchanged, comes back with usable questions. That confirmation was worth
//!   waiting for, because with an *invalid* key nothing here can be checked at all - the
//!   key is validated before the body, so a request of pure nonsense returns the same
//!   `API_KEY_INVALID` as a correct one.
//! - What that first real call found was `thinking_level`, which is not one vocabulary
//!   across models: see [`THINKING`]. It is the shape of failure to expect from this
//!   file. A name or a value that goes stale is a 400 at every gate, and the fallback
//!   turns that into stub questions rather than an error, so nothing is visibly wrong.
//!   `mytimeoff check` is what makes it visible. Worth running once after storing
//!   a key, and again after changing the model.

use mytimeoff_core::Question;
use serde_json::{Value, json};

use super::writing::{self, MAX_TOKENS, SYSTEM, brief_of, schema};
use super::{Page, QuestionSource, Questions, SourceError};

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";

/// Model names this provider answers for.
pub const PREFIX: &str = "gemini-";

/// This runs while the reader is standing at a closed gate. Comprehension questions about
/// a page of prose are not the kind of problem that gets better with deliberation, and
/// every second of it is a second of someone waiting.
///
/// "low" rather than "minimal", which is the lowest level this API accepts across models
/// rather than the lowest one it names. `gemini-3.5-flash` takes "minimal"; ask
/// `gemini-3.8-flash` for it and the whole request is a 400 - "'minimal' is not a
/// supported thinking level for this model. Allowed values are: high, low, medium." A
/// value only some models accept is the worst kind of constant to hold here, because the
/// fallback turns the refusal into silence: every gate quietly gets stub questions, and
/// the only way to see it is `mytimeoff check`. Which is how this was found.
const THINKING: &str = "low";

pub struct Gemini {
    key: String,
    model: String,
    http: reqwest::Client,
}

impl Gemini {
    pub fn new(key: String, model: String) -> Result<Self, SourceError> {
        Ok(Gemini { key, model, http: writing::client()? })
    }

    async fn ask(&self, pages: &[Page], wanted: usize) -> Result<Vec<Question>, SourceError> {
        let request = self
            .http
            .post(ENDPOINT)
            // The header form, not `?key=`. Both work; only one of them keeps the key out
            // of URLs, proxy logs and crash reports.
            .header("x-goog-api-key", &self.key)
            .json(&request(&self.model, pages, wanted));
        writing::ask(request, answer, pages, wanted).await
    }
}

impl QuestionSource for Gemini {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a> {
        Box::pin(self.ask(pages, wanted))
    }
}

/// The whole request body. Separate from sending it so it can be read, and tested,
/// without a network.
fn request(model: &str, pages: &[Page], wanted: usize) -> Value {
    json!({
        "model": model,
        // A plain string, not a Content object. This is the one field the reference had to
        // settle, since getting it wrong fails silently.
        "system_instruction": SYSTEM,
        "input": brief_of(pages, wanted),
        "generation_config": {
            "max_output_tokens": MAX_TOKENS,
            "thinking_level": THINKING,
        },
        // The equivalent of Anthropic's forced tool call: without it the model may answer
        // in prose, and prose is a parsing problem rather than a quiz.
        "response_format": {
            "type": "text",
            "mime_type": "application/json",
            "schema": schema(),
        },
    })
}

/// The answer object, dug out of the reply and parsed.
///
/// Unlike a tool call, this arrives as a string that happens to contain JSON, and a long
/// answer may have been assembled from several text parts - so they are joined in order
/// and parsed once.
fn answer(reply: &Value) -> Option<Value> {
    let text = text_of(reply);
    if text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(text.trim()).ok()
}

/// Every text part of the model's output, in order.
///
/// The documented path first. Failing that, any text part anywhere in the reply: the API
/// is young enough to move, and a reply that has moved is worth more than nothing given
/// that everything in it still has to pass checking afterwards.
fn text_of(reply: &Value) -> String {
    let Some(steps) = reply.get("steps").and_then(Value::as_array) else {
        return String::new();
    };

    let output = |only_model_output: bool| {
        steps
            .iter()
            .filter(|step| {
                !only_model_output
                    || step.get("type").and_then(Value::as_str) == Some("model_output")
            })
            .filter_map(|step| step.get("content")?.as_array())
            .flatten()
            .filter_map(|part| part.get("text")?.as_str())
            .collect::<Vec<_>>()
            .concat()
    };

    let documented = output(true);
    if documented.is_empty() { output(false) } else { documented }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::writing::fixtures::{page, sound};
    use super::super::writing::harvest;
    use super::*;

    /// A reply in the shape the docs describe: the answer as JSON text inside a
    /// `model_output` step.
    fn reply(questions: Value) -> Value {
        json!({
            "steps": [
                { "type": "thought", "content": [{ "type": "text", "text": "Thinking." }] },
                {
                    "type": "model_output",
                    "content": [{
                        "type": "text",
                        "text": json!({ "questions": questions }).to_string(),
                    }],
                },
            ],
        })
    }

    #[test]
    fn the_answer_is_read_out_of_a_model_output_step() {
        let payload = answer(&reply(json!([sound()]))).expect("an answer");
        let questions = harvest(&payload, &[page(1), page(2)], 3);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].source, page(2).locator);
    }

    #[test]
    fn a_thought_beside_the_output_is_not_mistaken_for_the_answer() {
        // The thought step's text is not JSON. If it were joined to the answer, nothing
        // would parse.
        assert!(answer(&reply(json!([sound()]))).is_some());
    }

    #[test]
    fn an_answer_split_across_parts_is_joined_before_parsing() {
        let whole = json!({ "questions": [sound()] }).to_string();
        let (head, tail) = whole.split_at(whole.len() / 2);
        let split = json!({
            "steps": [{
                "type": "model_output",
                "content": [
                    { "type": "text", "text": head },
                    { "type": "text", "text": tail },
                ],
            }],
        });
        let payload = answer(&split).expect("the halves make one document");
        assert_eq!(harvest(&payload, &[page(1), page(2)], 3).len(), 1);
    }

    #[test]
    fn an_untyped_step_is_still_read_rather_than_dropped() {
        // The lenient half: if the step type is renamed, the answer is still found.
        let moved = json!({
            "steps": [{
                "content": [{ "type": "text", "text": json!({ "questions": [] }).to_string() }],
            }],
        });
        assert!(answer(&moved).is_some());
    }

    #[test]
    fn a_reply_with_no_answer_yields_nothing_rather_than_a_panic() {
        assert!(answer(&json!({})).is_none());
        assert!(answer(&json!({ "steps": "not an array" })).is_none());
        assert!(answer(&json!({ "steps": [] })).is_none());
        // Present but not JSON: an error to report, not something to salvage.
        let prose = json!({
            "steps": [{ "type": "model_output", "content": [{ "text": "I would rather not." }] }],
        });
        assert!(answer(&prose).is_none());
    }

    #[test]
    fn the_request_asks_for_json_rather_than_hoping_for_it() {
        let body = request("gemini-3.5-flash", &[page(1)], 3);
        assert_eq!(body["model"], "gemini-3.5-flash");
        assert_eq!(body["response_format"]["mime_type"], "application/json");
        assert_eq!(body["response_format"]["schema"], schema());
        assert_eq!(body["generation_config"]["thinking_level"], THINKING);
    }

    #[test]
    fn the_thinking_level_is_one_that_every_model_accepts() {
        // "minimal" is named by the API and refused by some models, and the refusal is a
        // 400 on the whole request rather than a fallback to a level that works. There is
        // no network here to catch that, so this is the next best thing: the three levels
        // below are the ones a model has never refused.
        assert!(
            matches!(THINKING, "low" | "medium" | "high"),
            "{THINKING:?} is not accepted by every model; a gate would silently get stub questions",
        );
    }

    #[test]
    fn the_rules_go_in_the_field_google_has_for_them() {
        let body = request("gemini-3.5-flash", &[page(1)], 3);
        // A bare string. A Content object here would be accepted by serde and refused by
        // Google, which is exactly the failure that hides behind a fallback.
        assert_eq!(body["system_instruction"], json!(SYSTEM));
        let input = body["input"].as_str().expect("input is a plain string");
        assert!(input.contains("--- page 1 (printed page 41) ---"), "{input}");
        assert!(!input.contains(SYSTEM), "the rules travel once, not twice");
    }
}
