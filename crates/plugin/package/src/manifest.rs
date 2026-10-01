use std::collections::{BTreeMap, HashSet};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::PackageError;

/// Inclusive plugin protocol compatibility interval.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProtocolRange {
    /// Oldest supported protocol version.
    pub min: u32,
    /// Newest supported protocol version.
    pub max: u32,
}

/// Signed package metadata and SHA-256 file tree.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    /// Package schema version. Version `1` is currently supported.
    pub schema_version: u32,
    /// Signed publisher and signing-key identity.
    pub publisher: PublisherIdentity,
    /// Stable lowercase plugin identity.
    pub plugin_id: String,
    /// Semantic plugin version.
    pub plugin_version: String,
    /// Supported host protocol interval.
    pub protocol: ProtocolRange,
    /// Target triple to executable package path.
    pub targets: BTreeMap<String, String>,
    /// Canonical package path to lowercase SHA-256 digest.
    pub files: BTreeMap<String, String>,
    /// Generic capabilities exported by this plugin.
    pub exports: Vec<ExportDescriptor>,
    /// Versioned Host Capabilities requested by this plugin.
    pub host_capabilities: Vec<HostCapabilityRequirement>,
}

/// Signed identity used to select one trusted package verification key.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublisherIdentity {
    /// Stable publisher identity.
    pub publisher_id: String,
    /// Globally unique signing-key identity.
    pub key_id: String,
}

impl PublisherIdentity {
    pub(crate) fn validate(&self) -> Result<(), PackageError> {
        if !is_valid_stable_id(&self.publisher_id) {
            return Err(PackageError::InvalidPublisherId(self.publisher_id.clone()));
        }
        if !is_valid_stable_id(&self.key_id) {
            return Err(PackageError::InvalidPublisherKeyId(self.key_id.clone()));
        }
        Ok(())
    }
}

/// A generic behavior exposed by a compiled plugin.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExportDescriptor {
    /// Stable package-local export identity.
    pub id: String,
    /// Human-readable nonempty export name.
    pub name: String,
    /// Caller-facing guidance explaining what the export does and when to use it.
    #[serde(default)]
    pub description: String,
    /// Host integration surface and its signed surface-specific contract.
    #[serde(flatten)]
    pub surface: ExportSurface,
    /// JSON object schema accepted by the export.
    pub input_schema: serde_json::Value,
    /// JSON object schema returned by the export.
    pub output_schema: serde_json::Value,
    /// Optional host-enforced admission policy signed separately from payload schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<InvocationAdmissionPolicy>,
    /// Whether invocation occupies a foreground request or background job.
    pub execution: ExecutionMode,
}

/// Generic host scheduling semantics for one signed export.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvocationAdmissionPolicy {
    /// Serializes related invocations through one named FIFO lane.
    ExclusiveLane(ExclusiveLanePolicy),
}

/// Signed field and output mappings for a generic exclusive invocation lane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusiveLanePolicy {
    /// Stable host lane identity shared by related exports.
    pub lane_id: String,
    /// Admission or release effect applied by this export.
    pub operation: ExclusiveLaneOperation,
    /// Input field containing the caller owner identity.
    pub owner_argument: String,
    /// Input field containing the logical session identity.
    pub session_argument: String,
    /// Input field selecting queued rather than fail-fast admission.
    pub queue_argument: String,
    /// Input field allowing replacement of the caller's active session.
    pub replace_argument: String,
    /// Input field containing the maximum admission wait in milliseconds.
    pub timeout_ms_argument: String,
    /// Signed wait used when the caller omits the timeout field.
    pub default_timeout_ms: u64,
    /// Maximum caller-selected wait accepted by the host.
    pub max_timeout_ms: u64,
    /// Maximum FIFO waiters admitted for this plugin-scoped lane.
    pub max_queue_depth: u32,
    /// JSON pointer locating the canonical session identity in output.
    pub response_session_pointer: String,
    /// JSON pointer locating the terminal-state value in output.
    pub terminal_pointer: String,
    /// Output values that release the lane after invocation.
    pub terminal_values: Vec<String>,
}

