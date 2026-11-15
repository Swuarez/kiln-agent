use crate::message::Message;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

/// A conversation plus the directory it belongs to. Persisted as one JSON file
/// so a session can be inspected, copied, or deleted with normal file tools.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub created_ms: u64,
    pub updated_ms: u64,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub updated_ms: u64,
    pub messages: usize,
}

impl Session {
    pub fn new(cwd: &Path) -> Self {
        let ts = now_ms();
        Self {
            id: new_id(),
            title: String::from("untitled"),
            cwd: cwd.to_path_buf(),
            created_ms: ts,
            updated_ms: ts,
            messages: Vec::new(),
        }
    }

    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.json", self.id))
    }

    /// Write via a temp file so a crash mid-write never truncates history.
    pub fn save(&mut self, dir: &Path) -> Result<()> {
        self.updated_ms = now_ms();
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let target = self.path(dir);
        let tmp = target.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, body).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, &target).map_err(|e| Error::io(&target, e))?;
        Ok(())
    }

    pub fn load(dir: &Path, id: &str) -> Result<Self> {
        let path = dir.join(format!("{id}.json"));
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::config(format!("{}: {e}", path.display())))
    }

    pub fn delete(dir: &Path, id: &str) -> Result<()> {
        let path = dir.join(format!("{id}.json"));
        std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))
    }

    /// Newest first. Unreadable files are skipped rather than failing the listing.
    pub fn list(dir: &Path) -> Result<Vec<SessionMeta>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(dir, e)),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Ok(session) = serde_json::from_slice::<Session>(&bytes) {
                out.push(SessionMeta {
                    id: session.id,
                    title: session.title,
                    cwd: session.cwd,
                    updated_ms: session.updated_ms,
                    messages: session.messages.len(),
                });
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.updated_ms));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_roundtrip_through_disk() {
        let mut s = Session::new(Path::new("."));
        s.title = "test".into();
        s.messages.push(Message::user("hi"));
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Session = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.messages.len(), 1);
        assert_eq!(back.title, "test");
    }

    #[test]
    fn tool_messages_survive_serialization() {
        let m = Message::tool("call_1", "out", true);
        let json = serde_json::to_string(&m).expect("serialize");
        assert!(json.contains("\"role\":\"tool\""));
        let back: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, m);
    }
}
