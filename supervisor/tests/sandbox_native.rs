//! End-to-end smoke of the native `sandbox_exec` path: construct a manager from
//! the default config and actually run code through it. Guards the port of the
//! Supervisor-owned sandbox provider beyond mere compilation.

use kaguya_supervisor::config::SandboxConfig;
use kaguya_supervisor::sandbox::{SandboxManager, SandboxProvider};

#[tokio::test]
async fn native_backend_executes_python_and_persists_files() {
    let cfg = SandboxConfig::default(); // enabled=true, backend=native
    let mgr = SandboxManager::from_config(&cfg).expect("native manager should init");

    // The tool is advertised when enabled.
    assert!(mgr.is_enabled());

    let allocation = "test-allocation-native";

    // 1) Run a computation and read stdout back.
    let out = mgr
        .exec_from_json(allocation, r#"{"language":"python","code":"print(6*7)"}"#)
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exit_code"], 0, "python exec should succeed: {out}");
    assert!(
        v["stdout"].as_str().unwrap().contains("42"),
        "expected 42 in stdout, got: {out}"
    );
    assert!(v["error"].is_null(), "no backend error expected: {out}");

    // 2) Per-allocation affinity: a file written in one call is visible in the
    //    next call for the same allocation (shared scratch dir).
    let write = mgr
        .exec_from_json(
            allocation,
            r#"{"language":"python","code":"open('note.txt','w').write('hi')"}"#,
        )
        .await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&write).unwrap()["exit_code"],
        0
    );
    let read = mgr
        .exec_from_json(
            allocation,
            r#"{"language":"python","code":"print(open('note.txt').read())"}"#,
        )
        .await;
    let rv: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert!(
        rv["stdout"].as_str().unwrap().contains("hi"),
        "file should persist across calls in a allocation, got: {read}"
    );

    mgr.cleanup(allocation).await;
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
async fn concurrent_execs_in_one_allocation_do_not_clobber() {
    // Regression for the shared-runner-script race: two overlapping calls in the
    // same allocation must both run their own code (unique per-exec script names).
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg).unwrap();
    let allocation = "test-allocation-concurrent";

    let a = mgr.exec_from_json(allocation, r#"{"language":"python","code":"print('AAA')"}"#);
    let b = mgr.exec_from_json(allocation, r#"{"language":"python","code":"print('BBB')"}"#);
    let (ra, rb) = tokio::join!(a, b);
    let va: serde_json::Value = serde_json::from_str(&ra).unwrap();
    let vb: serde_json::Value = serde_json::from_str(&rb).unwrap();
    assert_eq!(va["exit_code"], 0, "{ra}");
    assert_eq!(vb["exit_code"], 0, "{rb}");
    assert!(va["stdout"].as_str().unwrap().contains("AAA"), "{ra}");
    assert!(vb["stdout"].as_str().unwrap().contains("BBB"), "{rb}");

    mgr.cleanup(allocation).await;
}

#[tokio::test]
async fn stdin_is_fed_to_the_program() {
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg).unwrap();
    let out = mgr
        .exec_from_json(
            "test-allocation-stdin",
            r#"{"language":"python","code":"import sys; print(sys.stdin.read().strip().upper())","stdin":"hello"}"#,
        )
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exit_code"], 0, "{out}");
    assert!(v["stdout"].as_str().unwrap().contains("HELLO"), "{out}");
    mgr.cleanup("test-allocation-stdin").await;
}

#[cfg(unix)]
#[tokio::test]
async fn native_timeout_kills_spawned_descendant_and_returns_promptly() {
    let mut cfg = SandboxConfig::default();
    cfg.default_timeout_secs = 1;
    let mgr = SandboxManager::from_config(&cfg).unwrap();
    let allocation = "test-allocation-timeout-tree";
    let allocation_dir = std::env::temp_dir().join("kaguya-sandbox").join(allocation);

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        mgr.exec_from_json(
            allocation,
            r#"{"language":"bash","code":"sleep 30 & echo $! > child.pid; wait"}"#,
        ),
    )
    .await
    .expect("timeout cleanup should not hang on inherited pipes");
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["timed_out"], true, "{output}");

    let child_pid = tokio::fs::read_to_string(allocation_dir.join("child.pid"))
        .await
        .expect("test script should record descendant pid")
        .trim()
        .to_string();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if !process_is_running(&child_pid) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "descendant process {child_pid} survived native timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    mgr.cleanup(allocation).await;
}

