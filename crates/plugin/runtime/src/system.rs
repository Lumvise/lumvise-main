use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use lumvise_plugin_package::InstalledPlugin;
use lumvise_plugin_protocol::{CURRENT_PROTOCOL_VERSION, MessageBody, WireMessage, WireOutcome};
use serde_json::Value;

use crate::{
    DenyAllHostCapabilityBroker, DenyExecutionSandbox, ExportDescriptor, HostCapabilityBroker,
    PluginInvocationCancellationRequest, PluginInvocationContext, PluginInvocationError,
    PluginInvocationRequest, PluginRuntimeError, PluginSandbox, PluginSandboxError,
    PluginSandboxRequest, ProductionPluginSandbox,
    catalog::{PluginEntry, PluginInstallation},
    process::{InvocationAccess, PluginProcess},
    schema::ExportValidators,
};

mod active_invocations;
mod background_exports;
mod commands;
mod exclusive_lane;
mod export_concurrency_policy;
mod invocation_admission;
mod plugin_invocation;
mod views;

use active_invocations::{ActiveInvocationGuard, ActivePluginInvocations};
pub use background_exports::{BackgroundExportKind, PublishedBackgroundExport};
pub use commands::PublishedCommand;
use exclusive_lane::LaneInvocation;
pub use exclusive_lane::{ExclusiveInvocationLanes, ExclusiveLaneSnapshot};
pub use export_concurrency_policy::{ExportConcurrencyPolicy, ExportConcurrencyRegistry};
pub use invocation_admission::PluginAdmissionSnapshot;
use invocation_admission::PluginInvocationAdmission;
pub use views::PluginViewAsset;

static NEXT_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

/// One host-owned deadline applied to every compiled-plugin process invocation.
pub const PLUGIN_INVOCATION_DEADLINE: Duration = Duration::from_secs(60);

/// Deadlines for supervised plugin processes.
#[derive(Clone, Debug)]
pub struct PluginRuntimeConfig {
    /// Identity reported to plugin processes during handshake.
    pub host_id: String,
    /// Maximum time allowed for the ready handshake.
    pub handshake_timeout: Duration,
    /// Maximum number of waiting invocations retained by each Plugin mailbox.
    pub maximum_queued_invocations_per_plugin: usize,
    /// Time allowed for graceful shutdown before forced termination.
    pub shutdown_grace: Duration,
    /// Per-export concurrency policies.
    pub export_concurrency: ExportConcurrencyRegistry,
    /// Maximum in-flight invocations per plugin process.
    ///
    /// The default is one; compatible plugins can use higher limits for
    /// multiplexed request/response over a shared stdio pipe.
    pub max_concurrent_invocations_per_plugin: u32,
    controlled_test_deadline: Option<Duration>,
}

impl Default for PluginRuntimeConfig {
    fn default() -> Self {
        Self {
            host_id: "lumvise-app".to_owned(),
            handshake_timeout: Duration::from_secs(5),
            maximum_queued_invocations_per_plugin: 8,
            shutdown_grace: Duration::from_secs(2),
            export_concurrency: ExportConcurrencyRegistry::new(),
            max_concurrent_invocations_per_plugin: 1,
            controlled_test_deadline: None,
        }
    }
}

impl PluginRuntimeConfig {
    /// Replaces the process deadline only for deterministic runtime tests.
    #[doc(hidden)]
    pub fn with_controlled_test_deadline(mut self, deadline: Duration) -> Self {
        self.controlled_test_deadline = Some(deadline);
        self
    }

    pub(crate) fn invocation_deadline(&self) -> Duration {
        self.controlled_test_deadline
            .unwrap_or(PLUGIN_INVOCATION_DEADLINE)
    }
}

