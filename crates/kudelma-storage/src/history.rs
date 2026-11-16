use std::fs;

use crate::paths::state_dir;
use crate::{StorageError, atomic_write};

pub const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone)]
pub struct InputHistory {
    path: std::path::PathBuf,
    lines: Vec<String>,
}

impl InputHistory {
    pub fn load() -> Result<Self, StorageError> {
        let path = state_dir()?.join("history.txt");
        let lines = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        Ok(Self { path, lines })
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn push(&mut self, line: &str) {
        if line.trim().is_empty() || self.lines.last().map(String::as_str) == Some(line) {
            return;
        }
        self.lines.push(line.to_owned());
        if self.lines.len() > MAX_HISTORY {
            self.lines.drain(..self.lines.len() - MAX_HISTORY);
        }
        let text = self.lines.join("\n") + "\n";
        let _ = atomic_write(&self.path, text.as_bytes());
    }
}
