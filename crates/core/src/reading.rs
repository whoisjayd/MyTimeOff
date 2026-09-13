//! What was actually read.
//!
//! These are records, not decisions - but the one decision that travels with them, whether
//! a page counted, lives here rather than in the reader. The surface measures dwell,
//! because only the surface knows about focus and visibility; it does not get to decide
//! what that dwell was worth. Otherwise the window and a TUI could disagree about a day's
//! progress, and the quiz could be drawn from pages the goal never counted.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Epub,
    Pdf,
}

/// A book the reader has opened.
///
/// The id is chosen by the surface and treated as opaque here. It has to be stable across
/// sessions or a day's progress splits in two - a content hash would guarantee that, and
/// is the obvious later refinement over the file name in use today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub format: BookFormat,
    pub title: String,
    pub author: Option<String>,
    pub path: Option<String>,
    /// Pages for a PDF; generated locations for an EPUB. Absent until indexing finishes.
    pub total_pages: Option<u32>,
}

/// Where reading picks up: the open book, and the last place it was left.
///
/// `at` is optional because a book that was registered and never read has no last place -
/// which is a resume onto page one, not a failure. Nothing here says *how* to reach that
/// position; a CFI means something only to an EPUB renderer and a page number only to a
/// PDF one, and that asymmetry is the [`Locator`]'s to carry, not this record's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    pub book: Book,
    pub at: Option<Locator>,
}

/// A position in a book, expressed the way that book can express it.
///
/// EPUB has no pages - it has CFIs into a reflowable document - so a page number would be
/// a lie there. The label is what the reader showed you, and is what a question cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Locator {
    Cfi { cfi: String, page_label: String },
    Page { page: u32, page_label: String },
}

impl Locator {
    pub fn page_label(&self) -> &str {
        match self {
            Locator::Cfi { page_label, .. } | Locator::Page { page_label, .. } => page_label,
        }
    }

    /// The stable identity of a position, for storing and for spotting a re-read.
    pub fn key(&self) -> String {
        match self {
            Locator::Cfi { cfi, .. } => cfi.clone(),
            Locator::Page { page, .. } => page.to_string(),
        }
    }
}

/// One contiguous visit to one page.
///
/// Timestamps are wall-clock milliseconds, unlike everything the state machine handles.
/// The machine cares about durations and must not be fooled by a clock change; this cares
/// about *which day you read*, which only a calendar can answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageView {
    pub book_id: String,
    pub locator: Locator,
    /// The visible text of the page. This is what makes a question answerable from what
    /// was on screen rather than from the book in general.
    pub text: String,
    pub entered_at: i64,
    pub exited_at: i64,
    /// Milliseconds actually visible and focused - not `exited_at - entered_at`, which
    /// would count a window left open behind a terminal.
    pub dwell_ms: u64,
}

impl PageView {
    /// Whether this visit was reading rather than page-turning.
    ///
    /// Text length is deliberately not part of this. A short page read slowly still
    /// counts, and weighting by length would let a book of half-empty pages inflate a
    /// day's total.
    pub fn counts(&self, skim_threshold_ms: u64) -> bool {
        self.dwell_ms >= skim_threshold_ms && !self.text.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(dwell_ms: u64, text: &str) -> PageView {
        PageView {
            book_id: "book".into(),
            locator: Locator::Page { page: 4, page_label: "4".into() },
            text: text.into(),
            entered_at: 1_000,
            exited_at: 1_000 + dwell_ms as i64,
            dwell_ms,
        }
    }

    #[test]
    fn a_page_held_long_enough_counts() {
        assert!(view(3_000, "some real text").counts(3_000));
    }

    #[test]
    fn a_page_turned_past_does_not() {
        assert!(!view(2_999, "some real text").counts(3_000));
    }

    #[test]
    fn a_blank_page_never_counts_however_long_you_stare() {
        // Chapter breaks and cover art are held for a long time while nothing is read,
        // and a quiz cannot be drawn from them either.
        assert!(!view(60_000, "   \n  ").counts(3_000));
    }

    #[test]
    fn a_resume_says_the_book_and_the_place_in_the_wire_shape_the_reader_reads() {
        // The TypeScript client mirrors these names by hand. A rename here that is not
        // mirrored there is a silent "no book kept" on every launch, so the shape is
        // pinned rather than assumed.
        let resume = Resume {
            book: Book {
                id: "the-book.epub".into(),
                format: BookFormat::Epub,
                title: "The Book".into(),
                author: None,
                path: None,
                total_pages: None,
            },
            at: Some(Locator::Cfi { cfi: "epubcfi(/6/4!/4/2)".into(), page_label: "12".into() }),
        };
        let wire = serde_json::to_value(&resume).expect("serialise");
        assert_eq!(wire["book"]["id"], "the-book.epub");
        assert_eq!(wire["book"]["format"], "epub");
        assert_eq!(wire["at"]["kind"], "cfi");
        assert_eq!(wire["at"]["page_label"], "12");
        assert_eq!(
            serde_json::to_value(Resume { at: None, ..resume }).expect("s")["at"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_locator_keeps_the_label_the_reader_showed() {
        let cfi = Locator::Cfi { cfi: "epubcfi(/6/4!/4/2)".into(), page_label: "vii".into() };
        assert_eq!(cfi.page_label(), "vii");
        assert_eq!(cfi.key(), "epubcfi(/6/4!/4/2)");
    }
}
