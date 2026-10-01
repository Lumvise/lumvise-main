mod control;
mod discovery;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use discovery::{RuntimeDiscovery, default_root, discovery_path};
use fs4::fs_std::FileExt;
use lumvise_mcp_core::{
    APP_RUNTIME_CONTROL_PROTOCOL_MAJOR, RuntimeControlKindV1, RuntimeControlRequestV1,
    RuntimeControlResponseV1, RuntimeControlStateV1,
};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUNTIME_NAME: &str = "lumvise";
const EXPLICIT_QUIT_MARKER_PREFIX: &str = "explicit-quit-";
const BRIDGE_CREDENTIAL_TTL_MS: u64 = 60_000;
const POLL_INTERVAL: Duration = Duration::from_millis(40);

/// Request used when the desktop is activated or a broker asks it to start.
#[derive(Clone, Debug, Default)]
pub struct ActivationRequest {
    /// Optional arguments forwarded to the desktop activation adapter.
    pub arguments: Vec<String>,
    /// Broker start: joins an existing generation without foregrounding its UI.
    pub background: bool,
}

/// Request for the orderly Runtime shutdown sequence.
#[derive(Clone, Debug, Default)]
pub struct QuitRequest {
    /// Whether active bridge work should be rejected immediately.
    pub reject_new_work: bool,
}

/// A bounded launcher used by brokers and tests without exposing OS details.
pub trait RuntimeLauncher: Send + Sync {
    /// Starts or activates the desktop executable.
    fn launch(&self, request: ActivationRequest) -> Result<(), RuntimeCoordinatorError>;
}

impl<F> RuntimeLauncher for F
where
    F: Fn(ActivationRequest) -> Result<(), RuntimeCoordinatorError> + Send + Sync,
{
    fn launch(&self, request: ActivationRequest) -> Result<(), RuntimeCoordinatorError> {
        self(request)
    }
}

/// App-owned reactions to authenticated Runtime control requests.
pub trait RuntimeControlPort: Send + Sync {
    /// Shows and focuses the existing desktop for a repeated launch.
    fn activate(&self, arguments: Vec<String>) -> Result<(), String>;
    /// Requests orderly desktop shutdown.
    fn quit(&self) -> Result<(), String>;
}

enum RuntimeControlEvent {
    Activate(Vec<String>),
    Quit,
}

#[derive(Default)]
struct RuntimeControlEvents {
    state: Mutex<RuntimeControlEventsState>,
}

#[derive(Default)]
struct RuntimeControlEventsState {
    port: Option<Arc<dyn RuntimeControlPort>>,
    queued: Vec<RuntimeControlEvent>,
}

impl RuntimeControlEvents {
    fn install(&self, port: Arc<dyn RuntimeControlPort>) -> Result<(), String> {
        let queued = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "runtime control event lock poisoned".to_string())?;
            state.port = Some(Arc::clone(&port));
            std::mem::take(&mut state.queued)
        };
        for event in queued {
            dispatch_control_event(port.as_ref(), event)?;
        }
        Ok(())
    }

    fn dispatch(&self, event: RuntimeControlEvent) -> Result<(), String> {
        let port = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "runtime control event lock poisoned".to_string())?;
            let Some(port) = state.port.as_ref() else {
                state.queued.push(event);
                return Ok(());
            };
            Arc::clone(port)
        };
        dispatch_control_event(port.as_ref(), event)
    }
}

fn dispatch_control_event(
    port: &dyn RuntimeControlPort,
    event: RuntimeControlEvent,
) -> Result<(), String> {
    match event {
        RuntimeControlEvent::Activate(arguments) => port.activate(arguments),
        RuntimeControlEvent::Quit => port.quit(),
    }
}

/// Per-user coordinator for lease, readiness, discovery, and ordered shutdown.
#[derive(Clone)]
pub struct AppRuntimeCoordinator {
    root: Arc<PathBuf>,
    launcher: Arc<dyn RuntimeLauncher>,
    observed_generation: Arc<Mutex<Option<String>>>,
}

