use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{BuiltinReleaseError, Result};

const BUILTIN_MEMBER_PREFIX: &str = "crates/plugin/builtins/";
const DESCRIPTOR_FILE: &str = "lumvise-builtin-release.toml";

/// Crate-owned declarative metadata used to build one Built-in Plugin package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinReleaseDescriptor {
    /// Cargo package containing the production plugin binary.
    pub package: String,
    /// Cargo binary target compiled into the Plugin Package.
    pub binary: String,
    /// Stable Plugin Protocol identity.
    pub plugin_id: String,
    /// Package SemVer used by the release manifest.
    pub plugin_version: String,
    /// Trusted publisher identity.
    pub publisher_id: String,
    /// Signing key identity.
    pub key_id: String,
    /// Oldest supported Plugin Protocol major.
    pub protocol_min: u32,
    /// Newest supported Plugin Protocol major.
    pub protocol_max: u32,
    /// Relative distribution path containing a `{target}` placeholder.
    pub payload_pattern: String,
    /// Crate-relative declarative Plugin Package manifest template.
    pub manifest_template: String,
    /// Optional crate-relative directory of View assets staged into the
    /// package. Its path doubles as the package-relative prefix: every file
    /// inside is copied to `<payload_assets_dir>/<relative path>`.
    pub payload_assets_dir: Option<String>,
    /// Crate root joined with `payload_assets_dir`, resolved at discovery.
    #[serde(skip)]
    pub payload_assets_path: Option<PathBuf>,
    /// Workspace-relative owner crate path discovered from Cargo membership.
    #[serde(skip)]
    pub crate_path: PathBuf,
    /// Source workspace owning this crate; omitted from release metadata.
    #[serde(skip)]
    pub workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct WorkspaceManifest {
    workspace: WorkspaceMembers,
}

#[derive(Deserialize)]
struct WorkspaceMembers {
    members: Vec<String>,
}

#[derive(Deserialize)]
struct PackageManifest {
    package: PackageIdentity,
    #[serde(default)]
    bin: Vec<BinaryIdentity>,
}

#[derive(Deserialize)]
struct PackageIdentity {
    name: String,
    version: String,
}

#[derive(Deserialize)]
struct BinaryIdentity {
    name: String,
}

/// Discovers and validates every Built-in Plugin declared by the Cargo workspace.
///
/// # Example
///
/// ```no_run
/// use lumvise_builtin_plugin_release::discover_descriptors;
///
/// let descriptors = discover_descriptors(std::path::Path::new("."))?;
/// assert!(descriptors.windows(2).all(|pair| pair[0].plugin_id < pair[1].plugin_id));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn discover_descriptors(workspace_root: &Path) -> Result<Vec<BuiltinReleaseDescriptor>> {
    let members = builtin_members(workspace_root)?;
    let mut descriptors = members
        .iter()
        .map(|member| read_descriptor(workspace_root, member))
        .collect::<Result<Vec<_>>>()?;
    reject_duplicate_identities(&descriptors)?;
    descriptors.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    Ok(descriptors)
}

/// Discovers one release across independent Cargo workspaces without copying sources.
///
/// Example: `discover_release_descriptors(&["core".into(), "private".into()])?`.
pub fn discover_release_descriptors(
    workspace_roots: &[PathBuf],
) -> Result<Vec<BuiltinReleaseDescriptor>> {
    if workspace_roots.is_empty() {
        return Err(invalid(
            Path::new("[]"),
            "at least one explicit source workspace",
        ));
    }
    let mut descriptors = Vec::new();
    for root in workspace_roots {
        descriptors.extend(discover_descriptors(root)?);
    }
    reject_duplicate_identities(&descriptors)?;
    descriptors.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    Ok(descriptors)
}

fn builtin_members(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let manifest: WorkspaceManifest = parse_toml(&workspace_root.join("Cargo.toml"))?;
    let members = manifest
        .workspace
        .members
        .into_iter()
        .filter(|member| member.starts_with(BUILTIN_MEMBER_PREFIX))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Err(invalid(
            workspace_root,
            "at least one built-in Cargo workspace member",
        ));
    }
    Ok(members)
}

fn read_descriptor(workspace_root: &Path, member: &Path) -> Result<BuiltinReleaseDescriptor> {
    let crate_root = workspace_root.join(member);
    let descriptor_path = crate_root.join(DESCRIPTOR_FILE);
    let mut descriptor: BuiltinReleaseDescriptor = parse_toml(&descriptor_path)?;
    descriptor.crate_path = member.to_path_buf();
    descriptor.workspace_root =
        workspace_root
            .canonicalize()
            .map_err(|source| BuiltinReleaseError::Io {
                path: workspace_root.to_path_buf(),
                source,
            })?;
    descriptor.payload_assets_path = descriptor
        .payload_assets_dir
        .as_deref()
        .map(|dir| crate_root.join(dir));
    let package: PackageManifest = parse_toml(&crate_root.join("Cargo.toml"))?;
    validate_descriptor(&descriptor, &descriptor_path)?;
    let manifest_template = crate_root.join(&descriptor.manifest_template);
    if !manifest_template.is_file() {
        return Err(invalid(
            &manifest_template,
            "existing declarative Plugin Package manifest template",
        ));
    }
    if let Some(assets_path) = &descriptor.payload_assets_path
        && !assets_path.is_dir()
    {
        return Err(invalid(
            assets_path,
            "existing plugin payload assets directory",
        ));
    }
    validate_package(&descriptor, &package, &descriptor_path)?;
    Ok(descriptor)
}

