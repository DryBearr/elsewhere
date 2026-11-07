use std::path::PathBuf;
use std::rc::Rc;

use rusqlite::Connection;
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

pub fn run(db_path: PathBuf) {
    let db = Connection::open(db_path).unwrap();
    db.execute(
        "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, text TEXT NOT NULL)",
        (),
    )
    .unwrap();

    let app = App::new().unwrap();
    let notes = Rc::new(VecModel::from(load_notes(&db)));
    app.set_notes(ModelRc::from(notes.clone()));

    app.on_add(move |text| {
        if text.trim().is_empty() {
            return;
        }
        db.execute("INSERT INTO notes (text) VALUES (?1)", [text.as_str()])
            .unwrap();
        notes.push(text);
    });

    app.run().unwrap();
}

fn load_notes(db: &Connection) -> Vec<SharedString> {
    let mut stmt = db.prepare("SELECT text FROM notes ORDER BY id").unwrap();
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