/// Result of trying to become the desktop owner.
pub enum AcquireResult {
    /// This process owns the generation and must retain the lease.
    Owner(OwnerLease),
    /// Another generation accepted the activation request.
    Forwarded(RuntimeConnection),
}

/// Non-cloneable lease retaining the OS-user ownership authority.
pub struct OwnerLease {
    coordinator: AppRuntimeCoordinator,
    generation_nonce: String,
    owner_credential: String,
    control_endpoint: String,
    lease_file: Option<File>,
    control_events: Arc<RuntimeControlEvents>,
    runtime_state: Arc<AtomicI32>,
    bridge_credentials: Arc<Mutex<HashMap<String, u64>>>,
    shutdown: Arc<AtomicBool>,
    control_thread: Option<thread::JoinHandle<()>>,
    quit_started: bool,
    app_bridge_grant: Option<(String, u64)>,
}

/// Generation-bound connection returned after authenticated readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConnection {
    /// Current generation nonce; never reuse across crashes.
    pub generation_nonce: String,
    /// Short-lived credential required by every App Bridge request.
    pub app_bridge_credential: String,
    /// Absolute expiry for `app_bridge_credential`.
    pub app_bridge_credential_expires_unix_ms: u64,
    /// Credentialed HTTP bridge base URL.
    pub app_bridge_base_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeCoordinatorError {
    #[error("runtime ownership unavailable")]
    OwnershipUnavailable,
    #[error("runtime discovery is invalid or stale")]
    InvalidDiscovery,
    #[error("runtime operation deadline elapsed")]
    DeadlineElapsed,
    #[error("runtime operation cancelled")]
    Cancelled,
    #[error("runtime is quitting")]
    Quitting,
    #[error("runtime control failed: {0}")]
    Control(String),
    #[error("runtime filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
}
impl AppRuntimeCoordinator {
    /// Constructs a coordinator. `runtime_root` is intentionally injectable for tests.
    pub fn new(runtime_root: impl Into<PathBuf>, launcher: impl RuntimeLauncher + 'static) -> Self {
        Self {
            root: Arc::new(runtime_root.into()),
            launcher: Arc::new(launcher),
            observed_generation: Arc::new(Mutex::new(None)),
        }
    }

    /// Constructs a production coordinator with the platform launcher.
    pub fn production() -> Self {
        Self::new(default_root(), launch_desktop)
    }

    /// Returns the private runtime root used by this coordinator.
    pub fn runtime_root(&self) -> &Path {
        self.root.as_path()
    }
    /// Returns the protected owner credential path for first-party native control clients.
    pub fn owner_credential_path(&self) -> PathBuf {
        owner_credential_path(self.runtime_root())
    }
    fn remember_observed_generation(
        &self,
        generation_nonce: &str,
    ) -> Result<(), RuntimeCoordinatorError> {
        *self.observed_generation.lock().map_err(|_| {
            RuntimeCoordinatorError::Control("observed generation lock poisoned".into())
        })? = Some(generation_nonce.to_owned());
        Ok(())
    }

    fn observed_generation_quit(&self) -> Result<bool, RuntimeCoordinatorError> {
        let observed = self
            .observed_generation
            .lock()
            .map_err(|_| {
                RuntimeCoordinatorError::Control("observed generation lock poisoned".into())
            })?
            .clone();
        Ok(observed.as_deref().is_some_and(|generation| {
            explicit_quit_marker_matches(self.runtime_root(), generation)
        }))
    }

