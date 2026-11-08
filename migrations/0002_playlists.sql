-- Saved playlists (src/playlists).
--
-- collection.source says where a playlist came from (for example "YoutubeTab:PLxxxx"),
-- so saving the same playlist again updates it instead of adding a second one.
-- Unique, but NULL for hand-made collections (SQLite lets NULLs repeat).
ALTER TABLE collection ADD COLUMN source TEXT;
CREATE UNIQUE INDEX collection_source ON collection(source);

-- Videos that are not movies, shows or anime: YouTube videos saved as mp4.
INSERT INTO category (name, media, direction, position) VALUES ('Videos', 'video', 'ltr', 8);
