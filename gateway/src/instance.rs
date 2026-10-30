//! Instance identity, portable data, explicit creation/forking, and active leases.
//! The selected instance-local manifest is the only durable identity authority.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::GatewayConfig;
use crate::deployment::{toml_error, validate_manifest_location, DeploymentSelection};
use crate::session_scope::SessionScope;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceIdentity {
    pub schema_version: u32,
    pub instance_id: String,
    pub default_session_id: String,
}

#[derive(Debug, Clone)]
pub struct InstancePaths {
    pub root: PathBuf,
    pub soul: PathBuf,
    pub persona_identity: PathBuf,
    pub rag_db: PathBuf,
}

/// File handles retain OS locks until Gateway or the explicit operation drops
/// the lease. Lock files are synchronization resources, not identity metadata.
pub struct InstanceLease {
    _instance_lock: File,
    _data_lock: File,
    pub identity: InstanceIdentity,
    pub paths: InstancePaths,
}

impl InstanceIdentity {
    /// The runtime scope this manifest selects.
    pub fn scope(&self) -> Result<SessionScope> {
        SessionScope::parse(&self.instance_id, &self.default_session_id)
    }

    fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            instance_id: Uuid::new_v4().to_string(),
            default_session_id: Uuid::new_v4().to_string(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!("unsupported instance manifest schema version");
        }
        self.scope()?;
        Ok(())
    }
}

impl InstancePaths {
    fn from_manifest(manifest: &Path, identity: &InstanceIdentity) -> Result<Self> {
        let root = validate_manifest_location(manifest, &identity.instance_id)?;
        Ok(Self {
            soul: root.join("SOUL.md"),
            persona_identity: root.join("IDENTITY.md"),
            rag_db: root.join("kaguya.db"),
            root,
        })
    }
}

fn readable_file(path: &Path, description: &str) -> Result<()> {
    if !path.is_file() {
        bail!(
            "{description} is missing or not a regular file: {}",
            path.display()
        );
    }
    File::open(path).with_context(|| format!("{description} is unreadable: {}", path.display()))?;
    Ok(())
}

/// Read-only inspection: no leases, directories, SQLite connections, or runtime
/// files are created by validation or effective-selection diagnostics.
pub fn inspect(manifest: &Path) -> Result<(InstanceIdentity, InstancePaths)> {
    let content = fs::read_to_string(manifest).with_context(|| {
        format!(
            "instance manifest missing or unreadable: {}",
            manifest.display()
        )
    })?;
    let identity: InstanceIdentity = toml::from_str(&content)
        .map_err(|error| toml_error("instance manifest", &content, &error))?;
    identity.validate()?;
    let paths = InstancePaths::from_manifest(manifest, &identity)?;
    readable_file(&paths.soul, "instance SOUL.md")?;
    readable_file(&paths.persona_identity, "instance persona IDENTITY.md")?;
    if paths.rag_db.try_exists()? {
        readable_file(&paths.rag_db, "instance RAG database")?;
    }
    Ok((identity, paths))
}

/// Check `init-instance` preconditions without creating anything.
pub fn check_init(selection: &DeploymentSelection, config: &GatewayConfig) -> Result<()> {
    if selection.manifest.is_some() {
        bail!("deployment already selects an instance; initialize from an unselected deployment, or use fork-instance");
    }
    readable_file(&config.files.soul_path, "initial persona SOUL.md source")?;
    readable_file(
        &config.files.identity_path,
        "initial persona IDENTITY.md source",
    )?;
    Ok(())
}

/// Per-user directory for instance-ID locks. It must not follow `TMPDIR`,
/// which differs between launch contexts and would let a restored copy of
/// the same instance run twice.
fn instance_lock_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .context("cannot determine the per-user data directory for instance locks")?
        .join("kaguya")
        .join("instance-locks");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    Ok(dir)
}

fn acquire_locks(identity: InstanceIdentity, paths: InstancePaths) -> Result<InstanceLease> {
    let instance_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(instance_lock_dir()?.join(format!("{}.lock", identity.instance_id)))?;
    instance_lock
        .try_lock()
        .context("instance identity is already owned by another local Gateway process")?;
    let data_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(paths.root.join(".owner.lock"))?;
    data_lock
        .try_lock()
        .context("instance data directory is already owned by another Gateway process")?;
    Ok(InstanceLease {
        _instance_lock: instance_lock,
        _data_lock: data_lock,
        identity,
        paths,
    })
}