/// Atomic catalog and lifecycle boundary for compiled plugins.
pub struct PluginSystem {
    config: PluginRuntimeConfig,
    broker: Arc<dyn HostCapabilityBroker>,
    sandbox: Arc<dyn PluginSandbox>,
    installations: Mutex<HashMap<String, Arc<PluginEntry>>>,
    invocation_waits: Mutex<HashMap<String, String>>,
    active_invocations: ActivePluginInvocations,
    invocation_admission: PluginInvocationAdmission,
    exclusive_lanes: Arc<ExclusiveInvocationLanes>,
}

/// One ready plugin and its signed generic exports.
#[derive(Clone, Debug)]
pub struct PublishedPlugin {
    /// Stable signed plugin identity.
    pub plugin_id: String,
    /// Signed exports available through the ready process.
    pub exports: Vec<ExportDescriptor>,
}

/// Read-only identity of one ready supervised plugin subprocess.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivePluginProcess {
    /// Stable signed plugin identity.
    pub plugin_id: String,
    /// Operating-system process identifier of the supervised child.
    pub process_id: u32,
}

/// A caller-owned, executor-neutral compiled-plugin invocation.
///
/// Construction registers the exact cancellation tuple synchronously; polling
/// then drives the same runtime path used by synchronous callers.
pub struct PluginInvocationHandle<'runtime> {
    cancellation: crate::PluginInvocationCancellation,
    future:
        Pin<Box<dyn Future<Output = Result<WireOutcome, PluginInvocationError>> + Send + 'runtime>>,
}

impl PluginInvocationHandle<'_> {
    /// Cancels this invocation only. Queued work is removed before dispatch;
    /// dispatched work emits its matching `HostCancel` during terminal cleanup.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Drives the handle on a private runtime for synchronous callers.
    pub fn blocking_wait(self) -> Result<WireOutcome, PluginInvocationError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| {
                PluginInvocationError::from_runtime(PluginRuntimeError::Protocol {
                    plugin_id: "runtime".to_owned(),
                    message: format!("failed to create invocation runtime: {error}"),
                    stderr: String::new(),
                })
            })?
            .block_on(self)
    }
}

impl Future for PluginInvocationHandle<'_> {
    type Output = Result<WireOutcome, PluginInvocationError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

struct PreparedInvocation<'a> {
    plugin_id: &'a str,
    capability_id: &'a str,
    input: Value,
    validators: &'a ExportValidators,
    context: &'a PluginInvocationContext,
}

impl PluginSystem {
    /// Creates a plugin system using production containment for the current platform.
    ///
    /// # Errors
    /// Returns a structured, fail-closed error when this platform or its required
    /// OS sandbox facility is unavailable.
    ///
    /// # Examples
    /// ```no_run
    /// use std::sync::Arc;
    /// use lumvise_plugin_runtime::{
    ///     DenyAllHostCapabilityBroker, PluginRuntimeConfig, PluginSystem,
    /// };
    /// let system = PluginSystem::production(
    ///     PluginRuntimeConfig::default(),
    ///     Arc::new(DenyAllHostCapabilityBroker),
    /// )?;
    /// # Ok::<(), lumvise_plugin_runtime::PluginSandboxError>(())
    /// ```
    pub fn production(
        config: PluginRuntimeConfig,
        broker: Arc<dyn HostCapabilityBroker>,
    ) -> Result<Self, PluginSandboxError> {
        Self::production_with_lanes(config, broker, Arc::new(ExclusiveInvocationLanes::new()))
    }

    /// Creates a production runtime sharing its generic admission lanes with host adapters.
    pub fn production_with_lanes(
        config: PluginRuntimeConfig,
        broker: Arc<dyn HostCapabilityBroker>,
        exclusive_lanes: Arc<ExclusiveInvocationLanes>,
    ) -> Result<Self, PluginSandboxError> {
        let sandbox = ProductionPluginSandbox::new()?;
        Ok(Self::with_broker_sandbox_and_lanes(
            config,
            broker,
            Arc::new(sandbox),
            exclusive_lanes,
        ))
    }

