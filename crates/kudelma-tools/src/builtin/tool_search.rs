use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, ToolRegistry, object_schema,
    required_str,
};

/// Reveals deferred tool definitions on demand, so a large MCP fleet does not
/// occupy every prompt.
pub struct ToolSearchTool {
    registry: ToolRegistry,
}

impl ToolSearchTool {
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &'static str {
        "tool_search"
    }

    fn description(&self) -> &'static str {
        "Search deferred tools (usually MCP servers) by name or description. Matching tools become callable on the next turn."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "query": { "type": "string", "description": "Words to match against tool names and descriptions" }
            }),
            &["query"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        Ok(ToolPermission::new(vec![Scope::exact("tool_search")]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let _ = ctx;
        let query = required_str(&args, "query")?;
        let matches = self.registry.search(&query);
        if matches.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No deferred tools matched `{query}`. {} tool(s) are already available.",
                self.registry.definitions().len()
            )));
        }
        let names: Vec<String> = matches.iter().map(|tool| tool.name().to_string()).collect();
        self.registry.reveal(&names);
        let mut output = format!("Revealed {} tool(s) for `{query}`:\n", matches.len());
        for tool in matches.iter().take(20) {
            output.push_str(&format!("\n- `{}`: {}", tool.name(), tool.description()));
            output.push_str(&format!("\n  input schema: {}", tool.schema()));
        }
        Ok(ToolOutput::success(output))
    }
}
