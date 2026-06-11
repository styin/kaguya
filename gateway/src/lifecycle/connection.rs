use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Starting,
    Ready,
    Degraded,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct ManagedConnection {
    name: String,
    readiness: Readiness,
}

impl ManagedConnection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            readiness: Readiness::Starting,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn readiness(&self) -> Readiness {
        self.readiness
    }

    pub fn set_readiness(&mut self, readiness: Readiness) {
        self.readiness = readiness;
    }

    pub(crate) fn snapshot(&self) -> ManagedConnectionSnapshot {
        ManagedConnectionSnapshot {
            name: self.name.clone(),
            readiness: self.readiness,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedConnectionSnapshot {
    pub name: String,
    pub readiness: Readiness,
}

#[derive(Clone)]
pub struct ManagedConnectionHandle {
    name: String,
    connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
}

impl ManagedConnectionHandle {
    pub(crate) fn new(
        name: impl Into<String>,
        connections: Arc<Mutex<HashMap<String, ManagedConnection>>>,
    ) -> Self {
        Self {
            name: name.into(),
            connections,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn readiness(&self) -> Readiness {
        self.connections
            .lock()
            .expect("managed connection registry lock poisoned")
            .get(&self.name)
            .map(ManagedConnection::readiness)
            .unwrap_or(Readiness::Stopped)
    }

    pub fn set_readiness(&self, readiness: Readiness) {
        let mut guard = self
            .connections
            .lock()
            .expect("managed connection registry lock poisoned");
        let connection = guard
            .entry(self.name.clone())
            .or_insert_with(|| ManagedConnection::new(self.name.clone()));
        connection.set_readiness(readiness);
    }
}
