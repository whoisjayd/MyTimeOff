-- The book that is open, and the bytes needed to open it again.
--
-- One row, enforced by the schema rather than by convention: this is a single-book
-- reader. A library that could hold two would need a way to choose between them, which
-- is a screen, a decision and a whole surface that does not exist - and the point of the
-- tool is that the book is simply *there* when the screen is taken, with no picking.
--
-- The bytes live here rather than in a folder beside the database for two reasons. A
-- file and a row can disagree - an orphaned file, or a row pointing at a file someone
-- moved - and a foreign key cannot fix that, whereas one row cannot be half-true.
-- And a reader served over loopback has to be handed the bytes anyway; it cannot open a
-- path. The cost is honest and belongs written down: this database is now as large as
-- the book in it, and SQLite reads a blob whole rather than streaming it.
--
-- `bytes` is nullable because a book can be registered before - or without - its bytes
-- ever arriving. A surface reading straight off disk (a TUI with a path) never uploads,
-- and even the window registers the book first and uploads second.
CREATE TABLE current_book (
    only_one  INTEGER PRIMARY KEY CHECK (only_one = 1),
    book_id   TEXT NOT NULL REFERENCES books(id),
    bytes     BLOB,
    opened_at INTEGER NOT NULL
);

-- Resuming asks one question - "the last place I was in this book" - and asks it of the
-- whole history of that book, not of a time window. Without this it is a scan.
CREATE INDEX page_views_by_book ON page_views (book_id, entered_at);
