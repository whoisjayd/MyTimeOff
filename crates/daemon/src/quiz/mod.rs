//! Where questions come from.
//!
//! The rules for getting past a quiz are in `mytimeoff-core`, because they are pure and
//! they are the part that must never be wrong. Making the questions is the opposite: it
//! needs the pages, a model and a network, and it will be replaced. So it sits behind a
//! trait, and the daemon holds one - which is what lets the offline stub and the real
//! generator be swapped without the gate noticing.

pub mod claude;
pub mod gemini;
pub mod stub;
pub mod writing;

use std::fmt;
use std::sync::Arc;

use mytimeoff_core::{Config, Locator, Question};

use crate::secret;

use self::stub::Stub;

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

/// Who to ask, worked out from the model name.
///
/// There is no `provider` field in the config, and adding one would be adding a second
/// thing to get wrong: `model = "gemini-3.5-flash"` with `provider = "anthropic"` is a
/// configuration that reads perfectly and cannot work. The model name already says which
/// family it belongs to, so it is the only thing asked for.
///
/// The cost is a name that fits no prefix, and that is refused at startup with a message
/// rather than guessed at - a guess would mean every gate quietly falling back to the
/// offline stub while the config looks right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Google,
}

impl Provider {
    /// The family a model name belongs to, or None if it names nobody known.
    pub fn for_model(model: &str) -> Option<Self> {
        if model.starts_with(claude::PREFIX) {
            Some(Provider::Anthropic)
        } else if model.starts_with(gemini::PREFIX) {
            Some(Provider::Google)
        } else {
            None
        }
    }

    /// The name a person would type at the command line: `mytimeoff-daemon key gemini`.
    pub fn from_word(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Provider::Anthropic),
            "gemini" | "google" => Some(Provider::Google),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude",
            Provider::Google => "gemini",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::Google => "Google",
        }
    }

    /// Model names that reach this provider, for a message that has to explain a config
    /// that named nobody.
    pub fn prefix(self) -> &'static str {
        match self {
            Provider::Anthropic => claude::PREFIX,
            Provider::Google => gemini::PREFIX,
        }
    }

    pub const ALL: [Provider; 2] = [Provider::Anthropic, Provider::Google];

    /// Where this provider's key is kept. Separate names, because a machine may well have
    /// both and overwriting one with the other would be a puzzling way to lose a key.
    pub fn credential(self) -> &'static str {
        match self {
            Provider::Anthropic => secret::ANTHROPIC_KEY,
            Provider::Google => secret::GEMINI_KEY,
        }
    }

    /// The variables checked when the store has nothing, named to match what every other
    /// tool for that provider already reads.
    pub fn env(self) -> &'static [&'static str] {
        match self {
            Provider::Anthropic => &["ANTHROPIC_API_KEY"],
            // Google's own libraries read either, and a machine set up for one of them
            // should not need a third copy of the same key.
            Provider::Google => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        }
    }

    /// The key, from the credential store or the environment, or None if there is neither.
    pub fn key(self) -> Option<String> {
        secret::find(self.credential(), self.env())
    }

    pub fn source(self, key: String, model: String) -> Result<Arc<dyn QuestionSource>, SourceError> {
        Ok(match self {
            Provider::Anthropic => Arc::new(claude::Claude::new(key, model)?),
            Provider::Google => Arc::new(gemini::Gemini::new(key, model)?),
        })
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

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

/// Who writes the questions for this config, and a sentence saying so.
///
/// The sentence is returned rather than printed because both surfaces need it and neither
/// can print for the other: the daemon has a console, the shell does not, and a decision
/// this consequential should not be made twice in two places that can drift apart.
///
/// Out loud at all because the two things a user most needs to know about this feature are
/// invisible otherwise: that pages are leaving the machine, or that they are not and the
/// questions are the weak offline ones. Neither should have to be inferred from the quiz.
///
/// The only hard error is a model name that belongs to nobody. Every other outcome is a
/// working install, so a typo would otherwise mean quietly using the offline questions
/// forever while the config looks exactly right.
pub fn source_for(config: &Config) -> Result<(Arc<dyn QuestionSource>, String), String> {
    let offline: Arc<dyn QuestionSource> = Arc::new(Stub);
    if config.model.is_empty() {
        let note = "offline (model is empty; nothing you read leaves this machine)";
        return Ok((offline, note.to_string()));
    }

    let Some(provider) = Provider::for_model(&config.model) else {
        return Err(unknown_model(&config.model));
    };

    let Some(key) = provider.key() else {
        return Ok((
            offline,
            format!(
                "offline (no {provider} API key stored)
                         store one with:  mytimeoff-daemon key {}",
                provider.word(),
            ),
        ));
    };

    match provider.source(key, config.model.clone()) {
        Ok(source) => Ok((
            // Behind it, the offline stub. A gate with no questions lets the reader
            // through, so a dropped connection would otherwise be a free pass.
            Arc::new(Fallback::new(source, offline, |note| eprintln!("{note}"))),
            format!(
                "{} via {provider} (the pages you read are sent to write the questions)",
                config.model,
            ),
        )),
        Err(error) => Ok((offline, format!("offline ({error})"))),
    }
}

/// What to say about a model name that names no provider.
pub fn unknown_model(model: &str) -> String {
    let families = Provider::ALL
        .iter()
        .map(|provider| format!("{}* ({})", provider.prefix(), provider))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "model \"{model}\" names no provider I can reach. Expected one of: {families}.          Set model to \"\" to make the questions on this machine instead."
    )
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
    fn a_model_name_says_who_to_ask() {
        assert_eq!(Provider::for_model("claude-haiku-4-5-20251001"), Some(Provider::Anthropic));
        assert_eq!(Provider::for_model("gemini-3.5-flash"), Some(Provider::Google));
    }

    #[test]
    fn a_model_name_nobody_answers_for_is_refused_rather_than_guessed_at() {
        // Guessing would mean a config that reads perfectly and quietly falls back to the
        // offline stub at every gate, forever.
        assert_eq!(Provider::for_model("gpt-5"), None);
        assert_eq!(Provider::for_model("haiku"), None, "the family prefix is the whole signal");
        assert_eq!(Provider::for_model(""), None);
    }

    #[test]
    fn the_two_providers_do_not_share_a_credential_or_a_variable() {
        // Sharing either would mean storing one key silently destroys the other.
        assert_ne!(Provider::Anthropic.credential(), Provider::Google.credential());
        for theirs in Provider::Google.env() {
            assert!(!Provider::Anthropic.env().contains(theirs), "{theirs}");
        }
    }

    #[test]
    fn every_provider_can_be_named_at_the_command_line_and_read_back() {
        for provider in Provider::ALL {
            assert_eq!(Provider::from_word(provider.word()), Some(provider));
            assert_eq!(Provider::from_word(&provider.word().to_uppercase()), Some(provider));
            // And its own prefix routes to it, which is what pairs `key <word>` with a
            // model name in the config.
            assert_eq!(Provider::for_model(&format!("{}x", provider.prefix())), Some(provider));
        }
        assert_eq!(Provider::from_word("openai"), None);
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
