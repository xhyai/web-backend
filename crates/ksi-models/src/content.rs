use serde::{Deserialize, Serialize};

/// Content directory/file listing (mirrors `util/content.py::dir_to_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentJson {
    pub id: String,
    pub files: Vec<String>,
    pub dirs: Vec<String>,
}

impl ContentJson {
    pub fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            files: Vec::new(),
            dirs: Vec::new(),
        }
    }

    /// List the entries of a directory on disk. Entries are sorted so the
    /// output is deterministic.
    pub fn list_dir(id: impl Into<String>, dir: &std::path::Path) -> Self {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir() {
                    dirs.push(name);
                } else {
                    files.push(name);
                }
            }
        }
        files.sort();
        dirs.sort();
        Self {
            id: id.into(),
            files,
            dirs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_has_no_entries() {
        let c = ContentJson::empty(".");
        assert!(c.files.is_empty() && c.dirs.is_empty());
        assert_eq!(c.id, ".");
    }

    #[test]
    fn list_dir_splits_files_and_dirs() {
        let tmp = std::env::temp_dir().join(format!(
            "ksi-content-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.txt"), b"x").unwrap();
        let c = ContentJson::list_dir("rel", &tmp);
        assert_eq!(c.files, vec!["a.txt".to_string()]);
        assert_eq!(c.dirs, vec!["sub".to_string()]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
