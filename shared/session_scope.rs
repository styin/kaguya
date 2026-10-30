//! The one instance/session pair a Rust process serves.
//!
//! A scope is validated once, where it enters the process (manifest, HTTP, or
//! gRPC). Inside the process the type carries that guarantee. Each crate that
//! includes this file defines `crate::proto` from the shared schema.

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use crate::proto;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionScope {
    instance_id: Uuid,
    session_id: Uuid,
}

impl SessionScope {
    /// Accept canonical, non-nil, distinct UUID strings.
    pub fn parse(instance_id: &str, session_id: &str) -> Result<Self> {
        let instance_id = canonical(instance_id, "instance_id")?;
        let session_id = canonical(session_id, "session_id")?;
        if instance_id == session_id {
            bail!("instance_id and session_id must differ");
        }
        Ok(Self {
            instance_id,
            session_id,
        })
    }

    /// Validate a wire scope; the only place generated Option fields are unwrapped.
    pub fn from_proto(scope: Option<&proto::SessionRef>) -> Result<Self> {
        let scope = scope.context("session reference is required")?;
        Self::parse(
            &scope
                .instance_id
                .as_ref()
                .context("instance_id is required")?
                .value,
            &scope
                .session_id
                .as_ref()
                .context("session_id is required")?
                .value,
        )
    }

    pub fn to_proto(self) -> proto::SessionRef {
        proto::SessionRef {
            instance_id: Some(proto::InstanceId {
                value: self.instance_id.to_string(),
            }),
            session_id: Some(proto::SessionId {
                value: self.session_id.to_string(),
            }),
        }
    }

    pub fn instance_id(self) -> Uuid {
        self.instance_id
    }

    pub fn session_id(self) -> Uuid {
        self.session_id
    }
}

fn canonical(value: &str, field: &str) -> Result<Uuid> {
    match Uuid::parse_str(value) {
        Ok(id) if !id.is_nil() && id.to_string() == value => Ok(id),
        _ => bail!("{field} must be a canonical non-nil UUID"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: &str = "01234567-89ab-4cde-8fab-0123456789ab";
    const SESSION: &str = "fedcba98-7654-4321-8fed-cba987654321";

    #[test]
    fn scope_requires_canonical_non_nil_distinct_ids() {
        let scope = SessionScope::parse(INSTANCE, SESSION).unwrap();
        assert_eq!(
            SessionScope::from_proto(Some(&scope.to_proto())).unwrap(),
            scope
        );
        assert!(SessionScope::from_proto(None).is_err());
        assert!(SessionScope::from_proto(Some(&proto::SessionRef::default())).is_err());
        for invalid in [
            "",
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "01234567-89AB-4CDE-8FAB-0123456789AB",
            "0123456789ab4cde8fab0123456789ab",
        ] {
            assert!(SessionScope::parse(invalid, SESSION).is_err());
            assert!(SessionScope::parse(INSTANCE, invalid).is_err());
        }
        assert!(SessionScope::parse(INSTANCE, INSTANCE).is_err());
    }
}
