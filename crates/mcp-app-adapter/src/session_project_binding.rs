//! Connection-scoped project selection and registry renewal. Only the MCP
//! adapter calls initialize/select/close; publication and heartbeat stay here.

use crate::AppBridgeConfig;
use lumvise_contracts::{
    HeartbeatRequest, McpInstanceStatus, RegisterMcpRequest, RegisterMcpResponse,
};
use lumvise_mcp_core::McpApplicationError;
use lumvise_mcp_core::app_bridge_transport::{AppBridgeHttpMethod, AppBridgeHttpTransport};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RENEWAL_INTERVAL: Duration = Duration::from_secs(10);
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

trait ProjectRegistryPort: Send + Sync {
    fn directory_exists(&self, path: &Path) -> bool;
    fn register(&self, connection_id: &str, root: &str) -> Result<(), String>;
    fn heartbeat(
        &self,
        connection_id: &str,
        root: &str,
        status: McpInstanceStatus,
    ) -> Result<(), String>;
}

struct AppProjectRegistry {
    config: AppBridgeConfig,
}

impl AppProjectRegistry {
    fn publish(
        &self,
        endpoint: &str,
        body: impl Serialize,
        id: &str,
        root: &str,
        status: McpInstanceStatus,
    ) -> Result<(), String> {
        let response = self.request_acknowledgement(endpoint, body)?;
        if response.accepted
            && response.instance.instance_id == id
            && response.instance.project_root == root
            && response.instance.status == status
        {
            return Ok(());
        }
        Err(format!(
            "MCP registry acknowledgement {response:?}: expected accepted {id:?}, {root:?}, {status:?}"
        ))
    }

    fn request_acknowledgement(
        &self,
        endpoint: &str,
        body: impl Serialize,
    ) -> Result<RegisterMcpResponse, String> {
        let connection = self.config.current_connection()?;
        let path = format!("{endpoint}?credential={}", connection.app_bridge_credential);
        let body = serde_json::to_value(body).map_err(|error| error.to_string())?;
        let response = AppBridgeHttpTransport::new()
            .request_json(
                &connection.app_bridge_base_url,
                AppBridgeHttpMethod::Post,
                &path,
                &body,
            )
            .map_err(|error| error.to_string())?;
        serde_json::from_value(response).map_err(|error| {
            format!("MCP registry {endpoint}: expected registration acknowledgement: {error}")
        })
    }
}

impl ProjectRegistryPort for AppProjectRegistry {
    fn directory_exists(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn register(&self, connection_id: &str, root: &str) -> Result<(), String> {
        let request = RegisterMcpRequest {
            instance_id: connection_id.into(),
            project_root: root.into(),
            display_name: None,
            capabilities: vec![],
            control_channel: None,
        };
        self.publish(
            "/api/mcp/register",
            request,
            connection_id,
            root,
            McpInstanceStatus::Starting,
        )
    }

    fn heartbeat(
        &self,
        connection_id: &str,
        root: &str,
        status: McpInstanceStatus,
    ) -> Result<(), String> {
        let request = HeartbeatRequest {
            instance_id: connection_id.into(),
            project_root: root.into(),
            status: status.clone(),
        };
        self.publish("/api/mcp/heartbeat", request, connection_id, root, status)
    }
}

#[derive(Default)]
struct BindingPublication {
    initialized: bool,
    project_root: Option<String>,
    ready: bool,
}

struct BindingOwner {
    connection_id: String,
    initial_root: Option<String>,
    registry: Arc<dyn ProjectRegistryPort>,
    closed: AtomicBool,
    publication: Mutex<BindingPublication>,
    wake: Condvar,
    wake_lock: Mutex<()>,
}

pub(crate) struct SessionProjectBinding {
    owner: Arc<BindingOwner>,
    worker: Option<JoinHandle<()>>,
    startup_error: Option<String>,
}

impl SessionProjectBinding {
    pub(crate) fn new(config: AppBridgeConfig, initial_root: Option<String>) -> Self {
        Self::with_registry(Arc::new(AppProjectRegistry { config }), initial_root)
    }

    fn with_registry(registry: Arc<dyn ProjectRegistryPort>, initial_root: Option<String>) -> Self {
        let owner = Arc::new(BindingOwner::new(registry, initial_root));
        let worker_owner = owner.clone();
        let worker = std::thread::Builder::new()
            .name("lumvise-mcp-project-presence".into())
            .spawn(move || renew_until_closed(worker_owner));
        let (worker, startup_error) =
            worker
                .map(|worker| (Some(worker), None))
                .unwrap_or_else(|error| {
                    (
                        None,
                        Some(format!("MCP presence worker unavailable: {error}")),
                    )
                });
        Self {
            owner,
            worker,
            startup_error,
        }
    }

