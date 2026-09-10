//! Everything that has to outlive the process.
//!
//! The store lives in the daemon rather than in the reader because the daemon is the only
//! part that is always running, and because a day's progress has to look the same to the
//! window, to a TUI, and to the quiz that decides whether you may leave. One writer, one
//! answer.

use std::path::Path;
use std::sync::Mutex;

use mytimeoff_core::{Book, BookFormat, Locator, PageView};
use rusqlite::{Connection, OptionalExtension, params};

use crate::quiz::Page;

pub struct Store {
    /// SQLite takes one writer at a time regardless, and the write rate here is one row
    /// per page turn. A pool, or moving these onto `spawn_blocking`, would be ceremony
    /// around a lock that is never contended.
    conn: Mutex<Connection>,
}

pub type Result<T> = rusqlite::Result<T>;

impl Store {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(std::io::Error::other)?;
        Self::prepare(conn).map_err(std::io::Error::other)
    }

    /// For tests, and for anyone who wants the tool to remember nothing.
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        // A page view pointing at a book that does not exist is not a row worth keeping,
        // and SQLite only enforces that if asked, per connection.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Survives an unclean shutdown better, and a reader crashing mid-page is exactly
        // the kind of unclean shutdown this will see.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&conn)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    /// Registers a book, or updates what is known about one.
    ///
    /// Re-opening a book is the common case, and a page count only arrives once indexing
    /// finishes, so this has to be an upsert rather than an insert.
    pub fn record_book(&self, book: &Book) -> Result<()> {
        let conn = self.conn.lock().expect("store lock");
        conn.execute(
            "INSERT INTO books (id, format, title, author, path, total_pages, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch() * 1000)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               author = excluded.author,
               path = excluded.path,
               total_pages = COALESCE(excluded.total_pages, books.total_pages)",
            params![
                book.id,
                format_name(book.format),
                book.title,
                book.author,
                book.path,
                book.total_pages,
            ],
        )?;
        Ok(())
    }

    /// Records one visit to one page. Returns false if that visit was already stored.
    ///
    /// A surface that loses its connection mid-report will retry, and a page view counted
    /// twice is a day's goal met by a network hiccup. `(book, entered_at)` identifies a
    /// visit precisely enough to make the retry harmless.
    pub fn record_page_view(&self, view: &PageView, counted: bool) -> Result<bool> {
        let (kind, key) = match &view.locator {
            Locator::Cfi { cfi, .. } => ("cfi", cfi.clone()),
            Locator::Page { page, .. } => ("page", page.to_string()),
        };
        // SQLite integers are signed. A dwell that does not fit in an i64 is a page held
        // for 292 million years, so saturating is as close to unreachable as it gets.
        let dwell_ms = i64::try_from(view.dwell_ms).unwrap_or(i64::MAX);
        let conn = self.conn.lock().expect("store lock");
        let rows = conn.execute(
            "INSERT OR IGNORE INTO page_views
               (book_id, locator_kind, locator_key, page_label, text,
                entered_at, exited_at, dwell_ms, counted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                view.book_id,
                kind,
                key,
                view.locator.page_label(),
                view.text,
                view.entered_at,
                view.exited_at,
                dwell_ms,
                counted,
            ],
        )?;
        Ok(rows == 1)
    }

    /// Distinct pages read today, in the machine's local timezone.
    ///
    /// Distinct, because reading page four five times is not five pages - a goal that
    /// could be met by holding one page and tapping the arrow keys would not be a goal.
    /// SQLite resolves "today" against the OS timezone, so no calendar dependency is
    /// needed for a question only ever asked about the machine you are sitting at.
    pub fn pages_read_today(&self) -> Result<u32> {
        let conn = self.conn.lock().expect("store lock");
        conn.query_row(
            "SELECT COUNT(DISTINCT book_id || char(31) || locator_key)
             FROM page_views
             WHERE counted = 1
               AND date(entered_at / 1000, 'unixepoch', 'localtime')
                   = date('now', 'localtime')",
            [],
            |row| row.get(0),
        )
    }

    /// The book the most recent counted page since `since` belongs to.
    ///
    /// A gate asks about one book. Reading two books in one stretch is rare enough that
    /// asking about the one you ended on is a better answer than a quiz that mixes them.
    pub fn latest_book_since(&self, since: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("store lock");
        conn.query_row(
            "SELECT book_id FROM page_views
             WHERE counted = 1 AND entered_at >= ?1
             ORDER BY entered_at DESC LIMIT 1",
            params![since],
            |row| row.get(0),
        )
        .optional()
    }

    /// The pages a gate may ask about: what was counted as read since `since`.
    ///
    /// Grouped by page, because a page revisited three times is one page to ask about,
    /// and returned in reading order after taking the most recent `limit` - the questions
    /// should be about what you have just read, not the start of the session.
    pub fn counted_pages_since(&self, book_id: &str, since: i64, limit: u32) -> Result<Vec<Page>> {
        let conn = self.conn.lock().expect("store lock");
        let mut statement = conn.prepare(
            "SELECT locator_kind, locator_key, page_label, text, MAX(entered_at) AS seen
             FROM page_views
             WHERE counted = 1 AND book_id = ?1 AND entered_at >= ?2
             GROUP BY locator_key
             ORDER BY seen DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![book_id, since, limit], |row| {
            let kind: String = row.get(0)?;
            let key: String = row.get(1)?;
            let page_label: String = row.get(2)?;
            let locator = if kind == "cfi" {
                Locator::Cfi { cfi: key, page_label }
            } else {
                Locator::Page { page: key.parse().unwrap_or(0), page_label }
            };
            Ok(Page { locator, text: row.get(3)? })
        })?;

        let mut pages = rows.collect::<Result<Vec<Page>>>()?;
        pages.reverse();
        Ok(pages)
    }

    /// What is known about a book, or None if it was never registered.
    pub fn book(&self, id: &str) -> Result<Option<Book>> {
        let conn = self.conn.lock().expect("store lock");
        conn.query_row(
            "SELECT id, format, title, author, path, total_pages FROM books WHERE id = ?1",
            params![id],
            |row| {
                Ok(Book {
                    id: row.get(0)?,
                    format: match row.get::<_, String>(1)?.as_str() {
                        "pdf" => BookFormat::Pdf,
                        _ => BookFormat::Epub,
                    },
                    title: row.get(2)?,
                    author: row.get(3)?,
                    path: row.get(4)?,
                    total_pages: row.get(5)?,
                })
            },
        )
        .optional()
    }
}

