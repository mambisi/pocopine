//! Usage examples for the process tool family.
//!
//! These drive the **real agent path** — a mock model issues `process.run` tool
//! calls by id with JSON args, and we assert how each outcome (success, non-zero
//! exit, timeout, blocked shell mode) surfaces back to the model — plus the
//! typed spawn → write → read → kill handle lifecycle.
//!
//! Process control is unix-only, so the whole file is gated on `unix`.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agenkitty::policy::ToolMode;
use agenkitty::tools::{
    InMemorySecretResolver, ProcessKillInput, ProcessKillTool, ProcessReadInput, ProcessReadTool,
    ProcessRunTool, ProcessSpawnInput, ProcessSpawnTool, ProcessTable, ProcessToolConfig,
    ProcessWriteInput, ProcessWriteTool, SECRET_REQUEST_TOOL_ID, SecretMetadata, SecretRuntime,
    SecretScope, register_process_tools, register_process_tools_with, register_secret_tools,
};
use futures::StreamExt;
use pocopine_agenkit::prelude::{Agenkit, ModelRef};
use pocopine_agenkit::server::{
    AgentConfig, AgentEvent, AgentSession, AuthUser, MockProvider, Principal, SecretString,
};
use serde_json::{Value, json};

// --- harness -------------------------------------------------------------

/// An `Agenkit` whose mock model issues one `process.run` call with `args` when
/// it sees a prompt containing "run". `shell` toggles the tool's shell grant.
fn agent_calling_process_run(root: &Path, args: Value, shell: bool) -> Agenkit {
    let provider = MockProvider::new("local")
        .on_prompt_tool("run", "process.run", args)
        .default_text("done");
    Agenkit::builder()
        .provider(provider)
        .default_model(ModelRef::new("local/default"))
        .tool(ProcessRunTool::new(root).unwrap().with_shell(shell))
        .build()
        .unwrap()
}

/// Drive one prompt through a fresh session and collect every event.
async fn drive(agenkit: &Agenkit, prompt: &str) -> Vec<AgentEvent> {
    let session = AgentSession::builder(agenkit)
        .agent_id("example")
        .principal(Principal::from_user(AuthUser::new("local:example")))
        .config(
            AgentConfig::new()
                .model(ModelRef::new("local/default"))
                .system("Use process.run to run commands.")
                .tools(vec!["process.run".to_string()])
                .max_steps_per_turn(4),
        )
        .open(None)
        .await
        .unwrap();
    session.prompt(prompt).collect().await
}

fn completed(events: &[AgentEvent]) -> &Value {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCompleted { tool, output, .. } if tool == "process.run" => Some(output),
            _ => None,
        })
        .expect("expected a completed process.run call")
}

fn failed(events: &[AgentEvent]) -> &str {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolFailed { tool, error, .. } if tool == "process.run" => {
                Some(error.as_str())
            }
            _ => None,
        })
        .expect("expected a failed process.run call")
}

// --- agent-path examples -------------------------------------------------

/// Happy path: the command runs and structured stdout + exit code come back.
#[tokio::test]
async fn agent_runs_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let agenkit =
        agent_calling_process_run(dir.path(), json!({ "command": ["echo", "marker"] }), false);
    let events = drive(&agenkit, "run a command").await;

    let output = completed(&events);
    assert_eq!(output["exit_code"], json!(0));
    assert_eq!(output["timed_out"], json!(false));
    assert!(
        output["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("marker")
    );
}

/// A non-zero exit is reported as data (the tool *completed*), not as a failure —
/// the model sees `exit_code` and decides what to do.
#[tokio::test]
async fn agent_sees_nonzero_exit_as_data() {
    let dir = tempfile::tempdir().unwrap();
    let agenkit = agent_calling_process_run(dir.path(), json!({ "command": ["false"] }), false);
    let events = drive(&agenkit, "run a command").await;

    let output = completed(&events);
    assert_eq!(output["exit_code"], json!(1));
}

/// A command that overruns its timeout is killed and reported with `timed_out`.
#[tokio::test]
async fn agent_run_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let agenkit = agent_calling_process_run(
        dir.path(),
        json!({ "command": ["sleep", "5"], "timeout_ms": 150 }),
        false,
    );
    let events = drive(&agenkit, "run a command").await;

    let output = completed(&events);
    assert_eq!(output["timed_out"], json!(true));
}

/// Shell mode is denied by default; the policy error surfaces as `ToolFailed`
/// and is fed back to the model (not silently dropped).
#[tokio::test]
async fn agent_shell_mode_is_denied_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let agenkit = agent_calling_process_run(
        dir.path(),
        json!({ "command": ["echo hi"], "shell": true }),
        false, // no shell grant
    );
    let events = drive(&agenkit, "run a command").await;

    let error = failed(&events);
    assert!(error.contains("shell"), "error was: {error:?}");
}

