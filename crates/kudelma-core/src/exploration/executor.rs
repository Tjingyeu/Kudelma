use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use kudelma_storage::{
    ArtifactStore, MemoryEntry, MemoryStore, exploration_dir, now_rfc3339, tasks_dir,
};
use kudelma_tools::{ToolContext, outline_source, supports_source};

use super::evidence::{
    Confidence, CoverageReport, EvidenceCite, ExplorationReport, ExplorationStatus, Finding,
    FindingKind, SymbolRelationship,
};
use super::request::{ExplorationRequest, InvestigationMode};
use crate::events::AgentEvent;
use crate::runtime::RuntimeState;
use crate::verification::{EvidenceResult, ExecutionEvidence};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES_PER_ROOT: usize = 4_000;
const MAX_TERMS: usize = 16;
const MAX_MATCHES_PER_FILE: usize = 12;

const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "where",
    "what",
    "when",
    "which",
    "does",
    "how",
    "why",
    "into",
    "about",
    "are",
    "was",
    "were",
    "find",
    "show",
    "explain",
    "please",
    "current",
    "existing",
    "code",
    "codebase",
    "project",
    "repository",
    "using",
    "use",
    "can",
    "you",
    "need",
    "want",
    "should",
    "would",
    "could",
    "there",
    "their",
    "have",
    "has",
];

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub max_files: usize,
    pub max_results: usize,
    pub include_memory: bool,
    pub include_runtime: bool,
    pub allow_external_fetch: bool,
}

struct Definition {
    file: String,
    name: String,
    lines: [usize; 2],
}

struct LineHit {
    file: String,
    line: usize,
    term: String,
    text: String,
}

