//! The MCP registry owns live connection membership and its revocation generation.
//! Ingress captures a canonical project; memory authority rechecks that same scope.
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

#[derive(Clone)]
struct Membership {
    project_root: Option<String>,
    project_id: Option<String>,
    storage_authority: Option<String>,
    epoch: u64,
}

pub(crate) struct McpProjectScope {
    pub(crate) project_root: String,
    pub(crate) project_id: String,
    pub(crate) storage_authority: Option<String>,
}

#[derive(Default)]
pub(crate) struct McpSessionMembership {
    connections: Mutex<HashMap<String, Membership>>,
}

impl McpSessionMembership {
    pub(crate) fn register(&self, connection: &str, root: &str) -> Result<(), String> {
        let mut connections = self.lock()?;
        replace_membership(&mut connections, connection, Some(root))
    }

    pub(crate) fn heartbeat(
        &self,
        connection: &str,
        root: &str,
        status: &str,
    ) -> Result<(), String> {
        let mut connections = self.lock()?;
        let observed = connections.get(connection);
        if observed
            .is_some_and(|entry| entry.project_root.as_deref().is_some_and(|old| old != root))
        {
            return Ok(());
        }
        if status == "unavailable" {
            return replace_membership(&mut connections, connection, None);
        }
        // A delayed heartbeat cannot revive an explicitly retired connection.
        if matches!(status, "ready" | "busy") && observed.is_none() {
            replace_membership(&mut connections, connection, Some(root))?;
        }
        Ok(())
    }

    pub(crate) fn project_root(&self, connection: &str) -> Result<Option<String>, String> {
        Ok(self
            .lock()?
            .get(connection)
            .and_then(|entry| entry.project_root.clone()))
    }

    pub(crate) fn roots(&self) -> Result<HashMap<String, Option<String>>, String> {
        Ok(self
            .lock()?
            .iter()
            .map(|(id, entry)| (id.clone(), entry.project_root.clone()))
            .collect())
    }

    pub(crate) fn capture_project(
        &self,
        connection: &str,
        root: &str,
        project_id: &str,
        storage_authority: Option<&str>,
    ) -> Result<u64, String> {
        let mut connections = self.lock()?;
        let entry = connections
            .get_mut(connection)
            .ok_or_else(|| missing(connection))?;
        if entry.project_root.as_deref() != Some(root)
            || project_id.is_empty()
            || storage_authority.is_some_and(|authority| authority.trim().is_empty())
        {
            return Err(format!(
                "MCP binding {connection:?}, {root:?}, {project_id:?}; expected current registered root and canonical project identity"
            ));
        }
        if entry.project_id.as_deref() != Some(project_id)
            || entry.storage_authority.as_deref() != storage_authority
        {
            entry.epoch = next_epoch(entry.epoch)?;
            entry.project_id = Some(project_id.into());
            entry.storage_authority = storage_authority.map(str::to_owned);
        }
        Ok(entry.epoch)
    }

    pub(crate) fn scope(
        &self,
        connection: &str,
        epoch: Option<u64>,
    ) -> Result<McpProjectScope, String> {
        let connections = self.lock()?;
        let entry = connections
            .get(connection)
            .ok_or_else(|| missing(connection))?;
        match (&entry.project_root, &entry.project_id) {
            (Some(root), Some(project_id)) if epoch == Some(entry.epoch) => Ok(McpProjectScope {
                project_root: root.clone(),
                project_id: project_id.clone(),
                storage_authority: entry.storage_authority.clone(),
            }),
            _ => Err(format!(
                "MCP binding {connection:?} epoch {epoch:?}; expected live captured project at epoch {}",
                entry.epoch
            )),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, HashMap<String, Membership>>, String> {
        self.connections.lock().map_err(|_| {
            "MCP membership lock poisoned; expected available connection registry".into()
        })
    }
}

fn replace_membership(
    connections: &mut HashMap<String, Membership>,
    connection: &str,
    root: Option<&str>,
) -> Result<(), String> {
    let previous = connections.get(connection);
    if previous.is_some_and(|entry| entry.project_root.as_deref() == root) {
        return Ok(());
    }
    let epoch = next_epoch(previous.map_or(0, |entry| entry.epoch))?;
    connections.insert(
        connection.into(),
        Membership {
            project_root: root.map(str::to_owned),
            project_id: None,
            storage_authority: None,
            epoch,
        },
    );
    Ok(())
}

fn next_epoch(epoch: u64) -> Result<u64, String> {
    epoch
        .checked_add(1)
        .ok_or_else(|| format!("MCP membership epoch {epoch}; expected incrementable u64"))
}

fn missing(connection: &str) -> String {
    format!("MCP connection {connection:?}; expected registered live project membership")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_storage_authority_retires_the_captured_scope_even_with_equal_lineage() {
        let membership = McpSessionMembership::default();
        membership.register("a", "/p").unwrap();
        let old = membership
            .capture_project("a", "/p", "p1", Some("authority-a"))
            .unwrap();
        let new = membership
            .capture_project("a", "/p", "p1", Some("authority-b"))
            .unwrap();
        assert!(new > old);
        assert!(membership.scope("a", Some(old)).is_err());
        assert_eq!(
            membership
                .scope("a", Some(new))
                .unwrap()
                .storage_authority
                .as_deref(),
            Some("authority-b")
        );
        assert!(
            membership
                .capture_project("a", "/p", "p1", Some(""))
                .is_err()
        );
    }

    #[test]
    fn switch_retirement_and_rebound_invalidate_the_original_scope() {
        let membership = McpSessionMembership::default();
        membership.register("a", "/p").unwrap();
        let first = membership.capture_project("a", "/p", "p1", None).unwrap();
        assert_eq!(membership.scope("a", Some(first)).unwrap().project_id, "p1");
        membership.register("a", "/q").unwrap();
        assert!(membership.scope("a", Some(first)).is_err());
        assert!(membership.capture_project("a", "/p", "p1", None).is_err());
        let second = membership.capture_project("a", "/q", "q1", None).unwrap();
        let rebound = membership.capture_project("a", "/q", "q2", None).unwrap();
        assert!(membership.scope("a", Some(second)).is_err());
        membership.heartbeat("a", "/q", "unavailable").unwrap();
        membership.heartbeat("a", "/q", "ready").unwrap();
        assert!(membership.scope("a", Some(rebound)).is_err());
        assert_eq!(membership.project_root("a").unwrap(), None);
        membership.register("a", "/q").unwrap();
        assert!(membership.capture_project("a", "/q", "q2", None).unwrap() > rebound);
    }

    #[test]
    fn heartbeat_and_repeat_registration_preserve_the_current_generation() {
        let membership = McpSessionMembership::default();
        assert!(membership.scope("a", None).is_err());
        membership.heartbeat("a", "/p", "ready").unwrap();
        let epoch = membership.capture_project("a", "/p", "p1", None).unwrap();
        membership.heartbeat("a", "/p", "busy").unwrap();
        membership.heartbeat("a", "/old", "unavailable").unwrap();
        membership.register("a", "/p").unwrap();
        assert_eq!(
            membership.capture_project("a", "/p", "p1", None).unwrap(),
            epoch
        );
        assert_eq!(
            membership.scope("a", Some(epoch)).unwrap().project_root,
            "/p"
        );
        assert_eq!(membership.roots().unwrap()["a"].as_deref(), Some("/p"));
        assert!(membership.capture_project("a", "/p", "", None).is_err());
    }
}