    /// Acquires the lease or forwards activation to the current owner.
    pub fn acquire_or_forward(
        &self,
        request: ActivationRequest,
    ) -> Result<AcquireResult, RuntimeCoordinatorError> {
        prepare_runtime_root(self.runtime_root())?;
        let lease_path = lease_path(self.runtime_root());
        let lease_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lease_path)?;
        restrict_owner_access(&lease_path)?;
        match lease_file.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => {
                let connection = self.forward_activation(request)?;
                return Ok(AcquireResult::Forwarded(connection));
            }
            Err(error) => return Err(error.into()),
        }
        let generation_nonce = random_token();
        let owner_credential = random_token();
        write_owner_credential(self.runtime_root(), &generation_nonce, &owner_credential)?;
        let (listener, endpoint) = control::bind(self.runtime_root(), &generation_nonce)?;
        let record = RuntimeDiscovery {
            schema_version: discovery::DISCOVERY_SCHEMA_VERSION,
            runtime_name: RUNTIME_NAME.into(),
            generation_nonce: generation_nonce.clone(),
            control_endpoint: endpoint.clone(),
            app_bridge_base_url: None,
            state: "starting".into(),
            pid: std::process::id(),
            process_start: now_string(),
        };
        let discovery_file = discovery_path(self.runtime_root());
        discovery::publish(&discovery_file, &record)?;
        restrict_discovery_access(&discovery_file)?;
        let runtime_state = Arc::new(AtomicI32::new(RuntimeControlStateV1::Starting as i32));
        let shutdown = Arc::new(AtomicBool::new(false));
        let control_events = Arc::new(RuntimeControlEvents::default());
        let bridge_credentials = Arc::new(Mutex::new(HashMap::new()));
        let control_thread = spawn_control_thread(
            listener,
            Arc::clone(&shutdown),
            generation_nonce.clone(),
            owner_credential.clone(),
            Arc::clone(&runtime_state),
            Arc::clone(&bridge_credentials),
            Arc::clone(&control_events),
        );
        Ok(AcquireResult::Owner(OwnerLease {
            coordinator: self.clone(),
            generation_nonce,
            owner_credential,
            control_endpoint: endpoint,
            control_events,
            runtime_state,
            bridge_credentials,
            shutdown,
            control_thread: Some(control_thread),
            lease_file: Some(lease_file),
            quit_started: false,
            app_bridge_grant: None,
        }))
    }

    /// Checks the current generation without launching or waiting for app initialization.
    ///
    /// Example: `let ready = coordinator.try_ready_connection()?;`
    pub fn try_ready_connection(
        &self,
    ) -> Result<Option<RuntimeConnection>, RuntimeCoordinatorError> {
        if self.observed_generation_quit()? {
            return Err(RuntimeCoordinatorError::Quitting);
        }
        let connection = self.ready_connection();
        if let Some(connection) = &connection {
            self.remember_observed_generation(&connection.generation_nonce)?;
        }
        Ok(connection)
    }

    pub fn ensure_ready(
        &self,
        request: ActivationRequest,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<RuntimeConnection, RuntimeCoordinatorError> {
        if let Some(connection) = self.try_ready_connection()? {
            return Ok(connection);
        }
        self.launcher.launch(request)?;
        while Instant::now() < deadline {
            if cancelled.load(Ordering::Acquire) {
                return Err(RuntimeCoordinatorError::Cancelled);
            }
            if let Some(connection) = self.try_ready_connection()? {
                return Ok(connection);
            }
            thread::sleep(POLL_INTERVAL);
        }
        Err(RuntimeCoordinatorError::DeadlineElapsed)
    }

    fn ready_connection(&self) -> Option<RuntimeConnection> {
        let path = discovery_path(self.runtime_root());
        let record = discovery::read(&path)?;
        if record.state != "ready" {
            return None;
        }
        let owner_credential =
            read_owner_credential(self.runtime_root(), &record.generation_nonce)?;
        let mut stream = control::connect(&record.control_endpoint).ok()?;
        let response = send_control(
            &mut stream,
            authenticated_request(
                &record,
                &owner_credential,
                RuntimeControlKindV1::Handshake,
                Vec::new(),
            ),
        )
        .ok()?;
        if !response.accepted
            || response.protocol_major != APP_RUNTIME_CONTROL_PROTOCOL_MAJOR
            || response.generation_nonce != record.generation_nonce
            || response.state != RuntimeControlStateV1::Ready as i32
        {
            return None;
        }
        Some(RuntimeConnection {
            generation_nonce: record.generation_nonce,
            app_bridge_credential: (!response.app_bridge_credential.is_empty())
                .then_some(response.app_bridge_credential)?,
            app_bridge_credential_expires_unix_ms: (response.credential_expires_unix_ms
                > epoch_ms())
            .then_some(response.credential_expires_unix_ms)?,
            app_bridge_base_url: record.app_bridge_base_url?,
        })
    }

    fn forward_activation(
        &self,
        request: ActivationRequest,
    ) -> Result<RuntimeConnection, RuntimeCoordinatorError> {
        let record = discovery::read(&discovery_path(self.runtime_root()))
            .ok_or(RuntimeCoordinatorError::InvalidDiscovery)?;
        let owner_credential = read_owner_credential(self.runtime_root(), &record.generation_nonce)
            .ok_or(RuntimeCoordinatorError::InvalidDiscovery)?;
        let mut handshake_stream = control::connect(&record.control_endpoint)
            .map_err(|error| RuntimeCoordinatorError::Control(error.to_string()))?;
        let handshake = send_control(
            &mut handshake_stream,
            authenticated_request(
                &record,
                &owner_credential,
                RuntimeControlKindV1::Handshake,
                Vec::new(),
            ),
        )?;
        if !handshake.accepted {
            return Err(RuntimeCoordinatorError::Control(handshake.message));
        }
        let activation_deadline = Instant::now() + Duration::from_secs(60);
        if !request.background {
            let mut stream = control::connect(&record.control_endpoint)
                .map_err(|error| RuntimeCoordinatorError::Control(error.to_string()))?;
            let response = send_control(
                &mut stream,
                authenticated_request(
                    &record,
                    &owner_credential,
                    RuntimeControlKindV1::Activate,
                    request.arguments,
                ),
            )?;
            if !response.accepted {
                return Err(RuntimeCoordinatorError::Control(response.message));
            }
        }
        while Instant::now() < activation_deadline {
            if let Some(connection) = self.ready_connection()
                && connection.generation_nonce == record.generation_nonce
            {
                return Ok(connection);
            }
            thread::sleep(POLL_INTERVAL);
        }
        Err(RuntimeCoordinatorError::DeadlineElapsed)
    }
}

