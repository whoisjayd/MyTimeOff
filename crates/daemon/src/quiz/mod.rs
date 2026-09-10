//! Where questions come from.
//!
//! The rules for getting past a quiz are in `mytimeoff-core`, because they are pure and
//! they are the part that must never be wrong. Making the questions is the opposite: it
//! needs the pages, a model and a network, and it will be replaced. So it sits behind a
//! trait, and the daemon holds one - which is what lets the offline stub and the real
//! generator be swapped without the gate noticing.

pub mod stub;

use std::fmt;

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
