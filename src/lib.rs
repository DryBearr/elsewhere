#[cfg(feature = "ytdlp")]
pub mod ytdlp;

use std::path::PathBuf;

use rusqlite::Connection;
use slint::{ModelRc, SharedString, VecModel};

mod db;

slint::include_modules!();

pub fn run(db_path: PathBuf) {
    let db = db::open(&db_path).unwrap();

    let app = App::new().unwrap();
    app.set_categories(ModelRc::new(VecModel::from(load_categories(&db))));
    app.run().unwrap();
}

fn load_categories(db: &Connection) -> Vec<SharedString> {
    let mut stmt = db
        .prepare("SELECT name FROM category ORDER BY position")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| row.unwrap().into())
        .collect()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    let dir = app.internal_data_path().unwrap();
    slint::android::init(app).unwrap();
    run(dir.join("elsewhere.db"));
}