    pub(crate) fn initialize(&self) -> Result<(), String> {
        if let Some(error) = &self.startup_error {
            return Err(error.clone());
        }
        let mut publication = self.owner.lock()?;
        self.owner.ensure_open()?;
        if publication.initialized {
            return Ok(());
        }
        publication.initialized = true;
        if let Some(root) = &self.owner.initial_root {
            // Explicit CLI paths retain their existing directory validation and exact identity.
            self.owner.validate_directory(root, false)?;
            self.owner.replace(&mut publication, Some(root.clone()))?;
        }
        Ok(())
    }

    pub(crate) fn select(&self, arguments: Value) -> Result<Value, McpApplicationError> {
        let root = self.validated_selection(&arguments)?;
        let mut publication = self.owner.lock().map_err(McpApplicationError::invocation)?;
        if !publication.initialized {
            return Err(McpApplicationError::invocation(
                "set_current_project requires a completed MCP initialization",
            ));
        }
        self.owner
            .replace(&mut publication, root)
            .map_err(McpApplicationError::invocation)?;
        Ok(
            json!({"connection_id":self.owner.connection_id, "project_root":publication.project_root,
            "binding_status":if publication.ready { "ready" } else { "unbound" }}),
        )
    }

    fn validated_selection(
        &self,
        arguments: &Value,
    ) -> Result<Option<String>, McpApplicationError> {
        let root = parse_selection(arguments).map_err(McpApplicationError::invalid_params)?;
        if let Some(root) = &root {
            self.owner
                .validate_directory(root, true)
                .map_err(McpApplicationError::invalid_params)?;
        }
        Ok(root)
    }

