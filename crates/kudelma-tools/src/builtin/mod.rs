mod bash;
mod edit;
mod glob;
mod grep;
mod index;
mod ls;
mod memory;
mod read;
mod todo;
mod tool_search;
mod webfetch;
mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use index::{IndexTool, SourceOutline, SymbolOutline, outline_source, supports_source};
pub use ls::LsTool;
pub use memory::MemoryTool;
pub use read::ReadTool;
pub use todo::TodoWriteTool;
pub use tool_search::ToolSearchTool;
pub use webfetch::WebfetchTool;
pub use write::WriteTool;

use std::sync::Arc;

use crate::Tool;

pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
        Arc::new(LsTool),
        Arc::new(IndexTool),
        Arc::new(BashTool),
        Arc::new(TodoWriteTool),
        Arc::new(MemoryTool),
        Arc::new(WebfetchTool),
    ]
}

pub(crate) fn diff_preview(old: &str, new: &str, max_lines: usize) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string();
    if unified.lines().count() <= max_lines {
        return unified;
    }
    let mut out: String = unified
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    out.push_str("\n[... diff truncated ...]");
    out
}
