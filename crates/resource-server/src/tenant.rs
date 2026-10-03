use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use lumvise_db_core::{LocalPersistence, RelationalPersistence, SemanticPersistence};
use lumvise_resource_routing::auth::AuthenticatedPrincipal;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Authenticated tenant identity. Subject is intentionally absent: it
/// authorizes and audits an invocation but does not partition the tenant store.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TenantKey {
    pub issuer: String,
    pub tenant_id: String,
}

impl From<&AuthenticatedPrincipal> for TenantKey {
    fn from(principal: &AuthenticatedPrincipal) -> Self {
        Self {
            issuer: principal.issuer.clone(),
            tenant_id: principal.tenant_id.clone(),
        }
    }
}

/// Derives the only filesystem namespace used for a tenant. Raw issuer and
/// claim values never become path segments.
pub fn tenant_database_path(data_dir: &Path, key: &TenantKey) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(key.issuer.as_bytes());
    digest.update([0]);
    digest.update(key.tenant_id.as_bytes());
    data_dir
        .join("tenants")
        .join(format!("{:x}", digest.finalize()))
        .join("lumvise.db")
}

pub struct TenantAdapters {
    pub semantic: Arc<dyn SemanticPersistence>,
    pub relational: Arc<dyn RelationalPersistence>,
}

/// Internal persistence construction seam. It exists so server tests can use
/// deterministic adapters without exposing a central adapter to the server.
pub trait TenantPersistenceFactory: Send + Sync {
    fn open(&self, database_path: &Path) -> Result<TenantAdapters, TenantOpenError>;
}

/// Builds server-owned adapters bound to a verified principal and client.
///
/// Unlike tenant-local storage, shared project stores must authorize every
/// operation against current membership. Implementations must keep that check
/// inside the returned adapters; a cached account identity is not a role grant.
/// Example: a private hub factory returns separate PostgreSQL and FalkorDB adapters.
pub trait PrincipalPersistenceFactory: Send + Sync {
    fn open(
        &self,
        principal: &AuthenticatedPrincipal,
        client_instance_id: &str,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> Result<Arc<TenantAdapters>, TenantOpenError>;
}

#[derive(Default)]
pub struct LocalTenantPersistenceFactory;

impl TenantPersistenceFactory for LocalTenantPersistenceFactory {
    fn open(&self, database_path: &Path) -> Result<TenantAdapters, TenantOpenError> {
        let persistence = Arc::new(
            LocalPersistence::open(database_path)
                .map_err(|error| TenantOpenError::Open(error.to_string()))?,
        );
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence;
        Ok(TenantAdapters {
            semantic,
            relational,
        })
    }
}

/// A cache holds already recovered local adapters. The mutex deliberately
/// includes creation: a tenant is opened and recovery completes exactly once
/// before any other request can observe it as ready.
pub struct TenantAdapterCache {
    data_dir: PathBuf,
    factory: Arc<dyn TenantPersistenceFactory>,
    adapters: Mutex<HashMap<TenantKey, Arc<TenantAdapters>>>,
}

impl TenantAdapterCache {
    pub fn new(data_dir: PathBuf, factory: Arc<dyn TenantPersistenceFactory>) -> Self {
        Self {
            data_dir,
            factory,
            adapters: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_or_open(&self, key: &TenantKey) -> Result<Arc<TenantAdapters>, TenantOpenError> {
        let mut adapters = self.adapters.lock();
        if let Some(existing) = adapters.get(key) {
            return Ok(Arc::clone(existing));
        }
        let opened = Arc::new(
            self.factory
                .open(&tenant_database_path(&self.data_dir, key))?,
        );
        adapters.insert(key.clone(), Arc::clone(&opened));
        Ok(opened)
    }

    pub fn database_path(&self, key: &TenantKey) -> PathBuf {
        tenant_database_path(&self.data_dir, key)
    }
}

#[derive(Debug, Error)]
pub enum TenantOpenError {
    #[error("opening tenant persistence failed: {0}")]
    Open(String),
}
