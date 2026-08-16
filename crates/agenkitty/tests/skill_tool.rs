//! Skill tool family integration tests (RFC-121): the context-token
//! handshake, activation gating, model-invocation rules, probe-safe
//! visibility, and attenuation-only narrowing.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use agenkitty::config::SkillsConfigSection;
use agenkitty::tools::{
    CurrentSkillContext, SkillReadInput, SkillReadTool, SkillRuntime, SkillUseInput, SkillUseTool,
};
use tempfile::TempDir;

fn write_skill(root: &Path, name: &str, extra: &str) {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("references")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Skill {name}. Use in tests.\n{extra}---\nBody of {name}.\n"),
    )
    .unwrap();
    fs::write(dir.join("references/guide.md"), format!("guide for {name}")).unwrap();
}

fn runtime_for(root: &Path) -> Arc<SkillRuntime> {
    let section = SkillsConfigSection {
        roots: vec![root.to_path_buf()],
        ..SkillsConfigSection::default()
    };
    // The section root is absolute, so the project root is irrelevant here.
    Arc::new(SkillRuntime::from_config(
        Path::new("/nonexistent"),
        &section,
    ))
}

fn context(runtime: &SkillRuntime, thread: &str) -> CurrentSkillContext {
    let _ = runtime;
    CurrentSkillContext {
        agent_id: "agent".to_string(),
        thread_id: Some(thread.to_string()),
        visible: None,
    }
}

fn use_input(runtime: &SkillRuntime, name: &str, ctx: CurrentSkillContext) -> SkillUseInput {
    let args = runtime
        .inject_context_args(&serde_json::json!({ "name": name }), ctx)
        .unwrap();
    serde_json::from_value(args).unwrap()
}

fn read_input(
    runtime: &SkillRuntime,
    name: &str,
    path: &str,
    ctx: CurrentSkillContext,
) -> SkillReadInput {
    let args = runtime
        .inject_context_args(&serde_json::json!({ "name": name, "path": path }), ctx)
        .unwrap();
    serde_json::from_value(args).unwrap()
}

#[tokio::test]
async fn use_serves_body_and_unlocks_read() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "deploy", "");
    let runtime = runtime_for(tmp.path());

    // skill.read before activation is a policy refusal.
    let read = SkillReadTool::new(runtime.clone());
    let err = read
        .run(read_input(
            &runtime,
            "deploy",
            "references/guide.md",
            context(&runtime, "t1"),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");

    let output = SkillUseTool::new(runtime.clone())
        .run(use_input(&runtime, "deploy", context(&runtime, "t1")))
        .await
        .unwrap();
    assert!(output.body.contains("Body of deploy"));
    assert_eq!(output.resources, ["references/guide.md"]);

    let chunk = read
        .run(read_input(
            &runtime,
            "deploy",
            "references/guide.md",
            context(&runtime, "t1"),
        ))
        .await
        .unwrap();
    assert_eq!(chunk.content, "guide for deploy");
    assert!(chunk.eof);
}

#[tokio::test]
async fn activation_is_per_session_key() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "deploy", "");
    let runtime = runtime_for(tmp.path());

    SkillUseTool::new(runtime.clone())
        .run(use_input(&runtime, "deploy", context(&runtime, "thread-a")))
        .await
        .unwrap();
    // A different session key has not activated the skill.
    let err = SkillReadTool::new(runtime.clone())
        .run(read_input(
            &runtime,
            "deploy",
            "references/guide.md",
            context(&runtime, "thread-b"),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
}

#[tokio::test]
async fn disable_model_invocation_refuses_the_tool_path() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "manual", "disable-model-invocation: true\n");
    let runtime = runtime_for(tmp.path());

    let err = SkillUseTool::new(runtime.clone())
        .run(use_input(&runtime, "manual", context(&runtime, "t1")))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
    // And it is absent from the prompt index.
    assert_eq!(runtime.system_prompt_part(), None);
}