fn authenticated_request(
    record: &RuntimeDiscovery,
    owner_credential: &str,
    kind: RuntimeControlKindV1,
    activation_arguments: Vec<String>,
) -> RuntimeControlRequestV1 {
    RuntimeControlRequestV1 {
        protocol_major: APP_RUNTIME_CONTROL_PROTOCOL_MAJOR,
        request_id: random_token(),
        generation_nonce: record.generation_nonce.clone(),
        owner_credential: owner_credential.into(),
        kind: kind as i32,
        activation_arguments,
        deadline_unix_ms: epoch_ms().saturating_add(60_000),
    }
}

impl OwnerLease {
    /// Installs App-owned lifecycle behavior and replays startup-time requests.
    pub fn install_control_port(
        &self,
        port: Arc<dyn RuntimeControlPort>,
    ) -> Result<(), RuntimeCoordinatorError> {
        self.control_events
            .install(port)
            .map_err(RuntimeCoordinatorError::Control)
    }

    /// Shares the generation grant registry with App Bridge authentication.
    pub fn bridge_credential_store(&self) -> Arc<Mutex<HashMap<String, u64>>> {
        Arc::clone(&self.bridge_credentials)
    }

    /// Allocates the first short-lived generation-bound App Bridge credential before readiness.
    pub fn prepare_bridge_credential(&mut self) -> Result<(String, u64), RuntimeCoordinatorError> {
        if let Some(grant) = self.app_bridge_grant.as_ref() {
            return Ok(grant.clone());
        }
        let credential = random_token();
        let expires_unix_ms = epoch_ms().saturating_add(BRIDGE_CREDENTIAL_TTL_MS);
        self.bridge_credentials
            .lock()
            .map_err(|_| {
                RuntimeCoordinatorError::Control("bridge credential lock poisoned".into())
            })?
            .insert(credential.clone(), expires_unix_ms);
        self.app_bridge_grant = Some((credential.clone(), expires_unix_ms));
        Ok((credential, expires_unix_ms))
    }