/// Effect one export has on its signed exclusive lane.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusiveLaneOperation {
    /// Acquires the lane before invoking the export.
    Acquire,
    /// Releases an acquired lane when output matches the signed terminal policy.
    ReleaseOnOutput,
}

/// Host-neutral plugin export surface and surface-specific signed metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportSurface {
    /// Model Context Protocol tool.
    McpTool,
    /// MCP tool discoverable only through one host-defined scoped channel.
    ScopedMcpTool {
        /// Stable generic discovery scope, such as `assistant_session`.
        scope: String,
    },
    /// HTTP route owned by the plugin process.
    HttpRoute {
        /// HTTP method accepted by the route.
        method: HttpMethod,
        /// Absolute host-mounted route template.
        path_template: String,
        /// Response streaming contract.
        stream_mode: HttpStreamMode,
        /// Signed polling and delivery quotas for an SSE route.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sse_policy: Option<SseStreamPolicy>,
    },
    /// Signed renderer view; NativeWindow opts into the trusted desktop bridge.
    View {
        /// Stable host-wide view identity.
        view_id: String,
        /// Requested host placement.
        surface: ViewSurface,
        /// Canonical package-relative signed entry asset.
        asset_path: String,
        /// Content Security Policy applied by the host renderer.
        content_security_policy: String,
        /// Explicit host APIs exposed to the sandboxed view.
        allowed_host_apis: Vec<String>,
        /// Optional native menu placement for opening this ready-only View.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        menu_placement: Option<ViewMenuPlacement>,
    },
    /// Recurring scheduled task with host-enforced delivery policy.
    RecurringTask {
        /// Fixed cadence between scheduled deliveries.
        interval_seconds: u64,
        /// Retry and terminal failure policy.
        delivery: BackgroundDeliveryPolicy,
    },
    /// Storage event trigger with signed event filters.
    StorageTrigger {
        /// Storage event kinds accepted by this trigger.
        event_kinds: Vec<String>,
        /// Optional entity-kind allowlist; empty accepts every entity kind.
        entity_kinds: Vec<String>,
        /// Retry and terminal failure policy.
        delivery: BackgroundDeliveryPolicy,
    },
    /// Explicit command.
    Command,
}

/// Supported HTTP route methods.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
    /// HEAD.
    Head,
    /// OPTIONS.
    Options,
}

/// HTTP response delivery contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpStreamMode {
    /// One buffered response body.
    Buffered,
    /// Server-sent event stream.
    ServerSentEvents,
}

/// Signed resource and timing policy for one server-sent event export.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SseStreamPolicy {
    /// Maximum events requested from one plugin poll.
    pub max_events_per_poll: u32,
    /// Delay between plugin polls when no plugin backoff is supplied.
    pub poll_interval_ms: u64,
    /// Maximum silence before the host emits an SSE heartbeat comment.
    pub heartbeat_interval_ms: u64,
    /// Maximum plugin-requested delay accepted by the host.
    pub max_backoff_ms: u64,
}

/// Signed retry, timeout, and dead-letter policy for one background export.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackgroundDeliveryPolicy {
    /// Total invocation attempts, including the initial attempt.
    pub max_attempts: u32,
    /// Delay before the first retry.
    pub initial_backoff_ms: u64,
    /// Maximum exponential retry delay.
    pub max_backoff_ms: u64,
    /// Maximum durable dead-letter records retained for this export.
    pub dead_letter_max_entries: u32,
    /// Dead-letter retention duration.
    pub dead_letter_retention_seconds: u64,
}

