//! What a good question is, and what makes one usable - independent of who writes it.
//!
//! Two providers now answer the same brief, and almost none of that brief is theirs. The
//! instructions, the way pages are presented, the shape asked for and the checking of what
//! comes back are all properties of *this tool*, not of an API. Only two things genuinely
//! differ between vendors: how a request is addressed, and where in the reply the answer
//! is buried. Those live in `claude.rs` and `gemini.rs`; everything else lives here.
//!
//! Keeping it this way is not tidiness. A second copy of `check` is a second place for the
//! answer-index bound to be wrong, and that bound is the difference between marking a quiz
//! and panicking in the middle of one.

use std::time::Duration;

use mytimeoff_core::Question;
use serde_json::{Value, json};

use super::{Page, SourceError, hash};

/// Bounds on a usable multiple choice question. Two choices is a coin toss; more than six
/// is a reading test of the answers rather than of the page.
pub const MIN_CHOICES: usize = 3;
pub const MAX_CHOICES: usize = 6;

/// Generous for a few short questions, and a ceiling on a runaway answer.
pub const MAX_TOKENS: u32 = 2_048;

/// The gate is standing between the reader and their work, so this is a limit on how long
/// they can be made to wait, not on how long a model might like. Past it, the fallback
/// makes the quiz instead. It is the same for every provider because it is a fact about
/// the reader's patience, not about anyone's infrastructure.
pub const TIMEOUT: Duration = Duration::from_secs(25);

/// The HTTP client every provider uses, so the wait is the same whoever is asked.
pub fn client() -> Result<reqwest::Client, SourceError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| SourceError::Unavailable(error.to_string()))
}

pub const SYSTEM: &str = "\
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

/// The shape both providers are asked for.
///
/// Plain JSON Schema with lowercase type names, which is the subset Anthropic's tool input
/// and Google's response format both accept. Where the two dialects diverge is in what
/// wraps this object, not in the object itself.
pub fn schema() -> Value {
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
pub fn brief_of(pages: &[Page], wanted: usize) -> String {
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

/// Sends a built request and keeps the questions that survive checking.
///
/// `extract` is the whole of what a provider has to explain about its replies: hand back
/// the object holding `questions`, or None if this reply does not contain one.
pub async fn ask(
    request: reqwest::RequestBuilder,
    extract: fn(&Value) -> Option<Value>,
    pages: &[Page],
    wanted: usize,
) -> Result<Vec<Question>, SourceError> {
    let response =
        request.send().await.map_err(|error| SourceError::Unavailable(error.to_string()))?;

    let status = response.status();
    let body = response.text().await.map_err(|error| SourceError::Unavailable(error.to_string()))?;
    if !status.is_success() {
        // The body carries the API's own explanation - a bad key, a rate limit, an
        // unknown model - and it is the only thing that makes this diagnosable. It never
        // contains the key: that went out in a header.
        return Err(SourceError::Unavailable(format!("HTTP {status}: {}", snippet(&body))));
    }

    let reply: Value = serde_json::from_str(&body)
        .map_err(|error| SourceError::Malformed(format!("unreadable JSON: {error}")))?;
    let Some(payload) = extract(&reply) else {
        return Err(SourceError::Malformed(format!("no answer in the reply: {}", snippet(&body))));
    };

    let questions = harvest(&payload, pages, wanted);
    if questions.is_empty() {
        return Err(SourceError::Malformed("no usable questions".to_string()));
    }
    Ok(questions)
}

/// Keeps the entries that survive checking, and no more than were asked for.
pub fn harvest(payload: &Value, pages: &[Page], wanted: usize) -> Vec<Question> {
    let Some(items) = payload.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(|item| check(item, pages)).take(wanted).collect()
}

/// Turns one entry into a `Question`, or None if it is not one.
///
/// Everything a wrong answer here would cost is checked: an answer index outside the
/// choices would panic the marker; a page outside the range would attribute the question
/// to something unread; two identical choices would make a question with two right
/// answers or none, depending on which the reader picked.
pub fn check(item: &Value, pages: &[Page]) -> Option<Question> {
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
        // Stable for the same question about the same page, so a quiz fetched twice marks
        // the same way. The page number is in it to keep two pages that produce the same
        // wording apart.
        id: format!("q{number}-{:08x}", hash(prompt)),
        prompt: prompt.to_string(),
        choices,
        answer_index,
        source: page.locator.clone(),
    })
}

/// Enough of an error body to recognise it, and not enough to fill a log line.
pub fn snippet(body: &str) -> String {
    let body = body.trim();
    match body.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None => body.to_string(),
    }
}

#[cfg(test)]
pub(super) mod fixtures {
    use mytimeoff_core::Locator;
    use serde_json::{Value, json};

    use super::super::Page;

    pub fn page(n: u32) -> Page {
        Page {
            locator: Locator::Page { page: n, page_label: format!("{}", n + 40) },
            text: format!("Something happened on page {n}."),
        }
    }

    /// One entry that passes every check, so a test can spoil exactly one field of it.
    pub fn sound() -> Value {
        json!({
            "page": 2,
            "question": "What did the cartographer do with the chart?",
            "choices": ["Folded it", "Burned it", "Sold it"],
            "answer": 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::fixtures::{page, sound};
    use super::*;

    #[test]
    fn a_question_is_attributed_to_the_page_the_daemon_sent() {
        let pages = vec![page(1), page(2), page(3)];
        let questions = harvest(&json!({ "questions": [sound()] }), &pages, 3);

        assert_eq!(questions.len(), 1);
        // Page 2 of the brief, whatever the book calls it. No provider chooses this.
        assert_eq!(questions[0].source, pages[1].locator);
        assert_eq!(questions[0].answer_index, 0);
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

        let payload = json!({
            "questions": [
                answer_out_of_range, page_never_read, repeated_choices, blank, too_few, sound(),
            ],
        });
        let questions = harvest(&payload, &[page(1), page(2)], 5);

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
        assert!(harvest(&json!({ "questions": [zero] }), &[page(1), page(2)], 3).is_empty());
    }

    #[test]
    fn more_questions_than_asked_for_are_cut() {
        let payload = json!({ "questions": [sound(), sound(), sound()] });
        assert_eq!(harvest(&payload, &[page(1), page(2)], 2).len(), 2);
    }

    #[test]
    fn a_payload_without_questions_yields_nothing_rather_than_a_panic() {
        assert!(harvest(&json!({}), &[page(1)], 3).is_empty());
        assert!(harvest(&json!({ "questions": "not an array" }), &[page(1)], 3).is_empty());
        assert!(harvest(&json!("not an object"), &[page(1)], 3).is_empty());
    }

    #[test]
    fn the_same_question_gets_the_same_id() {
        let payload = json!({ "questions": [sound()] });
        let first = harvest(&payload, &[page(1), page(2)], 3);
        let second = harvest(&payload, &[page(1), page(2)], 3);
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
    fn an_error_body_is_shortened_not_dumped() {
        assert_eq!(snippet("  short  "), "short");
        assert_eq!(snippet(&"x".repeat(500)).chars().count(), 201);
    }
}