#[cfg(unix)]
#[tokio::test]
async fn native_reader_cleanup_is_bounded_when_descendant_keeps_pipe_open() {
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg).unwrap();
    let allocation = "test-allocation-reader-pipe";
    let allocation_dir = std::env::temp_dir().join("kaguya-sandbox").join(allocation);

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        mgr.exec_from_json(
            allocation,
            r#"{"language":"bash","code":"python3 - <<'PY'\nimport os\nimport signal\nimport sys\nimport time\n\npid = os.fork()\nif pid == 0:\n    signal.signal(signal.SIGHUP, signal.SIG_IGN)\n    sys.stdout.flush()\n    time.sleep(30)\nelse:\n    with open('child.pid', 'w') as f:\n        f.write(str(pid))\n    sys.exit(0)\nPY"}"#,
        ),
    )
    .await
    .expect("reader cleanup should not hang on inherited pipes");
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["exit_code"], 0, "{output}");
    assert_eq!(
        value["truncated"], true,
        "aborted reader cleanup should mark output truncated: {output}"
    );

    if let Ok(child_pid) = tokio::fs::read_to_string(allocation_dir.join("child.pid")).await {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", child_pid.trim()])
            .status();
    }
    mgr.cleanup(allocation).await;
}

#[tokio::test]
async fn rejects_unknown_language() {
    let cfg = SandboxConfig::default();
    let mgr = SandboxManager::from_config(&cfg).unwrap();
    let out = mgr
        .exec_from_json("s", r#"{"language":"ruby","code":"puts 1"}"#)
        .await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("unsupported language"));
}

#[test]
fn rejects_invalid_language_allowlist() {
    for languages in [
        vec!["pythno".into()],
        vec!["python".into(), "pythno".into()],
    ] {
        let cfg = SandboxConfig {
            allowed_languages: languages,
            ..SandboxConfig::default()
        };
        assert!(SandboxManager::from_config(&cfg).is_err());
    }
}

#[test]
fn disabled_configuration_does_not_initialize_selected_backend() {
    let cfg = SandboxConfig {
        enabled: false,
        backend: kaguya_supervisor::config::SandboxBackendKind::Docker,
        image: "unused-disabled-backend".into(),
        ..SandboxConfig::default()
    };
    let manager = SandboxManager::from_config(&cfg).unwrap();
    assert!(!manager.is_enabled());
}

#[tokio::test]
async fn malformed_arguments_do_not_execute() {
    let manager = SandboxManager::from_config(&SandboxConfig::default()).unwrap();
    for args in [
        r#"{"code":"print('must not run')"}"#,
        r#"{"language":false,"code":"print('must not run')"}"#,
        r#"{"language":"python","code":"print('must not run')","stdin":42}"#,
        r#"{"language":"python","code":"print('must not run')","stale_argument":true}"#,
        r#"{"language":"py","code":"print('must not run')"}"#,
    ] {
        let output = manager.exec_from_json("invalid-arguments", args).await;
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(result["error"].is_string(), "{output}");
        assert_eq!(result["stdout"], "");
    }
}

#[tokio::test]
async fn opaque_handle_controls_execution_and_release() {
    let cfg = SandboxConfig::default();
    let provider = SandboxProvider::new(SandboxManager::from_config(&cfg).unwrap());
    let allocation_id = uuid::Uuid::new_v4().to_string();
    let handle = provider.acquire(&allocation_id).await.unwrap();
    assert_eq!(
        provider.acquire(&allocation_id).await.unwrap(),
        handle,
        "acquisition retries must return the existing opaque handle"
    );
    assert!(provider.acquire("not-a-uuid").await.is_err());
    let output = provider
        .execute(
            &handle,
            "request-one",
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
        .execute(
            &handle,
            "request-two",
            r#"{"language":"python","code":"print('no')"}"#,
        )
        .await;
    let value: serde_json::Value = serde_json::from_str(&after_release).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("unknown or released"));
}

#[cfg(unix)]
fn process_is_running(pid: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if stat
                .split_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                == Some("Z")
            {
                return false;
            }
        }
    }

    std::process::Command::new("kill")
        .args(["-0", pid])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