/// Placement requested by a packaged view.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewSurface {
    /// Trusted signed desktop view with the host's native renderer bridge.
    NativeWindow,
    /// Inline panel within the dashboard.
    DashboardPanel,
    /// Floating layer above the dashboard.
    Overlay,
    /// Full dashboard work area.
    Fullscreen,
}

/// Native host menu placements available to signed plugin Views.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMenuPlacement {
    /// Desktop settings menu.
    DesktopSettings,
}

/// Invocation lifecycle requested by an export.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Completes within the foreground caller lifecycle.
    Foreground,
    /// Runs through the host background lifecycle.
    Background,
}

/// A versioned Host Capability requested by the plugin.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostCapabilityRequirement {
    /// Stable Host Capability identity.
    pub id: String,
    /// Semantic version requirement understood by the host broker.
    pub version: String,
}

/// Host values used to select a compatible packaged executable.
#[derive(Clone, Debug)]
pub struct HostCompatibility {
    pub(crate) protocol_version: u32,
    pub(crate) target: String,
}

impl HostCompatibility {
    /// Creates host compatibility input from protocol version and Rust target triple.
    pub fn new(protocol_version: u32, target: impl Into<String>) -> Self {
        Self {
            protocol_version,
            target: target.into(),
        }
    }
}

impl PluginManifest {
    pub(crate) fn validate(&self, host: &HostCompatibility) -> Result<&str, PackageError> {
        self.validate_identity()?;
        self.validate_publisher()?;
        self.validate_protocol(host.protocol_version)?;
        self.validate_exports()?;
        self.validate_host_capabilities()?;
        self.targets
            .get(&host.target)
            .map(String::as_str)
            .ok_or_else(|| PackageError::UnsupportedTarget {
                target: host.target.clone(),
            })
    }

    fn validate_publisher(&self) -> Result<(), PackageError> {
        self.publisher.validate()
    }

    fn validate_identity(&self) -> Result<(), PackageError> {
        if self.schema_version != 1 {
            return Err(PackageError::UnsupportedSchema(self.schema_version));
        }
        if !is_valid_plugin_id(&self.plugin_id) {
            return Err(PackageError::InvalidPluginId(self.plugin_id.clone()));
        }
        Version::parse(&self.plugin_version)
            .map_err(|_| PackageError::InvalidPluginVersion(self.plugin_version.clone()))?;
        Ok(())
    }

    fn validate_protocol(&self, host_version: u32) -> Result<(), PackageError> {
        if self.protocol.min > self.protocol.max {
            return Err(PackageError::InvalidProtocolRange {
                min: self.protocol.min,
                max: self.protocol.max,
            });
        }
        if !(self.protocol.min..=self.protocol.max).contains(&host_version) {
            return Err(PackageError::UnsupportedProtocol {
                host: host_version,
                min: self.protocol.min,
                max: self.protocol.max,
            });
        }
        Ok(())
    }

    fn validate_exports(&self) -> Result<(), PackageError> {
        let mut ids = HashSet::new();
        let mut http_routes = HashSet::new();
        let mut view_ids = HashSet::new();
        for descriptor in &self.exports {
            if !is_valid_stable_id(&descriptor.id) {
                return Err(PackageError::InvalidExportId(descriptor.id.clone()));
            }
            if !ids.insert(descriptor.id.clone()) {
                return Err(PackageError::DuplicateExportId(descriptor.id.clone()));
            }
            if descriptor.name.trim().is_empty() {
                return Err(PackageError::InvalidExportName {
                    export_id: descriptor.id.clone(),
                    name: descriptor.name.clone(),
                });
            }
            validate_schema(&descriptor.id, "input_schema", &descriptor.input_schema)?;
            validate_schema(&descriptor.id, "output_schema", &descriptor.output_schema)?;
            validate_admission(descriptor)?;
            self.validate_export_surface(descriptor, &mut http_routes, &mut view_ids)?;
        }
        Ok(())
    }