pub fn load_required(selection: &DeploymentSelection) -> Result<InstanceLease> {
    let (identity, paths) = inspect(selection.require_manifest()?)?;
    acquire_locks(identity, paths)
}

fn copy_db(source: &Path, target: &Path) -> Result<()> {
    if !source.try_exists()? {
        return Ok(());
    }
    let destination = target
        .to_str()
        .context("SQLite snapshot destination is not Unicode")?;
    let connection =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute("VACUUM main INTO ?1", [destination])?;
    Ok(())
}

/// Publish after preparing persona and SQLite data. Failed writes never mint a
/// replacement identity as recovery. Interruption recovery is a pre-R1c gate.
fn create_instance(
    selection: &DeploymentSelection,
    identity: InstanceIdentity,
    soul: &Path,
    persona: &Path,
    db: Option<&Path>,
) -> Result<(InstanceIdentity, PathBuf)> {
    readable_file(soul, "persona SOUL.md source")?;
    readable_file(persona, "persona IDENTITY.md source")?;
    if let Some(db) = db {
        if db.try_exists()? {
            readable_file(db, "RAG snapshot source")?;
        }
    }
    let directory = selection
        .portable_root
        .join("instances")
        .join(&identity.instance_id);
    let manifest = directory.join("instance.toml");
    let paths = InstancePaths::from_manifest(&manifest, &identity)?;
    fs::create_dir_all(
        directory
            .parent()
            .context("instance directory has no parent")?,
    )?;
    fs::create_dir(&directory)
        .context("new instance directory already exists or cannot be created")?;
    fs::copy(soul, &paths.soul)?;
    fs::copy(persona, &paths.persona_identity)?;
    if let Some(db) = db {
        copy_db(db, &paths.rag_db)?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest)?;
    use std::io::Write;
    output.write_all(toml::to_string(&identity)?.as_bytes())?;
    output.sync_all()?;
    drop(output);
    let verified = inspect(&manifest)?;
    if verified.0 != identity {
        bail!("created instance failed identity readback");
    }
    Ok((identity, manifest))
}

pub fn init_instance(
    selection: &DeploymentSelection,
    config: &GatewayConfig,
) -> Result<(InstanceIdentity, PathBuf)> {
    check_init(selection, config)?;
    create_instance(
        selection,
        InstanceIdentity::new(),
        &config.files.soul_path,
        &config.files.identity_path,
        None,
    )
}

