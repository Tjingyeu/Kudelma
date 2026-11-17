use async_trait::async_trait;
use kudelma_permissions::Scope;
use kudelma_storage::MemoryStore;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, object_schema, optional_str,
    required_str,
};

pub struct MemoryTool;

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Store durable project or global notes (build commands, conventions, user preferences) and list or remove them later."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "action": { "type": "string", "enum": ["add", "list", "remove"] },
                "content": { "type": "string", "description": "Note text for add" },
                "id": { "type": "string", "description": "Entry id for remove" },
                "scope": { "type": "string", "enum": ["project", "global"], "description": "Default project" }
            }),
            &["action"],
        )
    }

    fn permission(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact("memory")]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = required_str(&args, "action")?;
        let scope = optional_str(&args, "scope").unwrap_or_else(|| "project".into());
        if scope != "project" && scope != "global" {
            return Err(ToolError::InvalidArgs(
                "scope must be `project` or `global`".into(),
            ));
        }
        let store = MemoryStore::new(ctx.cwd()).map_err(|e| ToolError::Io(e.to_string()))?;
        match action.as_str() {
            "add" => {
                let content = required_str(&args, "content")?;
                let entry = store
                    .add(&scope, &content)
                    .map_err(|e| ToolError::Io(e.to_string()))?;
                Ok(ToolOutput::success(format!(
                    "Remembered ({scope}) {}: {}",
                    entry.id, entry.content
                )))
            }
            "list" => {
                let entries = store.list(&scope);
                if entries.is_empty() {
                    return Ok(ToolOutput::success(format!("No {scope} memories yet.")));
                }
                let mut output = format!("{scope} memories ({}):", entries.len());
                for entry in entries {
                    output.push_str(&format!("\n- [{}] {}", entry.id, entry.content));
                }
                Ok(ToolOutput::success(output))
            }
            "remove" => {
                let id = required_str(&args, "id")?;
                let removed = store
                    .remove(&scope, &id)
                    .map_err(|e| ToolError::Io(e.to_string()))?;
                Ok(ToolOutput::success(if removed {
                    format!("Removed memory {id}.")
                } else {
                    format!("No {scope} memory with id {id}.")
                }))
            }
            other => Err(ToolError::InvalidArgs(format!(
                "unknown memory action `{other}`"
            ))),
        }
    }
}
