//! Keeps jobs in the library database (migrations/0003_jobs.sql): one row per job with the
//! params to run it again, and every log line.

use std::path::Path;

use rusqlite::{Connection, params};

use super::{JobId, JobState, KEEP_FINISHED, Level, LogLine, MAX_LOG_LINES, Status};

pub(super) struct Store {
    db: Connection,
}

/// A job as it comes back from the database.
pub(super) struct Saved {
    pub state: JobState,
    pub params: String,
    /// The newest lines, at most MAX_LOG_LINES.
    pub log: Vec<LogLine>,
}

const INTERRUPTED: &str = "Interrupted: the app closed while this job was queued or running.";

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        let db = crate::db::open(path)
            .map_err(|err| format!("could not open the jobs database: {err}"))?;
        Ok(Store { db })
    }

    /// Loads the jobs of earlier runs, oldest first. Keeps only the newest KEEP_FINISHED
    /// finished jobs, and turns jobs that were queued or running into Interrupted ones.
    pub fn load(&mut self) -> Result<Vec<Saved>, String> {
        self.load_jobs()
            .map_err(|err| format!("could not load the saved jobs: {err}"))
    }

    fn load_jobs(&mut self) -> rusqlite::Result<Vec<Saved>> {
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM job WHERE status IN ('done','failed','cancelled') AND id NOT IN (
               SELECT id FROM job WHERE status IN ('done','failed','cancelled')
               ORDER BY id DESC LIMIT ?1)",
            [KEEP_FINISHED as i64],
        )?;
        let cut_short: Vec<(i64, i64)> = tx
            .prepare("SELECT id, log_lines FROM job WHERE status IN ('queued','running')")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (id, lines) in cut_short {
            let number = lines + 1;
            tx.execute(
                "INSERT INTO job_log (job_id, number, level, message, at) VALUES (?1, ?2, 'warning', ?3, unixepoch())",
                params![id, number, INTERRUPTED],
            )?;
            tx.execute(
                "UPDATE job SET status = 'interrupted', log_lines = ?2, updated_at = unixepoch() WHERE id = ?1",
                params![id, number],
            )?;
        }

        let mut jobs = Vec::new();
        {
            let mut select = tx.prepare(
                "SELECT id, kind, title, params, status, done, failed, total, last_error, log_lines
                 FROM job ORDER BY id",
            )?;
            let mut lines = tx.prepare(
                "SELECT number, level, message, at FROM job_log
                 WHERE job_id = ?1 AND number > ?2 ORDER BY number",
            )?;
            let rows = select.query_map([], |row| {
                let status: String = row.get(4)?;
                let state = JobState {
                    id: row.get::<_, i64>(0)? as JobId,
                    revision: 1,
                    kind: row.get(1)?,
                    title: row.get(2)?,
                    status: status_from(&status),
                    done: row.get::<_, i64>(5)? as usize,
                    failed: row.get::<_, i64>(6)? as usize,
                    total: row.get::<_, Option<i64>>(7)?.map(|total| total as usize),
                    current: None,
                    last_error: row.get(8)?,
                    log_lines: row.get::<_, i64>(9)? as usize,
                };
                Ok((state, row.get::<_, String>(3)?))
            })?;
            for row in rows {
                let (state, params) = row?;
                let since = state.log_lines.saturating_sub(MAX_LOG_LINES) as i64;
                let log = lines
                    .query_map(params![state.id as i64, since], |row| {
                        let level: String = row.get(1)?;
                        Ok(LogLine {
                            number: row.get::<_, i64>(0)? as usize,
                            level: level_from(&level),
                            message: row.get(2)?,
                            at: row.get::<_, i64>(3)? as u64,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                jobs.push(Saved { state, params, log });
            }
        }
        tx.commit()?;
        Ok(jobs)
    }

    /// Saves a new job before it runs, so even a crash right after start keeps it.
    pub fn insert(&self, state: &JobState, params: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO job (id, kind, title, params, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                state.id as i64,
                state.kind,
                state.title,
                params,
                status_text(state.status)
            ],
        )?;
        Ok(())
    }

    /// Saves job states and new log lines together: all of them or none.
    pub fn save(
        &mut self,
        states: &[JobState],
        lines: &[(JobId, LogLine)],
    ) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut update = tx.prepare_cached(
                "UPDATE job SET title = ?2, status = ?3, done = ?4, failed = ?5, total = ?6,
                   last_error = ?7, log_lines = ?8, updated_at = unixepoch() WHERE id = ?1",
            )?;
            for state in states {
                update.execute(params![
                    state.id as i64,
                    state.title,
                    status_text(state.status),
                    state.done as i64,
                    state.failed as i64,
                    state.total.map(|total| total as i64),
                    state.last_error,
                    state.log_lines as i64,
                ])?;
            }
            let mut insert = tx.prepare_cached(
                "INSERT OR REPLACE INTO job_log (job_id, number, level, message, at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (id, line) in lines {
                insert.execute(params![
                    *id as i64,
                    line.number as i64,
                    level_text(line.level),
                    line.message,
                    line.at as i64,
                ])?;
            }
        }
        tx.commit()
    }

    pub fn delete(&self, id: JobId) -> rusqlite::Result<()> {
        self.db
            .execute("DELETE FROM job WHERE id = ?1", [id as i64])?;
        Ok(())
    }

    /// The id after every saved one, so new jobs never reuse an old id.
    pub fn next_id(&self) -> rusqlite::Result<JobId> {
        let last: i64 = self
            .db
            .query_row("SELECT coalesce(max(id), 0) FROM job", [], |row| row.get(0))?;
        Ok(last as JobId + 1)
    }
}

fn status_text(status: Status) -> &'static str {
    match status {
        Status::Queued => "queued",
        Status::Running => "running",
        Status::Interrupted => "interrupted",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

fn status_from(text: &str) -> Status {
    match text {
        "done" => Status::Done,
        "failed" => Status::Failed,
        "cancelled" => Status::Cancelled,
        // queued and running were turned into interrupted while loading.
        _ => Status::Interrupted,
    }
}

fn level_text(level: Level) -> &'static str {
    match level {
        Level::Info => "info",
        Level::Warning => "warning",
        Level::Error => "error",
    }
}

fn level_from(text: &str) -> Level {
    match text {
        "error" => Level::Error,
        "warning" => Level::Warning,
        _ => Level::Info,
    }
}