    fn validate_export_surface(
        &self,
        descriptor: &ExportDescriptor,
        http_routes: &mut HashSet<(HttpMethod, String)>,
        view_ids: &mut HashSet<String>,
    ) -> Result<(), PackageError> {
        match &descriptor.surface {
            ExportSurface::ScopedMcpTool { scope } => {
                if !is_valid_stable_id(scope) {
                    return Err(PackageError::InvalidScopedMcpScope {
                        export_id: descriptor.id.clone(),
                        scope: scope.clone(),
                    });
                }
                Ok(())
            }
            ExportSurface::HttpRoute {
                method,
                path_template,
                stream_mode,
                sse_policy,
            } => {
                validate_http_route(&descriptor.id, *method, path_template, http_routes)?;
                validate_sse_policy(&descriptor.id, *stream_mode, sse_policy.as_ref())
            }
            ExportSurface::View {
                view_id,
                asset_path,
                content_security_policy,
                allowed_host_apis,
                ..
            } => self.validate_view(
                &descriptor.id,
                view_id,
                asset_path,
                content_security_policy,
                allowed_host_apis,
                view_ids,
            ),
            ExportSurface::RecurringTask {
                interval_seconds,
                delivery,
            } => validate_recurring_task(descriptor, *interval_seconds, delivery),
            ExportSurface::StorageTrigger {
                event_kinds,
                entity_kinds,
                delivery,
            } => validate_storage_trigger(descriptor, event_kinds, entity_kinds, delivery),
            _ => Ok(()),
        }
    }

    fn validate_view(
        &self,
        export_id: &str,
        view_id: &str,
        asset_path: &str,
        content_security_policy: &str,
        allowed_host_apis: &[String],
        view_ids: &mut HashSet<String>,
    ) -> Result<(), PackageError> {
        validate_view_id(view_id, view_ids)?;
        crate::archive::validate_path(asset_path)?;
        if !self.files.contains_key(asset_path) {
            return Err(PackageError::UnsignedViewAsset {
                export_id: export_id.to_owned(),
                path: asset_path.to_owned(),
            });
        }
        validate_view_policy(export_id, content_security_policy, allowed_host_apis)
    }

    fn validate_host_capabilities(&self) -> Result<(), PackageError> {
        let mut ids = HashSet::new();
        for capability in &self.host_capabilities {
            if !is_valid_stable_id(&capability.id) {
                return Err(PackageError::InvalidHostCapabilityId(capability.id.clone()));
            }
            if !ids.insert(capability.id.clone()) {
                return Err(PackageError::DuplicateHostCapabilityId(
                    capability.id.clone(),
                ));
            }
            VersionReq::parse(&capability.version).map_err(|_| {
                PackageError::InvalidHostCapabilityVersion {
                    capability_id: capability.id.clone(),
                    version: capability.version.clone(),
                }
            })?;
        }
        Ok(())
    }
}

