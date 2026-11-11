//! Native semantic-graph and permission bridge adapters; authority remains in AppCore.
use super::*;
use crate::app::{canvas_files, source_file};

fn normalized_graph_identity(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn resolved_graph_project_root(request: &DesktopSemanticGraphRequest) -> Result<String, String> {
    let provider_id = normalized_graph_identity(request.provider_id.as_deref());
    let project_root = normalized_graph_identity(request.project_root.as_deref());
    match (provider_id, project_root) {
        (Some(provider_id), Some(project_root)) if provider_id != project_root => {
            Err("providerId must equal projectRoot for the app-owned semantic index".to_string())
        }
        (Some(provider_id), _) => Ok(provider_id),
        (_, Some(project_root)) => Ok(project_root),
        (None, None) => {
            Err("semantic graph request requires providerId or projectRoot".to_string())
        }
    }
}

impl DesktopSemanticGraphBridge for AppCoreDesktopBridge {
    fn connector_source_access(
        &self,
    ) -> Result<Vec<lumvise_contracts::ConnectorSourceAccess>, String> {
        self.app.connector_source_access()
    }

    fn change_connector_source_access(
        &self,
        change: lumvise_contracts::ConnectorSourceAccessChange,
    ) -> Result<lumvise_contracts::ConnectorSourceAccess, String> {
        self.app.change_connector_source_access(change)
    }

    fn connector_knowledge_grants(
        &self,
        root: &str,
        maintainer: &dyn lumvise_contracts::ConnectorGrantMaintainer,
    ) -> Result<Vec<lumvise_contracts::ConnectorKnowledgeGrant>, String> {
        self.app.connector_knowledge_grants(root, maintainer)
    }

    fn decide_connector_knowledge_grant(
        &self,
        root: &str,
        connector: &str,
        revision: u64,
        decision: lumvise_contracts::ConnectorGrantState,
        maintainer: &dyn lumvise_contracts::ConnectorGrantMaintainer,
    ) -> Result<lumvise_contracts::ConnectorKnowledgeGrant, String> {
        self.app
            .decide_connector_knowledge_grant(root, connector, revision, decision, maintainer)
    }
    fn wait_for_storage_revision(&self, after_revision: i64) -> Result<i64, String> {
        storage_revisions::wait_for_revision(self.app.semantic.as_ref(), after_revision)
    }

    fn list_indexed_semantic_graph_roots(&self) -> Result<Vec<String>, String> {
        match self.app.semantic.execute(
            SemanticOperation::ProjectRoots,
            &InvocationControl::sixty_seconds(),
        ) {
            Ok(SemanticResult::ProjectRoots(roots)) => Ok(roots
                .into_iter()
                .filter(|root| !lumvise_contracts::is_knowledge_space_root(root))
                .collect()),
            Ok(result) => Err(format!(
                "project roots returned unexpected semantic result: {result:?}"
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn project_indexed_semantic_graph(
        &self,
        request: DesktopSemanticGraphRequest,
    ) -> Result<CompactSemanticGraphProjection, String> {
        let project_root = resolved_graph_project_root(&request)?;
        if lumvise_contracts::is_knowledge_space_root(&project_root) {
            return Err(format!(
                "private document space `{project_root}` requires its Knowledge owner"
            ));
        }
        let operation = SemanticOperation::ProjectRendererGraph(SemanticGraphProjectionRequest {
            project_root,
            target_path: request.target_path,
            granularity: request.granularity,
            recursive: request.recursive,
            include_external: request.include_external,
            include_first_neighbors: request.include_first_neighbors,
        });
        match self
            .app
            .semantic
            .execute(operation, &InvocationControl::sixty_seconds())
        {
            Ok(SemanticResult::RendererGraphProjection(projection)) => {
                projection.into_compact().map_err(|error| error.to_string())
            }
            Ok(result) => Err(format!(
                "semantic graph returned unexpected semantic result: {result:?}"
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn read_source_file_bytes(
        &self,
        project_root: String,
        path: String,
    ) -> Result<Vec<u8>, String> {
        source_file::source_file_bytes(&self.app, &project_root, &path)
    }

    fn read_canvas_file(&self, content_ref: String) -> Result<Vec<u8>, String> {
        match canvas_files::read_canvas_file(&self.app, &content_ref) {
            Ok(Some((_, bytes))) => Ok(bytes),
            Ok(None) => Err(format!("canvas file {content_ref} was not found")),
            Err(error) => Err(error.to_string()),
        }
    }

    fn write_canvas_file(
        &self,
        artifact_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        let reference = canvas_files::put_canvas_file(&self.app, &artifact_id, &media_type, &bytes)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(reference).map_err(|error| error.to_string())
    }

    fn app_bridge_request(
        &self,
        request: DesktopAppBridgeRequest,
    ) -> Result<DesktopAppBridgeResponse, String> {
        let (status, body) = self.app.route_app_bridge_request(
            &request.method,
            &request.path,
            request.query,
            request.body.unwrap_or_default().into_bytes(),
        );
        Ok(DesktopAppBridgeResponse { status, body })
    }
}

impl DesktopSemanticGraphBridge for PendingDesktopBridge {
    fn connector_source_access(
        &self,
    ) -> Result<Vec<lumvise_contracts::ConnectorSourceAccess>, String> {
        self.with_ready("connector_source_access", |bridge| {
            bridge.connector_source_access()
        })
    }

    fn change_connector_source_access(
        &self,
        change: lumvise_contracts::ConnectorSourceAccessChange,
    ) -> Result<lumvise_contracts::ConnectorSourceAccess, String> {
        self.with_ready("change_connector_source_access", |bridge| {
            bridge.change_connector_source_access(change)
        })
    }

    fn connector_knowledge_grants(
        &self,
        root: &str,
        maintainer: &dyn lumvise_contracts::ConnectorGrantMaintainer,
    ) -> Result<Vec<lumvise_contracts::ConnectorKnowledgeGrant>, String> {
        self.with_ready("connector_knowledge_grants", |bridge| {
            bridge.connector_knowledge_grants(root, maintainer)
        })
    }

    fn decide_connector_knowledge_grant(
        &self,
        root: &str,
        connector: &str,
        revision: u64,
        decision: lumvise_contracts::ConnectorGrantState,
        maintainer: &dyn lumvise_contracts::ConnectorGrantMaintainer,
    ) -> Result<lumvise_contracts::ConnectorKnowledgeGrant, String> {
        self.with_ready("decide_connector_knowledge_grant", |bridge| {
            bridge.decide_connector_knowledge_grant(root, connector, revision, decision, maintainer)
        })
    }
    fn wait_for_storage_revision(&self, after_revision: i64) -> Result<i64, String> {
        self.with_ready("wait_for_storage_revision", |delegate| {
            delegate.wait_for_storage_revision(after_revision)
        })
    }

    fn list_indexed_semantic_graph_roots(&self) -> Result<Vec<String>, String> {
        self.with_ready("list_indexed_semantic_graph_roots", |delegate| {
            delegate.list_indexed_semantic_graph_roots()
        })
    }

    fn project_indexed_semantic_graph(
        &self,
        request: DesktopSemanticGraphRequest,
    ) -> Result<CompactSemanticGraphProjection, String> {
        self.with_ready("project_indexed_semantic_graph", |delegate| {
            delegate.project_indexed_semantic_graph(request)
        })
    }

    fn read_source_file_bytes(
        &self,
        project_root: String,
        path: String,
    ) -> Result<Vec<u8>, String> {
        self.with_ready("read_source_file_bytes", |delegate| {
            delegate.read_source_file_bytes(project_root, path)
        })
    }

    fn read_canvas_file(&self, content_ref: String) -> Result<Vec<u8>, String> {
        self.with_ready("read_canvas_file", |delegate| {
            delegate.read_canvas_file(content_ref)
        })
    }

    fn write_canvas_file(
        &self,
        artifact_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("write_canvas_file", |delegate| {
            delegate.write_canvas_file(artifact_id, media_type, bytes)
        })
    }

    fn app_bridge_request(
        &self,
        request: DesktopAppBridgeRequest,
    ) -> Result<DesktopAppBridgeResponse, String> {
        self.with_ready("app_bridge_request", |delegate| {
            delegate.app_bridge_request(request)
        })
    }
}
