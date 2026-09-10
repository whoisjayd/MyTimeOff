//! Questions from a model, which is the version of this that actually works.
//!
//! The stub asks which word completes a line it shows you. That is eyesight, and worse,
//! it is memorisable: the same pages give the same quiz forever. This asks about what the
//! pages *said* - what happened, what was claimed, why someone did something - which is
//! the thing the whole tool is pretending to check, and which cannot be done without
//! something that has read the text.
//!
//! Two properties are load-bearing and are enforced here rather than hoped for:
//!
//! **The model never names its own source.** It is given pages numbered 1..n and answers
//! with those numbers; the `Locator` is looked up from the page the daemon actually sent.
//! A model that invented a locator could attribute a question to a page you never read,
//! and marking would be against a page that does not exist.
//!
//! **A bad question is dropped, not repaired and not fatal.** Answer out of range,
//! duplicate choices, empty prompt: that question goes, the rest of the quiz stands. One
//! malformed entry must not cost the whole gate, and a quiz assembled from what survived
//! is still a quiz.
//!
//! What leaves the machine is the text of the pages you just read. That is the trade this
//! makes, and the reason `model = ""` in the config turns it off entirely.

use std::time::Duration;

use mytimeoff_core::Question;
use serde_json::{Value, json};

use super::{Page, QuestionSource, Questions, SourceError, hash};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const VERSION: &str = "2023-06-01";
const TOOL: &str = "write_questions";

/// Generous for a few short questions, and a ceiling on a runaway answer.
const MAX_TOKENS: u32 = 2_048;
/// The gate is standing between the reader and their work, so this is a limit on how long
/// they can be made to wait, not on how long the model might like. Past it, the fallback
/// makes the quiz instead.
const TIMEOUT: Duration = Duration::from_secs(25);

/// Bounds on a usable multiple choice question. Two choices is a coin toss; more than six
/// is a reading test of the answers rather than of the page.
const MIN_CHOICES: usize = 3;
const MAX_CHOICES: usize = 6;

const SYSTEM: &str = "\
You write short comprehension checks for someone who has just read a few pages of a book \
and has to answer them before returning to their work.

The point is to reward attention, not memory for trivia. A good question is one that \
someone who read the page answers without hesitation, and someone who scrolled past it \
cannot answer by guessing. Ask what happened, what was claimed, what changed, why someone \
acted, what an argument rests on.

Every question must obey all of this:
- Answerable from the one page it cites, and from that page alone.
- Exactly one choice is right. The wrong ones are plainly wrong to anyone who read the \
page, and plausible to anyone who did not: same subject, same register, same kind of thing.
- Never quote a line and blank a word out of it. That tests eyesight, not reading.
- Never ask about page numbers, headings, typography, or how a word is spelled.
- Keep the question under 25 words and each choice under 12.

If a page has nothing worth asking about - a heading, front matter, an illustration, a \
page of nothing but a date - skip it. Fewer good questions is better than padding.";

pub struct Claude {
    key: String,
    model: String,
    http: reqwest::Client,
}

impl Claude {
    pub fn new(key: String, model: String) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| SourceError::Unavailable(error.to_string()))?;
        Ok(Claude { key, model, http })
    }

    async fn ask(&self, pages: &[Page], wanted: usize) -> Result<Vec<Question>, SourceError> {
        let response = self
            .http
            .post(ENDPOINT)
            .header("x-api-key", &self.key)
            .header("anthropic-version", VERSION)
            .json(&request(&self.model, pages, wanted))
            .send()
            .await
            .map_err(|error| SourceError::Unavailable(error.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| SourceError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            // The body carries the API's own explanation - a bad key, a rate limit, an
            // unknown model - and it is the only thing that makes this diagnosable. It
            // never contains the key: that went out in a header.
            return Err(SourceError::Unavailable(format!("HTTP {status}: {}", brief(&body))));
        }

        let reply: Value = serde_json::from_str(&body)
            .map_err(|error| SourceError::Malformed(format!("unreadable JSON: {error}")))?;
        let questions = harvest(&reply, pages, wanted);
        if questions.is_empty() {
            return Err(SourceError::Malformed("no usable questions".to_string()));
        }
        Ok(questions)
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

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "page": {
                            "type": "integer",
                            "description": "Which page this asks about, numbered as in the \
                                            input: 1 for the first page shown.",
                        },
                        "question": { "type": "string" },
                        "choices": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": MIN_CHOICES,
                            "maxItems": MAX_CHOICES,
                        },
                        "answer": {
                            "type": "integer",
                            "description": "Index into choices of the right one, from 0.",
                        },
                    },
                    "required": ["page", "question", "choices", "answer"],
                },
            },
        },
        "required": ["questions"],
    })
}

