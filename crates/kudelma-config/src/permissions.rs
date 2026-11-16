use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToolKey {
    Wildcard,
    Native(String),
    McpServer(String),
    McpTool(String, String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolKeyParseError {
    #[error("tool name is empty")]
    Empty,
    #[error("invalid tool name {0:?}: expected [A-Za-z0-9_-]{{1,64}}")]
    Invalid(String),
    #[error("malformed MCP tool key {0:?}")]
    Malformed(String),
}

impl ToolKey {
    pub fn native(name: impl Into<String>) -> Self {
        Self::Native(name.into())
    }

    pub fn parse(raw: &str) -> Result<Self, ToolKeyParseError> {
        if raw.is_empty() {
            return Err(ToolKeyParseError::Empty);
        }
        if raw == "*" {
            return Ok(Self::Wildcard);
        }
        match raw.split_once('.') {
            Some((server, "*")) => {
                validate(server)?;
                Ok(Self::McpServer(server.into()))
            }
            Some((server, tool)) => {
                validate(server)?;
                validate(tool)?;
                Ok(Self::McpTool(server.into(), tool.into()))
            }
            None => {
                validate(raw)?;
                Ok(Self::Native(raw.into()))
            }
        }
    }

    pub fn is_mcp(&self) -> bool {
        matches!(self, Self::McpServer(_) | Self::McpTool(..))
    }
}

fn validate(name: &str) -> Result<(), ToolKeyParseError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ToolKeyParseError::Invalid(name.into()));
    }
    Ok(())
}

impl fmt::Display for ToolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wildcard => f.write_str("*"),
            Self::Native(name) => f.write_str(name),
            Self::McpServer(server) => write!(f, "{server}.*"),
            Self::McpTool(server, tool) => write!(f, "{server}.{tool}"),
        }
    }
}

impl FromStr for ToolKey {
    type Err = ToolKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for ToolKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ToolKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultEffect {
    Allow,
    Deny,
    #[default]
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PermissionRule {
    pub tool: ToolKey,
    pub scope: Option<String>,
    pub effect: Effect,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsConfig {
    pub default: DefaultEffect,
    pub yolo: bool,
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default: DefaultEffect::Prompt,
            yolo: false,
            rules: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_keys() {
        assert_eq!(ToolKey::parse("*").unwrap(), ToolKey::Wildcard);
        assert_eq!(
            ToolKey::parse("bash").unwrap(),
            ToolKey::Native("bash".into())
        );
        assert_eq!(
            ToolKey::parse("deepwiki.*").unwrap(),
            ToolKey::McpServer("deepwiki".into())
        );
        assert_eq!(
            ToolKey::parse("deepwiki.search").unwrap(),
            ToolKey::McpTool("deepwiki".into(), "search".into())
        );
        assert!(ToolKey::parse("").is_err());
        assert!(ToolKey::parse("bad name").is_err());
    }
}
