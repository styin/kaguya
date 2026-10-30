//! Shared deployment-selection boundary for the two Rust executables.
//! Component settings and process topology remain in their typed loaders.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default)]
pub struct DeploymentOptions {
    pub runtime_config: Option<PathBuf>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Run,
    InitInstance,
    ForkInstance,
    ValidateConfig,
    ShowConfig,
    Help,
}

#[derive(Debug)]
pub struct Cli {
    pub operation: Operation,
    pub options: DeploymentOptions,
    /// `init-instance --dry-run`: check and print, create nothing.
    pub dry_run: bool,
}

impl Cli {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut operation = None;
        let mut options = DeploymentOptions::default();
        let mut dry_run = false;
        while let Some(argument) = args.next() {
            let text = argument.to_str().context("command/flag is not Unicode")?;
            match text {
                "--help" | "-h" => {
                    return Ok(Self {
                        operation: Operation::Help,
                        options,
                        dry_run,
                    })
                }
                "--runtime-config" => {
                    if options.runtime_config.is_some() {
                        bail!("--runtime-config was supplied twice");
                    }
                    let value = args.next().context("--runtime-config requires a path")?;
                    if value.is_empty() {
                        bail!("--runtime-config requires a nonempty path");
                    }
                    options.runtime_config = Some(value.into());
                }
                "--profile" => {
                    if options.profile.is_some() {
                        bail!("--profile was supplied twice");
                    }
                    let value = args.next().context("--profile requires a value")?;
                    let value = value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("profile is not Unicode"))?;
                    require_nonempty(&value, "profile")?;
                    options.profile = Some(value);
                }
                "--dry-run" => dry_run = true,
                _ if text.starts_with("--runtime-config=") => {
                    if options.runtime_config.is_some() {
                        bail!("--runtime-config was supplied twice");
                    }
                    let value = &text["--runtime-config=".len()..];
                    require_nonempty(value, "runtime config path")?;
                    options.runtime_config = Some(value.into());
                }
                _ if text.starts_with("--profile=") => {
                    if options.profile.is_some() {
                        bail!("--profile was supplied twice");
                    }
                    let value = &text["--profile=".len()..];
                    require_nonempty(value, "profile")?;
                    options.profile = Some(value.to_string());
                }
                "run" | "init-instance" | "fork-instance" | "validate-config" | "show-config" => {
                    if operation.is_some() {
                        bail!("supply only one operation");
                    }
                    operation = Some(match text {
                        "run" => Operation::Run,
                        "init-instance" => Operation::InitInstance,
                        "fork-instance" => Operation::ForkInstance,
                        "validate-config" => Operation::ValidateConfig,
                        _ => Operation::ShowConfig,
                    });
                }
                _ => bail!("unknown command or option; use --help"),
            }
        }
        let operation = operation.unwrap_or(Operation::Run);
        if dry_run && operation != Operation::InitInstance {
            bail!("--dry-run is supported by init-instance only");
        }
        Ok(Self {
            operation,
            options,
            dry_run,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentSelection {
    pub runtime_config: PathBuf,
    pub profile: Option<String>,
    pub gateway_settings: PathBuf,
    pub path_mode: String,
    pub portable_root: PathBuf,
    pub manifest: Option<PathBuf>,
    pub sources: BTreeMap<String, String>,
    pub overridden_sources: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentFile {
    profile: Option<String>,
    gateway_settings: PathBuf,
    paths: PathSettings,
    #[serde(default)]
    instance: InstanceSelection,
    // Accepted topology fields are interpreted by component loaders, not here.
    supervisor_addr: Option<toml::Value>,
    sandbox: Option<toml::Value>,
    #[serde(default)]
    processes: BTreeMap<String, toml::Value>,
    #[serde(default)]
    profiles: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSettings {
    mode: String,
    portable_root: PathBuf,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceSelection {
    manifest: Option<PathBuf>,
}

fn require_nonempty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(())
}

fn discover_root(cwd: &Path) -> Result<PathBuf> {
    for candidate in [
        "config/kaguya.runtime.toml",
        "../config/kaguya.runtime.toml",
        "kaguya.runtime.toml",
    ] {
        let path = cwd.join(candidate);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(path),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .context("cannot inspect runtime configuration discovery candidate")
            }
        }
    }
    bail!("runtime config not found; select it with --runtime-config or KAGUYA_RUNTIME_CONFIG")
}

/// Resolve declared paths without creating them. Missing data roots are valid
/// during initialization; drive-relative Windows paths are deliberately rejected.
pub fn resolve_path(base: &Path, declared: &Path) -> Result<PathBuf> {
    if declared.as_os_str().is_empty() {
        bail!("configured path must not be empty");
    }
    if !declared.is_absolute()
        && (declared.has_root()
            || declared
                .components()
                .any(|c| matches!(c, Component::Prefix(_))))
    {
        bail!("path must be absolute or relative to its documented configuration base");
    }
    let joined = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        base.join(declared)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.file_name().is_none() {
                    bail!("path escapes its filesystem root");
                }
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Resolve `instances/<InstanceId>/instance.toml` under the portable root.
fn resolve_manifest(portable_root: &Path, declared: &Path) -> Result<PathBuf> {
    if declared.is_absolute()
        || declared.has_root()
        || declared
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::ParentDir))
    {
        bail!("instance.manifest must be relative to paths.portable_root without parent traversal");
    }
    let resolved = resolve_path(portable_root, declared)?;
    let relative = resolved
        .strip_prefix(portable_root.join("instances"))
        .context("manifest must be inside the portable instances directory")?;
    if relative.components().count() != 2
        || resolved.file_name() != Some(std::ffi::OsStr::new("instance.toml"))
    {
        bail!("manifest must select instances/<InstanceId>/instance.toml");
    }
    let directory_id = resolved
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .context("instance directory name is not Unicode")?;
    match uuid::Uuid::parse_str(directory_id) {
        Ok(id) if !id.is_nil() && id.to_string() == directory_id => Ok(resolved),
        _ => bail!("instance directory name must be a non-nil canonical UUID"),
    }
}

/// Describe a TOML error by location and kind. Errors that name only a key are
/// shown; others can quote a configured value, so only their kind is shown.
pub fn toml_error(
    what: impl std::fmt::Display,
    content: &str,
    error: &toml::de::Error,
) -> anyhow::Error {
    let first_line = error.message().lines().next().unwrap_or_default();
    let reason = if ["unknown field", "missing field", "duplicate key"]
        .iter()
        .any(|prefix| first_line.starts_with(prefix))
    {
        first_line
    } else {
        "invalid value or syntax"
    };
    match error.span() {
        Some(span) => {
            let before = content.get(..span.start).unwrap_or(content);
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            anyhow::anyhow!("invalid {what}: {reason} at line {line}, column {column}")
        }
        None => anyhow::anyhow!("invalid {what}: {reason}"),
    }
}

impl DeploymentSelection {
    pub fn discover(options: DeploymentOptions) -> Result<Self> {
        Self::resolve_at(
            options,
            std::env::var_os("KAGUYA_RUNTIME_CONFIG"),
            std::env::var_os("KAGUYA_RUNTIME_PROFILE"),
            &std::env::current_dir()?,
        )
    }

    pub fn load(path: impl AsRef<Path>, profile: Option<String>) -> Result<Self> {
        Self::resolve_at(
            DeploymentOptions {
                runtime_config: Some(path.as_ref().to_path_buf()),
                profile,
            },
            None,
            None,
            &std::env::current_dir()?,
        )
    }

    fn resolve_at(
        options: DeploymentOptions,
        env_root: Option<OsString>,
        env_profile: Option<OsString>,
        cwd: &Path,
    ) -> Result<Self> {
        let root_source;
        let mut overridden_sources = BTreeMap::new();
        let root = if let Some(root) = options.runtime_config {
            root_source = "command_line";
            if env_root.is_some() {
                overridden_sources.insert(
                    "runtime_config".into(),
                    vec!["environment:KAGUYA_RUNTIME_CONFIG".into()],
                );
            }
            root
        } else if let Some(root) = env_root {
            if root.is_empty() {
                bail!("KAGUYA_RUNTIME_CONFIG must not be empty");
            }
            root_source = "environment:KAGUYA_RUNTIME_CONFIG";
            root.into()
        } else {
            root_source = "discovery";
            discover_root(cwd)?
        };
        let root = resolve_path(cwd, &root)?
            .canonicalize()
            .context("selected runtime config is missing or unreadable")?;
        let content =
            std::fs::read_to_string(&root).context("cannot read selected runtime config")?;
        let file: DeploymentFile = toml::from_str(&content).map_err(|error| {
            toml_error(
                format_args!("runtime configuration {}", root.display()),
                &content,
                &error,
            )
        })?;
        let file_profile_source = file.profile.as_ref().map(|_| "runtime_file:profile");
        let (profile, profile_source) = if let Some(profile) = options.profile {
            let mut overridden = Vec::new();
            if env_profile.is_some() {
                overridden.push("environment:KAGUYA_RUNTIME_PROFILE".into());
            }
            if file.profile.is_some() {
                overridden.push("runtime_file:profile".into());
            }
            if !overridden.is_empty() {
                overridden_sources.insert("profile".into(), overridden);
            }
            (Some(profile), Some("command_line"))
        } else if let Some(profile) = env_profile {
            let profile = profile
                .into_string()
                .map_err(|_| anyhow::anyhow!("KAGUYA_RUNTIME_PROFILE is not Unicode"))?;
            if file.profile.is_some() {
                overridden_sources.insert("profile".into(), vec!["runtime_file:profile".into()]);
            }
            (Some(profile), Some("environment:KAGUYA_RUNTIME_PROFILE"))
        } else {
            (file.profile, file_profile_source)
        };
        if let Some(profile) = &profile {
            require_nonempty(profile, "selected profile")?;
            if !file.profiles.contains_key(profile) {
                bail!("selected runtime profile is not defined");
            }
        } else if !file.profiles.is_empty() {
            bail!("runtime profile must be selected when named profiles are defined");
        }
        if file.paths.mode != "portable" {
            bail!("unsupported path mode; only explicit portable mode is implemented");
        }
        let base = root.parent().context("runtime config has no parent")?;
        let gateway_settings = resolve_path(base, &file.gateway_settings)?;
        if !gateway_settings.is_file() {
            bail!(
                "selected Gateway settings file is missing: {}",
                gateway_settings.display()
            );
        }
        let portable_root = resolve_path(base, &file.paths.portable_root)?;
        if portable_root.try_exists()? && !portable_root.is_dir() {
            bail!("portable_root exists but is not a directory");
        }
        let manifest = file
            .instance
            .manifest
            .map(|path| resolve_manifest(&portable_root, &path))
            .transpose()?;
        // Provenance lists only settings that have a value.
        let mut sources = BTreeMap::new();
        sources.insert("runtime_config".into(), root_source.into());
        if let Some(profile_source) = profile_source {
            sources.insert("profile".into(), profile_source.into());
        }
        let manifest_key = manifest.as_ref().map(|_| "instance.manifest");
        for key in ["gateway_settings", "paths.mode", "paths.portable_root"]
            .into_iter()
            .chain(manifest_key)
        {
            sources.insert(key.into(), format!("runtime_file:{key}"));
        }
        // Keep projection fields consumed explicitly; their contents are never
        // copied into the redacted diagnostics below.
        let _ = (file.supervisor_addr, file.sandbox, file.processes);
        Ok(Self {
            runtime_config: root,
            profile,
            gateway_settings,
            path_mode: "portable".into(),
            portable_root,
            manifest,
            sources,
            overridden_sources,
        })
    }

    pub fn require_manifest(&self) -> Result<&Path> {
        self.manifest.as_deref().context("no instance is selected; run init-instance and select its printed manifest path in the deployment")
    }

    /// An allowlist: process environment, shell commands, URLs and component
    /// values are intentionally absent, including overridden secret values.
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "scope": "deployment-selection",
            "selection": {
                "runtime_config": self.runtime_config,
                "profile": self.profile,
                "gateway_settings": self.gateway_settings,
                "path_mode": self.path_mode,
                "portable_root": self.portable_root,
                "manifest": self.manifest,
                "instance_directory": self.manifest.as_ref().and_then(|path| path.parent()),
            },
            "sources": self.sources,
            "overridden_sources": self.overridden_sources,
            "launch_configuration_trust": "deployment files contain trusted executable launch instructions",
            "deferred_diagnostics": ["endpoint reconciliation", "environment/default precedence", "launch descriptors"],
        })
    }
}