fn validate_admission(descriptor: &ExportDescriptor) -> Result<(), PackageError> {
    let Some(InvocationAdmissionPolicy::ExclusiveLane(policy)) = &descriptor.admission else {
        return Ok(());
    };
    let identifiers = [
        &policy.lane_id,
        &policy.owner_argument,
        &policy.session_argument,
        &policy.queue_argument,
        &policy.replace_argument,
        &policy.timeout_ms_argument,
    ];
    if identifiers.iter().any(|value| !is_valid_stable_id(value)) {
        return Err(invalid_admission(
            descriptor,
            "stable lane and argument identifiers",
        ));
    }
    if policy.operation == ExclusiveLaneOperation::Acquire {
        validate_schema_property(descriptor, &policy.owner_argument, "string")?;
        validate_schema_property(descriptor, &policy.session_argument, "string")?;
        validate_schema_property(descriptor, &policy.queue_argument, "boolean")?;
        validate_schema_property(descriptor, &policy.replace_argument, "boolean")?;
        validate_schema_property(descriptor, &policy.timeout_ms_argument, "integer")?;
    }
    if !valid_json_pointer(&policy.response_session_pointer)
        || !valid_json_pointer(&policy.terminal_pointer)
        || policy.terminal_values.is_empty()
        || policy
            .terminal_values
            .iter()
            .any(|value| value.trim().is_empty())
    {
        return Err(invalid_admission(
            descriptor,
            "nonempty response pointers and terminal values",
        ));
    }
    if policy.default_timeout_ms == 0
        || policy.default_timeout_ms > policy.max_timeout_ms
        || policy.max_timeout_ms > 300_000
        || policy.max_queue_depth == 0
        || policy.max_queue_depth > 1_024
    {
        return Err(invalid_admission(
            descriptor,
            "bounded default/max timeout and queue depth",
        ));
    }
    validate_output_pointer(
        descriptor,
        &policy.response_session_pointer,
        policy.operation == ExclusiveLaneOperation::ReleaseOnOutput,
    )?;
    validate_output_pointer(descriptor, &policy.terminal_pointer, false)?;
    Ok(())
}

fn valid_json_pointer(value: &str) -> bool {
    if !value.starts_with('/') || value.chars().any(char::is_whitespace) {
        return false;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            index += 1;
            if index >= bytes.len() || !matches!(bytes[index], b'0' | b'1') {
                return false;
            }
        }
        index += 1;
    }
    true
}

fn validate_schema_property(
    descriptor: &ExportDescriptor,
    property: &str,
    expected_type: &str,
) -> Result<(), PackageError> {
    let actual = descriptor
        .input_schema
        .pointer(&format!("/properties/{property}/type"))
        .and_then(serde_json::Value::as_str);
    if actual == Some(expected_type) {
        return Ok(());
    }
    Err(invalid_admission(
        descriptor,
        &format!("input property `{property}` with type `{expected_type}`"),
    ))
}

fn validate_output_pointer(
    descriptor: &ExportDescriptor,
    pointer: &str,
    allow_null: bool,
) -> Result<(), PackageError> {
    let mut schema = &descriptor.output_schema;
    for segment in pointer.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        schema = schema
            .pointer(&format!("/properties/{segment}"))
            .ok_or_else(|| {
                invalid_admission(descriptor, &format!("output schema path `{pointer}`"))
            })?;
    }
    if is_lane_session_string(schema, allow_null) {
        return Ok(());
    }
    Err(invalid_admission(
        descriptor,
        &format!("string output schema path `{pointer}`"),
    ))
}

fn is_lane_session_string(schema: &serde_json::Value, allow_null: bool) -> bool {
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("string") {
        return true;
    }
    // An idle observation has no lease to release. Acquiring still requires an ID.
    allow_null
        && schema
            .get("anyOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| {
                branches.len() == 2
                    && branches.iter().any(|branch| branch["type"] == "string")
                    && branches.iter().any(|branch| branch["type"] == "null")
            })
}

fn invalid_admission(descriptor: &ExportDescriptor, expected: &str) -> PackageError {
    PackageError::InvalidInvocationAdmission {
        export_id: descriptor.id.clone(),
        message: format!("expected {expected}"),
    }
}

fn is_valid_plugin_id(value: &str) -> bool {
    is_valid_stable_id(value)
}

fn is_valid_stable_id(value: &str) -> bool {
    let mut previous_separator = true;
    for byte in value.bytes() {
        let separator = matches!(byte, b'.' | b'-' | b'_');
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || separator)
            || separator && previous_separator
        {
            return false;
        }
        previous_separator = separator;
    }
    !value.is_empty() && !previous_separator
}

