//! Instance identity, portable data, explicit creation/rekey, and active leases.
//! The selected instance-local manifest is the only durable identity authority.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::GatewayConfig;
use crate::deployment::{validate_manifest_location, DeploymentSelection};
use crate::proto::{self, SessionRef};

const SCHEMA_VERSION: u32 = 1;

/// Validate generated request/runtime scope and borrow its scalar boundary IDs.
pub fn validate_session_ref(session: Option<&SessionRef>) -> Result<(&str, &str)> {
    let session = session.context("session reference is required")?;
    let instance = session
        .instance_id
        .as_ref()
        .context("instance_id is required")?
        .value
        .as_str();
    let selected = session
        .session_id
        .as_ref()
        .context("session_id is required")?
        .value
        .as_str();
    for (field, value) in [("instance_id", instance), ("session_id", selected)] {
        let id = Uuid::parse_str(value)
            .map_err(|_| anyhow::anyhow!("{field} must be a canonical non-nil UUID"))?;
        if id.is_nil() || id.to_string() != value {
            bail!("{field} must be a canonical non-nil UUID");
        }
    }
    if instance == selected {
        bail!("instance_id and session_id must differ");
    }
    Ok((instance, selected))
}

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
    /// Project the persisted manifest onto the validated runtime association.
    pub fn session_ref(&self) -> Result<SessionRef> {
        let session = SessionRef {
            instance_id: Some(proto::InstanceId {
                value: self.instance_id.clone(),
            }),
            session_id: Some(proto::SessionId {
                value: self.default_session_id.clone(),
            }),
        };
        validate_session_ref(Some(&session))?;
        Ok(session)
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
        self.session_ref()?;
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
    let identity: InstanceIdentity =
        toml::from_str(&content).map_err(|error: toml::de::Error| {
            anyhow::anyhow!(
                "invalid instance manifest at byte {:?}",
                error.span().map(|span| span.start)
            )
        })?;
    identity.validate()?;
    let paths = InstancePaths::from_manifest(manifest, &identity)?;
    readable_file(&paths.soul, "instance SOUL.md")?;
    readable_file(&paths.persona_identity, "instance persona IDENTITY.md")?;
    if paths.rag_db.try_exists()? {
        readable_file(&paths.rag_db, "instance RAG database")?;
    }
    Ok((identity, paths))
}

pub fn validate_selection(
    selection: &DeploymentSelection,
    for_init: bool,
) -> Result<Option<(InstanceIdentity, InstancePaths)>> {
    match selection.manifest.as_deref() {
        Some(path) if path.try_exists()? => inspect(path).map(Some),
        _ if for_init => Ok(None),
        Some(path) => bail!("selected instance manifest is missing: {}", path.display()),
        None => {
            selection.require_manifest()?;
            unreachable!()
        }
    }
}

pub fn validate_initialization_sources(config: &GatewayConfig) -> Result<()> {
    readable_file(&config.files.soul_path, "initial persona SOUL.md source")?;
    readable_file(
        &config.files.identity_path,
        "initial persona IDENTITY.md source",
    )?;
    if config.rag.db_path.try_exists()? {
        readable_file(&config.rag.db_path, "initial RAG source")?;
    }
    Ok(())
}

