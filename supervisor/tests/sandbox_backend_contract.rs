//! Shared sandbox backend contract tests.
//!
//! These tests exercise enabled backends through the public Supervisor-owned
//! `SandboxManager` API. Environment-specific backends are gated so a Windows
//! laptop without Docker or a Linux runner without bwrap does not fail the
//! suite just because the backend is unavailable.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use kaguya_supervisor::config::{SandboxBackendKind, SandboxConfig, SandboxModeKind};
use kaguya_supervisor::sandbox::SandboxManager;

fn command_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn docker_cli() -> Option<PathBuf> {
    if command_ok("docker", &["--version"]) {
        return Some(PathBuf::from("docker"));
    }
    #[cfg(windows)]
    {
        let default = PathBuf::from(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe");
        if default.exists() {
            return Some(default);
        }
    }
    None
}

async fn docker_ok(args: &[&str]) -> bool {
    let Some(cli) = docker_cli() else {
        return false;
    };
    let mut child = match tokio::process::Command::new(cli)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}

fn docker_image_exists(image: &str) -> bool {
    let Some(cli) = docker_cli() else {
        return false;
    };
    Command::new(cli)
        .args(["image", "inspect", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn base_config(backend: SandboxBackendKind) -> SandboxConfig {
    SandboxConfig {
        backend,
        mode: SandboxModeKind::SingleUser,
        default_timeout_secs: 1,
        max_output_bytes: 32,
        pool_size: 0,
        ..SandboxConfig::default()
    }
}

async fn run_contract(name: &str, cfg: SandboxConfig) {
    let mgr = SandboxManager::from_config(&cfg, std::env::temp_dir())
        .unwrap_or_else(|error| panic!("{name} manager should initialize: {error}"));

    stdin_delivery(&mgr, name).await;
    output_truncation(&mgr, name).await;
    timeout_returns_promptly(&mgr, name).await;
    session_filesystem_contract(&mgr, name).await;

    mgr.shutdown().await;
}

async fn stdin_delivery(mgr: &SandboxManager, name: &str) {
    let session = format!("{name}-contract-stdin");
    let output = mgr
        .exec_from_json(
            &session,
            r#"{"language":"python","code":"import sys; print(sys.stdin.read().strip().upper())","stdin":"hello backend"}"#,
        )
        .await;
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["exit_code"], 0, "{name} stdin failed: {output}");
    assert!(
        value["stdout"].as_str().unwrap().contains("HELLO BACKEND"),
        "{name} stdin was not delivered: {output}"
    );
    mgr.cleanup(&session).await;
}

async fn output_truncation(mgr: &SandboxManager, name: &str) {
    let session = format!("{name}-contract-truncation");
    let output = mgr
        .exec_from_json(
            &session,
            r#"{"language":"python","code":"print('x' * 4096)"}"#,
        )
        .await;
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["exit_code"], 0, "{name} truncation failed: {output}");
    assert_eq!(
        value["truncated"], true,
        "{name} did not report truncated output: {output}"
    );
    assert!(
        value["stdout"].as_str().unwrap().len() <= 32,
        "{name} retained more than max_output_bytes: {output}"
    );
    mgr.cleanup(&session).await;
}

async fn timeout_returns_promptly(mgr: &SandboxManager, name: &str) {
    let session = format!("{name}-contract-timeout");
    let output = tokio::time::timeout(
        Duration::from_secs(7),
        mgr.exec_from_json(
            &session,
            r#"{"language":"python","code":"import time; time.sleep(30)"}"#,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("{name} timeout cleanup hung"));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        value["timed_out"], true,
        "{name} did not report timeout: {output}"
    );
    mgr.cleanup(&session).await;
}

async fn session_filesystem_contract(mgr: &SandboxManager, name: &str) {
    let session_a = format!("{name}-contract-session-a");
    let session_b = format!("{name}-contract-session-b");

    let write = mgr
        .exec_from_json(
            &session_a,
            r#"{"language":"python","code":"open('note.txt','w').write('session-a')"}"#,
        )
        .await;
    let write_value: serde_json::Value = serde_json::from_str(&write).unwrap();
    assert_eq!(write_value["exit_code"], 0, "{name} write failed: {write}");

    let read_same = mgr
        .exec_from_json(
            &session_a,
            r#"{"language":"python","code":"print(open('note.txt').read())"}"#,
        )
        .await;
    let read_same_value: serde_json::Value = serde_json::from_str(&read_same).unwrap();
    assert!(
        read_same_value["stdout"]
            .as_str()
            .unwrap()
            .contains("session-a"),
        "{name} did not persist files within a session: {read_same}"
    );

    let read_other = mgr
        .exec_from_json(
            &session_b,
            r#"{"language":"python","code":"import pathlib; print(pathlib.Path('note.txt').exists())"}"#,
        )
        .await;
    let read_other_value: serde_json::Value = serde_json::from_str(&read_other).unwrap();
    assert!(
        read_other_value["stdout"]
            .as_str()
            .unwrap()
            .contains("False"),
        "{name} leaked files across sessions: {read_other}"
    );

    mgr.cleanup(&session_a).await;

    let read_after_cleanup = mgr
        .exec_from_json(
            &session_a,
            r#"{"language":"python","code":"import pathlib; print(pathlib.Path('note.txt').exists())"}"#,
        )
        .await;
    let read_after_cleanup_value: serde_json::Value =
        serde_json::from_str(&read_after_cleanup).unwrap();
    assert!(
        read_after_cleanup_value["stdout"]
            .as_str()
            .unwrap()
            .contains("False"),
        "{name} cleanup/reacquisition reused dirty filesystem: {read_after_cleanup}"
    );

    mgr.cleanup(&session_a).await;
    mgr.cleanup(&session_b).await;
}

#[tokio::test]
async fn native_backend_contract() {
    run_contract("native", base_config(SandboxBackendKind::Native)).await;
}

#[tokio::test]
async fn docker_backend_contract_when_available() {
    let image = SandboxConfig::default().image;
    if !docker_ok(&["version"]).await {
        assert!(
            std::env::var_os("KAGUYA_REQUIRE_DOCKER").is_none(),
            "Docker backend contract is required, but the Docker CLI or daemon is unavailable"
        );
        eprintln!("skipping docker backend contract: docker CLI/daemon unavailable");
        return;
    }
    if !docker_image_exists(&image) {
        assert!(
            std::env::var_os("KAGUYA_REQUIRE_DOCKER").is_none(),
            "Docker backend contract is required, but image {image} is unavailable"
        );
        eprintln!("skipping docker backend contract: image {image} not found");
        return;
    }

    run_contract("docker", base_config(SandboxBackendKind::Docker)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn bubblewrap_backend_contract_when_available() {
    if !command_ok("bwrap", &["--version"]) {
        assert!(
            std::env::var_os("KAGUYA_REQUIRE_BUBBLEWRAP").is_none(),
            "Bubblewrap backend contract is required, but bwrap is unavailable"
        );
        eprintln!("skipping bubblewrap backend contract: bwrap unavailable");
        return;
    }

    run_contract("bubblewrap", base_config(SandboxBackendKind::Bubblewrap)).await;
}

#[cfg(all(windows, feature = "sandbox-jobobject"))]
#[tokio::test]
async fn job_object_backend_contract_when_available() {
    run_contract("job_object", base_config(SandboxBackendKind::JobObject)).await;
}