/// Copy the selected instance's data under new instance/session IDs.
pub fn fork_instance(selection: &DeploymentSelection) -> Result<(InstanceIdentity, PathBuf)> {
    let source = load_required(selection)?;
    create_instance(
        selection,
        InstanceIdentity::new(),
        &source.paths.soul,
        &source.paths.persona_identity,
        Some(&source.paths.rag_db),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(root: &Path, manifest_reference: Option<&str>) -> DeploymentSelection {
        let manifest = manifest_reference
            .map(|value| format!("[instance]\nmanifest='{value}'\n"))
            .unwrap_or_default();
        let content = format!("gateway_settings='gateway.toml'\n[paths]\nmode='portable'\nportable_root='data'\n{manifest}");
        fs::write(root.join("runtime.toml"), content).unwrap();
        DeploymentSelection::load(root.join("runtime.toml"), None).unwrap()
    }

    #[test]
    fn invalid_instance_identifiers_are_rejected() {
        let mut identity = InstanceIdentity::new();
        identity.instance_id = "invalid".into();
        assert!(identity.validate().is_err());
        identity.instance_id = Uuid::new_v4().to_string();
        identity.default_session_id = identity.instance_id.clone();
        assert!(identity.validate().is_err());
        identity.default_session_id = Uuid::new_v4().to_string();
        identity.schema_version = 2;
        assert!(identity.validate().is_err());
    }

    #[test]
    fn local_manifest_init_fork_and_locks_preserve_data() {
        let root = std::env::temp_dir().join(format!("kaguya instance {} spaces", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("gateway.toml"), "# component fixture").unwrap();
        fs::write(root.join("soul.md"), "original soul").unwrap();
        fs::write(root.join("persona.md"), "original persona identity").unwrap();
        let mut config = GatewayConfig::default();
        config.files.soul_path = root.join("soul.md");
        config.files.identity_path = root.join("persona.md");
        let selection = deployment(&root, None);
        check_init(&selection, &config).unwrap();
        assert!(load_required(&selection).is_err());
        assert!(!selection.portable_root.exists());
        let (identity, manifest) = init_instance(&selection, &config).unwrap();
        let (loaded, paths) = inspect(&manifest).unwrap();
        assert_eq!(loaded, identity);
        assert!(
            !paths.rag_db.exists(),
            "initialization starts without old data"
        );
        let db = rusqlite::Connection::open(&paths.rag_db).unwrap();
        db.execute_batch(
            "CREATE TABLE retained (value TEXT); INSERT INTO retained VALUES ('preserved');",
        )
        .unwrap();
        drop(db);
        assert_eq!(paths.root, manifest.parent().unwrap());
        assert!(!paths.root.join("owner.toml").exists());
        assert!(!paths.root.join("instances").exists());
        assert!(!paths.root.join(".owner.lock").exists());
        assert!(!instance_lock_dir()
            .unwrap()
            .join(format!("{}.lock", identity.instance_id))
            .exists());
        let selected = deployment(
            &root,
            Some(&format!("instances/{}/instance.toml", identity.instance_id)),
        );
        let lease = load_required(&selected).unwrap();
        // Read-only validation is usable even while the running instance holds
        // leases; it checks metadata without attempting to acquire ownership.
        assert_eq!(
            inspect(selected.require_manifest().unwrap()).unwrap().0,
            identity
        );
        assert!(load_required(&selected).is_err());
        assert!(fork_instance(&selected).is_err());
        assert!(check_init(&selected, &config).is_err());
        let restored_root = root.join("restored");
        fs::create_dir(&restored_root).unwrap();
        fs::write(restored_root.join("gateway.toml"), "# fixture").unwrap();
        let restored_directory = restored_root
            .join("data/instances")
            .join(&identity.instance_id);
        fs::create_dir_all(&restored_directory).unwrap();
        fs::copy(&manifest, restored_directory.join("instance.toml")).unwrap();
        fs::copy(&paths.soul, restored_directory.join("SOUL.md")).unwrap();
        fs::copy(
            &paths.persona_identity,
            restored_directory.join("IDENTITY.md"),
        )
        .unwrap();
        let restored = deployment(
            &restored_root,
            Some(&format!("instances/{}/instance.toml", identity.instance_id)),
        );
        assert!(load_required(&restored).is_err());
        drop(lease);
        let restored_lease = load_required(&restored).unwrap();
        assert!(load_required(&selected).is_err());
        drop(restored_lease);
        let data_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.root.join(".owner.lock"))
            .unwrap();
        data_lock.try_lock().unwrap();
        assert!(load_required(&selected)
            .err()
            .unwrap()
            .to_string()
            .contains("data directory"));
        drop(data_lock);
        let mut changed = identity.clone();
        changed.default_session_id = Uuid::new_v4().to_string();
        fs::write(&manifest, toml::to_string(&changed).unwrap()).unwrap();
        assert_eq!(inspect(&manifest).unwrap().0, changed);
        let root_before = fs::read(&selected.runtime_config).unwrap();
        let (independent, new_manifest) = fork_instance(&selected).unwrap();
        assert_ne!(independent.instance_id, identity.instance_id);
        assert_ne!(independent.default_session_id, changed.default_session_id);
        assert_eq!(fs::read(&selected.runtime_config).unwrap(), root_before);
        assert_eq!(inspect(&manifest).unwrap().0, changed);
        let connection =
            rusqlite::Connection::open(new_manifest.parent().unwrap().join("kaguya.db")).unwrap();
        let retained: String = connection
            .query_row("SELECT value FROM retained", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, "preserved");
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_existing_manifest_and_wrong_directory_fail_read_only() {
        let root = std::env::temp_dir().join(format!("kaguya inspect {}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let identity = InstanceIdentity::new();
        let manifest = root.join("instance.toml");
        fs::write(&manifest, toml::to_string(&identity).unwrap()).unwrap();
        assert!(inspect(&manifest).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn instance_locks_live_in_a_per_user_directory_not_tmpdir() {
        let dir = instance_lock_dir().unwrap();
        assert!(dir.ends_with(Path::new("kaguya").join("instance-locks")));
        assert!(!dir.starts_with(std::env::temp_dir()));
    }
}