    /// Marks the generation ready after the bridge has enforced its credential.
    pub fn mark_ready(
        &mut self,
        app_bridge_base_url: impl Into<String>,
    ) -> Result<RuntimeConnection, RuntimeCoordinatorError> {
        let path = discovery_path(self.coordinator.runtime_root());
        let mut record = discovery::read(&path).ok_or(RuntimeCoordinatorError::InvalidDiscovery)?;
        if record.generation_nonce != self.generation_nonce {
            return Err(RuntimeCoordinatorError::InvalidDiscovery);
        }
        let (credential, expires_unix_ms) = match self.app_bridge_grant.clone() {
            Some(grant) => grant,
            None => self.prepare_bridge_credential()?,
        };
        let app_bridge_base_url = app_bridge_base_url.into();
        record.state = "ready".into();
        record.app_bridge_base_url = Some(app_bridge_base_url.clone());
        discovery::publish(&path, &record)?;
        restrict_discovery_access(&path)?;
        self.runtime_state
            .store(RuntimeControlStateV1::Ready as i32, Ordering::Release);
        Ok(RuntimeConnection {
            generation_nonce: self.generation_nonce.clone(),
            app_bridge_credential: credential,
            app_bridge_credential_expires_unix_ms: expires_unix_ms,
            app_bridge_base_url,
        })
    }

    /// Begins the explicit ordered quit sequence.
    pub fn begin_quit(&mut self, _request: QuitRequest) -> Result<(), RuntimeCoordinatorError> {
        if self.quit_started {
            return Ok(());
        }
        write_explicit_quit_marker(self.coordinator.runtime_root(), &self.generation_nonce)?;
        self.coordinator
            .remember_observed_generation(&self.generation_nonce)?;
        self.quit_started = true;
        self.runtime_state
            .store(RuntimeControlStateV1::Quitting as i32, Ordering::Release);
        let path = discovery_path(self.coordinator.runtime_root());
        if let Some(mut record) = discovery::read(&path)
            && record.generation_nonce == self.generation_nonce
        {
            record.state = "quitting".into();
            discovery::publish(&path, &record)?;
            restrict_discovery_access(&path)?;
        }
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.control_thread.take() {
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            } else {
                std::mem::forget(thread);
            }
        }
        if let Ok(mut credentials) = self.bridge_credentials.lock() {
            credentials.clear();
        }
        control::remove_endpoint(&self.control_endpoint);
        discovery::clear_if_generation(&path, &self.generation_nonce)?;
        remove_owner_credential_if_generation(
            self.coordinator.runtime_root(),
            &self.generation_nonce,
        );
        self.runtime_state
            .store(RuntimeControlStateV1::Exited as i32, Ordering::Release);
        self.lease_file.take();
        Ok(())
    }

    /// Returns the generation metadata used to correlate control and bridge sessions.
    pub fn generation_nonce(&self) -> &str {
        &self.generation_nonce
    }

    /// Returns the private owner credential for owner-side native handshakes.
    pub fn owner_credential(&self) -> &str {
        &self.owner_credential
    }
}