fn format_name(format: BookFormat) -> &'static str {
    match format {
        BookFormat::Epub => "epub",
        BookFormat::Pdf => "pdf",
    }
}

/// Schema migrations, applied in order.
///
/// `user_version` is a counter SQLite keeps inside the database file, so the schema's
/// version travels with the data and needs no bookkeeping table of its own. Adding a
/// migration means appending to this list and never editing an earlier entry.
fn migrate(conn: &Connection) -> Result<()> {
    const MIGRATIONS: &[&str] = &[include_str!("../migrations/001-initial.sql")];

    let applied: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        conn.execute_batch(sql)?;
        // Not a bind parameter: PRAGMA does not take them. The value is a loop counter.
        conn.pragma_update(None, "user_version", index as u32 + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        Book {
            id: "book-1".into(),
            format: BookFormat::Pdf,
            title: "Some Book".into(),
            author: Some("A Writer".into()),
            path: Some("C:/books/some.pdf".into()),
            total_pages: Some(200),
        }
    }

    fn view(page: u32, entered_at: i64) -> PageView {
        PageView {
            book_id: "book-1".into(),
            locator: Locator::Page { page, page_label: page.to_string() },
            text: "words on the page".into(),
            entered_at,
            exited_at: entered_at + 5_000,
            dwell_ms: 5_000,
        }
    }

    /// Wall-clock now, which is what the store's "today" is measured against.
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64
    }

    #[test]
    fn a_fresh_database_is_ready_to_use() {
        let store = Store::in_memory().expect("open");
        assert_eq!(store.pages_read_today().expect("progress"), 0);
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let store = Store::in_memory().expect("open");
        let conn = store.conn.lock().expect("lock");
        migrate(&conn).expect("second migration");
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("v");
        assert_eq!(version, 1);
    }

    #[test]
    fn a_book_can_be_reopened_without_losing_what_is_known() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("first open");

        // Reopened before indexing has produced a page count.
        let reopened = Book { total_pages: None, ..book() };
        store.record_book(&reopened).expect("second open");

        let stored = store.book("book-1").expect("query").expect("book exists");
        assert_eq!(stored.total_pages, Some(200), "a later open must not erase the count");
    }

    #[test]
    fn counted_pages_add_up() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        for page in 1..=3 {
            store.record_page_view(&view(page, now_ms() + i64::from(page)), true).expect("view");
        }
        assert_eq!(store.pages_read_today().expect("progress"), 3);
    }

    #[test]
    fn a_skimmed_page_is_stored_but_does_not_count() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        store.record_page_view(&view(1, now_ms()), false).expect("view");
        assert_eq!(store.pages_read_today().expect("progress"), 0);
    }

    #[test]
    fn rereading_a_page_is_not_a_second_page() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        store.record_page_view(&view(1, now_ms()), true).expect("first");
        store.record_page_view(&view(1, now_ms() + 60_000), true).expect("second visit");
        assert_eq!(store.pages_read_today().expect("progress"), 1);
    }

    #[test]
    fn a_retried_report_is_stored_once() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        let view = view(1, now_ms());
        assert!(store.record_page_view(&view, true).expect("first"));
        assert!(!store.record_page_view(&view, true).expect("retry"), "retry must be a no-op");
    }

    #[test]
    fn yesterdays_reading_is_not_todays_progress() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        store.record_page_view(&view(1, now_ms() - 36 * 60 * 60 * 1000), true).expect("view");
        assert_eq!(store.pages_read_today().expect("progress"), 0);
    }

    #[test]
    fn a_gate_asks_about_the_pages_just_read() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        let start = now_ms();
        for page in 1..=5 {
            store.record_page_view(&view(page, start + i64::from(page)), true).expect("view");
        }

        let pages = store.counted_pages_since("book-1", start, 3).expect("pages");
        assert_eq!(
            pages.iter().map(|p| p.locator.page_label().to_string()).collect::<Vec<_>>(),
            vec!["3", "4", "5"],
            "the most recent three, in reading order",
        );
    }

    #[test]
    fn a_page_read_twice_is_one_page_to_ask_about() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        let start = now_ms();
        store.record_page_view(&view(1, start), true).expect("first");
        store.record_page_view(&view(1, start + 1_000), true).expect("revisit");
        assert_eq!(store.counted_pages_since("book-1", start, 10).expect("pages").len(), 1);
    }

    #[test]
    fn a_gate_does_not_ask_about_pages_you_only_skimmed() {
        let store = Store::in_memory().expect("open");
        store.record_book(&book()).expect("book");
        let start = now_ms();
        store.record_page_view(&view(1, start), false).expect("skimmed");
        store.record_page_view(&view(2, start + 1), true).expect("read");
        let pages = store.counted_pages_since("book-1", start, 10).expect("pages");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].locator.page_label(), "2");
    }

    #[test]
    fn a_gate_before_any_reading_has_nothing_to_ask_about() {
        let store = Store::in_memory().expect("open");
        assert_eq!(store.latest_book_since(now_ms()).expect("book"), None);
    }

    #[test]
    fn a_page_view_for_an_unknown_book_is_refused() {
        // Otherwise a typo in a book id silently becomes a second book with no title.
        let store = Store::in_memory().expect("open");
        assert!(store.record_page_view(&view(1, now_ms()), true).is_err());
    }
}
