//! Shared importer state is composed before plugin restore; no AppCore/runtime back-reference.
use super::*;
use crate::app::project_import::{ProjectFolderImportService, ProjectImportSessions};
use lumvise_db_core::RelationalPersistence;
use lumvise_project_indexer::DocumentConverter;

#[derive(Default)]
pub(super) struct ProjectImportHostState {
    sessions: Arc<Mutex<ProjectImportSessions>>,
    converter: Arc<Mutex<Arc<DocumentConverter>>>,
    service: OnceLock<Arc<ProjectFolderImportService>>,
}

impl PluginHostServices {
    pub(crate) fn source_import_authorization(
        &self,
    ) -> Arc<super::super::host_capability_broker::SourceImportAuthorization> {
        Arc::clone(&self.source_import_authorization)
    }

    pub(crate) fn project_import_control(
        context: &PluginInvocationContext,
    ) -> InvocationControlBridge {
        invocation_control(context)
    }

    pub(crate) fn install_project_import_service(
        &self,
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
    ) {
        self.project_import.service.get_or_init(|| {
            Arc::new(ProjectFolderImportService::new(
                semantic,
                relational,
                self.index_publication_client(),
                self.project_import_sessions(),
                self.frontend(),
                self.project_document_converter(),
            ))
        });
    }

    pub(crate) fn project_import_service(&self) -> Arc<ProjectFolderImportService> {
        Arc::clone(
            self.project_import
                .service
                .get()
                .expect("import service composed before plugin restore"),
        )
    }

    pub(crate) fn project_import_sessions(&self) -> Arc<Mutex<ProjectImportSessions>> {
        Arc::clone(&self.project_import.sessions)
    }

    pub(crate) fn project_document_converter(&self) -> Arc<Mutex<Arc<DocumentConverter>>> {
        Arc::clone(&self.project_import.converter)
    }
}