fn acquire_locks(identity: InstanceIdentity, paths: InstancePaths) -> Result<InstanceLease> {
    let runtime = std::env::temp_dir().join("kaguya-instance-locks");
    fs::create_dir_all(&runtime)?;
    let instance_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(runtime.join(format!("{}.lock", identity.instance_id)))?;
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
    db: &Path,
) -> Result<(InstanceIdentity, PathBuf)> {
    readable_file(soul, "persona SOUL.md source")?;
    readable_file(persona, "persona IDENTITY.md source")?;
    if db.try_exists()? {
        readable_file(db, "RAG snapshot source")?;
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
    copy_db(db, &paths.rag_db)?;
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
    if selection.manifest.is_some() {
        bail!("deployment already selects an instance; initialize from an unselected deployment, or use rekey-instance");
    }
    validate_initialization_sources(config)?;
    create_instance(
        selection,
        InstanceIdentity::new(),
        &config.files.soul_path,
        &config.files.identity_path,
        &config.rag.db_path,
    )
}

pub fn rekey_instance(selection: &DeploymentSelection) -> Result<(InstanceIdentity, PathBuf)> {
    let source = load_required(selection)?;
    create_instance(
        selection,
        InstanceIdentity::new(),
        &source.paths.soul,
        &source.paths.persona_identity,
        &source.paths.rag_db,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_session_reference_validates_required_canonical_distinct_ids() {
        let identity = InstanceIdentity {
            schema_version: SCHEMA_VERSION,
            instance_id: "01234567-89ab-4cde-8fab-0123456789ab".into(),
            default_session_id: "fedcba98-7654-4321-8fed-cba987654321".into(),
        };
        let session = identity.session_ref().unwrap();
        assert_eq!(
            validate_session_ref(Some(&session)).unwrap(),
            (
                identity.instance_id.as_str(),
                identity.default_session_id.as_str()
            )
        );
        assert!(validate_session_ref(None).is_err());
        assert!(validate_session_ref(Some(&SessionRef::default())).is_err());
        let mut missing = session.clone();
        missing.instance_id = None;
        assert!(validate_session_ref(Some(&missing)).is_err());
        missing = session.clone();
        missing.session_id = None;
        assert!(validate_session_ref(Some(&missing)).is_err());
        for invalid in [
            "",
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "01234567-89AB-4CDE-8FAB-0123456789AB",
            "0123456789ab4cde8fab0123456789ab",
        ] {
            let mut wrong_instance = session.clone();
            wrong_instance.instance_id.as_mut().unwrap().value = invalid.into();
            assert!(validate_session_ref(Some(&wrong_instance)).is_err());
            let mut wrong_session = session.clone();
            wrong_session.session_id.as_mut().unwrap().value = invalid.into();
            assert!(validate_session_ref(Some(&wrong_session)).is_err());
        }
        let mut same_ids = session;
        same_ids.session_id.as_mut().unwrap().value = identity.instance_id;
        assert!(validate_session_ref(Some(&same_ids)).is_err());
    }

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
    fn local_manifest_init_rekey_and_locks_preserve_data() {
        let root = std::env::temp_dir().join(format!("kaguya instance {} spaces", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("gateway.toml"), "# component fixture").unwrap();
        fs::write(root.join("soul.md"), "original soul").unwrap();
        fs::write(root.join("persona.md"), "original persona identity").unwrap();
        let mut config = GatewayConfig::default();
        config.files.soul_path = root.join("soul.md");
        config.files.identity_path = root.join("persona.md");
        config.rag.db_path = root.join("source.db");
        let db = rusqlite::Connection::open(&config.rag.db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE retained (value TEXT); INSERT INTO retained VALUES ('preserved');",
        )
        .unwrap();
        drop(db);
        let selection = deployment(&root, None);
        assert!(validate_selection(&selection, true).unwrap().is_none());
        assert!(load_required(&selection).is_err());
        assert!(!selection.portable_root.exists());
        let (identity, manifest) = init_instance(&selection, &config).unwrap();
        let (loaded, paths) = inspect(&manifest).unwrap();
        assert_eq!(loaded, identity);
        assert_eq!(paths.root, manifest.parent().unwrap());
        assert!(!paths.root.join("owner.toml").exists());
        assert!(!paths.root.join("instances").exists());
        assert!(!paths.root.join(".owner.lock").exists());
        assert!(!std::env::temp_dir()
            .join("kaguya-instance-locks")
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
            validate_selection(&selected, false).unwrap().unwrap().0,
            identity
        );
        assert!(load_required(&selected).is_err());
        assert!(rekey_instance(&selected).is_err());
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
        let (independent, new_manifest) = rekey_instance(&selected).unwrap();
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
}
