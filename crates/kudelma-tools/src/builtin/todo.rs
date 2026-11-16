use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use crate::{Tool, ToolContext, ToolError, ToolOutput, ToolPermission, object_schema};

pub use crate::{TodoItem, TodoStatus};

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn description(&self) -> &'static str {
        "Replace the session's structured task list. Use it to plan multi-step work and keep progress visible."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "todos": {
                    "type": "array",
                    "description": "The full task list, in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            }),
            &["todos"],
        )
    }

    fn permission(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact("todos")]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = args
            .get("todos")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::InvalidArgs("missing array argument `todos`".into()))?;
        let mut todos = Vec::with_capacity(raw.len());
        for item in raw {
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs("todo.content must be a string".into()))?;
            let status = match item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
            {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                other => {
                    return Err(ToolError::InvalidArgs(format!(
                        "invalid todo status `{other}`"
                    )));
                }
            };
            todos.push(TodoItem {
                content: content.trim().to_string(),
                status,
            });
        }
        *ctx.todos.write().unwrap_or_else(|e| e.into_inner()) = todos.clone();
        let path = ctx
            .state_dir
            .join("sessions")
            .join("data")
            .join(&ctx.session_id)
            .join("todos.json");
        let _ = kudelma_storage::write_json(&path, &todos);
        if todos.is_empty() {
            return Ok(ToolOutput::success("Task list cleared."));
        }
        let completed = todos
            .iter()
            .filter(|item| item.status == TodoStatus::Completed)
            .count();
        let mut output = format!("Tasks ({completed}/{} complete):", todos.len());
        for item in &todos {
            output.push_str(&format!("\n{} {}", item.status.marker(), item.content));
        }
        Ok(ToolOutput::success(output))
    }
}