    pub(crate) fn close(&self) {
        // Set before taking any publication lock: EOF must fence blocked registry I/O.
        self.owner.closed.store(true, Ordering::Release);
        let _wake_guard = self.owner.wake_lock.lock();
        self.owner.wake.notify_all();
    }
}

impl Drop for SessionProjectBinding {
    fn drop(&mut self) {
        self.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl BindingOwner {
    fn new(registry: Arc<dyn ProjectRegistryPort>, initial_root: Option<String>) -> Self {
        Self {
            connection_id: new_connection_id(),
            initial_root,
            registry,
            closed: AtomicBool::new(false),
            publication: Mutex::new(BindingPublication::default()),
            wake: Condvar::new(),
            wake_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, BindingPublication>, String> {
        self.publication
            .lock()
            .map_err(|_| "MCP binding publication lock unavailable".into())
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("MCP connection is closed; expected an open initialized connection".into());
        }
        Ok(())
    }

    fn validate_directory(&self, root: &str, require_absolute: bool) -> Result<(), String> {
        if root.is_empty()
            || root.trim() != root
            || (require_absolute && !Path::new(root).is_absolute())
            || !self.registry.directory_exists(Path::new(root))
        {
            return Err(format!(
                "project_root {root:?}: expected an existing {}directory",
                if require_absolute { "absolute " } else { "" }
            ));
        }
        Ok(())
    }

    fn retire(&self, publication: &mut BindingPublication) -> Result<(), String> {
        let Some(root) = publication.project_root.as_deref() else {
            return Ok(());
        };
        self.registry
            .heartbeat(&self.connection_id, root, McpInstanceStatus::Unavailable)?;
        publication.project_root = None;
        publication.ready = false;
        Ok(())
    }

    fn replace(
        &self,
        publication: &mut BindingPublication,
        root: Option<String>,
    ) -> Result<(), String> {
        self.ensure_open()?;
        if publication.ready && publication.project_root == root {
            return self.renew(publication);
        }
        self.retire(publication)?;
        self.ensure_open()?;
        let Some(root) = root else {
            return Ok(());
        };
        self.publish_ready(publication, root)
    }

    fn publish_ready(
        &self,
        publication: &mut BindingPublication,
        root: String,
    ) -> Result<(), String> {
        self.registry.register(&self.connection_id, &root)?;
        publication.project_root = Some(root.clone());
        self.ensure_open_or_retire(publication)?;
        self.registry
            .heartbeat(&self.connection_id, &root, McpInstanceStatus::Ready)?;
        self.ensure_open_or_retire(publication)?;
        publication.ready = true;
        Ok(())
    }

    fn ensure_open_or_retire(&self, publication: &mut BindingPublication) -> Result<(), String> {
        if let Err(error) = self.ensure_open() {
            let _ = self.retire(publication);
            return Err(error);
        }
        Ok(())
    }

    fn renew(&self, publication: &mut BindingPublication) -> Result<(), String> {
        self.ensure_open()?;
        if !publication.ready {
            return Ok(());
        }
        let root = publication
            .project_root
            .as_deref()
            .expect("ready binding has a root");
        self.registry
            .heartbeat(&self.connection_id, root, McpInstanceStatus::Ready)?;
        self.ensure_open_or_retire(publication)
    }
}

fn renew_until_closed(owner: Arc<BindingOwner>) {
    while wait_for_renewal(&owner).is_ok() {
        let Ok(mut publication) = owner.lock() else {
            return;
        };
        if owner.closed.load(Ordering::Acquire) {
            let _ = owner.retire(&mut publication);
            return;
        }
        if let Err(error) = owner.renew(&mut publication) {
            tracing::warn!(event="mcp_project_presence_renewal_failed", %error);
        }
    }
}

fn wait_for_renewal(owner: &BindingOwner) -> Result<(), String> {
    let wake_guard = owner
        .wake_lock
        .lock()
        .map_err(|_| "MCP renewal lock unavailable")?;
    let _renewal_wait = owner
        .wake
        .wait_timeout_while(wake_guard, RENEWAL_INTERVAL, |_| {
            !owner.closed.load(Ordering::Acquire)
        })
        .map_err(|_| "MCP renewal wait unavailable")?;
    Ok(())
}

fn parse_selection(arguments: &Value) -> Result<Option<String>, String> {
    let invalid = || {
        format!(
            "set_current_project input {arguments}: expected only required project_root string or null"
        )
    };
    let object = arguments.as_object().ok_or_else(invalid)?;
    if object.len() != 1 {
        return Err(invalid());
    }
    match object.get("project_root") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(root)) => Ok(Some(root.clone())),
        _ => Err(invalid()),
    }
}

fn new_connection_id() -> String {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    format!("mcp-connection-{}-{created}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeProjectRegistry {
        records: Mutex<Vec<(String, String, McpInstanceStatus)>>,
        registered: Mutex<std::collections::HashSet<String>>,
        fail_register: AtomicBool,
        fail_ready: AtomicBool,
        fail_unavailable: AtomicBool,
    }

    impl ProjectRegistryPort for FakeProjectRegistry {
        fn directory_exists(&self, path: &Path) -> bool {
            path.to_string_lossy().starts_with("/projects/")
        }
        fn register(&self, id: &str, root: &str) -> Result<(), String> {
            if self.fail_register.swap(false, Ordering::AcqRel) {
                return Err("fake registration failed before applying".into());
            }
            self.registered.lock().unwrap().insert(id.into());
            self.records.lock().unwrap().push((
                id.into(),
                root.into(),
                McpInstanceStatus::Starting,
            ));
            Ok(())
        }
        fn heartbeat(&self, id: &str, root: &str, status: McpInstanceStatus) -> Result<(), String> {
            if !self.registered.lock().unwrap().contains(id) {
                return Err("fake heartbeat 404: instance not registered".into());
            }
            if status == McpInstanceStatus::Ready && self.fail_ready.load(Ordering::Acquire) {
                return Err("fake ready publication failed".into());
            }
            if status == McpInstanceStatus::Unavailable
                && self.fail_unavailable.load(Ordering::Acquire)
            {
                return Err("fake cleanup delivery failed".into());
            }
            self.records
                .lock()
                .unwrap()
                .push((id.into(), root.into(), status));
            Ok(())
        }
    }

    #[test]
    fn registration_failure_before_apply_does_not_poison_retry_or_selection() {
        let registry = Arc::new(FakeProjectRegistry::default());
        let binding = SessionProjectBinding::with_registry(registry.clone(), None);
        binding.initialize().unwrap();
        registry.fail_register.store(true, Ordering::Release);
        assert!(
            binding
                .select(json!({"project_root":"/projects/a"}))
                .is_err()
        );
        let selected = binding
            .select(json!({"project_root":"/projects/a"}))
            .unwrap();
        assert_eq!(selected["binding_status"], "ready");
        for malformed in [
            json!({}),
            json!({"project_root":3}),
            json!({"project_root":""}),
            json!({"project_root":"relative"}),
            json!({"project_root":"/projects/b","extra":true}),
        ] {
            assert!(binding.select(malformed).is_err());
            assert_eq!(
                binding.owner.lock().unwrap().project_root.as_deref(),
                Some("/projects/a")
            );
        }
        binding.select(json!({"project_root":null})).unwrap();
    }

    #[test]
    fn failed_ready_ack_does_not_enable_renewal_and_can_be_retried() {
        let registry = Arc::new(FakeProjectRegistry::default());
        let binding = SessionProjectBinding::with_registry(registry.clone(), None);
        binding.initialize().unwrap();
        registry.fail_ready.store(true, Ordering::Release);
        assert!(
            binding
                .select(json!({"project_root":"/projects/a"}))
                .is_err()
        );
        let before = registry.records.lock().unwrap().len();
        binding
            .owner
            .renew(&mut binding.owner.lock().unwrap())
            .unwrap();
        assert_eq!(registry.records.lock().unwrap().len(), before);
        registry.fail_ready.store(false, Ordering::Release);
        assert_eq!(
            binding
                .select(json!({"project_root":"/projects/a"}))
                .unwrap()["binding_status"],
            "ready"
        );
    }

    #[test]
    fn close_is_sticky_even_when_cleanup_delivery_fails() {
        let registry = Arc::new(FakeProjectRegistry::default());
        let binding =
            SessionProjectBinding::with_registry(registry.clone(), Some("/projects/a".into()));
        binding.initialize().unwrap();
        registry.fail_unavailable.store(true, Ordering::Release);
        binding.close();
        assert!(binding.initialize().is_err());
        assert!(
            binding
                .select(json!({"project_root":"/projects/b"}))
                .is_err()
        );
        assert!(
            binding
                .owner
                .renew(&mut binding.owner.lock().unwrap())
                .is_err()
        );
        drop(binding);
        assert_eq!(
            registry.records.lock().unwrap().len(),
            2,
            "only acknowledged starting/ready; failed cleanup relies on existing freshness"
        );
    }

    struct FakeBlockedProjectRegistry {
        delegate: FakeProjectRegistry,
        phase: McpInstanceStatus,
        block_next: AtomicBool,
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl FakeBlockedProjectRegistry {
        fn new(
            phase: McpInstanceStatus,
        ) -> (
            Arc<Self>,
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::Sender<()>,
        ) {
            let (entered, receive_entered) = std::sync::mpsc::channel();
            let (release, receive_release) = std::sync::mpsc::channel();
            (
                Arc::new(Self {
                    delegate: FakeProjectRegistry::default(),
                    phase,
                    block_next: AtomicBool::new(false),
                    entered,
                    release: Mutex::new(receive_release),
                }),
                receive_entered,
                release,
            )
        }
        fn block_publication(&self, phase: McpInstanceStatus) {
            if phase == self.phase && self.block_next.swap(false, Ordering::AcqRel) {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }
    }

    impl ProjectRegistryPort for FakeBlockedProjectRegistry {
        fn directory_exists(&self, path: &Path) -> bool {
            self.delegate.directory_exists(path)
        }
        fn register(&self, id: &str, root: &str) -> Result<(), String> {
            self.block_publication(McpInstanceStatus::Starting);
            self.delegate.register(id, root)
        }
        fn heartbeat(&self, id: &str, root: &str, status: McpInstanceStatus) -> Result<(), String> {
            self.block_publication(status.clone());
            self.delegate.heartbeat(id, root, status)
        }
    }

    #[test]
    fn closed_binding_retires_delayed_registration_without_publishing_ready() {
        let (registry, entered, release) =
            FakeBlockedProjectRegistry::new(McpInstanceStatus::Starting);
        registry.block_next.store(true, Ordering::Release);
        let binding =
            SessionProjectBinding::with_registry(registry.clone(), Some("/projects/a".into()));
        std::thread::scope(|scope| {
            let initialization = scope.spawn(|| binding.initialize());
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            binding.close();
            release.send(()).unwrap();
            assert!(initialization.join().unwrap().is_err());
        });
        drop(binding);
        let records = registry.delegate.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].2, McpInstanceStatus::Starting);
        assert_eq!(records[1].2, McpInstanceStatus::Unavailable);
    }

    #[test]
    fn delayed_heartbeat_and_queued_selection_cannot_revive_closed_binding() {
        let (registry, entered, release) =
            FakeBlockedProjectRegistry::new(McpInstanceStatus::Ready);
        let binding =
            SessionProjectBinding::with_registry(registry.clone(), Some("/projects/a".into()));
        binding.initialize().unwrap();
        registry.block_next.store(true, Ordering::Release);
        std::thread::scope(|scope| {
            let heartbeat = scope.spawn(|| binding.select(json!({"project_root":"/projects/a"})));
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let selection = scope.spawn(|| binding.select(json!({"project_root":"/projects/b"})));
            binding.close();
            release.send(()).unwrap();
            assert!(heartbeat.join().unwrap().is_err());
            assert!(selection.join().unwrap().is_err());
        });
        drop(binding);
        let records = registry.delegate.records.lock().unwrap();
        assert_eq!(records.last().unwrap().2, McpInstanceStatus::Unavailable);
        assert!(records.iter().all(|(_, root, _)| root == "/projects/a"));
    }
}