    /// Creates a fail-closed plugin system with explicit process limits.
    ///
    /// Compiled execution is denied until an OS-specific [`PluginSandbox`] is
    /// supplied through [`PluginSystem::with_broker_and_sandbox`].
    ///
    /// # Examples
    /// ```
    /// use lumvise_plugin_runtime::{PluginRuntimeConfig, PluginSystem};
    /// let system = PluginSystem::new(PluginRuntimeConfig::default());
    /// assert!(!system.is_installed("knowledge").expect("catalog available"));
    /// ```
    pub fn new(config: PluginRuntimeConfig) -> Self {
        Self::with_broker_and_sandbox(
            config,
            Arc::new(DenyAllHostCapabilityBroker),
            Arc::new(DenyExecutionSandbox),
        )
    }

    /// Creates a fail-closed plugin system with an injected Host Capability policy.
    ///
    /// This constructor still denies compiled execution. Use
    /// [`PluginSystem::with_broker_and_sandbox`] with an OS-specific sandbox.
    pub fn with_broker(config: PluginRuntimeConfig, broker: Arc<dyn HostCapabilityBroker>) -> Self {
        Self::with_broker_and_sandbox(config, broker, Arc::new(DenyExecutionSandbox))
    }

    /// Creates a deny-execution runtime sharing generic admission lanes with its broker.
    pub fn with_broker_and_lanes(
        config: PluginRuntimeConfig,
        broker: Arc<dyn HostCapabilityBroker>,
        exclusive_lanes: Arc<ExclusiveInvocationLanes>,
    ) -> Self {
        Self::with_broker_sandbox_and_lanes(
            config,
            broker,
            Arc::new(DenyExecutionSandbox),
            exclusive_lanes,
        )
    }

    /// Creates a plugin system with explicit host policy and OS sandbox.
    ///
    /// Production callers must inject an OS-specific [`PluginSandbox`].
    pub fn with_broker_and_sandbox(
        config: PluginRuntimeConfig,
        broker: Arc<dyn HostCapabilityBroker>,
        sandbox: Arc<dyn PluginSandbox>,
    ) -> Self {
        Self::with_broker_sandbox_and_lanes(
            config,
            broker,
            sandbox,
            Arc::new(ExclusiveInvocationLanes::new()),
        )
    }

    /// Creates a runtime with an explicit shared generic admission registry.
    pub fn with_broker_sandbox_and_lanes(
        config: PluginRuntimeConfig,
        broker: Arc<dyn HostCapabilityBroker>,
        sandbox: Arc<dyn PluginSandbox>,
        exclusive_lanes: Arc<ExclusiveInvocationLanes>,
    ) -> Self {
        let maximum_queued = config.maximum_queued_invocations_per_plugin;
        let pipeline_limit = config.max_concurrent_invocations_per_plugin;
        Self {
            config,
            broker,
            sandbox,
            installations: Mutex::new(HashMap::new()),
            invocation_waits: Mutex::new(HashMap::new()),
            active_invocations: ActivePluginInvocations::new(),
            invocation_admission: PluginInvocationAdmission::new(maximum_queued, pipeline_limit),
            exclusive_lanes,
        }
    }

    /// Adds a verified immutable package to the runtime catalog.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::AlreadyInstalled`] for duplicate identity.
    pub fn install(&self, package: &InstalledPlugin) -> Result<(), PluginRuntimeError> {
        let entry = Arc::new(PluginEntry::from_package(package)?);
        let plugin_id = entry.plugin_id().to_owned();
        let mut catalog = self.catalog()?;
        if catalog.contains_key(&plugin_id) {
            return Err(PluginRuntimeError::AlreadyInstalled(plugin_id));
        }
        catalog.insert(plugin_id, entry);
        Ok(())
    }

