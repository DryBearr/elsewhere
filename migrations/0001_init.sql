-- Initial media library schema.
--
-- Shape:
--   category   = one UI tab (Music, Movies, Anime, Manga, ...)
--   library    = a folder the user adds to a tab
--   item       = everything, two levels: 'group' (album, show, series) -> 'entry'
--                (track, episode, chapter, movie, single book)
--   tag        = genres, artists, authors, studios: one filter mechanism
--   collection = playlists, collections and smart (saved query) lists
--   progress   = resume point per entry
--
-- Connection pragmas (WAL, foreign_keys) are set in code, not here:
-- they cannot change inside the migration transaction.

-- One row per tab.
CREATE TABLE category (
  id        INTEGER PRIMARY KEY,
  name      TEXT NOT NULL UNIQUE,
  media     TEXT NOT NULL CHECK (media IN ('audio','video','ebook','pages')),  -- which player/reader opens
  direction TEXT NOT NULL DEFAULT 'ltr' CHECK (direction IN ('ltr','rtl','vertical')),  -- reader default
  position  INTEGER NOT NULL DEFAULT 0   -- tab order
) STRICT;

-- Folders the user adds to a tab.
CREATE TABLE library (
  id          INTEGER PRIMARY KEY,
  category_id INTEGER NOT NULL REFERENCES category(id) ON DELETE CASCADE,
  root        TEXT NOT NULL UNIQUE,      -- folder path, or SAF tree URI on Android
  scanned_at  INTEGER
) STRICT;

-- Albums, tracks, movies, shows, episodes, series, chapters.
--   Music     album  -> track     group_no = disc
--   Movies    movie (entry, no parent)
--   TV/Anime  show   -> episode   group_no = season
--   Books     book (entry), or series -> book
--   Comics    series -> issue
--   Manga     series -> chapter   group_no = volume, number may be 10.5
--   Manhwa    series -> chapter
--
-- Rescan must upsert on (library_id, path) so ids stay stable and favorites,
-- collections and progress survive. A vanished file sets missing_since; it is
-- not deleted.
CREATE TABLE item (
  id             INTEGER PRIMARY KEY,
  category_id    INTEGER NOT NULL REFERENCES category(id) ON DELETE CASCADE,
  library_id     INTEGER REFERENCES library(id) ON DELETE CASCADE,
  parent_id      INTEGER REFERENCES item(id) ON DELETE CASCADE,
  kind           TEXT NOT NULL CHECK (kind IN ('group','entry')),
  path           TEXT,                   -- relative to library.root: file for entry, folder for group
  mtime          INTEGER,                -- skip unchanged files on rescan
  group_no       INTEGER,                -- season / disc / volume
  number         REAL,                   -- episode / track / chapter
  title          TEXT NOT NULL,
  sort_title     TEXT NOT NULL,          -- app-normalized: lowercase, zero-padded numbers
  original_title TEXT,                   -- e.g. Japanese or Korean title
  year           INTEGER,
  summary        TEXT,
  cover          TEXT,                   -- cached image path
  duration_ms    INTEGER,
  page_count     INTEGER,
  favorite       INTEGER NOT NULL DEFAULT 0,
  rating         INTEGER CHECK (rating BETWEEN 1 AND 10),
  extra          TEXT CHECK (extra IS NULL OR json_valid(extra)),  -- provider ids, rare fields
  added_at       INTEGER NOT NULL DEFAULT (unixepoch()),
  missing_since  INTEGER,
  UNIQUE (library_id, path)
) STRICT;
CREATE INDEX item_grid     ON item(category_id, parent_id, sort_title);
CREATE INDEX item_children ON item(parent_id, group_no, number);
CREATE INDEX item_recent   ON item(category_id, added_at);
CREATE INDEX item_favorite ON item(category_id, sort_title) WHERE favorite = 1;

-- Genres, artists, authors, studios, publishers, free tags.
CREATE TABLE tag (
  id   INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,                    -- genre, artist, author, studio, publisher, tag
  name TEXT NOT NULL COLLATE NOCASE,
  UNIQUE (kind, name)
) STRICT;

CREATE TABLE item_tag (
  item_id INTEGER NOT NULL REFERENCES item(id) ON DELETE CASCADE,
  tag_id  INTEGER NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
  PRIMARY KEY (item_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX item_tag_by_tag ON item_tag(tag_id);

-- Playlists, collections and smart lists.
CREATE TABLE collection (
  id          INTEGER PRIMARY KEY,
  category_id INTEGER NOT NULL REFERENCES category(id) ON DELETE CASCADE,
  name        TEXT NOT NULL,
  query       TEXT CHECK (query IS NULL OR json_valid(query)),  -- NULL = manual list, JSON = saved filter
  cover       TEXT,
  position    INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE TABLE collection_item (
  id            INTEGER PRIMARY KEY,     -- own id: the same song can appear twice
  collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
  item_id       INTEGER NOT NULL REFERENCES item(id) ON DELETE CASCADE,
  position      REAL NOT NULL,           -- insert between two rows = midpoint, no renumbering
  added_at      INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
CREATE INDEX collection_item_order   ON collection_item(collection_id, position);
CREATE INDEX collection_item_by_item ON collection_item(item_id);

-- Resume point. Written often during playback, so kept narrow and apart from item.
CREATE TABLE progress (
  item_id   INTEGER PRIMARY KEY REFERENCES item(id) ON DELETE CASCADE,
  position  INTEGER NOT NULL DEFAULT 0,  -- ms for audio/video, page index for pages
  locator   TEXT,                        -- EPUB CFI for ebook
  percent   REAL NOT NULL DEFAULT 0,     -- 0.0 .. 1.0
  done      INTEGER NOT NULL DEFAULT 0,
  plays     INTEGER NOT NULL DEFAULT 0,
  opened_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
CREATE INDEX progress_continue ON progress(opened_at) WHERE done = 0;

INSERT INTO category (name, media, direction, position) VALUES
  ('Music',    'audio', 'ltr',      0),
  ('Movies',   'video', 'ltr',      1),
  ('TV Shows', 'video', 'ltr',      2),
  ('Anime',    'video', 'ltr',      3),
  ('Books',    'ebook', 'ltr',      4),
  ('Comics',   'pages', 'ltr',      5),
  ('Manga',    'pages', 'rtl',      6),
  ('Manhwa',   'pages', 'vertical', 7);
