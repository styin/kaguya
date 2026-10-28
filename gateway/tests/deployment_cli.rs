//! Black-box checks for deployment selection and explicit instance operations.
//! These commands must never launch the application or acquire runtime ports.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

const SECRET: &str = "deployment-test-secret-must-not-be-printed";

struct Fixture {
    root: PathBuf,
    runtime: PathBuf,
    settings: PathBuf,
    sources: PathBuf,
    portable: PathBuf,
    unrelated_cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kaguya CLI {} with spaces", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let settings = root.join("component settings/gateway.toml");
        let sources = root.join("init sources");
        let portable = root.join("portable data");
        let unrelated_cwd = root.join("unrelated working directory");
        for directory in [settings.parent().unwrap(), &sources, &unrelated_cwd] {
            fs::create_dir(directory).unwrap();
        }
        fs::create_dir(root.join("workspace")).unwrap();
        fs::write(sources.join("SOUL.md"), "source soul\n").unwrap();
        fs::write(sources.join("IDENTITY.md"), "source persona identity\n").unwrap();
        fs::write(
            &settings,
            r#"[server]
ws_addr = "127.0.0.1:0"
grpc_addr = "127.0.0.1:0"
[files]
soul_path = "../init sources/SOUL.md"
identity_path = "../init sources/IDENTITY.md"
workspace_root = "../workspace"
[history]
max_recent_turns = 10
[silence]
soft_prompt_secs = 1
follow_up_secs = 2
context_shift_secs = 3
[rag]
top_k = 4
"#,
        )
        .unwrap();
        let fixture = Self {
            runtime: root.join("selected deployment.toml"),
            root,
            settings,
            sources,
            portable,
            unrelated_cwd,
        };
        fixture.select_manifest(None);
        fixture
    }

    fn select_manifest(&self, reference: Option<&str>) {
        let instance = reference
            .map(|reference| format!("[instance]\nmanifest = {reference:?}\n"))
            .unwrap_or_default();
        fs::write(
            &self.runtime,
            format!(
                "profile = \"app\"\ngateway_settings = \"component settings/gateway.toml\"\n\
                 [paths]\nmode = \"portable\"\nportable_root = \"portable data\"\n\
                 {instance}\
                 [profiles.app.processes]\n\
                 [profiles.direct.processes]\n\
                 [profiles.app.processes.external]\nenabled = false\n\
                 command = \"echo {SECRET}\"\nenv = {{ TOKEN = \"{SECRET}\" }}\n"
            ),
        )
        .unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kaguya-gateway"));
        command
            .current_dir(&self.unrelated_cwd)
            .env_remove("KAGUYA_RUNTIME_CONFIG")
            .env_remove("KAGUYA_RUNTIME_PROFILE")
            .env_remove("KAGUYA_SUPERVISOR_URL")
            .env("RUST_LOG", "off")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn invoke(&self, operation: &str, extra: &[&str]) -> Output {
        let mut command = self.command();
        command
            .arg(operation)
            .arg("--runtime-config")
            .arg(&self.runtime)
            .args(extra);
        run(command)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The canonical, UUID-named fixture directory is the complete cleanup target.
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        assert!(self.root.starts_with(temporary));
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("kaguya CLI "));
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn run(mut command: Command) -> Output {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI command did not terminate: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON stdout ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn collect(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                entries.insert(relative, None);
                collect(root, &path, entries);
            } else {
                entries.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut entries = BTreeMap::new();
    collect(root, root, &mut entries);
    entries
}

fn json_path(value: &Value) -> PathBuf {
    PathBuf::from(value.as_str().expect("absolute JSON path"))
}

fn assert_instance_data(directory: &Path) {
    assert_eq!(
        fs::read_to_string(directory.join("SOUL.md")).unwrap(),
        "source soul\n"
    );
    assert_eq!(
        fs::read_to_string(directory.join("IDENTITY.md")).unwrap(),
        "source persona identity\n"
    );
    let database = rusqlite::Connection::open_with_flags(
        directory.join("kaguya.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let value: String = database
        .query_row("SELECT value FROM retained", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "kept");
    assert!(!directory.join("owner.toml").exists());
    assert!(!directory.join("instances").exists());
}

#[test]
fn diagnostics_are_read_only_and_explicit_selection_wins_over_environment() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.root);
    for operation in ["show-config", "validate-config"] {
        let mut command = fixture.command();
        command
            .arg("--runtime-config")
            .arg(&fixture.runtime)
            .args(["--profile", "app", operation, "--for-init"])
            .env(
                "KAGUYA_RUNTIME_CONFIG",
                fixture.root.join("must not be selected.toml"),
            )
            .env("KAGUYA_RUNTIME_PROFILE", "must-not-be-selected");
        let output = successful_json(run(command));
        assert_eq!(output["scope"], "deployment-selection");
        assert_eq!(output["validation_mode"], "initialization");
        assert_eq!(output["selection"]["profile"], "app");
        assert_eq!(
            json_path(&output["selection"]["runtime_config"]),
            fixture.runtime
        );
        assert_eq!(
            json_path(&output["selection"]["gateway_settings"]),
            fixture.settings
        );
        assert_eq!(
            json_path(&output["selection"]["portable_root"]),
            fixture.portable
        );
        assert_eq!(output["selection"]["path_mode"], "portable");
        assert!(output["selection"]["manifest"].is_null());
        assert!(output["instance"].is_null());
        assert_eq!(output["sources"]["runtime_config"], "command_line");
        assert_eq!(output["sources"]["profile"], "command_line");
        assert_eq!(
            json_path(&output["initialization_sources"]["soul_path"]),
            fixture.sources.join("SOUL.md")
        );
        assert_eq!(
            json_path(&output["initialization_sources"]["persona_identity_path"]),
            fixture.sources.join("IDENTITY.md")
        );
        assert_eq!(
            snapshot(&fixture.root),
            before,
            "{operation} must not write files or locks"
        );

        let missing = fixture.invoke(operation, &[]);
        assert!(
            !missing.status.success(),
            "startup validation must require a selected instance"
        );
        assert_eq!(
            snapshot(&fixture.root),
            before,
            "failed diagnostics must also be read-only"
        );
    }
    assert!(!fixture.portable.exists());
}

#[test]
fn init_and_rekey_publish_handoffs_without_rewriting_the_deployment_or_sources() {
    let fixture = Fixture::new();
    let source_before = snapshot(&fixture.sources);
    let root_before = fs::read(&fixture.runtime).unwrap();
    let initialized = successful_json(fixture.invoke("init-instance", &["--profile", "direct"]));
    assert_eq!(initialized["operation"], "init-instance");
    assert_eq!(initialized["status"], "created");
    assert_eq!(initialized["selection_changed"], false);
    let instance_id = initialized["instance_id"].as_str().unwrap();
    let session_id = initialized["default_session_id"].as_str().unwrap();
    assert_ne!(instance_id, session_id);
    let directory = fixture.portable.join("instances").join(instance_id);
    let manifest = directory.join("instance.toml");
    assert_eq!(json_path(&initialized["manifest"]), manifest);
    assert_eq!(json_path(&initialized["instance_directory"]), directory);
    assert_eq!(
        initialized["manifest_reference"],
        format!("instances/{instance_id}/instance.toml")
    );
    assert!(!directory.join("kaguya.db").exists());
    let database = rusqlite::Connection::open(directory.join("kaguya.db")).unwrap();
    database
        .execute_batch("CREATE TABLE retained(value TEXT); INSERT INTO retained VALUES ('kept');")
        .unwrap();
    drop(database);
    assert_instance_data(&directory);
    assert_eq!(snapshot(&fixture.sources), source_before);
    assert_eq!(fs::read(&fixture.runtime).unwrap(), root_before);

    fixture.select_manifest(initialized["manifest_reference"].as_str());
    let root_selected = fs::read(&fixture.runtime).unwrap();
    let data_before = snapshot(&directory);
    for (operation, profile) in [("show-config", "app"), ("validate-config", "direct")] {
        let output = successful_json(fixture.invoke(operation, &["--profile", profile]));
        assert_eq!(output["validation_mode"], "startup");
        assert_eq!(output["selection"]["profile"], profile);
        assert_eq!(json_path(&output["selection"]["manifest"]), manifest);
        assert_eq!(
            json_path(&output["selection"]["instance_directory"]),
            directory
        );
        assert_eq!(output["instance"]["instance_id"], instance_id);
        assert_eq!(output["instance"]["default_session_id"], session_id);
        assert_eq!(
            snapshot(&directory),
            data_before,
            "diagnostics must not acquire data locks"
        );
    }
    let refusal = fixture.invoke("init-instance", &[]);
    assert!(
        !refusal.status.success(),
        "init must refuse an already selected instance"
    );
    assert_eq!(snapshot(&directory), data_before);

    let rekeyed = successful_json(fixture.invoke("rekey-instance", &["--profile", "direct"]));
    assert_eq!(rekeyed["operation"], "rekey-instance");
    assert_eq!(rekeyed["status"], "created");
    assert_eq!(rekeyed["selection_changed"], false);
    assert_ne!(rekeyed["instance_id"], initialized["instance_id"]);
    assert_ne!(
        rekeyed["default_session_id"],
        initialized["default_session_id"]
    );
    let rekeyed_directory = json_path(&rekeyed["instance_directory"]);
    assert_ne!(rekeyed_directory, directory);
    assert_eq!(
        json_path(&rekeyed["manifest"]),
        rekeyed_directory.join("instance.toml")
    );
    assert_instance_data(&rekeyed_directory);
    assert_instance_data(&directory);
    for filename in ["instance.toml", "SOUL.md", "IDENTITY.md", "kaguya.db"] {
        assert_eq!(
            fs::read(directory.join(filename)).unwrap(),
            data_before[Path::new(filename)]
                .as_ref()
                .unwrap()
                .as_slice()
        );
    }
    assert_eq!(snapshot(&fixture.sources), source_before);
    assert_eq!(fs::read(&fixture.runtime).unwrap(), root_selected);
    let still_selected = successful_json(fixture.invoke("show-config", &[]));
    assert_eq!(still_selected["instance"]["instance_id"], instance_id);
}

#[test]
fn invalid_existing_manifests_fail_even_for_initialization_diagnostics_without_writes() {
    let fixture = Fixture::new();
    let directory_id = Uuid::new_v4().to_string();
    let directory = fixture.portable.join("instances").join(&directory_id);
    fixture.select_manifest(Some(&format!("instances/{directory_id}/instance.toml")));
    let missing_before = snapshot(&fixture.root);
    for operation in ["show-config", "validate-config"] {
        let output = successful_json(fixture.invoke(operation, &["--for-init"]));
        assert!(output["instance"].is_null());
        assert_eq!(
            json_path(&output["selection"]["manifest"]),
            directory.join("instance.toml")
        );
        assert!(!fixture.invoke(operation, &[]).status.success());
        assert_eq!(snapshot(&fixture.root), missing_before);
    }
    assert!(!fixture.invoke("init-instance", &[]).status.success());
    assert_eq!(snapshot(&fixture.root), missing_before);
    fs::create_dir_all(&directory).unwrap();
    for (schema, declared_id, session_id) in [
        (1, Uuid::new_v4().to_string(), Uuid::new_v4().to_string()),
        (2, directory_id.clone(), Uuid::new_v4().to_string()),
        (1, directory_id.clone(), "invalid-session".into()),
    ] {
        fs::write(
            directory.join("instance.toml"),
            format!("schema_version = {schema}\ninstance_id = {declared_id:?}\ndefault_session_id = {session_id:?}\n"),
        )
        .unwrap();
        let before = snapshot(&fixture.root);
        for operation in ["show-config", "validate-config"] {
            let result = fixture.invoke(operation, &["--for-init"]);
            assert!(
                !result.status.success(),
                "{operation} must reject an invalid existing manifest"
            );
            assert_eq!(snapshot(&fixture.root), before);
        }
    }
}

#[test]
fn failed_endpoint_diagnostics_redact_credentials_and_query_values_without_writes() {
    let fixture = Fixture::new();
    let root_configuration = fs::read_to_string(&fixture.runtime).unwrap();
    for (bind, endpoint) in [
        (
            format!("http://user:{SECRET}@localhost/path?token={SECRET}"),
            "http://127.0.0.1:50053".into(),
        ),
        (
            "0.0.0.0:50053".into(),
            format!("http://user:{SECRET}@localhost/path?token={SECRET}"),
        ),
        (
            "0.0.0.0:50053".into(),
            format!("http://user:{SECRET}@127.0.0.1:50054/path?token={SECRET}"),
        ),
    ] {
        fs::write(
            &fixture.runtime,
            format!(
                "{root_configuration}\n[profiles.app.processes.voice_stack.bind]\n\
                 talker_grpc = {bind:?}\n\
                 [profiles.app.processes.voice_stack.endpoints]\n\
                 talker_grpc = {endpoint:?}\n"
            ),
        )
        .unwrap();
        let before = snapshot(&fixture.root);
        for operation in ["show-config", "validate-config"] {
            let output = fixture.invoke(operation, &["--for-init"]);
            assert!(!output.status.success());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stdout.contains(SECRET),
                "{operation} exposed a configured secret"
            );
            assert!(
                !stderr.contains(SECRET),
                "{operation} exposed a configured secret"
            );
            assert!(
                stderr.contains("voice_stack"),
                "expected topology failure: {stderr}"
            );
            assert_eq!(snapshot(&fixture.root), before);
        }
    }
}

#[test]
fn initialization_diagnostics_reject_removed_or_invalid_settings() {
    let fixture = Fixture::new();
    let settings = fs::read_to_string(&fixture.settings).unwrap();
    for extra in [
        "db_path = 'old.db'\n",
        "rrf_k = 0\n",
        "rrf_k = nan\n",
        "[runtime]\nprofile = 'ignored'\n",
    ] {
        fs::write(&fixture.settings, format!("{settings}{extra}")).unwrap();
        let before = snapshot(&fixture.root);
        let output = fixture.invoke("validate-config", &["--for-init"]);
        assert!(!output.status.success(), "must reject {extra}");
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn initialization_diagnostics_reject_an_existing_file_as_portable_root_without_writes() {
    let fixture = Fixture::new();
    fs::write(&fixture.portable, "existing file must be retained\n").unwrap();
    let before = snapshot(&fixture.root);
    for operation in ["show-config", "validate-config"] {
        let output = fixture.invoke(operation, &["--for-init"]);
        assert!(
            !output.status.success(),
            "{operation} must reject a file as its data root"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("portable"));
        assert_eq!(snapshot(&fixture.root), before);
    }
}