impl Drop for OwnerLease {
    fn drop(&mut self) {
        if self.quit_started {
            return;
        }
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.control_thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut credentials) = self.bridge_credentials.lock() {
            credentials.clear();
        }
        control::remove_endpoint(&self.control_endpoint);
        remove_owner_credential_if_generation(
            self.coordinator.runtime_root(),
            &self.generation_nonce,
        );
        self.runtime_state
            .store(RuntimeControlStateV1::Exited as i32, Ordering::Release);
        self.lease_file.take();
    }
}

fn spawn_control_thread(
    listener: control::ControlListener,
    shutdown: Arc<AtomicBool>,
    generation_nonce: String,
    owner_credential: String,
    runtime_state: Arc<AtomicI32>,
    bridge_credentials: Arc<Mutex<HashMap<String, u64>>>,
    control_events: Arc<RuntimeControlEvents>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown.load(Ordering::Acquire) {
            match control::accept(&listener) {
                Ok(stream) => serve_control(
                    stream,
                    &generation_nonce,
                    &owner_credential,
                    &runtime_state,
                    &bridge_credentials,
                    &control_events,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(_) => thread::sleep(POLL_INTERVAL),
            }
        }
    })
}

fn serve_control(
    mut stream: control::ControlStream,
    generation_nonce: &str,
    owner_credential: &str,
    runtime_state: &AtomicI32,
    bridge_credentials: &Mutex<HashMap<String, u64>>,
    control_events: &RuntimeControlEvents,
) {
    control::configure_stream(&stream);
    let Ok(request) = control::read_frame::<RuntimeControlRequestV1>(&mut stream) else {
        return;
    };
    let kind = RuntimeControlKindV1::try_from(request.kind).ok();
    let mut response = control_response(
        &request,
        generation_nonce,
        owner_credential,
        runtime_state,
        bridge_credentials,
    );
    if response.accepted && matches!(kind, Some(RuntimeControlKindV1::Quit)) {
        runtime_state.store(RuntimeControlStateV1::Quitting as i32, Ordering::Release);
        response.state = RuntimeControlStateV1::Quitting as i32;
    }
    if response.accepted && matches!(kind, Some(RuntimeControlKindV1::Quit)) {
        let _ = control::write_frame(&mut stream, &response);
        let _ = control_events.dispatch(RuntimeControlEvent::Quit);
        return;
    }
    if response.accepted
        && let Some(event) = control_event(&request)
        && let Err(error) = control_events.dispatch(event)
    {
        response.error_code = "foreground_failed".into();
        response.message = error;
        response.foreground_succeeded = false;
    } else if response.accepted
        && matches!(kind, Some(RuntimeControlKindV1::Activate))
        && response.state == RuntimeControlStateV1::Ready as i32
    {
        response.foreground_succeeded = true;
    }
    let _ = control::write_frame(&mut stream, &response);
}

fn control_event(request: &RuntimeControlRequestV1) -> Option<RuntimeControlEvent> {
    match RuntimeControlKindV1::try_from(request.kind).ok()? {
        RuntimeControlKindV1::Activate => Some(RuntimeControlEvent::Activate(
            request.activation_arguments.clone(),
        )),
        RuntimeControlKindV1::Quit => Some(RuntimeControlEvent::Quit),
        RuntimeControlKindV1::Handshake | RuntimeControlKindV1::State => None,
    }
}