fn validate_http_route(
    export_id: &str,
    method: HttpMethod,
    path_template: &str,
    routes: &mut HashSet<(HttpMethod, String)>,
) -> Result<(), PackageError> {
    if !is_valid_absolute_path_template(path_template) {
        return Err(PackageError::InvalidHttpPathTemplate {
            export_id: export_id.to_owned(),
            path: path_template.to_owned(),
        });
    }
    if !routes.insert((method, path_template.to_owned())) {
        return Err(PackageError::DuplicateHttpRoute {
            method: format!("{method:?}"),
            path: path_template.to_owned(),
        });
    }
    Ok(())
}

fn validate_sse_policy(
    export_id: &str,
    stream_mode: HttpStreamMode,
    policy: Option<&SseStreamPolicy>,
) -> Result<(), PackageError> {
    match (stream_mode, policy) {
        (HttpStreamMode::Buffered, None) => Ok(()),
        (HttpStreamMode::Buffered, Some(_)) => Err(invalid_sse_policy(
            export_id,
            "sse_policy",
            1,
            "absent for buffered routes",
        )),
        (HttpStreamMode::ServerSentEvents, None) => Err(invalid_sse_policy(
            export_id,
            "sse_policy",
            0,
            "present for SSE routes",
        )),
        (HttpStreamMode::ServerSentEvents, Some(policy)) => {
            validate_sse_policy_fields(export_id, policy)
        }
    }
}

fn validate_recurring_task(
    descriptor: &ExportDescriptor,
    interval_seconds: u64,
    delivery: &BackgroundDeliveryPolicy,
) -> Result<(), PackageError> {
    require_background_execution(descriptor)?;
    validate_background_range(
        &descriptor.id,
        "interval_seconds",
        interval_seconds,
        1,
        31_536_000,
    )?;
    validate_background_delivery(&descriptor.id, delivery)
}

fn validate_storage_trigger(
    descriptor: &ExportDescriptor,
    event_kinds: &[String],
    entity_kinds: &[String],
    delivery: &BackgroundDeliveryPolicy,
) -> Result<(), PackageError> {
    require_background_execution(descriptor)?;
    validate_filter_ids(&descriptor.id, "event_kinds", event_kinds, false)?;
    validate_filter_ids(&descriptor.id, "entity_kinds", entity_kinds, true)?;
    validate_background_delivery(&descriptor.id, delivery)
}

fn require_background_execution(descriptor: &ExportDescriptor) -> Result<(), PackageError> {
    if descriptor.execution == ExecutionMode::Background {
        return Ok(());
    }
    Err(PackageError::InvalidBackgroundExecution(
        descriptor.id.clone(),
    ))
}

fn validate_filter_ids(
    export_id: &str,
    field: &'static str,
    values: &[String],
    empty_allowed: bool,
) -> Result<(), PackageError> {
    if values.is_empty() && !empty_allowed {
        return Err(invalid_storage_filter(export_id, field, "<empty>"));
    }
    let mut unique = HashSet::new();
    for value in values {
        if !is_valid_stable_id(value) || !unique.insert(value) {
            return Err(invalid_storage_filter(export_id, field, value));
        }
    }
    Ok(())
}

fn invalid_storage_filter(export_id: &str, field: &'static str, value: &str) -> PackageError {
    PackageError::InvalidStorageTriggerFilter {
        export_id: export_id.to_owned(),
        field,
        value: value.to_owned(),
    }
}

fn validate_background_delivery(
    export_id: &str,
    policy: &BackgroundDeliveryPolicy,
) -> Result<(), PackageError> {
    validate_background_range(
        export_id,
        "max_attempts",
        u64::from(policy.max_attempts),
        1,
        20,
    )?;
    validate_background_range(
        export_id,
        "initial_backoff_ms",
        policy.initial_backoff_ms,
        1,
        policy.max_backoff_ms,
    )?;
    validate_background_range(
        export_id,
        "max_backoff_ms",
        policy.max_backoff_ms,
        policy.initial_backoff_ms,
        86_400_000,
    )?;
    validate_background_range(
        export_id,
        "dead_letter_max_entries",
        u64::from(policy.dead_letter_max_entries),
        1,
        10_000,
    )?;
    validate_background_range(
        export_id,
        "dead_letter_retention_seconds",
        policy.dead_letter_retention_seconds,
        1,
        31_536_000,
    )
}