fn parse_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).map_err(|source| BuiltinReleaseError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|error| invalid(path, &error.to_string()))
}

fn validate_descriptor(descriptor: &BuiltinReleaseDescriptor, path: &Path) -> Result<()> {
    let fields = [
        &descriptor.package,
        &descriptor.binary,
        &descriptor.plugin_id,
        &descriptor.plugin_version,
        &descriptor.publisher_id,
        &descriptor.key_id,
        &descriptor.manifest_template,
    ];
    let assets_path_is_safe = descriptor
        .payload_assets_dir
        .as_deref()
        .map_or(true, safe_relative_path);
    let invalid_path = !safe_payload_pattern(&descriptor.payload_pattern)
        || !safe_relative_path(&descriptor.manifest_template)
        || !assets_path_is_safe;
    if fields.iter().any(|value| value.trim().is_empty())
        || descriptor.protocol_min > descriptor.protocol_max
        || invalid_path
    {
        return Err(invalid(
            path,
            "non-empty identities, valid protocol range, and safe `{target}` payload path",
        ));
    }
    Ok(())
}

fn validate_package(
    descriptor: &BuiltinReleaseDescriptor,
    package: &PackageManifest,
    path: &Path,
) -> Result<()> {
    let has_binary = package
        .bin
        .iter()
        .any(|binary| binary.name == descriptor.binary);
    if descriptor.package != package.package.name
        || descriptor.plugin_version != package.package.version
        || !has_binary
    {
        return Err(invalid(
            path,
            "package name/version and binary matching the crate Cargo manifest",
        ));
    }
    Ok(())
}

fn reject_duplicate_identities(descriptors: &[BuiltinReleaseDescriptor]) -> Result<()> {
    let mut package_names = BTreeSet::new();
    let mut plugin_ids = BTreeSet::new();
    let mut binary_names = BTreeSet::new();
    for descriptor in descriptors {
        if !package_names.insert(&descriptor.package)
            || !plugin_ids.insert(&descriptor.plugin_id)
            || !binary_names.insert(&descriptor.binary)
        {
            return Err(invalid(
                Path::new(DESCRIPTOR_FILE),
                "unique package names, binary names and plugin IDs across source workspaces",
            ));
        }
    }
    Ok(())
}

fn safe_payload_pattern(value: &str) -> bool {
    !Path::new(value).is_absolute()
        && value.contains("{target}")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn invalid(path: &Path, expected: &str) -> BuiltinReleaseError {
    BuiltinReleaseError::InvalidInput {
        value: path.display().to_string(),
        expected: expected.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_discovery_finds_every_builtin_in_plugin_id_order() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .expect("workspace root");
        let descriptors = discover_descriptors(root).expect("discover built-ins");
        let ids = descriptors
            .iter()
            .map(|descriptor| descriptor.plugin_id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(descriptors.len(), builtin_members(root).unwrap().len());
        for required in ["builtin.knowledge", "builtin.semantic"] {
            assert!(ids.contains(&required), "missing {required}");
        }
    }

    #[test]
    fn added_builtin_member_is_discovered_without_release_allowlist_change() {
        let root = tempfile::tempdir().expect("workspace");
        write_fixture_builtin(root.path(), true);
        let descriptors = discover_descriptors(root.path()).expect("discover fixture built-in");
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].plugin_id, "builtin.fixture");
    }

    #[test]
    fn builtin_member_without_descriptor_fails_closed() {
        let root = tempfile::tempdir().expect("workspace");
        write_fixture_builtin(root.path(), false);
        let error = discover_descriptors(root.path()).unwrap_err();
        assert!(error.to_string().contains(DESCRIPTOR_FILE));
    }

    fn write_fixture_builtin(root: &Path, with_descriptor: bool) {
        let member = root.join("crates/plugin/builtins/fixture");
        fs::create_dir_all(member.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/plugin/builtins/fixture\"]\n",
        )
        .unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"lumvise-plugin-fixture\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"fixture-plugin\"\npath = \"src/main.rs\"\n",
        )
        .unwrap();
        fs::write(member.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(member.join("manifest.json"), "{}\n").unwrap();
        if with_descriptor {
            fs::write(
                member.join(DESCRIPTOR_FILE),
                "package = \"lumvise-plugin-fixture\"\nbinary = \"fixture-plugin\"\nplugin_id = \"builtin.fixture\"\nplugin_version = \"0.1.0\"\npublisher_id = \"lumvise.builtin\"\nkey_id = \"lumvise.release.1\"\nprotocol_min = 1\nprotocol_max = 1\npayload_pattern = \"bin/{target}/fixture-plugin\"\nmanifest_template = \"manifest.json\"\n",
            )
            .unwrap();
        }
    }
}
