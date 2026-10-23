//! Writes the example sessions as JSON, for previewing the app in a browser:
//! `cargo run --release -p culpritcore --example export_demo -- ui-dev/sessions`

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "ui-dev/sessions".into()));
    std::fs::create_dir_all(&dir).expect("create output folder");
    let mut index = Vec::new();
    for rec in culpritcore::demo::all() {
        let session = culpritcore::summarize(&rec);
        let path = dir.join(format!("{}.json", session.id));
        std::fs::write(&path, serde_json::to_vec(&session).unwrap()).expect("write session");
        index.push(serde_json::json!({
            "id": session.id,
            "startedAt": session.started_at,
            "game": session.report.game,
            "durationMs": session.report.duration_ms,
            "headline": session.report.headline,
            "mood": session.report.mood,
        }));
        println!(
            "{} {:>7} KB  {}",
            path.display(),
            std::fs::metadata(&path).unwrap().len() / 1024,
            session.report.headline
        );
    }
    std::fs::write(dir.join("index.json"), serde_json::to_vec_pretty(&index).unwrap()).expect("write index");
}
