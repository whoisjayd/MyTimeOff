//! Where questions come from.
//!
//! The rules for getting past a quiz are in `mytimeoff-core`, because they are pure and
//! they are the part that must never be wrong. Making the questions is the opposite: it
//! needs the pages, a model and a network, and it will be replaced. So it sits behind a
//! trait, and the daemon holds one - which is what lets the offline stub and the real
//! generator be swapped without the gate noticing.

pub mod claude;
pub mod stub;

use std::fmt;
use std::sync::Arc;

use mytimeoff_core::{Locator, Question};

/// One page as a question source sees it: where it was, and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub locator: Locator,
    pub text: String,
}

#[derive(Debug)]
pub enum SourceError {
    /// The generator could not be reached, or refused. The caller decides what to do
    /// about it - and for a gate, the answer is never "keep them there".
    Unavailable(String),
    /// It answered, but not with questions.
    Malformed(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::Unavailable(detail) => write!(f, "question source unavailable: {detail}"),
            SourceError::Malformed(detail) => write!(f, "question source returned {detail}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// Anything that can turn pages into questions.
///
/// Fewer questions than asked for is a normal answer, not an error: a page can be a
/// chapter heading and a photograph, and there is nothing in that to ask about.
///
/// Boxed rather than `async fn` in the trait so the daemon can hold `dyn QuestionSource`
/// and swap the implementation at construction. This is what `#[async_trait]` writes for
/// you; written out, it needs no dependency.
pub trait QuestionSource: Send + Sync {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a>;
}

pub type Questions<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<Question>, SourceError>> + Send + 'a>,
>;

/// One source, with another behind it.
///
/// A gate that cannot ask a question releases the reader - which is right when the tool
/// genuinely has nothing to ask, and wrong when the only thing that failed was a network.
/// Under a flaky connection that rule turns into "close the laptop lid, get a free pass",
/// which is a hole big enough to walk the whole tool through.
///
/// So a failure falls through to something that cannot fail: no key, no network, an API
/// having a bad afternoon, and the reader still gets a quiz. A worse quiz - the stub's
/// questions are memorisable, and that is the honest cost - but the gate stays a gate.
///
/// An empty answer falls through too, though the trait calls it a normal result. Empty
/// from a real generator means "I found nothing to ask about"; empty at the gate means
/// "go on through". They are not the same thing, and the second is the wrong one to give
/// away on the first's say-so.
pub struct Fallback {
    first: Arc<dyn QuestionSource>,
    then: Arc<dyn QuestionSource>,
    /// Where a fall-through gets reported, so a reader working offline can find out why
    /// the questions got worse instead of just noticing that they did.
    note: Box<dyn Fn(String) + Send + Sync>,
}

impl Fallback {
    pub fn new(
        first: Arc<dyn QuestionSource>,
        then: Arc<dyn QuestionSource>,
        note: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Fallback { first, then, note: Box::new(note) }
    }
}

impl QuestionSource for Fallback {
    fn questions<'a>(&'a self, pages: &'a [Page], wanted: usize) -> Questions<'a> {
        Box::pin(async move {
            match self.first.questions(pages, wanted).await {
                Ok(questions) if !questions.is_empty() => Ok(questions),
                Ok(_) => {
                    (self.note)("quiz: no questions came back; falling back".to_string());
                    self.then.questions(pages, wanted).await
                }
                Err(error) => {
                    (self.note)(format!("quiz: {error}; falling back"));
                    self.then.questions(pages, wanted).await
                }
            }
        })
    }
}

/// FNV-1a. Wanted for being short, stable across runs and platforms, and completely
/// unrelated to security - which `DefaultHasher`, seeded per process, is not. Question ids
/// are built from it, and an id that changed between two fetches of the same quiz would
/// mark a correct answer wrong.
pub(super) fn hash(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mytimeoff_core::{Locator, Question};

    use super::*;

    struct Fixed(Result<usize, &'static str>);

    impl QuestionSource for Fixed {
        fn questions<'a>(&'a self, _pages: &'a [Page], _wanted: usize) -> Questions<'a> {
            let answer = match self.0 {
                Ok(count) => Ok((0..count).map(question).collect()),
                Err(why) => Err(SourceError::Unavailable(why.to_string())),
            };
            Box::pin(async move { answer })
        }
    }

    fn question(n: usize) -> Question {
        Question {
            id: format!("q{n}"),
            prompt: "?".to_string(),
            choices: vec!["a".to_string(), "b".to_string()],
            answer_index: 0,
            source: Locator::Page { page: 1, page_label: "1".to_string() },
        }
    }

    fn fallback(first: Fixed, then: Fixed) -> (Fallback, Arc<Mutex<Vec<String>>>) {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&notes);
        let source = Fallback::new(Arc::new(first), Arc::new(then), move |note| {
            heard.lock().expect("notes").push(note);
        });
        (source, notes)
    }

    fn ask(source: &Fallback) -> Vec<Question> {
        let pages = vec![Page {
            locator: Locator::Page { page: 1, page_label: "1".to_string() },
            text: "text".to_string(),
        }];
        futures_of(source.questions(&pages, 3)).expect("a fallback never fails")
    }

    /// Runs a future to completion without a runtime. These sources never yield, so one
    /// poll is the whole of it - and it keeps the test free of a runtime it would
    /// otherwise need only to prove a match arm.
    fn futures_of(future: Questions<'_>) -> Result<Vec<Question>, SourceError> {
        use std::task::{Context, Poll, Waker};
        let mut future = future;
        match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(answer) => answer,
            Poll::Pending => panic!("these sources do not wait on anything"),
        }
    }

    #[test]
    fn the_first_source_is_used_when_it_answers() {
        let (source, notes) = fallback(Fixed(Ok(2)), Fixed(Ok(9)));
        assert_eq!(ask(&source).len(), 2);
        assert!(notes.lock().expect("notes").is_empty(), "nothing to explain");
    }

    #[test]
    fn a_failure_falls_through_rather_than_opening_the_gate() {
        // The hole this closes: unplug the network, and without a fallback every gate
        // from then on releases with no questions at all.
        let (source, notes) = fallback(Fixed(Err("no network")), Fixed(Ok(3)));
        assert_eq!(ask(&source).len(), 3);
        assert!(notes.lock().expect("notes")[0].contains("no network"));
    }

    #[test]
    fn an_empty_answer_falls_through_too() {
        let (source, notes) = fallback(Fixed(Ok(0)), Fixed(Ok(3)));
        assert_eq!(ask(&source).len(), 3);
        assert_eq!(notes.lock().expect("notes").len(), 1);
    }

    #[test]
    fn nothing_anywhere_is_still_an_answer_and_never_an_error() {
        // An empty quiz releases the reader. A gate with two dead sources must not become
        // a gate that cannot be opened.
        let (source, _) = fallback(Fixed(Err("no network")), Fixed(Ok(0)));
        assert!(ask(&source).is_empty());
    }
}