/// With the shell grant, the same call runs through `/bin/sh -c`.
#[tokio::test]
async fn agent_shell_mode_runs_when_granted() {
    let dir = tempfile::tempdir().unwrap();
    let agenkit = agent_calling_process_run(
        dir.path(),
        json!({ "command": ["echo from-shell"], "shell": true }),
        true, // shell granted
    );
    let events = drive(&agenkit, "run a command").await;

    let output = completed(&events);
    assert_eq!(output["exit_code"], json!(0));
    assert!(
        output["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("from-shell")
    );
}

/// `register_process_tools` makes the whole family callable through the runtime.
#[tokio::test]
async fn register_process_tools_exposes_the_family() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new("local")
        .on_prompt_tool(
            "run",
            "process.run",
            json!({ "command": ["echo", "registered"] }),
        )
        .default_text("done");
    let agenkit = register_process_tools(
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default")),
        dir.path(),
    )
    .unwrap()
    .build()
    .unwrap();

    let events = drive(&agenkit, "run a command").await;
    assert!(
        completed(&events)["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("registered")
    );
}

/// `ProcessToolConfig` plumbs through `register_process_tools_with`: enabling
/// `allow_shell` lets the registered `process.run` use shell mode (which the
/// default registration denies).
#[tokio::test]
async fn config_enables_shell_mode() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new("local")
        .on_prompt_tool(
            "run",
            "process.run",
            json!({ "command": ["echo from-config-shell"], "shell": true }),
        )
        .default_text("done");
    let config = ProcessToolConfig {
        allow_shell: true,
        ..Default::default()
    };
    let agenkit = register_process_tools_with(
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default")),
        dir.path(),
        &config,
    )
    .unwrap()
    .build()
    .unwrap();

    let events = drive(&agenkit, "run a command").await;
    let output = completed(&events);
    assert_eq!(output["exit_code"], json!(0));
    assert!(
        output["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("from-config-shell")
    );
}

#[tokio::test]
async fn agent_can_request_a_secret_handle_without_value() {
    let provider = MockProvider::new("local")
        .on_prompt_tool(
            "secret",
            SECRET_REQUEST_TOOL_ID,
            json!({
                "secret_ref": "api-token",
                "purpose": "command-auth",
                "target_tool": "process.run",
                "destination": "API_TOKEN"
            }),
        )
        .default_text("done");
    let runtime = Arc::new(
        SecretRuntime::new(Arc::new(
            InMemorySecretResolver::new().insert(
                SecretMetadata::new("api-token", "API token", SecretScope::User)
                    .with_purposes(["command-auth"])
                    .with_target_tools(["process.run"])
                    .with_destinations(["API_TOKEN"]),
                SecretString::new("agent-secret-value".to_string()),
            ),
        ))
        .with_request_mode(ToolMode::Allow),
    );
    let agenkit = register_secret_tools(
        Agenkit::builder()
            .provider(provider)
            .default_model(ModelRef::new("local/default")),
        runtime,
    )
    .unwrap()
    .build()
    .unwrap();

    let session = AgentSession::builder(&agenkit)
        .agent_id("secret-example")
        .principal(Principal::from_user(AuthUser::new("local:example")))
        .config(
            AgentConfig::new()
                .model(ModelRef::new("local/default"))
                .system("Use secret.request to request secret handles.")
                .tools(vec![SECRET_REQUEST_TOOL_ID.to_string()])
                .max_steps_per_turn(4),
        )
        .open(None)
        .await
        .unwrap();
    let events: Vec<_> = session.prompt("request a secret").collect().await;

    let output = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCompleted { tool, output, .. } if tool == SECRET_REQUEST_TOOL_ID => {
                Some(output)
            }
            _ => None,
        })
        .expect("expected a completed secret.request call");
    assert_eq!(output["grant"]["secret_ref"], json!("api-token"));
    assert!(
        output["grant"]["handle_id"]
            .as_str()
            .unwrap()
            .starts_with("secret-")
    );
    assert!(!output.to_string().contains("agent-secret-value"));
}

// --- typed handle lifecycle ---------------------------------------------

/// Spawn a long-running process, drive its stdin, read what it echoed back, and
/// stop it — the typed lifecycle a host uses for REPLs / dev servers.
#[tokio::test]
async fn spawn_interact_and_kill() {
    let dir = tempfile::tempdir().unwrap();

    // One shared table backs all four handle tools; dropping it group-kills any
    // survivors, so nothing leaks past the session.
    let table = ProcessTable::new();
    let spawn = ProcessSpawnTool::new(dir.path(), table.clone()).unwrap();
    let write = ProcessWriteTool::new(table.clone());
    let read = ProcessReadTool::new(table.clone());
    let kill = ProcessKillTool::new(table.clone());
    let me = Principal::anonymous();

    // `cat` echoes its stdin to stdout — a stand-in for an interactive process.
    let handle = spawn
        .run(
            ProcessSpawnInput {
                command: vec!["cat".to_string()],
                cwd: None,
                env: None,
                secret_env: Default::default(),
                shell: None,
            },
            me.clone(),
        )
        .await
        .unwrap();
    assert!(handle.pid > 0);

    write
        .run(
            ProcessWriteInput {
                handle_id: handle.handle_id.clone(),
                data: "hello-handle".to_string(),
                newline: Some(true),
            },
            &me,
        )
        .await
        .unwrap();

    // Give the reader task a moment to drain the echoed line into the ring.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let snapshot = read
        .run(
            ProcessReadInput {
                handle_id: handle.handle_id.clone(),
            },
            &me,
        )
        .unwrap();
    assert!(snapshot.running, "cat should still be running");
    assert!(snapshot.stdout.text.contains("hello-handle"));

    let killed = kill
        .run(
            ProcessKillInput {
                handle_id: handle.handle_id.clone(),
            },
            &me,
        )
        .await
        .unwrap();
    assert!(killed.signal.is_some() || killed.exit_code.is_some());

    // The handle is gone after kill: a follow-up read is a clean not_found.
    let err = read
        .run(
            ProcessReadInput {
                handle_id: handle.handle_id,
            },
            &me,
        )
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}