    /// Starts a plugin and publishes it active only after identity-bound readiness.
    ///
    /// # Errors
    /// Returns lifecycle, spawn, timeout, protocol, or identity errors.
    pub fn start(&self, plugin_id: &str) -> Result<(), PluginRuntimeError> {
        let entry = self.entry(plugin_id)?;
        let mut installation = entry.lifecycle()?;
        require_cataloged(&entry)?;
        if installation.process.is_some() {
            return Err(PluginRuntimeError::AlreadyActive(plugin_id.to_owned()));
        }
        let mut process = self.spawn_and_hello(&entry, &installation)?;
        self.require_ready(&entry, &installation, &mut process)?;
        installation.process = Some(Arc::new(process));
        drop(installation);
        entry.publish();
        Ok(())
    }

    /// Invokes one ready Plugin export through the caller-owned lifecycle contract.
    ///
    /// Queueing and cross-boundary cancellation are enforced by runtime
    /// admission and transport handling; pre-cancelled and expired work never
    /// dispatches.
    ///
    /// The plugin lifecycle mutex is held only long enough to clone the shared
    /// process handle, so up to `max_concurrent_invocations_per_plugin`
    /// invocations multiplex over one process pipe.
    ///
    /// # Errors
    /// Returns a typed [`PluginInvocationError`] retaining the detailed Runtime source.
    ///
    /// # Examples
    /// ```ignore
    /// use std::time::{Duration, Instant};
    /// use lumvise_plugin_runtime::{
    ///     PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest,
    /// };
    /// let context = PluginInvocationContext::new(
    ///     "request-1",
    ///     "mcp-owner-1",
    ///     PluginInvocationClass::Foreground,
    ///     Instant::now() + Duration::from_secs(1),
    /// );
    /// let outcome = system.invoke_controlled(PluginInvocationRequest::new(
    ///     "builtin.knowledge",
    ///     "knowledge.events",
    ///     serde_json::json!({}),
    ///     context,
    /// ))?;
    /// # Ok::<(), lumvise_plugin_runtime::PluginInvocationError>(())
    /// ```
    pub fn start_controlled_invocation(
        &self,
        request: PluginInvocationRequest,
    ) -> Result<PluginInvocationHandle<'_>, PluginInvocationError> {
        request
            .context()
            .ensure_active(request.plugin_id(), self.config.invocation_deadline())
            .map_err(PluginInvocationError::from_runtime)?;
        let cancellation = request.context().cancellation();
        let active: ActiveInvocationGuard<'_> = self
            .active_invocations
            .register(request.plugin_id(), request.context())
            .map_err(PluginInvocationError::from_runtime)?;
        let future = async move {
            let _active = active;
            self.invoke_request_async(request)
                .await
                .map_err(PluginInvocationError::from_runtime)
        };
        Ok(PluginInvocationHandle {
            cancellation,
            future: Box::pin(future),
        })
    }

    /// Synchronously drives [`Self::start_controlled_invocation`] for
    /// background, catalog, and direct callers.
    pub fn invoke_controlled(
        &self,
        request: PluginInvocationRequest,
    ) -> Result<WireOutcome, PluginInvocationError> {
        self.start_controlled_invocation(request)?.blocking_wait()
    }

    /// Cancels one active invocation only when its full owner tuple matches.
    ///
    /// # Errors
    /// Returns an internal typed failure if active-invocation state is unavailable.
    pub fn cancel_controlled(
        &self,
        request: &PluginInvocationCancellationRequest,
    ) -> Result<bool, PluginInvocationError> {
        self.active_invocations
            .cancel(request)
            .map_err(PluginInvocationError::from_runtime)
    }

    async fn invoke_request_async(
        &self,
        request: PluginInvocationRequest,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let (plugin_id, export_id, input, context) = request.into_parts();
        context.ensure_active(&plugin_id, self.config.invocation_deadline())?;
        self.invoke_ready_export_async(&plugin_id, &export_id, input, &context)
            .await
    }

    async fn invoke_ready_export_async(
        &self,
        plugin_id: &str,
        capability_id: &str,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        context.ensure_active(plugin_id, self.config.invocation_deadline())?;
        let entry = self.entry(plugin_id)?;
        let (export, validators) = ready_export(&entry, plugin_id, capability_id)?;
        validators.validate_input(plugin_id, capability_id, &input)?;
        let lane_invocation = self
            .exclusive_lanes
            .admit_async(
                plugin_id,
                export,
                &input,
                context,
                self.config.invocation_deadline(),
            )
            .await?;
        let invocation = PreparedInvocation {
            plugin_id,
            capability_id,
            input,
            validators,
            context,
        };
        self.invoke_admitted_export_async(&entry, invocation, lane_invocation)
            .await
    }

    async fn invoke_admitted_export_async(
        &self,
        entry: &Arc<PluginEntry>,
        invocation: PreparedInvocation<'_>,
        lane_invocation: LaneInvocation,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let plugin_id = invocation.plugin_id;
        let export_id = invocation.capability_id;
        let lane = self.exclusive_lanes.guard(plugin_id, lane_invocation);
        let result = match self
            .invocation_admission
            .admit_async(
                plugin_id,
                export_id,
                invocation.context,
                &self.config.export_concurrency,
                self.config.invocation_deadline(),
            )
            .await
        {
            Ok(_permit) => self.invoke_with_installation_async(entry, invocation).await,
            Err(error) => Err(error),
        };
        lane.finish(&result);
        result
    }

    async fn invoke_with_installation_async(
        &self,
        entry: &Arc<PluginEntry>,
        invocation: PreparedInvocation<'_>,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let plugin_id = invocation.plugin_id;
        let (process, declared_host_capabilities) = {
            let installation = entry.lifecycle()?;
            require_cataloged(entry)?;
            let process = installation
                .process
                .clone()
                .ok_or_else(|| PluginRuntimeError::NotReady(plugin_id.to_owned()))?;
            (process, installation.host_capabilities().clone())
        };
        let result = self
            .invoke_process_async(&process, declared_host_capabilities, invocation)
            .await;
        if result.is_err() {
            match entry.lifecycle() {
                Ok(mut installation) => {
                    terminate_and_unpublish(entry, &mut installation, &process);
                }
                Err(_) => {
                    entry.unpublish();
                    process.terminate();
                }
            }
            self.exclusive_lanes.release_plugin(plugin_id);
        }
        result
    }

    async fn invoke_process_async(
        &self,
        process: &Arc<PluginProcess>,
        declared_host_capabilities: HashMap<String, String>,
        invocation: PreparedInvocation<'_>,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        invocation
            .context
            .ensure_active(invocation.plugin_id, self.config.invocation_deadline())?;
        let invocation_id = next_id("invocation");
        let outcome = process
            .invoke_async(
                invocation.plugin_id,
                &invocation_id,
                invocation.capability_id,
                invocation.input,
                &self.config,
                InvocationAccess {
                    declared_host_capabilities: &declared_host_capabilities,
                    broker: Arc::clone(&self.broker),
                    plugin_system: self,
                    context: invocation.context,
                },
            )
            .await?;
        invocation.validators.validate_output(
            invocation.plugin_id,
            invocation.capability_id,
            &outcome,
        )?;
        Ok(outcome)
    }

    /// Stops a ready process, forcing termination after the shutdown grace.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::NotReady`] when no active process exists.
    pub fn stop(&self, plugin_id: &str) -> Result<(), PluginRuntimeError> {
        let entry = self.entry(plugin_id)?;
        let mut installation = entry.lifecycle()?;
        require_cataloged(&entry)?;
        entry.unpublish();
        self.exclusive_lanes.release_plugin(plugin_id);
        let process = installation
            .process
            .take()
            .ok_or_else(|| PluginRuntimeError::NotReady(plugin_id.to_owned()))?;
        process.stop(plugin_id, &self.config)
    }

    /// Removes a stopped plugin from the runtime catalog.
    ///
    /// Immutable package files remain installer-owned on disk.
    ///
    /// # Errors
    /// Returns an error when the plugin is absent or active.
    pub fn uninstall(&self, plugin_id: &str) -> Result<(), PluginRuntimeError> {
        let entry = self.entry(plugin_id)?;
        let installation = entry.lifecycle()?;
        require_cataloged(&entry)?;
        if installation.process.is_some() {
            return Err(PluginRuntimeError::ActiveUninstall(plugin_id.to_owned()));
        }
        let mut catalog = self.catalog()?;
        let same_entry = catalog
            .get(plugin_id)
            .is_some_and(|cataloged| Arc::ptr_eq(cataloged, &entry));
        if !same_entry {
            return Err(PluginRuntimeError::NotInstalled(plugin_id.to_owned()));
        }
        entry.remove_from_catalog();
        self.exclusive_lanes.release_plugin(plugin_id);
        catalog.remove(plugin_id);
        Ok(())
    }

    /// Reports whether an identity is cataloged.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] after a catalog panic.
    pub fn is_installed(&self, plugin_id: &str) -> Result<bool, PluginRuntimeError> {
        Ok(self.catalog()?.contains_key(plugin_id))
    }

    /// Returns sorted installed identities independently of process readiness.
    ///
    /// Hosts use this snapshot to keep disabled or failed compiled plugins from
    /// silently falling back to a same-identity static implementation.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] after a catalog panic.
    ///
    /// # Examples
    /// ```
    /// use lumvise_plugin_runtime::{PluginRuntimeConfig, PluginSystem};
    /// let system = PluginSystem::new(PluginRuntimeConfig::default());
    /// assert!(system.cataloged_plugin_ids()?.is_empty());
    /// # Ok::<(), lumvise_plugin_runtime::PluginRuntimeError>(())
    /// ```
    pub fn cataloged_plugin_ids(&self) -> Result<Vec<String>, PluginRuntimeError> {
        let mut plugin_ids = self.catalog()?.keys().cloned().collect::<Vec<_>>();
        plugin_ids.sort();
        Ok(plugin_ids)
    }

    /// Reports whether a process completed its ready handshake.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] after a catalog panic.
    pub fn is_active(&self, plugin_id: &str) -> Result<bool, PluginRuntimeError> {
        Ok(self
            .catalog()?
            .get(plugin_id)
            .is_some_and(|entry| entry.is_ready()))
    }

    /// Returns signed generic exports only while the plugin is ready.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::NotReady`] before start and after removal.
    pub fn exports(&self, plugin_id: &str) -> Result<Vec<ExportDescriptor>, PluginRuntimeError> {
        let entry = self.entry(plugin_id)?;
        if !entry.is_ready() {
            return Err(PluginRuntimeError::NotReady(plugin_id.to_owned()));
        }
        Ok(entry.exports().to_vec())
    }

    /// Returns a ready-only snapshot without waiting for plugin lifecycle locks.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] after a catalog panic.
    pub fn published_plugins(&self) -> Result<Vec<PublishedPlugin>, PluginRuntimeError> {
        self.with_catalog_entries(|entries| {
            entries
                .into_iter()
                .filter(|entry| entry.is_ready())
                .map(|entry| PublishedPlugin {
                    plugin_id: entry.plugin_id().to_owned(),
                    exports: entry.exports().to_vec(),
                })
                .collect()
        })
    }

    /// Returns signed exports for every installed plugin, independent of process readiness.
    pub fn cataloged_plugins(&self) -> Result<Vec<PublishedPlugin>, PluginRuntimeError> {
        self.with_catalog_entries(|entries| {
            entries
                .into_iter()
                .map(|entry| PublishedPlugin {
                    plugin_id: entry.plugin_id().to_owned(),
                    exports: entry.exports().to_vec(),
                })
                .collect()
        })
    }

    /// Runs a catalog observation against an Arc snapshot, never while the catalog mutex is held.
    fn with_catalog_entries<T>(
        &self,
        observe: impl FnOnce(Vec<Arc<PluginEntry>>) -> T,
    ) -> Result<T, PluginRuntimeError> {
        let entries = {
            let catalog = self.catalog()?;
            catalog.values().cloned().collect::<Vec<_>>()
        };
        Ok(observe(entries))
    }

    /// Returns sorted process identities for ready supervised plugins.
    ///
    /// This observation does not expose lifecycle mutation or process handles.
    /// A plugin racing with stop is omitted from the snapshot.
    ///
    /// # Errors
    /// Returns catalog or plugin lifecycle lock errors.
    ///
    /// # Examples
    /// ```
    /// use lumvise_plugin_runtime::{PluginRuntimeConfig, PluginSystem};
    /// let system = PluginSystem::new(PluginRuntimeConfig::default());
    /// assert!(system.active_processes()?.is_empty());
    /// # Ok::<(), lumvise_plugin_runtime::PluginRuntimeError>(())
    /// ```
    pub fn active_processes(&self) -> Result<Vec<ActivePluginProcess>, PluginRuntimeError> {
        let entries = self.catalog()?.values().cloned().collect::<Vec<_>>();
        let mut processes = entries
            .into_iter()
            .filter(|entry| entry.is_ready())
            .filter_map(|entry| active_process(&entry).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        processes.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        Ok(processes)
    }

    /// Returns ready-only signed exports for one generic scoped MCP channel.
    ///
    /// Stopped, failed, and uninstalled processes are excluded. The caller receives plugin
    /// identities with only exports whose signed scope exactly matches `scope`.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] after a catalog panic.
    pub fn scoped_exports(&self, scope: &str) -> Result<Vec<PublishedPlugin>, PluginRuntimeError> {
        Ok(self
            .published_plugins()?
            .into_iter()
            .filter_map(|plugin| filter_scoped_plugin(plugin, scope))
            .collect())
    }

    /// Returns Plugin-local execution, queue, and terminal admission counters.
    ///
    /// This snapshot never takes the Plugin process lifecycle lock.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::InvocationAdmissionPoisoned`] after an admission panic.
    pub fn invocation_admission_snapshot(
        &self,
        plugin_id: &str,
    ) -> Result<PluginAdmissionSnapshot, PluginRuntimeError> {
        self.invocation_admission.snapshot(plugin_id)
    }

    fn catalog(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, Arc<PluginEntry>>>, PluginRuntimeError>
    {
        self.installations
            .lock()
            .map_err(|_| PluginRuntimeError::CatalogPoisoned)
    }

    fn entry(&self, plugin_id: &str) -> Result<Arc<PluginEntry>, PluginRuntimeError> {
        self.catalog()?
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginRuntimeError::NotInstalled(plugin_id.to_owned()))
    }

    fn spawn_and_hello(
        &self,
        entry: &PluginEntry,
        installation: &PluginInstallation,
    ) -> Result<PluginProcess, PluginRuntimeError> {
        let session_id = next_id("session");
        let command = self
            .sandbox
            .prepare_command(PluginSandboxRequest {
                plugin_id: entry.plugin_id(),
                package_digest: installation.package_digest(),
                package_root: installation.package_root(),
                executable: installation.executable(),
                executable_sha256: installation.executable_sha256(),
            })
            .map_err(|source| PluginRuntimeError::Sandbox {
                plugin_id: entry.plugin_id().to_owned(),
                source,
            })?;
        let process = PluginProcess::spawn(
            command,
            installation.executable(),
            entry.plugin_id(),
            session_id.clone(),
        )?;
        let hello = WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body: MessageBody::HostHello {
                session_id,
                host_id: self.config.host_id.clone(),
                package_digest: installation.package_digest().to_owned(),
            },
        };
        process.send(&hello, entry.plugin_id())?;
        Ok(process)
    }

    fn require_ready(
        &self,
        entry: &PluginEntry,
        installation: &PluginInstallation,
        process: &mut PluginProcess,
    ) -> Result<(), PluginRuntimeError> {
        let message =
            match process.receive_handshake(entry.plugin_id(), self.config.handshake_timeout) {
                Ok(message) => message,
                Err(error) => {
                    process.terminate();
                    return Err(error);
                }
            };
        match message.body {
            MessageBody::PluginReady { ref session_id, .. }
                if session_id != process.session_id() =>
            {
                let error = PluginRuntimeError::ReadySessionMismatch {
                    plugin_id: entry.plugin_id().to_owned(),
                    expected: process.session_id().to_owned(),
                    actual: session_id.to_owned(),
                };
                process.terminate();
                Err(error)
            }
            MessageBody::PluginReady {
                plugin_id,
                package_digest,
                ..
            } if plugin_id == entry.plugin_id()
                && package_digest == installation.package_digest() =>
            {
                Ok(())
            }
            MessageBody::PluginReady {
                plugin_id,
                package_digest,
                ..
            } => {
                let error = PluginRuntimeError::ReadyIdentityMismatch {
                    expected_id: entry.plugin_id().to_owned(),
                    expected_digest: installation.package_digest().to_owned(),
                    actual_id: plugin_id,
                    actual_digest: package_digest,
                };
                process.terminate();
                Err(error)
            }
            other => {
                let error = PluginRuntimeError::Protocol {
                    plugin_id: entry.plugin_id().to_owned(),
                    message: format!("unexpected handshake message `{other:?}`"),
                    stderr: process.diagnostics(),
                };
                process.terminate();
                Err(error)
            }
        }
    }
}

