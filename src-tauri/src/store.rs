//! Saved sessions: one JSON file per session plus a small index, so listing them never means
//! reading every report.

use culpritcore::report::Mood;
use culpritcore::Session;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub started_at: u64,
    pub game: Option<String>,
    pub duration_ms: f64,
    pub headline: String,
    pub mood: Mood,
}

impl Summary {
    pub fn of(s: &Session) -> Self {
        Summary {
            id: s.id.clone(),
            started_at: s.started_at,
            game: s.report.game.clone(),
            duration_ms: s.report.duration_ms,
            headline: s.report.headline.clone(),
            mood: s.report.mood,
        }
    }
}

pub struct Store {
    dir: PathBuf,
}

/// Session ids are the start time in milliseconds, so anything else is refused before it
/// can be used as a file name.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Writes next to the target and renames over it, so a crash never leaves half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("couldn't save: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("couldn't save: {e}")
    })
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn session_path(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.dir.join(format!("{id}.json")))
    }

    /// Every saved session, newest first. Rebuilds the index if it is missing or damaged.
    pub fn list(&self) -> Vec<Summary> {
        let from_index = fs::read(self.index_path()).ok().and_then(|b| serde_json::from_slice::<Vec<Summary>>(&b).ok());
        let mut list = match from_index {
            Some(list) => list,
            None => self.rebuild(),
        };
        list.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        list
    }

    fn rebuild(&self) -> Vec<Summary> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let list: Vec<Summary> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_suffix(".json")?;
                if !valid_id(id) {
                    return None;
                }
                let session: Session = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
                Some(Summary::of(&session))
            })
            .collect();
        let _ = self.write_index(&list);
        list
    }

    fn write_index(&self, list: &[Summary]) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("couldn't create the sessions folder: {e}"))?;
        write_atomic(&self.index_path(), &serde_json::to_vec(list).map_err(|e| e.to_string())?)
    }

    /// The session file's bytes, exactly as saved.
    pub fn read(&self, id: &str) -> Result<Vec<u8>, String> {
        let path = self.session_path(id).ok_or("not a session")?;
        fs::read(path).map_err(|_| "that session is gone".to_string())
    }

    /// Saves a finished session. The recorder calls this; until it lands only the tests do.
    #[allow(dead_code)]
    pub fn save(&self, session: &Session) -> Result<(), String> {
        let path = self.session_path(&session.id).ok_or("not a session")?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("couldn't create the sessions folder: {e}"))?;
        write_atomic(&path, &serde_json::to_vec(session).map_err(|e| e.to_string())?)?;
        let mut list = self.list();
        list.retain(|s| s.id != session.id);
        list.push(Summary::of(session));
        self.write_index(&list)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.session_path(id).ok_or("not a session")?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("couldn't delete: {e}")),
        }
        let mut list = self.list();
        list.retain(|s| s.id != id);
        self.write_index(&list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("culprit-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saves_lists_reads_and_deletes() {
        let dir = temp_dir("basic");
        let store = Store::new(dir.clone());
        assert!(store.list().is_empty());
        let a = culpritcore::summarize(&culpritcore::demo::clean());
        let b = culpritcore::summarize(&culpritcore::demo::server());
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 2);
        assert!(list[0].started_at > list[1].started_at, "newest first");
        let back: Session = serde_json::from_slice(&store.read(&a.id).unwrap()).unwrap();
        assert_eq!(back, a);

        // A lost index is rebuilt from the files.
        fs::remove_file(dir.join("index.json")).unwrap();
        assert_eq!(store.list().len(), 2);

        store.delete(&a.id).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(store.read(&a.id).is_err());
        store.delete(&a.id).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_paths_as_ids() {
        let store = Store::new(temp_dir("ids"));
        for id in ["../index", "..\\x", "", "12a", "/etc/passwd", "1234567890123456789012"] {
            assert!(store.read(id).is_err(), "{id}");
            assert!(store.delete(id).is_err(), "{id}");
        }
    }
}
