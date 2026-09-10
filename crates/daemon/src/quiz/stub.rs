//! A question source that needs no model and no network.
//!
//! It exists so the whole gate - generate, ask, mark, release - can be built and tested
//! before any API key does. The questions it makes are cloze: a real line from a page
//! with one word removed, and the removed word offered among words taken from the other
//! pages you read. That is recognition rather than comprehension, which is exactly the
//! honest limit of what can be done without a model. It is correct by construction: the
//! right answer really was on the page, and the wrong ones really were not on that line.
//!
//! Everything here is deterministic. The same pages produce the same quiz, which is what
//! makes it testable - and what makes it obviously the wrong thing to ship, because a
//! quiz you can memorise is not a gate. That is what step D replaces.

use mytimeoff_core::Question;

use super::{Page, Questions, QuestionSource};

/// Choices per question, including the right one.
const CHOICES: usize = 4;
/// Shorter words carry too little of the sentence to be worth blanking out.
const DISTINCTIVE_LEN: usize = 6;
/// A line shorter than this gives too little context to complete.
const MIN_SENTENCE_LEN: usize = 40;

pub struct Stub;

impl QuestionSource for Stub {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a> {
        Box::pin(async move { Ok(build(pages, wanted)) })
    }
}

fn build(pages: &[Page], wanted: usize) -> Vec<Question> {
    let mut questions = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        if questions.len() == wanted {
            break;
        }
        if let Some(question) = ask_about(page, index, pages) {
            questions.push(question);
        }
    }
    questions
}

/// Builds one question about `page`, or None if the page has nothing to ask about.
fn ask_about(page: &Page, index: usize, all: &[Page]) -> Option<Question> {
    // The line with the most to say is the one worth asking about.
    let sentence = sentences(&page.text)
        .into_iter()
        .max_by_key(|sentence| distinctive_words(sentence).len())?;
    let answer = *distinctive_words(sentence).iter().max_by_key(|word| word.len())?;

    let distractors = distractors(sentence, answer, index, all);
    if distractors.len() + 1 < CHOICES {
        // Too little read to offer a real choice. Asking anyway would mean a question
        // answerable by elimination, which teaches the wrong lesson about this tool.
        return None;
    }

    // Deterministic, but not the same slot every time: an answer always at index 0 is a
    // quiz you pass without reading.
    let slot = (hash(sentence) as usize) % CHOICES;
    let mut choices: Vec<String> = distractors.into_iter().map(str::to_string).collect();
    choices.insert(slot, answer.to_string());

    Some(Question {
        id: format!("q{}-{:08x}", index + 1, hash(sentence)),
        prompt: format!(
            "Page {}: which word completes this line?\n\n{}",
            page.locator.page_label(),
            sentence.replace(answer, "______"),
        ),
        choices,
        answer_index: slot,
        source: page.locator.clone(),
    })
}

/// Wrong answers, preferring words from the other pages you read.
///
/// Preferring, not requiring: a single-page gate would otherwise have no question at all.
/// Falling back to the same page keeps the wrong answers plausible - same book, same
/// vocabulary - and they are still words that were not on the line being completed.
fn distractors<'a>(
    sentence: &str,
    answer: &str,
    index: usize,
    all: &'a [Page],
) -> Vec<&'a str> {
    let usable = |word: &&str| {
        !word.eq_ignore_ascii_case(answer) && !sentence.contains(*word)
    };

    let elsewhere = all
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .flat_map(|(_, page)| page.text.split_whitespace().collect::<Vec<_>>());
    let same_page = all.get(index).into_iter().flat_map(|page| page.text.split_whitespace());

    let mut chosen: Vec<&str> = Vec::new();
    for word in elsewhere.chain(same_page) {
        let word = trim_word(word);
        if word.len() < DISTINCTIVE_LEN || !word.chars().all(char::is_alphabetic) {
            continue;
        }
        if !usable(&word) || chosen.iter().any(|seen| seen.eq_ignore_ascii_case(word)) {
            continue;
        }
        chosen.push(word);
        if chosen.len() == CHOICES - 1 {
            break;
        }
    }
    chosen
}

/// Splits on sentence enders, keeping only lines long enough to be completable.
fn sentences(text: &str) -> Vec<&str> {
    text.split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|sentence| sentence.len() >= MIN_SENTENCE_LEN)
        .collect()
}

