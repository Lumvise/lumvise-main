//! Host-configured filesystem authority is independent of MCP project selection.
use super::{
    AppCoreHostCapabilityBroker, HostCapabilityRequest, HostPluginInvoker, PluginInvocationContext,
    require_active_host_call,
};
use lumvise_plugin_runtime::HostCapabilityError;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ImportDelegation {
    ImportFolder { request: Value },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FolderRoute {
    folder_path: String,
    project_root: Option<String>,
    #[serde(rename = "replacePaths")]
    _replace_paths: Option<Vec<String>>,
    #[serde(rename = "importId")]
    _import_id: Option<String>,
    #[serde(rename = "cancel", default)]
    _cancel: bool,
    #[serde(rename = "statusOnly", default)]
    _status_only: bool,
}

pub(super) fn invoke(
    broker: &AppCoreHostCapabilityBroker,
    call: HostCapabilityRequest,
    context: &PluginInvocationContext,
    _nested: &dyn HostPluginInvoker,
) -> Result<Value, HostCapabilityError> {
    require_semantic_import(&call, context)?;
    let ImportDelegation::ImportFolder { mut request } = decode(call.input)?;
    let route: FolderRoute = decode(request.clone())?;
    let services = broker
        .services
        .as_ref()
        .ok_or_else(requires_nested_context)?;
    require_import_membership(services, context)?;
    let policy = services.source_import_authorization();
    let control = super::super::plugin_host_capabilities::invocation_control(context);
    let candidate = SourceImportAuthorization::inspect_folder(&route.folder_path)?;
    let root = effective_project_root(&route, Path::new(&candidate.canonical_path))?;
    authorize_project(broker, context, &root)?;
    let folder = broker.knowledge_authority.require_source_folder(
        context,
        &policy,
        &route.folder_path,
        &control.control(),
        true,
    )?;
    // The shared scanner receives the authorized canonical path, not a mutable alias.
    request["folderPath"] = serde_json::json!(folder);
    request["projectRoot"] = serde_json::json!(root);
    let service = services.project_import_service();
    let permit = Arc::new(ScopedImportAuthorization {
        authority: super::super::knowledge_authority::KnowledgeAuthority::new(
            broker.relational.clone(),
            services.mcp_session_membership(),
        ),
        semantic: broker.semantic.clone(),
        policy: policy.clone(),
        folder: folder.clone(),
        route: root.clone(),
        context: context.clone(),
        membership: services.mcp_session_membership(),
    });
    let publisher = service.publisher(Some(context.clone()), Some(permit));
    let output = service.import_folder(request, context, publisher)?;
    require_active_host_call("project.import", context)?;
    authorize_project(broker, context, &root)?;
    broker.knowledge_authority.require_source_folder(
        context,
        &policy,
        folder.to_str().ok_or_else(requires_nested_context)?,
        &control.control(),
        false,
    )?;
    Ok(output)
}

fn effective_project_root(
    route: &FolderRoute,
    canonical: &Path,
) -> Result<String, HostCapabilityError> {
    if let Some(root) = &route.project_root {
        return Ok(root.clone());
    }
    canonical.to_str().map(str::to_owned).ok_or_else(|| {
        import_denied(&format!(
            "canonical folder {canonical:?}; expected a UTF-8 project root"
        ))
    })
}

fn require_import_membership(
    services: &super::super::PluginHostServices,
    context: &PluginInvocationContext,
) -> Result<(), HostCapabilityError> {
    let connection = context.session_id().ok_or_else(requires_nested_context)?;
    services
        .mcp_session_membership()
        .scope(connection, context.scope_epoch())
        .map(|_| ())
        .map_err(|error| {
            HostCapabilityError::new("project.import", "source_import_scope_denied", error, false)
        })
}

fn require_semantic_import(
    call: &HostCapabilityRequest,
    context: &PluginInvocationContext,
) -> Result<(), HostCapabilityError> {
    if call.plugin_id != "builtin.semantic"
        || context.scope_id() != Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE)
        || context.plugin_chain().last().is_none_or(|frame| {
            frame.plugin_id != "builtin.semantic" || frame.export_id != "index_project"
        })
    {
        return Err(import_denied(&format!(
            "caller {:?}, scope {:?}; expected the live Semantic MCP index_project export",
            call.plugin_id,
            context.scope_id()
        )));
    }
    Ok(())
}

