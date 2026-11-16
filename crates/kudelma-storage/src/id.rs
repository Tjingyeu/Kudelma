use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KudelmaId(String);

impl KudelmaId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let uuid = uuid::Uuid::parse_str(raw).ok()?;
        Some(Self(uuid.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for KudelmaId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for KudelmaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn new_id() -> String {
    KudelmaId::new().to_string()
}

pub fn now_rfc3339() -> String {
    let now: DateTime<Utc> = Utc::now();
    now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
