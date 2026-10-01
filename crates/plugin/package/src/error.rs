use std::{io, path::PathBuf};

/// Precise package verification and installation failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackageError {
    /// Persistent publisher trust policy is malformed or unsupported.
    #[error("invalid publisher trust store `{path}`: {message}")]
    InvalidTrustStore {
        /// Offending trust document path.
        path: PathBuf,
        /// Parse or validation diagnostic.
        message: String,
    },
    /// Signed publisher identity is malformed.
    #[error("invalid publisher id `{0}`; expected lowercase ASCII stable identity")]
    InvalidPublisherId(String),
    /// Signed publisher key identity is malformed.
    #[error("invalid publisher key id `{0}`; expected lowercase ASCII stable identity")]
    InvalidPublisherKeyId(String),
    /// No keys are trusted for the signed publisher.
    #[error("publisher `{0}` is not trusted")]
    UnknownPublisher(String),
    /// Signed key identity is absent from the trust store.
    #[error("publisher key `{0}` is not trusted")]
    UnknownPublisherKey(String),
    /// Signed key identity has been revoked.
    #[error("publisher key `{0}` is revoked")]
    RevokedPublisherKey(String),
    /// Signed publisher does not own the selected key identity.
    #[error("publisher key `{key_id}` belongs to `{expected_publisher}`, not `{actual_publisher}`")]
    PublisherKeyMismatch {
        /// Signed key identity.
        key_id: String,
        /// Publisher registered for the key.
        expected_publisher: String,
        /// Publisher declared by the package.
        actual_publisher: String,
    },
    /// Key identity is already registered.
    #[error("publisher key id `{0}` already exists; expected globally unique key ids")]
    DuplicatePublisherKeyId(String),
    /// A payload tries to occupy package-owned metadata.
    #[error(
        "reserved package path `{0}`; expected a payload path outside manifest/signature metadata"
    )]
    ReservedPackagePath(String),
    /// Export identity is malformed.
    #[error("invalid export id `{0}`; expected lowercase ASCII stable identity")]
    InvalidExportId(String),
    /// Two exports share an identity.
    #[error("duplicate export id `{0}`; expected unique manifest.exports ids")]
    DuplicateExportId(String),
    /// Export display name is empty.
    #[error("invalid export name `{name}` for `{export_id}`; expected nonempty text")]
    InvalidExportName {
        /// Offending export identity.
        export_id: String,
        /// Offending name.
        name: String,
    },
    /// Export schema is not an object schema.
    #[error("invalid `{field}` for export `{export_id}`: got `{actual}`, expected JSON object")]
    InvalidExportSchema {
        /// Offending export identity.
        export_id: String,
        /// Offending schema field.
        field: &'static str,
        /// Offending JSON shape.
        actual: String,
    },
    /// A scoped MCP export declares a malformed discovery scope.
    #[error(
        "invalid scoped MCP scope `{scope}` for export `{export_id}`; expected lowercase ASCII stable identity"
    )]
    InvalidScopedMcpScope {
        /// Offending export identity.
        export_id: String,
        /// Offending scope identity.
        scope: String,
    },
    /// HTTP path template is not absolute and canonical.
    #[error(
        "invalid HTTP path template `{path}` for export `{export_id}`; expected `/segment/{{parameter}}` without traversal, query, fragment, percent, or empty segments"
    )]
    InvalidHttpPathTemplate {
        /// Offending export identity.
        export_id: String,
        /// Offending path template.
        path: String,
    },
    /// A signed SSE delivery quota is absent or outside its safe range.
    #[error(
        "invalid SSE policy `{field}` value `{value}` for export `{export_id}`; expected {expected}"
    )]
    InvalidSsePolicy {
        /// Offending export identity.
        export_id: String,
        /// Offending policy field.
        field: &'static str,
        /// Offending numeric value or presence marker.
        value: u64,
        /// Accepted shape or range.
        expected: String,
    },
    /// Background delivery metadata is missing, unsafe, or inconsistent.
    #[error(
        "invalid background delivery policy `{field}` value `{value}` for export `{export_id}`; expected {expected}"
    )]
    InvalidBackgroundDeliveryPolicy {
        /// Offending export identity.
        export_id: String,
        /// Offending policy field.
        field: &'static str,
        /// Offending numeric value.
        value: u64,
        /// Accepted range or relationship.
        expected: String,
    },
    /// A background surface uses foreground execution.
    #[error("invalid execution mode for background export `{0}`; expected background")]
    InvalidBackgroundExecution(String),
    /// A StorageTrigger filter is empty, malformed, or repeated.
    #[error(
        "invalid storage trigger `{field}` value `{value}` for export `{export_id}`; expected nonempty unique lowercase ASCII stable identities"
    )]
    InvalidStorageTriggerFilter {
        /// Offending export identity.
        export_id: String,
        /// Offending filter field.
        field: &'static str,
        /// Offending filter value.
        value: String,
    },
    /// Two HTTP exports claim the same method and path template.
    #[error("duplicate HTTP route `{method} {path}`; expected unique method and path pairs")]
    DuplicateHttpRoute {
        /// Duplicate HTTP method.
        method: String,
        /// Duplicate path template.
        path: String,
    },
    /// View identity is malformed.
    #[error("invalid view id `{0}`; expected lowercase ASCII stable identity")]
    InvalidViewId(String),
    /// Two View exports claim the same host-wide view identity.
    #[error("duplicate view id `{0}`; expected unique packaged view ids")]
    DuplicateViewId(String),
    /// View entry asset is not part of the signed package file tree.
    #[error(
        "unsigned view asset `{path}` for export `{export_id}`; expected asset path in manifest.files"
    )]
    UnsignedViewAsset {
        /// Offending export identity.
        export_id: String,
        /// Offending asset path.
        path: String,
    },
    /// View Content Security Policy is empty.
    #[error(
        "invalid Content Security Policy `{policy}` for view export `{export_id}`; expected nonempty single-line policy"
    )]
    InvalidViewContentSecurityPolicy {
        /// Offending export identity.
        export_id: String,
        /// Offending policy.
        policy: String,
    },
    /// View host API identity is malformed or repeated.
    #[error(
        "invalid allowed host API `{api_id}` for view export `{export_id}`; expected unique lowercase ASCII stable identities"
    )]
    InvalidViewHostApi {
        /// Offending export identity.
        export_id: String,
        /// Offending host API identity.
        api_id: String,
    },
    /// Requested Host Capability identity is malformed.
    #[error("invalid Host Capability id `{0}`; expected lowercase ASCII stable identity")]
    InvalidHostCapabilityId(String),
    /// Requested Host Capability identity is repeated.
    #[error("duplicate Host Capability id `{0}`; expected unique request ids")]
    DuplicateHostCapabilityId(String),
    /// Requested Host Capability version is not a semantic requirement.
    #[error(
        "invalid Host Capability version `{version}` for `{capability_id}`; expected semver requirement"
    )]
    InvalidHostCapabilityVersion {
        /// Offending Host Capability identity.
        capability_id: String,
        /// Offending version requirement.
        version: String,
    },
    /// Signed invocation admission metadata is malformed.
    #[error("invalid invocation admission for export `{export_id}`: {message}")]
    InvalidInvocationAdmission {
        /// Export owning the invalid policy.
        export_id: String,
        /// Expected policy shape.
        message: String,
    },
    /// Compressed package exceeds its configured maximum.
    #[error("compressed package size `{actual}` exceeds maximum `{max}` bytes")]
    ArchiveTooLarge {
        /// Observed compressed bytes.
        actual: u64,
        /// Configured maximum bytes.
        max: u64,
    },
    /// One uncompressed entry exceeds its configured maximum.
    #[error("uncompressed entry `{path}` size `{actual}` exceeds maximum `{max}` bytes")]
    EntryTooLarge {
        /// Offending package path.
        path: String,
        /// Declared uncompressed bytes.
        actual: u64,
        /// Configured maximum bytes.
        max: u64,
    },
    /// Combined uncompressed tree exceeds its configured maximum.
    #[error("total uncompressed package size `{actual}` exceeds maximum `{max}` bytes")]
    UncompressedPackageTooLarge {
        /// Observed combined uncompressed bytes.
        actual: u64,
        /// Configured maximum bytes.
        max: u64,
    },
    /// Package I/O failed.
    #[error("package I/O failed for `{path}`: {source}")]
    Io {
        /// Offending path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// ZIP parsing failed.
    #[error("invalid plugin ZIP package: {0}")]
    InvalidArchive(String),
    /// A required metadata entry is absent.
    #[error("package is missing required entry `{0}`")]
    MissingEntry(&'static str),
    /// An entry path can escape or is non-canonical.
    #[error("unsafe package path `{0}`; expected canonical relative slash path")]
    UnsafePath(String),
    /// ZIP repeats a path.
    #[error("duplicate package path `{0}`; expected each path exactly once")]
    DuplicatePath(String),
    /// Symlinks are not allowed.
    #[error("symlink package entry `{0}` is forbidden")]
    Symlink(String),
    /// An archive payload is absent from the signed tree.
    #[error("unsigned package file `{0}` is not listed in manifest.files")]
    UnsignedFile(String),
    /// A signed payload is absent.
    #[error("signed package file `{0}` is missing from archive")]
    MissingFile(String),
    /// SHA-256 mismatch.
    #[error("hash mismatch for `{path}`: expected `{expected}`, got `{actual}`")]
    HashMismatch {
        /// Offending package path.
        path: String,
        /// Signed digest.
        expected: String,
        /// Computed digest.
        actual: String,
    },
    /// Signature bytes have the wrong length or encoding.
    #[error("invalid Ed25519 signature encoding; expected 64 raw bytes, got {0}")]
    InvalidSignatureEncoding(usize),
    /// Signature does not match the trusted key.
    #[error("Ed25519 signature verification failed for canonical manifest")]
    SignatureVerification,
    /// Manifest JSON is invalid or non-canonical.
    #[error("invalid canonical manifest JSON: {0}")]
    InvalidManifest(String),
    /// Unsupported schema.
    #[error("unsupported package schema `{0}`; expected `1`")]
    UnsupportedSchema(u32),
    /// Plugin identity is malformed.
    #[error("invalid plugin id `{0}`; expected lowercase ASCII identity")]
    InvalidPluginId(String),
    /// Version is not semantic.
    #[error("invalid plugin version `{0}`; expected semantic version")]
    InvalidPluginVersion(String),
    /// Protocol bounds are inverted.
    #[error("invalid protocol range `{min}..={max}`; expected min <= max")]
    InvalidProtocolRange {
        /// Minimum protocol.
        min: u32,
        /// Maximum protocol.
        max: u32,
    },
    /// Host protocol is outside the signed interval.
    #[error("unsupported host protocol `{host}`; package supports `{min}..={max}`")]
    UnsupportedProtocol {
        /// Host protocol.
        host: u32,
        /// Minimum protocol.
        min: u32,
        /// Maximum protocol.
        max: u32,
    },
    /// Package has no executable for the host target.
    #[error("unsupported host target `{target}`; expected a manifest.targets entry")]
    UnsupportedTarget {
        /// Host target triple.
        target: String,
    },
    /// Target executable path is absent from files.
    #[error("target executable `{0}` is not listed in manifest.files")]
    UnlistedExecutable(String),
    /// Destination already exists.
    #[error("plugin destination `{0}` already exists; immutable installs cannot be replaced")]
    AlreadyInstalled(PathBuf),
}