/// Execute the shared read-only exploration capability. This performs
/// deterministic retrieval only: no model, provider, or agent session is
/// created. Agency or Lead interprets the returned report with its own model.
pub async fn explore(
    runtime: &RuntimeState,
    ctx: &ToolContext,
    mut request: ExplorationRequest,
) -> Result<ExplorationReport, String> {
    let config = runtime.config.exploration.clone();
    if !config.enabled {
        return Err("the shared exploration capability is disabled".into());
    }
    request.validate()?;
    request.limits.read_only = true;
    request.limits.max_files = request.limits.max_files.max(1).min(config.file_limit());
    request.limits.max_results = request.limits.max_results.max(1).min(config.result_limit());

    let roots = if request.scope.paths.is_empty() {
        vec![runtime.cwd.clone()]
    } else {
        request
            .scope
            .paths
            .iter()
            .map(|path| ctx.env.resolve(path))
            .collect()
    };

    let revision = git_revision(&runtime.cwd).await;
    let cache = revision.as_ref().and_then(|revision| {
        exploration_cache().map(|store| (store, cache_key(&request, revision, &runtime.cwd)))
    });
    if let Some((store, key)) = &cache {
        if let Ok(mut report) = store.load::<ExplorationReport>(key) {
            report.cached = true;
            let _ = runtime.events.send(AgentEvent::Info(format!(
                "Exploration (cached): {}",
                report.summary
            )));
            return Ok(report);
        }
    }

    let evidence: Vec<ExecutionEvidence> = runtime
        .verifier
        .list()
        .unwrap_or_default()
        .into_iter()
        .take(5)
        .collect();
    let memory: Vec<MemoryEntry> = MemoryStore::new(&runtime.cwd)
        .map(|store| {
            let mut entries = store.list("project");
            entries.extend(store.list("global"));
            entries
        })
        .unwrap_or_default();

    let options = ScanOptions {
        max_files: request.limits.max_files,
        max_results: request.limits.max_results,
        include_memory: request.investigation_mode.includes_memory(),
        include_runtime: request.investigation_mode.includes_runtime(),
        allow_external_fetch: config.allow_external_fetch,
    };
    let cwd = runtime.cwd.clone();
    let scanning = request.clone();
    let mut report = tokio::task::spawn_blocking(move || {
        scan(&scanning, &cwd, &roots, &options, &evidence, &memory)
    })
    .await
    .map_err(|error| error.to_string())?;
    report.revision = revision.clone();
    report.cached = false;

    if let Some((store, key)) = &cache {
        let _ = store.save(key, &report);
    }
    if let Ok(dir) = tasks_dir() {
        let store = ArtifactStore::new(dir);
        let _ = store.save(&format!("exploration-{}", report.request_id), &report);
    }
    let _ = runtime
        .events
        .send(AgentEvent::Info(format!("Exploration: {}", report.summary)));
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
pub fn scan(
    request: &ExplorationRequest,
    cwd: &Path,
    roots: &[PathBuf],
    options: &ScanOptions,
    evidence: &[ExecutionEvidence],
    memory: &[MemoryEntry],
) -> ExplorationReport {
    let mut omitted = Vec::new();
    let mut findings = Vec::new();
    let mut unknowns = Vec::new();
    let mut relationships = Vec::new();
    let mut searched_files = Vec::new();
    let mut searched_symbols = Vec::new();
    let mut definitions = Vec::new();
    let mut all_symbols: Vec<Definition> = Vec::new();
    let mut hits = Vec::new();
    let mut result_limit_reached = false;

    let terms = extract_terms(request);
    let mut files = Vec::new();
    if request.investigation_mode.includes_codebase() {
        for root in roots {
            let mut root_files = collect_files(root, options.max_files * 4, &mut omitted);
            files.append(&mut root_files);
        }
        files.sort_by(|a, b| {
            file_priority(a, &terms)
                .cmp(&file_priority(b, &terms))
                .then_with(|| a.cmp(b))
        });
        files.dedup();
        if files.len() > options.max_files {
            omitted.push(format!(
                "{} additional file(s) were not inspected because the file limit is {}",
                files.len() - options.max_files,
                options.max_files
            ));
            files.truncate(options.max_files);
        }
    }

    if request.investigation_mode.includes_codebase() && terms.is_empty() {
        unknowns.push(
            "no searchable identifiers were extracted from the objective; ask a narrower question"
                .into(),
        );
    }

    let mut files_inspected = 0usize;
    for path in &files {
        if findings.len() >= options.max_results {
            result_limit_reached = true;
            break;
        }
        let Some(text) = read_text(path) else {
            omitted.push(format!(
                "{} could not be inspected (binary, unreadable, or larger than 2 MiB)",
                display_path(cwd, path)
            ));
            continue;
        };
        let rel = display_path(cwd, path);
        files_inspected += 1;
        if searched_files.len() < 200 {
            searched_files.push(rel.clone());
        }
        let lines: Vec<&str> = text.lines().collect();

        if let Some(outline) = outline_source(path, &text) {
            for symbol in &outline.symbols {
                if searched_symbols.len() < 200 {
                    searched_symbols.push(symbol.name.clone());
                }
                if all_symbols.len() < 1_000 {
                    all_symbols.push(Definition {
                        file: rel.clone(),
                        name: symbol.name.clone(),
                        lines: [symbol.start, symbol.end],
                    });
                }
                let requested = request
                    .scope
                    .symbols
                    .iter()
                    .any(|candidate| candidate == &symbol.name);
                let matched =
                    requested || terms.iter().any(|term| term_matches(term, &symbol.name));
                if !matched {
                    continue;
                }
                if findings.len() >= options.max_results {
                    result_limit_reached = true;
                    break;
                }
                definitions.push(Definition {
                    file: rel.clone(),
                    name: symbol.name.clone(),
                    lines: [symbol.start, symbol.end],
                });
                findings.push(Finding {
                    id: String::new(),
                    claim: format!(
                        "Defined `{}` ({}) at lines {}-{}",
                        symbol.name, symbol.kind, symbol.start, symbol.end
                    ),
                    kind: FindingKind::ObservedFact,
                    evidence: vec![
                        EvidenceCite::file(rel.clone(), [symbol.start, symbol.end])
                            .symbol(symbol.name.clone()),
                    ],
                    confidence: Confidence::High,
                });
            }
        }

        let mut matches_in_file = 0usize;
        for (index, line) in lines.iter().enumerate() {
            if matches_in_file >= MAX_MATCHES_PER_FILE || findings.len() >= options.max_results {
                result_limit_reached = findings.len() >= options.max_results;
                break;
            }
            let line_lower = line.to_lowercase();
            for term in &terms {
                if !line_lower.contains(&term.to_lowercase()) {
                    continue;
                }
                let line_no = index + 1;
                let definition_here = definitions.iter().any(|definition| {
                    definition.file == rel
                        && definition.name.eq_ignore_ascii_case(term)
                        && line_no >= definition.lines[0]
                        && line_no <= definition.lines[1]
                });
                if definition_here {
                    continue;
                }
                let snippet = clip_line(line.trim(), 160);
                findings.push(Finding {
                    id: String::new(),
                    claim: format!("`{term}` appears at line {line_no}"),
                    kind: FindingKind::ObservedFact,
                    evidence: vec![
                        EvidenceCite::file(rel.clone(), [line_no, line_no]).note(snippet.clone()),
                    ],
                    confidence: if request
                        .scope
                        .symbols
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(term))
                        || is_definition_line(line)
                    {
                        Confidence::High
                    } else {
                        Confidence::Medium
                    },
                });
                hits.push(LineHit {
                    file: rel.clone(),
                    line: line_no,
                    term: term.clone(),
                    text: snippet,
                });
                matches_in_file += 1;
                if matches_in_file >= MAX_MATCHES_PER_FILE || findings.len() >= options.max_results
                {
                    break;
                }
            }
        }
    }

    for hit in &hits {
        for definition in &definitions {
            if !hit.term.eq_ignore_ascii_case(&definition.name) {
                continue;
            }
            if hit.file == definition.file
                && hit.line >= definition.lines[0]
                && hit.line <= definition.lines[1]
            {
                continue;
            }
            if relationships.len() >= options.max_results {
                break;
            }
            let kind = if hit.text.contains(&format!("{}(", definition.name)) {
                "call"
            } else {
                "reference"
            };
            let from = all_symbols
                .iter()
                .filter(|symbol| {
                    symbol.file == hit.file
                        && hit.line >= symbol.lines[0]
                        && hit.line <= symbol.lines[1]
                })
                .max_by_key(|symbol| symbol.lines[0])
                .map(|symbol| symbol.name.clone())
                .unwrap_or_else(|| hit.file.clone());
            relationships.push(SymbolRelationship {
                from,
                to: definition.name.clone(),
                kind: kind.into(),
                file: hit.file.clone(),
                line: hit.line,
            });
        }
    }
    relationships.sort_by(|a, b| {
        a.to.cmp(&b.to)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });
    relationships.dedup_by(|a, b| a.to == b.to && a.file == b.file && a.line == b.line);

    if options.include_runtime {
        for item in evidence.iter().take(5) {
            if findings.len() >= options.max_results {
                result_limit_reached = true;
                break;
            }
            let log = item.log_path.clone().unwrap_or_else(|| item.cwd.clone());
            findings.push(Finding {
                id: String::new(),
                claim: format!(
                    "Recent verification `{}` -> {} (exit {})",
                    item.command,
                    item.result.name(),
                    item.exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".into())
                ),
                kind: FindingKind::ObservedFact,
                evidence: vec![
                    EvidenceCite::file(log, [1, 1])
                        .note(format!("evidence {} · {:?}", item.id, item.kind)),
                ],
                confidence: Confidence::High,
            });
            if item.result == EvidenceResult::Failed {
                unknowns.push(format!(
                    "The last run of `{}` failed; inspect evidence {} before claiming success",
                    item.command, item.id
                ));
            }
        }
    }

    if options.include_memory {
        for entry in memory.iter().take(10) {
            if findings.len() >= options.max_results {
                result_limit_reached = true;
                break;
            }
            findings.push(Finding {
                id: String::new(),
                claim: format!("Project memory: {}", clip_line(entry.content.trim(), 180)),
                kind: FindingKind::ObservedFact,
                evidence: vec![
                    EvidenceCite::file("(project memory)", [1, 1]).note(entry.created_at.clone()),
                ],
                confidence: Confidence::Medium,
            });
        }
    }

    let mut external_sources = Vec::new();
    if request.investigation_mode == InvestigationMode::TechnicalResearch {
        for source in urls_in(&request.objective, &request.questions) {
            external_sources.push(source);
        }
        if options.allow_external_fetch {
            unknowns.push(
                "External documentation was not fetched automatically; use `webfetch` and cite the source version/date."
                    .into(),
            );
        } else {
            unknowns.push("External fetch is disabled by configuration.".into());
        }
    }

    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("F-{:03}", index + 1);
    }

    let status = if files_inspected == 0 && findings.is_empty() {
        ExplorationStatus::Blocked
    } else if result_limit_reached
        || !omitted.is_empty()
        || (findings.is_empty() && !files.is_empty())
        || terms.is_empty()
    {
        ExplorationStatus::Partial
    } else {
        ExplorationStatus::Completed
    };

    let summary = format!(
        "Inspected {} file(s) under {}; {} finding(s), {} relationship(s), {} unknown(s).",
        files_inspected,
        roots
            .iter()
            .map(|root| display_path(cwd, root))
            .collect::<Vec<_>>()
            .join(", "),
        findings.len(),
        relationships.len(),
        unknowns.len()
    );

    ExplorationReport {
        request_id: request.request_id.clone(),
        caller: request.caller,
        investigation_mode: request.investigation_mode,
        status,
        summary,
        findings,
        relationships,
        unknowns,
        coverage: CoverageReport {
            searched_files,
            searched_symbols,
            omitted_or_unavailable: omitted,
            files_inspected,
            result_limit_reached,
        },
        external_sources,
        revision: None,
        cached: false,
        created_at: now_rfc3339(),
    }
}

