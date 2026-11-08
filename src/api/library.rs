//! The library database.

use super::{ApiError, core, failed};

/// Category names (the library's tabs), in display order.
#[uniffi::export]
pub fn categories() -> Result<Vec<String>, ApiError> {
    let db = crate::db::open(&core()?.db).map_err(failed)?;
    let mut stmt = db
        .prepare("SELECT name FROM category ORDER BY position")
        .map_err(failed)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(failed)?;
    names.collect::<Result<_, _>>().map_err(failed)
}
