use std::collections::HashSet;

use tree_sitter::Node;

use crate::scope::Scope;

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "select", "function", "time", "!", "{", "}", "(",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashAnalysis {
    pub scopes: Vec<Scope>,
    /// A redirection or unparsable script: never approve it through a broad
    /// wildcard, even when the default effect is allow.
    pub force_prompt: bool,
}

/// Split a shell script into the command scopes the permission engine checks.
/// Parsing failures fail closed: the whole script becomes one exact scope.
pub fn analyze_bash(script: &str) -> BashAnalysis {
    let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return BashAnalysis {
            scopes: vec![Scope::exact(script.trim())],
            force_prompt: true,
        };
    }
    let Some(tree) = parser.parse(script, None) else {
        return BashAnalysis {
            scopes: vec![Scope::exact(script.trim())],
            force_prompt: true,
        };
    };
    if tree.root_node().has_error() {
        return BashAnalysis {
            scopes: vec![Scope::exact(script.trim())],
            force_prompt: true,
        };
    }

    let mut commands = Vec::new();
    let mut redirect = false;
    walk(
        tree.root_node(),
        script.as_bytes(),
        &mut commands,
        &mut redirect,
    );

    if commands.is_empty() {
        return BashAnalysis {
            scopes: vec![Scope::exact(script.trim())],
            force_prompt: true,
        };
    }

    let mut seen = HashSet::new();
    let mut scopes = Vec::new();
    for command in commands {
        let command = command.trim();
        if command.is_empty() || !seen.insert(command.to_string()) {
            continue;
        }
        let exact = redirect || needs_exact(command);
        scopes.push(if exact {
            Scope::exact(command)
        } else {
            Scope::command(command)
        });
    }
    BashAnalysis {
        scopes,
        force_prompt: redirect,
    }
}

fn walk(node: Node<'_>, source: &[u8], commands: &mut Vec<String>, redirect: &mut bool) {
    if node.kind() == "command" {
        if let Ok(text) = node.utf8_text(source) {
            commands.push(text.to_string());
        }
    }
    if node.kind().contains("redirect") || node.kind() == "heredoc_body" {
        *redirect = true;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, commands, redirect);
    }
}

fn needs_exact(command: &str) -> bool {
    command.contains("$(")
        || command.contains('`')
        || command.contains('\n')
        || command.contains('<')
        || command.contains('>')
        || command.contains("${")
}

/// Turn an approved command into a session rule. Only a plain command word is
/// widened to `<cmd> *`; loops, assignments and redirections stay literal.
pub fn generalize_command(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or_default();
    let plain = !first.is_empty()
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:/@+-".contains(c))
        && !SHELL_KEYWORDS.contains(&first)
        && !needs_exact(command);
    if plain {
        format!("{first} *")
    } else {
        command.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_chained_commands() {
        let analysis = analyze_bash("git status && rm -rf /tmp/x");
        let values: Vec<&str> = analysis.scopes.iter().map(|s| s.value.as_str()).collect();
        assert_eq!(values, vec!["git status", "rm -rf /tmp/x"]);
        assert!(!analysis.force_prompt);
    }

    #[test]
    fn redirections_require_exact_approval() {
        let analysis = analyze_bash("echo hi > /etc/passwd");
        assert!(analysis.force_prompt);
        assert!(analysis.scopes.iter().all(|scope| scope.exact));
    }

    #[test]
    fn parse_errors_fail_closed() {
        let analysis = analyze_bash("echo 'unterminated");
        assert!(analysis.force_prompt);
        assert_eq!(analysis.scopes.len(), 1);
        assert!(analysis.scopes[0].exact);
    }

    #[test]
    fn only_plain_commands_are_widened() {
        assert_eq!(generalize_command("git status"), "git *");
        assert_eq!(generalize_command("FOO=1 git status"), "FOO=1 git status");
        assert_eq!(generalize_command("echo hi > /tmp/x"), "echo hi > /tmp/x");
    }
}