fn extract_terms(request: &ExplorationRequest) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for symbol in &request.scope.symbols {
        push_term(&mut terms, &mut seen, symbol);
    }
    let mut texts = vec![request.objective.clone()];
    texts.extend(request.questions.iter().cloned());
    for text in &texts {
        for quoted in quoted_phrases(text) {
            push_term(&mut terms, &mut seen, &quoted);
        }
        for token in text.split(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '-')
        }) {
            push_term(&mut terms, &mut seen, token);
        }
    }
    terms.truncate(MAX_TERMS);
    terms
}

fn push_term(terms: &mut Vec<String>, seen: &mut HashSet<String>, candidate: &str) {
    let candidate = candidate
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
    if candidate.len() < 3 || candidate.len() > 64 {
        return;
    }
    let lower = candidate.to_ascii_lowercase();
    if STOPWORDS.contains(&lower.as_str()) || !seen.insert(lower) {
        return;
    }
    terms.push(candidate.to_string());
}

fn quoted_phrases(text: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if !matches!(character, '`' | '"' | '\'') {
            continue;
        }
        let mut phrase = String::new();
        for next in chars.by_ref() {
            if next == character {
                break;
            }
            phrase.push(next);
        }
        if phrase.len() >= 3 && phrase.len() <= 64 {
            phrases.push(phrase);
        }
    }
    phrases
}