fn control_response(
    request: &RuntimeControlRequestV1,
    generation_nonce: &str,
    owner_credential: &str,
    runtime_state: &AtomicI32,
    bridge_credentials: &Mutex<HashMap<String, u64>>,
) -> RuntimeControlResponseV1 {
    let state = RuntimeControlStateV1::try_from(runtime_state.load(Ordering::Acquire))
        .unwrap_or(RuntimeControlStateV1::Exited);
    let mut response = RuntimeControlResponseV1 {
        protocol_major: APP_RUNTIME_CONTROL_PROTOCOL_MAJOR,
        request_id: request.request_id.clone(),
        generation_nonce: generation_nonce.into(),
        state: state as i32,
        accepted: false,
        error_code: String::new(),
        message: String::new(),
        app_bridge_base_url: String::new(),
        app_bridge_credential: String::new(),
        retry_after_ms: 0,
        foreground_succeeded: false,
        credential_expires_unix_ms: 0,
    };
    if request.protocol_major != APP_RUNTIME_CONTROL_PROTOCOL_MAJOR {
        response.error_code = "wrong_major".into();
        response.message = "unsupported runtime control protocol major".into();
        return response;
    }
    if request.generation_nonce != generation_nonce || request.owner_credential != owner_credential
    {
        response.error_code = "auth_failed".into();
        response.message = "runtime control authentication failed".into();
        return response;
    }
    if request.deadline_unix_ms != 0 && request.deadline_unix_ms < epoch_ms() {
        response.error_code = "deadline_exceeded".into();
        response.message = "runtime control request deadline elapsed".into();
        return response;
    }
    let Some(kind) = RuntimeControlKindV1::try_from(request.kind).ok() else {
        response.error_code = "invalid_kind".into();
        response.message = "unknown runtime control command".into();
        return response;
    };
    if matches!(
        state,
        RuntimeControlStateV1::Quitting | RuntimeControlStateV1::Exited
    ) {
        response.error_code = "state_rejected".into();
        response.message = "runtime is no longer accepting work".into();
        return response;
    }
    response.accepted = true;
    response.message = "accepted".into();
    if matches!(kind, RuntimeControlKindV1::Handshake) && state == RuntimeControlStateV1::Ready {
        let credential = random_token();
        let expires_unix_ms = epoch_ms().saturating_add(BRIDGE_CREDENTIAL_TTL_MS);
        if let Ok(mut credentials) = bridge_credentials.lock() {
            credentials.retain(|_, expires| *expires >= epoch_ms());
            credentials.insert(credential.clone(), expires_unix_ms);
        }
        response.app_bridge_credential = credential;
        response.credential_expires_unix_ms = expires_unix_ms;
        if response.app_bridge_credential.is_empty() {
            response.accepted = false;
            response.error_code = "state_rejected".into();
            response.message = "runtime bridge credential is not ready".into();
        }
    }
    response
}

fn send_control(
    stream: &mut control::ControlStream,
    request: RuntimeControlRequestV1,
) -> Result<RuntimeControlResponseV1, RuntimeCoordinatorError> {
    control::write_frame(stream, &request)?;
    control::read_frame(stream).map_err(RuntimeCoordinatorError::from)
}

fn owner_credential_path(root: &Path) -> PathBuf {
    root.join("owner.credential")
}

fn write_owner_credential(
    root: &Path,
    generation_nonce: &str,
    owner_credential: &str,
) -> std::io::Result<()> {
    let path = owner_credential_path(root);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    restrict_owner_access(&path)?;
    file.write_all(format!("{generation_nonce}\n{owner_credential}\n").as_bytes())?;
    file.sync_all()
}

fn read_owner_credential(root: &Path, generation_nonce: &str) -> Option<String> {
    let mut body = String::new();
    File::open(owner_credential_path(root))
        .ok()?
        .read_to_string(&mut body)
        .ok()?;
    let mut lines = body.lines();
    (lines.next()? == generation_nonce)
        .then(|| lines.next().map(str::to_string))
        .flatten()
}

fn remove_owner_credential_if_generation(root: &Path, generation_nonce: &str) {
    if read_owner_credential(root, generation_nonce).is_some() {
        let _ = fs::remove_file(owner_credential_path(root));
    }
}

fn explicit_quit_marker_path(root: &Path, generation_nonce: &str) -> PathBuf {
    root.join(format!(
        "{EXPLICIT_QUIT_MARKER_PREFIX}{generation_nonce}.marker"
    ))
}

fn write_explicit_quit_marker(root: &Path, generation_nonce: &str) -> std::io::Result<()> {
    let path = explicit_quit_marker_path(root, generation_nonce);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    restrict_owner_access(&path)?;
    file.write_all(generation_nonce.as_bytes())?;
    file.sync_all()
}

