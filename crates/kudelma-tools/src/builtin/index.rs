use std::path::Path;

use async_trait::async_trait;
use kudelma_permissions::Scope;
use serde_json::{Value, json};
use tree_sitter::{Node, Parser};

use crate::{
    Tool, ToolContext, ToolError, ToolOutput, ToolPermission, display_path, object_schema,
    required_str,
};

const MAX_SYMBOLS: usize = 400;

pub struct IndexTool;

#[async_trait]
impl Tool for IndexTool {
    fn name(&self) -> &'static str {
        "index"
    }

    fn description(&self) -> &'static str {
        "Return a compact symbol outline (functions, classes, structs, traits, methods) with line ranges for a source file. Use this before read to find the lines you need without loading the whole file."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "path": { "type": "string", "description": "Source file to outline" }
            }),
            &["path"],
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let path = ctx.env.resolve(&required_str(args, "path")?);
        Ok(ToolPermission::new(vec![Scope::path(path)]))
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = ctx.env.resolve(&required_str(&args, "path")?);
        let bytes =
            ctx.env.read(&path).await.map_err(|error| {
                ToolError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        let language = language_for(&path).ok_or_else(|| {
            ToolError::InvalidArgs(format!(
                "index does not support {}; supported: {}. Use read instead.",
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("this file"),
                "rs, ts, tsx, js, jsx, py, go, c, h, cc, cpp, hpp"
            ))
        })?;
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|error| ToolError::Other(format!("cannot load parser: {error}")))?;
        let tree = parser
            .parse(&text, None)
            .ok_or_else(|| ToolError::Other("failed to parse source".into()))?;
        let has_errors = tree.root_node().has_error();
        let mut symbols = Vec::new();
        collect(tree.root_node(), text.as_bytes(), 0, &mut symbols);
        let total_lines = text.lines().count();
        let mut output = format!(
            "{} — {total_lines} lines, {} symbol{}{}\n",
            display_path(ctx.cwd(), &path),
            symbols.len(),
            if symbols.len() == 1 { "" } else { "s" },
            if has_errors {
                " (parse errors present)"
            } else {
                ""
            }
        );
        if symbols.is_empty() {
            output.push_str("[no top-level symbols found; use read]");
            return Ok(ToolOutput::success(output));
        }
        for symbol in symbols.iter().take(MAX_SYMBOLS) {
            let indent = "  ".repeat(symbol.depth.min(3));
            output.push_str(&format!(
                "{indent}{} {}  [L{}-L{}]\n",
                symbol.kind, symbol.name, symbol.start, symbol.end
            ));
        }
        if symbols.len() > MAX_SYMBOLS {
            output.push_str(&format!(
                "[... {} more symbols ...]\n",
                symbols.len() - MAX_SYMBOLS
            ));
        }
        Ok(ToolOutput::success(output))
    }
}

struct Symbol {
    kind: &'static str,
    name: String,
    start: usize,
    end: usize,
    depth: usize,
}

fn collect(node: Node<'_>, source: &[u8], depth: usize, out: &mut Vec<Symbol>) {
    let mut child_depth = depth;
    if let Some(kind) = symbol_kind(node.kind()) {
        if !inside_function(node) {
            let name = symbol_name(node, source).unwrap_or_else(|| "<anonymous>".into());
            out.push(Symbol {
                kind,
                name,
                start: node.start_position().row + 1,
                end: node.end_position().row + 1,
                depth,
            });
            child_depth = depth + 1;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, child_depth, out);
    }
}

fn inside_function(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "function_item"
                | "function_declaration"
                | "function_definition"
                | "method_definition"
                | "method_declaration"
                | "function"
                | "arrow_function"
                | "closure_expression"
        ) {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn symbol_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "function_item" | "function_declaration" | "function_definition" => "fn",
        "method_definition" | "method_declaration" | "method_signature" => "method",
        "struct_item" | "struct_specifier" | "struct_declaration" => "struct",
        "enum_item" | "enum_declaration" | "enum_specifier" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "mod_item" => "mod",
        "const_item" | "const_declaration" => "const",
        "static_item" => "static",
        "type_item" | "type_alias_declaration" | "type_declaration" => "type",
        "macro_definition" => "macro",
        "union_item" => "union",
        "class_declaration"
        | "class_definition"
        | "class_specifier"
        | "abstract_class_declaration" => "class",
        "interface_declaration" => "interface",
        "namespace_definition" => "namespace",
        "property_declaration" | "public_field_definition" => "property",
        "type_spec" => "type",
        "lexical_declaration" => "const",
        _ => return None,
    })
}

fn symbol_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        return name.utf8_text(source).ok().map(|text| text.to_string());
    }
    if let Some(name) = node.child_by_field_name("type") {
        if let Ok(text) = name.utf8_text(source) {
            return Some(
                text.lines()
                    .next()
                    .unwrap_or(text)
                    .chars()
                    .take(80)
                    .collect(),
            );
        }
    }
    if let Some(name) = node.child_by_field_name("declarator") {
        return first_identifier(name, source);
    }
    first_identifier(node, source)
}

fn first_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier" | "property_identifier"
    ) {
        return node.utf8_text(source).ok().map(|text| text.to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(name) = first_identifier(child, source) {
            return Some(name);
        }
    }
    None
}

fn language_for(path: &Path) -> Option<tree_sitter::Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_rust_symbols() {
        let source = "pub struct A;\nfn helper() {\n    fn nested() {}\n}\nimpl A {\n    fn method(&self) {}\n}\n";
        let mut parser = Parser::new();
        let language = language_for(Path::new("x.rs")).unwrap();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut symbols = Vec::new();
        collect(tree.root_node(), source.as_bytes(), 0, &mut symbols);
        let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"method"));
        assert!(!names.contains(&"nested"));
    }
}