fn urls_in(objective: &str, questions: &[String]) -> Vec<String> {
    let mut urls = Vec::new();
    for text in std::iter::once(objective).chain(questions.iter().map(String::as_str)) {
        for token in text.split_whitespace() {
            if (token.starts_with("https://") || token.starts_with("http://"))
                && !urls.iter().any(|existing| existing == token)
            {
                urls.push(token.trim_end_matches(['.', ',', ')', ']']).to_string());
            }
        }
    }
    urls
}

fn collect_files(root: &Path, max: usize, omitted: &mut Vec<String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if root.is_file() {
        if fs::symlink_metadata(root)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            omitted.push(format!(
                "{} is a symbolic link and was not followed",
                root.display()
            ));
            return files;
        }
        files.push(root.to_path_buf());
        return files;
    }
    if !root.exists() {
        omitted.push(format!("{} does not exist", root.display()));
        return files;
    }
    if !root.is_dir() {
        omitted.push(format!(
            "{} is not a regular file or directory",
            root.display()
        ));
        return files;
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .follow_links(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .build();
    for entry in walker.flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        if files.len() >= max || files.len() >= MAX_FILES_PER_ROOT {
            omitted.push(format!("file scan limit reached under {}", root.display()));
            break;
        }
        files.push(entry.into_path());
    }
    files
}

fn file_priority(path: &Path, terms: &[String]) -> u8 {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if terms
        .iter()
        .any(|term| name.contains(&term.to_ascii_lowercase()))
    {
        return 0;
    }
    if supports_source(path) {
        return 1;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "md" | "txt" | "toml" | "json" | "yaml" | "yml"
    ) {
        return 2;
    }
    3
}