fn explicit_quit_marker_matches(root: &Path, generation_nonce: &str) -> bool {
    let mut marker = String::new();
    File::open(explicit_quit_marker_path(root, generation_nonce))
        .ok()
        .and_then(|mut file| file.read_to_string(&mut marker).ok())
        .is_some_and(|_| marker.trim() == generation_nonce)
}

#[cfg(unix)]
fn prepare_runtime_root(root: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if !root.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(root)?;
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let mode = fs::metadata(root)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "runtime root must be private to the current user",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_runtime_root(root: &Path) -> std::io::Result<()> {
    windows::prepare_runtime_root(root)
}

#[cfg(unix)]
fn restrict_owner_access(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_owner_access(path: &Path) -> std::io::Result<()> {
    windows::restrict_owner_access(path)
}

#[cfg(windows)]
fn restrict_discovery_access(path: &Path) -> std::io::Result<()> {
    windows::restrict_owner_access(path)
}

#[cfg(not(windows))]
fn restrict_discovery_access(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn lease_path(root: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return macos::lease_path(root);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::lease_path(root);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::lease_path(root);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    root.join("owner.lock")
}

fn launch_desktop(request: ActivationRequest) -> Result<(), RuntimeCoordinatorError> {
    #[cfg(target_os = "macos")]
    let result = macos::launch_desktop(&request);
    #[cfg(target_os = "windows")]
    let result = windows::launch_desktop(&request);
    #[cfg(target_os = "linux")]
    let result = linux::launch_desktop(&request);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let result = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Lumvise desktop launch requires macOS, Windows, or Linux",
    ));
    result.map_err(RuntimeCoordinatorError::from)
}
fn random_token() -> String {
    let mut bytes = [0_u8; 24];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default()
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: RuntimeControlKindV1) -> RuntimeControlRequestV1 {
        RuntimeControlRequestV1 {
            protocol_major: APP_RUNTIME_CONTROL_PROTOCOL_MAJOR,
            request_id: "request-1".into(),
            generation_nonce: "generation-1".into(),
            owner_credential: "owner-credential".into(),
            kind: kind as i32,
            activation_arguments: Vec::new(),
            deadline_unix_ms: 0,
        }
    }

    #[test]
    fn control_rejects_wrong_major_and_foreign_owner_credential() {
        let state = AtomicI32::new(RuntimeControlStateV1::Ready as i32);
        let credentials = Mutex::new(HashMap::new());
        let mut wrong_major = request(RuntimeControlKindV1::State);
        wrong_major.protocol_major = APP_RUNTIME_CONTROL_PROTOCOL_MAJOR + 1;
        let response = control_response(
            &wrong_major,
            "generation-1",
            "owner-credential",
            &state,
            &credentials,
        );
        assert!(!response.accepted);
        assert_eq!(response.error_code, "wrong_major");

        let mut foreign = request(RuntimeControlKindV1::State);
        foreign.owner_credential = "foreign".into();
        let response = control_response(
            &foreign,
            "generation-1",
            "owner-credential",
            &state,
            &credentials,
        );
        assert!(!response.accepted);
        assert_eq!(response.error_code, "auth_failed");
    }

    #[test]
    fn ready_handshake_issues_expiring_generation_bound_grant() {
        let state = AtomicI32::new(RuntimeControlStateV1::Ready as i32);
        let credentials = Mutex::new(HashMap::new());
        let response = control_response(
            &request(RuntimeControlKindV1::Handshake),
            "generation-1",
            "owner-credential",
            &state,
            &credentials,
        );
        assert!(response.accepted);
        assert!(!response.app_bridge_credential.is_empty());
        assert!(response.credential_expires_unix_ms > epoch_ms());
        assert!(
            credentials.lock().ok().is_some_and(
                |credentials| credentials.contains_key(&response.app_bridge_credential)
            )
        );
    }
}
