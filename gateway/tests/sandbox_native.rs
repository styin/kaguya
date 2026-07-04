//! End-to-end smoke of the native `sandbox_exec` path: construct a manager from
//! the default config and actually run code through it. Guards the port of the
//! pluggable sandbox (REF-014) beyond mere compilation.

use kaguya_gateway::config::SandboxConfig;
use kaguya_gateway::sandbox::SandboxManager;

#[tokio::test]
async fn native_backend_executes_python_and_persists_files() {
    let cfg = SandboxConfig::default(); // enabled=true, backend=native
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir())
        .expect("native manager should init");

    // The tool is advertised when enabled.
    assert!(mgr.tool_definition().is_some());

    let session = "test-conv-native";

    // 1) Run a computation and read stdout back.
    let out = mgr
        .exec_from_json(session, r#"{"language":"python","code":"print(6*7)"}"#)
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exit_code"], 0, "python exec should succeed: {out}");
    assert!(
        v["stdout"].as_str().unwrap().contains("42"),
        "expected 42 in stdout, got: {out}"
    );
    assert!(v["error"].is_null(), "no backend error expected: {out}");

    // 2) Per-conversation affinity: a file written in one call is visible in the
    //    next call for the same session (shared scratch dir).
    let write = mgr
        .exec_from_json(
            session,
            r#"{"language":"python","code":"open('note.txt','w').write('hi')"}"#,
        )
        .await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&write).unwrap()["exit_code"],
        0
    );
    let read = mgr
        .exec_from_json(
            session,
            r#"{"language":"python","code":"print(open('note.txt').read())"}"#,
        )
        .await;
    let rv: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert!(
        rv["stdout"].as_str().unwrap().contains("hi"),
        "file should persist across calls in a session, got: {read}"
    );

    mgr.cleanup(session).await;
}

#[tokio::test]
async fn disabled_manager_hides_tool_and_refuses_exec() {
    let mgr = SandboxManager::disabled();
    assert!(
        mgr.tool_definition().is_none(),
        "disabled ⇒ tool not advertised"
    );

    let out = mgr
        .exec_from_json("s", r#"{"language":"python","code":"print(1)"}"#)
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        v["error"].as_str().unwrap_or_default().contains("disabled"),
        "disabled manager should refuse: {out}"
    );
}

#[tokio::test]
async fn concurrent_execs_in_one_session_do_not_clobber() {
    // Regression for the shared-runner-script race: two overlapping calls in the
    // same session must both run their own code (unique per-exec script names).
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir()).unwrap();
    let session = "test-conv-concurrent";

    let a = mgr.exec_from_json(session, r#"{"language":"python","code":"print('AAA')"}"#);
    let b = mgr.exec_from_json(session, r#"{"language":"python","code":"print('BBB')"}"#);
    let (ra, rb) = tokio::join!(a, b);
    let va: serde_json::Value = serde_json::from_str(&ra).unwrap();
    let vb: serde_json::Value = serde_json::from_str(&rb).unwrap();
    assert_eq!(va["exit_code"], 0, "{ra}");
    assert_eq!(vb["exit_code"], 0, "{rb}");
    assert!(va["stdout"].as_str().unwrap().contains("AAA"), "{ra}");
    assert!(vb["stdout"].as_str().unwrap().contains("BBB"), "{rb}");

    mgr.cleanup(session).await;
}

#[tokio::test]
async fn stdin_is_fed_to_the_program() {
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir()).unwrap();
    let out = mgr
        .exec_from_json(
            "test-conv-stdin",
            r#"{"language":"python","code":"import sys; print(sys.stdin.read().strip().upper())","stdin":"hello"}"#,
        )
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exit_code"], 0, "{out}");
    assert!(v["stdout"].as_str().unwrap().contains("HELLO"), "{out}");
    mgr.cleanup("test-conv-stdin").await;
}

#[tokio::test]
async fn rejects_unknown_language() {
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir()).unwrap();
    let out = mgr
        .exec_from_json("s", r#"{"language":"ruby","code":"puts 1"}"#)
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("unsupported language"));
}
