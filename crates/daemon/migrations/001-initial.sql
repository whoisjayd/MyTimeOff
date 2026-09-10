-- Books the reader has opened. `id` is chosen by the surface and opaque here.
CREATE TABLE books (
    id          TEXT PRIMARY KEY,
    format      TEXT NOT NULL,
    title       TEXT NOT NULL,
    author      TEXT,
    path        TEXT,
    total_pages INTEGER,
    added_at    INTEGER NOT NULL
);

-- One row per contiguous visit to one page.
--
-- `text` is the visible text of that page. It is what lets a question be answerable from
-- what was on screen rather than from the book in general, and it never leaves this file.
--
-- `counted` is stored rather than derived so that changing the skim threshold later does
-- not silently rewrite history: what counted on the day is what counted.
CREATE TABLE page_views (
    id           INTEGER PRIMARY KEY,
    book_id      TEXT NOT NULL REFERENCES books(id),
    locator_kind TEXT NOT NULL,
    locator_key  TEXT NOT NULL,
    page_label   TEXT NOT NULL,
    text         TEXT NOT NULL,
    entered_at   INTEGER NOT NULL,
    exited_at    INTEGER NOT NULL,
    dwell_ms     INTEGER NOT NULL,
    counted      INTEGER NOT NULL,
    -- A visit is identified by when it began, so a retried report is a no-op.
    UNIQUE (book_id, entered_at)
);

-- Every progress question is "what happened between these two times".
CREATE INDEX page_views_by_time ON page_views (entered_at);