fn active_process(entry: &PluginEntry) -> Result<Option<ActivePluginProcess>, PluginRuntimeError> {
    let installation = entry.lifecycle()?;
    Ok(installation
        .process
        .as_ref()
        .map(|process| ActivePluginProcess {
            plugin_id: entry.plugin_id().to_owned(),
            process_id: process.process_id(),
        }))
}

fn filter_scoped_plugin(plugin: PublishedPlugin, scope: &str) -> Option<PublishedPlugin> {
    let exports = plugin
        .exports
        .into_iter()
        .filter(|export| {
            matches!(&export.surface,
            lumvise_plugin_package::ExportSurface::ScopedMcpTool { scope: signed }
                if signed == scope)
        })
        .collect::<Vec<_>>();
    (!exports.is_empty()).then_some(PublishedPlugin {
        plugin_id: plugin.plugin_id,
        exports,
    })
}

impl Default for PluginSystem {
    fn default() -> Self {
        Self::new(PluginRuntimeConfig::default())
    }
}

fn ready_export<'a>(
    entry: &'a PluginEntry,
    plugin_id: &str,
    capability_id: &str,
) -> Result<(&'a ExportDescriptor, &'a ExportValidators), PluginRuntimeError> {
    let missing_export = || PluginRuntimeError::UndeclaredExport {
        plugin_id: plugin_id.to_owned(),
        capability_id: capability_id.to_owned(),
    };
    let export = entry
        .exports()
        .iter()
        .find(|export| export.id == capability_id)
        .ok_or_else(&missing_export)?;
    let validators = entry.validators(capability_id).ok_or_else(missing_export)?;
    Ok((export, validators))
}

fn require_cataloged(entry: &PluginEntry) -> Result<(), PluginRuntimeError> {
    if entry.is_cataloged() {
        return Ok(());
    }
    Err(PluginRuntimeError::NotInstalled(
        entry.plugin_id().to_owned(),
    ))
}

/// Terminates the process an invocation ran on and unpublishes the plugin.
///
/// When the installation already swapped to a newer process (stop + restart
/// raced with the failing invocation), the stale failure leaves the healthy
/// process and its publication untouched.
fn terminate_and_unpublish(
    entry: &PluginEntry,
    installation: &mut PluginInstallation,
    invoked: &Arc<PluginProcess>,
) {
    let Some(process) = installation.process.take() else {
        entry.unpublish();
        return;
    };
    if Arc::ptr_eq(&process, invoked) {
        entry.unpublish();
        process.terminate();
    } else {
        installation.process = Some(process);
    }
}

fn next_id(prefix: &str) -> String {
    let sequence = NEXT_CORRELATION_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests;