fn authorize_project(
    broker: &AppCoreHostCapabilityBroker,
    context: &PluginInvocationContext,
    root: &str,
) -> Result<(), HostCapabilityError> {
    let bridge = super::super::plugin_host_capabilities::invocation_control(context);
    broker.knowledge_authority.authorize_source(
        context,
        broker.semantic.as_ref(),
        root,
        true,
        &bridge.control(),
    )
}

fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, HostCapabilityError> {
    serde_json::from_value(input.clone()).map_err(|error| {
        HostCapabilityError::new(
            "project.import",
            "invalid_source_import_request",
            format!("request {input}: {error}; expected the closed import_folder request"),
            false,
        )
    })
}

pub(super) fn requires_nested_context() -> HostCapabilityError {
    HostCapabilityError::new(
        "project.import",
        "source_import_context_required",
        "source import requires an active controlled invocation with runtime publication access",
        false,
    )
}

pub(crate) struct SourceImportAuthorization {
    roots: Result<Vec<ApprovedSourceRoot>, String>,
}

struct ApprovedSourceRoot {
    configured: PathBuf,
    canonical: PathBuf,
}

impl SourceImportAuthorization {
    /// Inspects metadata only; permission must still be checked before reading source.
    /// Example: native Settings pins a selected alias to its canonical directory.
    pub(crate) fn inspect_folder(
        folder: &str,
    ) -> Result<lumvise_contracts::ConnectorSourceFolder, HostCapabilityError> {
        let requested = Path::new(folder);
        if !requested.is_absolute() {
            return Err(import_denied(&format!(
                "folder {folder:?}; expected an absolute directory"
            )));
        }
        let root =
            ApprovedSourceRoot::read(requested.into()).map_err(|error| import_denied(&error))?;
        let canonical = root
            .canonical
            .to_str()
            .ok_or_else(requires_nested_context)?;
        Ok(lumvise_contracts::ConnectorSourceFolder {
            requested_path: folder.into(),
            canonical_path: canonical.into(),
        })
    }