/// The pages as the model sees them: numbered from one, in reading order.
///
/// The label is shown too, because it is what the reader saw at the bottom of their
/// screen and it makes a question recognisable - but it is never what comes back.
fn brief_of(pages: &[Page], wanted: usize) -> String {
    let mut brief = format!(
        "Here are the pages I just read, in order. Write at most {wanted} \
         question{}, each about a different page.\n",
        if wanted == 1 { "" } else { "s" },
    );
    for (index, page) in pages.iter().enumerate() {
        brief.push_str(&format!(
            "\n--- page {} (printed page {}) ---\n{}\n",
            index + 1,
            page.locator.page_label(),
            page.text.trim(),
        ));
    }
    brief
}

/// Pulls the tool call out of the reply and keeps the questions that survive checking.
fn harvest(reply: &Value, pages: &[Page], wanted: usize) -> Vec<Question> {
    let Some(input) = tool_input(reply) else { return Vec::new() };
    let Some(items) = input.get("questions").and_then(Value::as_array) else { return Vec::new() };

    items
        .iter()
        .filter_map(|item| check(item, pages))
        .take(wanted)
        .collect()
}

/// The input of the first `write_questions` block, ignoring any thinking or prose beside it.
fn tool_input(reply: &Value) -> Option<&Value> {
    reply.get("content")?.as_array()?.iter().find_map(|block| {
        (block.get("type")? == "tool_use" && block.get("name")? == TOOL).then(|| block.get("input"))?
    })
}

/// Turns one entry into a `Question`, or None if it is not one.
///
/// Everything a wrong answer here would cost is checked: an answer index outside the
/// choices would panic the marker; a page outside the range would attribute the question
/// to something unread; two identical choices would make a question with two right
/// answers or none, depending on which the reader picked.
fn check(item: &Value, pages: &[Page]) -> Option<Question> {
    let number = item.get("page")?.as_u64()?;
    let page = pages.get(usize::try_from(number).ok()?.checked_sub(1)?)?;

    let prompt = item.get("question")?.as_str()?.trim();
    if prompt.is_empty() {
        return None;
    }

    let choices: Vec<String> = item
        .get("choices")?
        .as_array()?
        .iter()
        .filter_map(|choice| Some(choice.as_str()?.trim().to_string()))
        .filter(|choice| !choice.is_empty())
        .collect();
    if !(MIN_CHOICES..=MAX_CHOICES).contains(&choices.len()) {
        return None;
    }
    let distinct = choices
        .iter()
        .enumerate()
        .all(|(at, choice)| !choices[..at].iter().any(|seen| seen.eq_ignore_ascii_case(choice)));
    if !distinct {
        return None;
    }

    let answer_index = usize::try_from(item.get("answer")?.as_u64()?).ok()?;
    if answer_index >= choices.len() {
        return None;
    }

    Some(Question {
        // Stable for the same question about the same page, so a quiz fetched twice
        // marks the same way. The page number is in it to keep two pages that produce
        // the same wording apart.
        id: format!("q{number}-{:08x}", hash(prompt)),
        prompt: prompt.to_string(),
        choices,
        answer_index,
        source: page.locator.clone(),
    })
}