fn distinctive_words(sentence: &str) -> Vec<&str> {
    sentence
        .split_whitespace()
        .map(trim_word)
        .filter(|word| word.len() >= DISTINCTIVE_LEN && word.chars().all(char::is_alphabetic))
        .collect()
}

/// Strips the punctuation a word carries, so "chapter," and "chapter" are one word.
fn trim_word(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

/// FNV-1a. Wanted here for being short, stable across runs and platforms, and completely
/// unrelated to security - which `DefaultHasher`, seeded per process, is not.
fn hash(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use mytimeoff_core::Locator;

    use super::*;

    fn page(n: u32, text: &str) -> Page {
        Page {
            locator: Locator::Page { page: n, page_label: n.to_string() },
            text: text.to_string(),
        }
    }

    fn library() -> Vec<Page> {
        vec![
            page(1, "The cartographer folded the enormous chart against the wind. \
                     Nothing else happened."),
            page(2, "Her brother inherited the observatory and its broken telescope. \
                     He never mentioned it."),
            page(3, "Every harbour on that coastline remembered the shipwreck differently. \
                     Some remembered nothing."),
            page(4, "The librarian catalogued each pamphlet before the building was demolished."),
        ]
    }

    #[test]
    fn a_question_is_asked_about_each_page() {
        let questions = build(&library(), 3);
        assert_eq!(questions.len(), 3);
        assert_eq!(
            questions.iter().map(|q| q.source.page_label().to_string()).collect::<Vec<_>>(),
            vec!["1", "2", "3"],
        );
    }

    #[test]
    fn the_right_answer_was_really_on_the_page() {
        for question in build(&library(), 4) {
            let answer = &question.choices[question.answer_index];
            let page = library()
                .into_iter()
                .find(|p| p.locator == question.source)
                .expect("source page");
            assert!(page.text.contains(answer.as_str()), "{answer} was not on {}", page.text);
        }
    }

    #[test]
    fn the_wrong_answers_were_not() {
        // The blanked line must have exactly one word that completes it, or the question
        // is unanswerable rather than hard.
        for question in build(&library(), 4) {
            let line = question.prompt.split("\n\n").nth(1).expect("line");
            for (index, choice) in question.choices.iter().enumerate() {
                if index == question.answer_index {
                    continue;
                }
                assert!(!line.contains(choice.as_str()), "{choice} was already on the line");
            }
        }
    }

    #[test]
    fn the_answer_is_not_always_in_the_same_place() {
        let slots: Vec<usize> = build(&library(), 4).iter().map(|q| q.answer_index).collect();
        assert!(slots.iter().any(|slot| *slot != slots[0]), "{slots:?} is a guessable quiz");
    }

    #[test]
    fn every_question_offers_a_full_set_of_choices() {
        for question in build(&library(), 4) {
            assert_eq!(question.choices.len(), CHOICES);
            let mut unique = question.choices.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), CHOICES, "a repeated choice is a free elimination");
        }
    }

    #[test]
    fn the_word_is_actually_removed_from_the_line() {
        let question = build(&library(), 1).pop().expect("a question");
        let answer = &question.choices[question.answer_index];
        assert!(question.prompt.contains("______"));
        assert!(!question.prompt.contains(answer.as_str()), "the answer is still in the prompt");
    }

    #[test]
    fn a_page_with_nothing_to_ask_about_is_skipped() {
        let pages = vec![page(1, "Chapter Four"), library()[1].clone(), library()[2].clone()];
        let questions = build(&pages, 3);
        assert!(
            questions.iter().all(|q| q.source != pages[0].locator),
            "a chapter heading is not a question",
        );
    }

    #[test]
    fn one_page_alone_can_still_be_asked_about() {
        // A short reading stretch must not silently produce an empty quiz.
        let long = page(
            1,
            "The cartographer folded the enormous chart against the freezing wind. \
             Somewhere beneath the harbour the machinery continued turning.",
        );
        assert_eq!(build(&[long], 1).len(), 1);
    }

    #[test]
    fn the_same_pages_produce_the_same_quiz() {
        assert_eq!(build(&library(), 4), build(&library(), 4));
    }
}