#[tokio::test]
async fn out_of_view_and_unknown_names_look_identical() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "visible-skill", "");
    write_skill(tmp.path(), "hidden-skill", "");
    let runtime = runtime_for(tmp.path());

    let narrowed = CurrentSkillContext {
        agent_id: "agent".to_string(),
        thread_id: Some("t1".to_string()),
        visible: Some(Arc::new(BTreeSet::from(["visible-skill".to_string()]))),
    };
    let tool = SkillUseTool::new(runtime.clone());
    let hidden_err = tool
        .run(use_input(&runtime, "hidden-skill", narrowed.clone()))
        .await
        .unwrap_err();
    let unknown_err = tool
        .run(use_input(&runtime, "no-such-skill", narrowed))
        .await
        .unwrap_err();
    assert_eq!(hidden_err.kind(), "not_found");
    assert_eq!(unknown_err.kind(), "not_found");
    // Same shape modulo the name: no existence oracle (S7).
    assert_eq!(
        hidden_err.to_string().replace("hidden-skill", "X"),
        unknown_err.to_string().replace("no-such-skill", "X")
    );
}

#[tokio::test]
async fn fork_narrows_prompt_and_enforcement_together() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "alpha", "");
    write_skill(tmp.path(), "beta", "");
    let runtime = runtime_for(tmp.path());

    let parent_part = runtime.system_prompt_part().expect("parent sees both");
    assert!(parent_part.contains("- alpha:"));
    assert!(parent_part.contains("- beta:"));

    let child = Arc::new(
        runtime
            .fork(Some(&BTreeSet::from(["alpha".to_string()])))
            .unwrap(),
    );
    let child_part = child.system_prompt_part().expect("child sees alpha");
    assert!(child_part.contains("- alpha:"));
    assert!(!child_part.contains("- beta:"));

    // Enforcement matches the child's prompt.
    let err = SkillUseTool::new(child.clone())
        .run(use_input(&child, "beta", context(&child, "child-t")))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
    assert!(
        SkillUseTool::new(child.clone())
            .run(use_input(&child, "alpha", context(&child, "child-t")))
            .await
            .is_ok()
    );

    // Widening is a loud error, and a grandchild may only narrow further.
    assert!(
        child
            .fork(Some(&BTreeSet::from(["beta".to_string()])))
            .is_err()
    );

    // Activations do not inherit across the fork boundary.
    SkillUseTool::new(runtime.clone())
        .run(use_input(&runtime, "alpha", context(&runtime, "t1")))
        .await
        .unwrap();
    let err = SkillReadTool::new(child.clone())
        .run(read_input(
            &child,
            "alpha",
            "references/guide.md",
            context(&child, "t1"),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "tool_policy");
}

#[test]
fn compose_system_prompt_gates_on_the_tool_set() {
    let tmp = TempDir::new().unwrap();
    // Default config discovers `.agents/skills` under the project root.
    write_skill(&tmp.path().join(".agents/skills"), "greeter", "");
    let runner = agenkitty::supervisor::FrameworkRunner::mock_for_project(tmp.path()).unwrap();

    let with_skills = runner.compose_system_prompt("base", &["skill.use".to_string()]);
    assert!(with_skills.starts_with("base\n\n## Skills"));
    assert!(with_skills.contains("- greeter:"));

    // Without skill.use in the tool set the prompt never advertises it.
    let without = runner.compose_system_prompt("base", &["fs.read".to_string()]);
    assert_eq!(without, "base");
}

#[tokio::test]
async fn disabled_config_yields_an_empty_runtime() {
    let tmp = TempDir::new().unwrap();
    write_skill(tmp.path(), "deploy", "");
    let section = SkillsConfigSection {
        enabled: false,
        roots: vec![tmp.path().to_path_buf()],
        ..SkillsConfigSection::default()
    };
    let runtime = SkillRuntime::from_config(Path::new("/nonexistent"), &section);
    assert_eq!(runtime.system_prompt_part(), None);
}
