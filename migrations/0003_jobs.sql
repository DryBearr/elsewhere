-- Background jobs (src/jobs), kept so a sudden close loses nothing: the state the app
-- showed, the params to run the job again, and every log line.
CREATE TABLE job (
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,                -- which runner: 'playlist', ...
  title      TEXT NOT NULL,
  params     TEXT NOT NULL,                -- what the runner needs to run it again
  status     TEXT NOT NULL CHECK (status IN ('queued','running','interrupted','done','failed','cancelled')),
  done       INTEGER NOT NULL DEFAULT 0,
  failed     INTEGER NOT NULL DEFAULT 0,
  total      INTEGER,
  last_error TEXT,
  log_lines  INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE TABLE job_log (
  job_id  INTEGER NOT NULL REFERENCES job(id) ON DELETE CASCADE,
  number  INTEGER NOT NULL,                -- 1, 2, 3, ... per job
  level   TEXT NOT NULL CHECK (level IN ('info','warning','error')),
  message TEXT NOT NULL,
  at      INTEGER NOT NULL,                -- seconds since 1970
  PRIMARY KEY (job_id, number)
) STRICT, WITHOUT ROWID;