fn validate_background_range(
    export_id: &str,
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), PackageError> {
    if (min..=max).contains(&value) {
        return Ok(());
    }
    Err(PackageError::InvalidBackgroundDeliveryPolicy {
        export_id: export_id.to_owned(),
        field,
        value,
        expected: format!("value in {min}..={max}"),
    })
}

fn validate_sse_policy_fields(
    export_id: &str,
    policy: &SseStreamPolicy,
) -> Result<(), PackageError> {
    validate_sse_range(
        export_id,
        "max_events_per_poll",
        u64::from(policy.max_events_per_poll),
        1,
        100,
    )?;
    validate_sse_range(
        export_id,
        "poll_interval_ms",
        policy.poll_interval_ms,
        10,
        10_000,
    )?;
    validate_sse_range(
        export_id,
        "heartbeat_interval_ms",
        policy.heartbeat_interval_ms,
        policy.poll_interval_ms,
        300_000,
    )?;
    validate_sse_range(
        export_id,
        "max_backoff_ms",
        policy.max_backoff_ms,
        policy.poll_interval_ms,
        30_000,
    )
}

fn validate_sse_range(
    export_id: &str,
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), PackageError> {
    if (min..=max).contains(&value) {
        return Ok(());
    }
    Err(invalid_sse_policy(
        export_id,
        field,
        value,
        format!("integer in {min}..={max}"),
    ))
}

fn invalid_sse_policy(
    export_id: &str,
    field: &'static str,
    value: u64,
    expected: impl Into<String>,
) -> PackageError {
    PackageError::InvalidSsePolicy {
        export_id: export_id.to_owned(),
        field,
        value,
        expected: expected.into(),
    }
}

fn is_valid_absolute_path_template(path: &str) -> bool {
    if !path.starts_with('/') || path.contains(['?', '#', '\\', '%']) || path.contains("//") {
        return false;
    }
    path.split('/').skip(1).all(is_valid_path_template_segment)
}

fn is_valid_path_template_segment(segment: &str) -> bool {
    if segment.is_empty() || matches!(segment, "." | "..") {
        return false;
    }
    if let Some(parameter) = segment
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    {
        return is_valid_stable_id(parameter);
    }
    segment
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_view_id(view_id: &str, view_ids: &mut HashSet<String>) -> Result<(), PackageError> {
    if !is_valid_stable_id(view_id) {
        return Err(PackageError::InvalidViewId(view_id.to_owned()));
    }
    if !view_ids.insert(view_id.to_owned()) {
        return Err(PackageError::DuplicateViewId(view_id.to_owned()));
    }
    Ok(())
}

fn validate_view_policy(
    export_id: &str,
    content_security_policy: &str,
    allowed_host_apis: &[String],
) -> Result<(), PackageError> {
    if content_security_policy.trim().is_empty() || content_security_policy.contains(['\r', '\n']) {
        return Err(PackageError::InvalidViewContentSecurityPolicy {
            export_id: export_id.to_owned(),
            policy: content_security_policy.to_owned(),
        });
    }
    let mut ids = HashSet::new();
    for api_id in allowed_host_apis {
        if !is_valid_stable_id(api_id) || !ids.insert(api_id) {
            return Err(PackageError::InvalidViewHostApi {
                export_id: export_id.to_owned(),
                api_id: api_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_schema(
    export_id: &str,
    field: &'static str,
    schema: &serde_json::Value,
) -> Result<(), PackageError> {
    if !schema.is_object() {
        return Err(PackageError::InvalidExportSchema {
            export_id: export_id.to_owned(),
            field,
            actual: schema.to_string(),
        });
    }
    Ok(())
}