/// Enough of an error body to recognise it, and not enough to fill a log line.
fn brief(body: &str) -> String {
    let body = body.trim();
    match body.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use mytimeoff_core::Locator;
    use serde_json::json;

    use super::*;

    fn page(n: u32) -> Page {
        Page {
            locator: Locator::Page { page: n, page_label: format!("{}", n + 40) },
            text: format!("Something happened on page {n}."),
        }
    }

    fn reply(questions: Value) -> Value {
        json!({
            "content": [
                { "type": "text", "text": "Here you go." },
                { "type": "tool_use", "name": TOOL, "input": { "questions": questions } },
            ],
        })
    }

    fn sound() -> Value {
        json!({
            "page": 2,
            "question": "What did the cartographer do with the chart?",
            "choices": ["Folded it", "Burned it", "Sold it"],
            "answer": 0,
        })
    }

    #[test]
    fn a_question_is_attributed_to_the_page_the_daemon_sent() {
        let pages = vec![page(1), page(2), page(3)];
        let questions = harvest(&reply(json!([sound()])), &pages, 3);

        assert_eq!(questions.len(), 1);
        // Page 2 of the brief, whatever the book calls it. The model never chooses this.
        assert_eq!(questions[0].source, pages[1].locator);
        assert_eq!(questions[0].answer_index, 0);
    }

    #[test]
    fn prose_beside_the_tool_call_is_ignored() {
        let questions = harvest(&reply(json!([sound()])), &[page(1), page(2)], 3);
        assert_eq!(questions.len(), 1);
    }

    #[test]
    fn a_bad_question_is_dropped_and_the_rest_stand() {
        let mut answer_out_of_range = sound();
        answer_out_of_range["answer"] = json!(7);
        let mut page_never_read = sound();
        page_never_read["page"] = json!(99);
        let mut repeated_choices = sound();
        repeated_choices["choices"] = json!(["Folded it", "folded it", "Sold it"]);
        let mut blank = sound();
        blank["question"] = json!("   ");
        let mut too_few = sound();
        too_few["choices"] = json!(["Folded it", "Burned it"]);

        let items =
            json!([answer_out_of_range, page_never_read, repeated_choices, blank, too_few, sound()]);
        let questions = harvest(&reply(items), &[page(1), page(2)], 5);

        // One malformed entry must not cost the gate the whole quiz.
        assert_eq!(questions.len(), 1, "{questions:?}");
        assert_eq!(questions[0].prompt, "What did the cartographer do with the chart?");
    }

    #[test]
    fn page_zero_is_not_a_page() {
        // 1-based on the wire; 0 would silently become the last page under wrapping
        // arithmetic, which is a question about something you did not read.
        let mut zero = sound();
        zero["page"] = json!(0);
        assert!(harvest(&reply(json!([zero])), &[page(1), page(2)], 3).is_empty());
    }

    #[test]
    fn more_questions_than_asked_for_are_cut() {
        let items = json!([sound(), sound(), sound()]);
        assert_eq!(harvest(&reply(items), &[page(1), page(2)], 2).len(), 2);
    }

    #[test]
    fn a_reply_with_no_tool_call_yields_nothing_rather_than_a_panic() {
        let prose = json!({ "content": [{ "type": "text", "text": "I would rather not." }] });
        assert!(harvest(&prose, &[page(1)], 3).is_empty());
        assert!(harvest(&json!({}), &[page(1)], 3).is_empty());
        assert!(harvest(&reply(json!("not an array")), &[page(1)], 3).is_empty());
    }

    #[test]
    fn the_same_question_gets_the_same_id() {
        let first = harvest(&reply(json!([sound()])), &[page(1), page(2)], 3);
        let second = harvest(&reply(json!([sound()])), &[page(1), page(2)], 3);
        assert_eq!(first[0].id, second[0].id, "a refetched quiz must mark the same way");
    }

    #[test]
    fn the_brief_numbers_pages_from_one_and_shows_what_the_reader_saw() {
        let brief = brief_of(&[page(1), page(2)], 3);
        assert!(brief.contains("--- page 1 (printed page 41) ---"), "{brief}");
        assert!(brief.contains("--- page 2 (printed page 42) ---"), "{brief}");
        assert!(brief.contains("at most 3 questions"), "{brief}");
    }

    #[test]
    fn the_request_forces_the_tool_rather_than_suggesting_it() {
        let body = request("claude-haiku-4-5-20251001", &[page(1)], 3);
        assert_eq!(body["tool_choice"], json!({ "type": "tool", "name": TOOL }));
        assert_eq!(body["model"], "claude-haiku-4-5-20251001");
        assert_eq!(body["tools"][0]["name"], TOOL);
    }

    #[test]
    fn an_error_body_is_shortened_not_dumped() {
        assert_eq!(brief("  short  "), "short");
        assert_eq!(brief(&"x".repeat(500)).chars().count(), 201);
    }
}