    pub(crate) fn bootstrap_allows(
        &self,
        folder: &lumvise_contracts::ConnectorSourceFolder,
    ) -> Result<bool, HostCapabilityError> {
        let roots = self.roots.as_ref().map_err(|error| import_denied(error))?;
        for root in roots {
            if root.contains_requested(Path::new(&folder.requested_path))
                && Path::new(&folder.canonical_path).starts_with(&root.canonical)
            {
                let current = root
                    .configured
                    .canonicalize()
                    .map_err(|error| import_denied(&format!("configured root {:?}: {error}; expected an accessible authorized directory", root.configured)))?;
                if current != root.canonical {
                    return Err(import_denied(&format!(
                        "configured root {:?} resolves to {current:?}; expected {:?}",
                        root.configured, root.canonical
                    )));
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn configured(value: Option<OsString>) -> Self {
        let roots = value.map_or_else(
            || Ok(Vec::new()),
            |value| {
                std::env::split_paths(&value)
                    .map(ApprovedSourceRoot::read)
                    .collect()
            },
        );
        Self { roots }
    }

    #[cfg(test)]
    pub(super) fn canonical_folder(&self, folder: &str) -> Result<PathBuf, HostCapabilityError> {
        let inspected = Self::inspect_folder(folder)?;
        if self.bootstrap_allows(&inspected)? {
            return Ok(PathBuf::from(inspected.canonical_path));
        }
        Err(import_denied(&format!(
            "folder {folder:?}; expected a host-authorized absolute source root"
        )))
    }
}

impl ApprovedSourceRoot {
    fn read(configured: PathBuf) -> Result<Self, String> {
        if !configured.is_absolute() {
            return Err(format!(
                "source-import root {configured:?}; expected an absolute configured directory"
            ));
        }
        let canonical = configured.canonicalize().map_err(|error|
            format!("source-import root {configured:?}: {error}; expected an accessible configured directory"))?;
        require_directory(&canonical)?;
        Ok(Self {
            configured,
            canonical,
        })
    }

    fn contains_requested(&self, requested: &Path) -> bool {
        requested.starts_with(&self.configured) || requested.starts_with(&self.canonical)
    }
}

fn require_directory(path: &Path) -> Result<(), String> {
    let metadata = path.metadata().map_err(|error| {
        format!("source-import path {path:?}: {error}; expected an accessible directory")
    })?;
    if !metadata.is_dir() {
        return Err(format!("source-import path {path:?}; expected a directory"));
    }
    Ok(())
}

fn import_denied(message: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        "project.import",
        "source_import_root_denied",
        message,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_import_project_uses_the_same_canonical_route_as_publication() {
        let route = FolderRoute {
            folder_path: "/allowed/alias".into(),
            project_root: None,
            _replace_paths: None,
            _import_id: None,
            _cancel: false,
            _status_only: false,
        };
        assert_eq!(
            effective_project_root(&route, Path::new("/allowed/actual")).unwrap(),
            "/allowed/actual"
        );
        let explicit = FolderRoute {
            project_root: Some("/logical/project".into()),
            ..route
        };
        assert_eq!(
            effective_project_root(&explicit, Path::new("/allowed/actual")).unwrap(),
            "/logical/project"
        );
    }

    fn configured(root: &Path) -> SourceImportAuthorization {
        SourceImportAuthorization::configured(Some(std::env::join_paths([root]).unwrap()))
    }

    #[test]
    fn source_import_needs_a_separate_host_grant_and_canonical_containment() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("src");
        std::fs::create_dir(&child).unwrap();
        let path = child.to_str().unwrap();
        assert!(
            SourceImportAuthorization::configured(None)
                .canonical_folder(path)
                .is_err()
        );
        assert_eq!(
            configured(root.path()).canonical_folder(path).unwrap(),
            child.canonicalize().unwrap()
        );
        let outside = tempfile::tempdir().unwrap();
        assert!(
            configured(root.path())
                .canonical_folder(outside.path().to_str().unwrap())
                .is_err()
        );
    }

    #[test]
    fn malformed_host_policy_and_file_targets_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file.rs");
        std::fs::write(&file, "synthetic").unwrap();
        let relative = SourceImportAuthorization::configured(Some("relative".into()));
        assert!(
            relative
                .canonical_folder(root.path().to_str().unwrap())
                .is_err()
        );
        assert!(
            configured(root.path())
                .canonical_folder(file.to_str().unwrap())
                .is_err()
        );
        assert!(
            configured(&file)
                .canonical_folder(root.path().to_str().unwrap())
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_import_rejects_symlink_and_parent_escapes_before_inventory() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let escape = root.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape).unwrap();
        assert!(
            configured(root.path())
                .canonical_folder(escape.to_str().unwrap())
                .is_err()
        );
        assert!(
            configured(root.path())
                .canonical_folder(root.path().join("..").to_str().unwrap())
                .is_err()
        );
    }
}

struct ScopedImportAuthorization {
    authority: super::super::knowledge_authority::KnowledgeAuthority,
    semantic: Arc<dyn lumvise_db_core::SemanticPersistence>,
    policy: Arc<SourceImportAuthorization>,
    folder: PathBuf,
    route: String,
    context: PluginInvocationContext,
    membership: Arc<crate::app::mcp_session_registry::McpSessionMembership>,
}

impl crate::app::project_import::ImportAuthorization for ScopedImportAuthorization {
    fn check(
        &self,
        control: &lumvise_resource_routing::InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let connection = self
            .context
            .session_id()
            .ok_or_else(requires_nested_context)?;
        self.membership
            .scope(connection, self.context.scope_epoch())
            .map_err(|error| import_denied(&error))?;
        let canonical = self.authority.require_source_folder(
            &self.context,
            &self.policy,
            self.folder.to_str().ok_or_else(requires_nested_context)?,
            control,
            false,
        )?;
        if canonical != self.folder {
            return Err(import_denied(&format!(
                "source folder {:?} resolves to {canonical:?}; expected the original directory",
                self.folder
            )));
        }
        self.authority.authorize_source(
            &self.context,
            self.semantic.as_ref(),
            &self.route,
            true,
            control,
        )
    }
}
