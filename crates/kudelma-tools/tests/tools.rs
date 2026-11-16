use std::sync::Arc;

use kudelma_config::Config;
use kudelma_providers::CancelToken;
use kudelma_tools::{
    BashTool, EditTool, GlobTool, GrepTool, LocalEnv, ReadTool, Tool, ToolContext, WriteTool,
};
use serde_json::json;

fn context(cwd: &std::path::Path, state: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Arc::new(LocalEnv::new(cwd.to_path_buf())),
        state.to_path_buf(),
        "test-session",
        CancelToken::new(),
        Arc::new(Config::default()),
    )
}

#[tokio::test]
async fn write_read_then_edit_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), dir.path());

    WriteTool
        .execute(
            json!({ "path": "src/lib.rs", "content": "fn old() {}\n" }),
            &ctx,
        )
        .await
        .unwrap();
    let read = ReadTool
        .execute(json!({ "path": "src/lib.rs" }), &ctx)
        .await
        .unwrap();
    assert!(read.content.contains("1: fn old() {}"));

    EditTool
        .execute(
            json!({
                "path": "src/lib.rs",
                "old_string": "fn old() {}",
                "new_string": "fn new() {}"
            }),
            &ctx,
        )
        .await
        .unwrap();
    let contents = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert_eq!(contents, "fn new() {}\n");
}

#[tokio::test]
async fn edit_refuses_a_file_changed_since_read() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), dir.path());
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    ReadTool
        .execute(json!({ "path": "a.txt" }), &ctx)
        .await
        .unwrap();
    std::fs::write(dir.path().join("a.txt"), "changed elsewhere\n").unwrap();
    let error = EditTool
        .execute(
            json!({ "path": "a.txt", "old_string": "hello", "new_string": "bye" }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed on disk"));
}

#[tokio::test]
async fn glob_and_grep_ignore_gitignored_files() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), dir.path());
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("src.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("target/hidden.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

    let glob = GlobTool
        .execute(json!({ "pattern": "**/*.rs" }), &ctx)
        .await
        .unwrap();
    assert!(glob.content.contains("src.rs"));
    assert!(!glob.content.contains("hidden.rs"));

    let grep = GrepTool
        .execute(json!({ "pattern": "needle" }), &ctx)
        .await
        .unwrap();
    assert!(grep.content.contains("src.rs"));
    assert!(!grep.content.contains("hidden.rs"));
}

#[tokio::test]
async fn bash_captures_stdout_and_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), dir.path());
    let output = BashTool
        .execute(json!({ "command": "printf 'hello\\n'" }), &ctx)
        .await
        .unwrap();
    assert!(output.content.contains("hello"));
    assert!(!output.is_error);

    let failed = BashTool
        .execute(json!({ "command": "exit 3" }), &ctx)
        .await
        .unwrap();
    assert!(failed.content.contains("exit code: 3"));
    assert!(failed.is_error);
}
