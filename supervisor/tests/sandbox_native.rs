//! End-to-end smoke of the native `sandbox_exec` path: construct a manager from
//! the default config and actually run code through it. Guards the port of the
//! Supervisor-owned sandbox provider (REF-015) beyond mere compilation.

use kaguya_supervisor::config::SandboxConfig;
use kaguya_supervisor::sandbox::{SandboxManager, SandboxProvider};

#[tokio::test]
async fn native_backend_executes_python_and_persists_files() {
    let cfg = SandboxConfig::default(); // enabled=true, backend=native
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir())
        .expect("native manager should init");

    // The tool is advertised when enabled.
    assert!(mgr.is_enabled());

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
async fn disabled_manager_refuses_exec() {
    let mgr = SandboxManager::disabled();
    assert!(!mgr.is_enabled(), "disabled ⇒ tool not advertised");

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

#[tokio::test]
async fn opaque_handle_controls_execution_and_release() {
    let cfg = SandboxConfig::default();
    let provider =
        SandboxProvider::new(SandboxManager::from_config(&cfg, std::env::temp_dir()).unwrap());
    let handle = provider.acquire("handle-session").await.unwrap();
    assert_eq!(
        provider.acquire("handle-session").await.unwrap(),
        handle,
        "acquisition retries must return the existing opaque handle"
    );

    let output = provider
        .execute(
            &handle,
            r#"{"language":"python","code":"print('through-supervisor')"}"#,
        )
        .await;
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["exit_code"], 0, "{output}");
    assert!(value["stdout"]
        .as_str()
        .unwrap()
        .contains("through-supervisor"));

    provider.release(&handle).await.unwrap();
    let after_release = provider
        .execute(&handle, r#"{"language":"python","code":"print('no')"}"#)
        .await;
    let value: serde_json::Value = serde_json::from_str(&after_release).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("unknown or released"));
}