fn read_text(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.iter().take(4096).any(|byte| *byte == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    Some(text.replace("\r\n", "\n"))
}

fn term_matches(term: &str, name: &str) -> bool {
    name.eq_ignore_ascii_case(term)
        || (term.len() >= 3
            && name
                .to_ascii_lowercase()
                .contains(&term.to_ascii_lowercase()))
}

fn is_definition_line(line: &str) -> bool {
    let line = line.trim_start();
    [
        "fn ",
        "pub fn ",
        "def ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "function ",
        "const ",
        "static ",
        "type ",
        "interface ",
        "mod ",
        "namespace ",
        "let ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn clip_line(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut clipped: String = line.chars().take(max).collect();
    clipped.push('…');
    clipped
}

fn display_path(cwd: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(cwd)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    if relative.is_empty() {
        ".".into()
    } else {
        relative
    }
}

fn exploration_cache() -> Option<ArtifactStore> {
    exploration_dir().ok().map(ArtifactStore::new)
}

fn cache_key(request: &ExplorationRequest, revision: &str, cwd: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let parts = [
        cwd.display().to_string(),
        revision.to_string(),
        request.caller.name().to_string(),
        request.investigation_mode.name().to_string(),
        request.objective.clone(),
        request.questions.join("\u{1f}"),
        request.scope.paths.join("\u{1f}"),
        request.scope.symbols.join("\u{1f}"),
        request.scope.sources.join("\u{1f}"),
        request.expected_output.clone(),
        request.limits.max_files.to_string(),
        request.limits.max_results.to_string(),
    ];
    for part in parts {
        for byte in part.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0x1f;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("cache-{hash:016x}")
}

async fn git_revision(cwd: &Path) -> Option<String> {
    let head = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())?;
    let diff = tokio::process::Command::new("git")
        .args(["diff", "HEAD", "--binary"])
        .current_dir(cwd)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();
    let status = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in diff.into_iter().chain(status) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(format!("{head}:{hash:016x}"))
}

#[allow(clippy::too_many_arguments)]
pub fn request_from_tool(
    caller: super::request::Caller,
    objective: String,
    questions: Vec<String>,
    investigation_mode: InvestigationMode,
    paths: Vec<String>,
    symbols: Vec<String>,
    sources: Vec<String>,
    expected_output: Option<String>,
    limits: super::request::ExplorationLimits,
) -> ExplorationRequest {
    let mut request = ExplorationRequest::new(caller, investigation_mode, objective);
    request.questions = questions;
    request.scope.paths = paths;
    request.scope.symbols = symbols;
    request.scope.sources = sources;
    request.limits = limits;
    if let Some(expected) = expected_output {
        request.expected_output = expected;
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exploration::request::{Caller, ExplorationLimits};

    fn options() -> ScanOptions {
        ScanOptions {
            max_files: 20,
            max_results: 50,
            include_memory: false,
            include_runtime: false,
            allow_external_fetch: false,
        }
    }

    #[test]
    fn finds_symbols_and_references_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn parse_config() -> u8 { 1 }\n\nfn caller() {\n    let _ = parse_config();\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/other.rs"),
            "use crate::parse_config;\n",
        )
        .unwrap();
        let request = request_from_tool(
            Caller::Agency,
            "where is parse_config used?".into(),
            Vec::new(),
            InvestigationMode::Codebase,
            vec!["src".into()],
            vec!["parse_config".into()],
            Vec::new(),
            None,
            ExplorationLimits::default(),
        );
        let report = scan(
            &request,
            dir.path(),
            &[dir.path().join("src")],
            &options(),
            &[],
            &[],
        );
        assert_eq!(report.status, ExplorationStatus::Completed);
        assert_eq!(report.coverage.files_inspected, 2);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.claim.contains("Defined `parse_config`"))
        );
        let text_hits: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.claim.contains("`parse_config` appears"))
            .collect();
        assert_eq!(text_hits.len(), 2);
        assert!(
            text_hits
                .iter()
                .all(|finding| finding.claim.contains("line 4") || finding.claim.contains("line 1"))
        );
        assert!(!report.relationships.is_empty());
        assert!(
            report
                .relationships
                .iter()
                .all(|relationship| relationship.to == "parse_config")
        );
        assert!(
            report
                .coverage
                .searched_files
                .iter()
                .any(|file| file.ends_with("src/lib.rs"))
        );
    }

    #[test]
    fn reports_partial_without_terms() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let request = request_from_tool(
            Caller::Lead,
            "what is this?".into(),
            Vec::new(),
            InvestigationMode::Codebase,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            ExplorationLimits::default(),
        );
        let report = scan(
            &request,
            dir.path(),
            &[dir.path().to_path_buf()],
            &options(),
            &[],
            &[],
        );
        assert_eq!(report.status, ExplorationStatus::Partial);
        assert!(!report.unknowns.is_empty());
    }

    #[test]
    fn reports_blocked_for_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let request = request_from_tool(
            Caller::Lead,
            "parse_config".into(),
            Vec::new(),
            InvestigationMode::Codebase,
            vec!["missing".into()],
            Vec::new(),
            Vec::new(),
            None,
            ExplorationLimits::default(),
        );
        let report = scan(
            &request,
            dir.path(),
            &[dir.path().join("missing")],
            &options(),
            &[],
            &[],
        );
        assert_eq!(report.status, ExplorationStatus::Blocked);
        assert!(
            report
                .coverage
                .omitted_or_unavailable
                .iter()
                .any(|entry| entry.contains("does not exist"))
        );
    }
}
