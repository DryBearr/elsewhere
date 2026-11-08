-- A job's last error as a whole problem (src/problem.rs), so after a restart the app can
-- still tell what kind of failure it was and whether trying again can help.
-- job.last_error keeps the text for people.
ALTER TABLE job ADD COLUMN error_kind TEXT;      -- input, unavailable, network, storage, setup, cancelled, bug
ALTER TABLE job ADD COLUMN error_detail TEXT;    -- technical, for the log
ALTER TABLE job ADD COLUMN error_retry INTEGER;  -- 1 when trying again can help