pub fn validate_manifest_location(manifest: &Path, instance_id: &str) -> Result<PathBuf> {
    let parent = manifest
        .parent()
        .context("instance manifest has no parent")?;
    if manifest.file_name() != Some(std::ffi::OsStr::new("instance.toml"))
        || parent.file_name() != Some(std::ffi::OsStr::new(instance_id))
    {
        bail!("instance manifest must be named instance.toml inside its declared InstanceId directory");
    }
    Ok(parent.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_selection_and_diagnostic_mode_are_explicit() {
        let cli = Cli::parse(
            [
                "init-instance",
                "--runtime-config",
                "root with spaces.toml",
                "--profile=dev",
                "--dry-run",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(cli.operation, Operation::InitInstance);
        assert_eq!(
            cli.options.runtime_config,
            Some("root with spaces.toml".into())
        );
        assert_eq!(cli.options.profile.as_deref(), Some("dev"));
        assert!(cli.dry_run);
        assert!(Cli::parse(["--profile="].map(OsString::from)).is_err());
        assert!(Cli::parse(["--dry-run"].map(OsString::from)).is_err());
        assert_eq!(
            Cli::parse(["fork-instance"].map(OsString::from))
                .unwrap()
                .operation,
            Operation::ForkInstance
        );
    }

    #[test]
    fn selection_precedence_paths_and_redaction_do_not_write() {
        let root =
            std::env::temp_dir().join(format!("kaguya selection {} spaces", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("gateway.toml"), "# settings").unwrap();
        let config = root.join("runtime.toml");
        std::fs::write(&config, "profile='app'\ngateway_settings='gateway.toml'\n[paths]\nmode='portable'\nportable_root='missing data'\n[profiles.app.processes.gateway.env]\nTOKEN='never-print-this'\n[profiles.dev.processes]\n").unwrap();
        let selected = DeploymentSelection::resolve_at(
            DeploymentOptions {
                runtime_config: Some(config.clone()),
                profile: Some("dev".into()),
            },
            Some("missing.toml".into()),
            Some("invalid".into()),
            &root,
        )
        .unwrap();
        assert_eq!(selected.profile.as_deref(), Some("dev"));
        assert_eq!(
            selected.portable_root,
            config
                .canonicalize()
                .unwrap()
                .parent()
                .unwrap()
                .join("missing data")
        );
        assert!(selected.manifest.is_none());
        assert!(!selected
            .diagnostics()
            .to_string()
            .contains("never-print-this"));
        assert!(!root.join("missing data").exists());
        assert!(DeploymentSelection::resolve_at(
            DeploymentOptions::default(),
            Some("".into()),
            None,
            &root
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn toml_errors_name_keys_and_location_but_not_values() {
        let content = "gateway_settings = 'g.toml'\nsupervisor_addrr = 'x'\n";
        let Err(error) = toml::from_str::<DeploymentFile>(content) else {
            panic!("unknown field must be rejected");
        };
        let message = toml_error("runtime configuration", content, &error).to_string();
        assert!(
            message.contains("unknown field `supervisor_addrr`"),
            "{message}"
        );
        assert!(message.contains("line 2, column 1"), "{message}");
        assert!(!message.contains("Some("), "{message}");

        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Flag {
            enabled: bool,
        }
        let content = "enabled = 'never-print-this'\n";
        let error = toml::from_str::<Flag>(content).unwrap_err();
        assert!(error.message().contains("never-print-this"));
        let message = toml_error("flag", content, &error).to_string();
        assert!(message.contains("invalid value or syntax"), "{message}");
        assert!(!message.contains("never-print-this"), "{message}");
    }
}
