//! Composes shared host services once, before plugin restoration.
use super::*;

impl PluginHostServices {
    pub(crate) fn new(
        frontend: FrontendCore,
        llms: LlmProviderRegistry,
        semantic: Arc<dyn SemanticPersistence>,
    ) -> Arc<Self> {
        let llms = Arc::new(Mutex::new(llms));
        let llm_executors = LlmExecutorRegistry::new(Arc::clone(&llms));
        let assistant_llm_executors = LlmExecutorRegistry::new(Arc::clone(&llms));
        let workspace_llm_executors = LlmExecutorRegistry::new(Arc::clone(&llms));
        let activity = Arc::new(crate::workspace_activity::WorkspaceActivity::default());
        let frontend = Arc::new(Mutex::new(frontend));
        let speech_recognizer = Arc::new(Mutex::new(None));
        let speech_synthesizer = Arc::new(Mutex::new(None));
        Arc::new(Self {
            project_import: Default::default(),
            source_import_authorization: Arc::new(
                super::super::host_capability_broker::SourceImportAuthorization::configured(
                    std::env::var_os("LUMVISE_SOURCE_IMPORT_ROOTS"),
                ),
            ),
            semantic,
            index_publication_client: Arc::new(OnceLock::new()),
            mcp_session_membership: Arc::default(),
            audio_connections: LiveAudioConnections::default(),
            frontend: Arc::clone(&frontend),
            llms: Arc::clone(&llms),
            llm_executors: Arc::clone(&llm_executors),
            assistant_llm_executors: Arc::clone(&assistant_llm_executors),
            workspace_llm_executors: Arc::clone(&workspace_llm_executors),
            activity: Arc::clone(&activity),
            frontend_actions: Arc::new(Mutex::new(Vec::new())),
            frontend_action_ids: Mutex::new(HashSet::new()),
            scoped_mcp_base_url: Arc::new(Mutex::new(None)),
            exclusive_lanes: Arc::new(ExclusiveInvocationLanes::new()),
            speech_recognizer,
            speech_synthesizer,
            speech_to_text_executor: Mutex::new(None),
            text_to_speech_executor: Mutex::new(None),
            workspace_speech_executor: Mutex::new(None),
            voice_playback_transport: Arc::new(
                super::super::voice_playback_transport::VoicePlaybackTransport::new(),
            ),
            playback_owners: Arc::new(Mutex::new(HashMap::new())),
            playback_controls: Arc::new(Mutex::new(HashMap::new())),
            playback_opened: Arc::new(Mutex::new(HashSet::new())),
            playback_sequences: Arc::new(Mutex::new(HashMap::new())),
            background_jobs: BackgroundJobAdapter::new(
                llm_executors,
                assistant_llm_executors,
                workspace_llm_executors,
                frontend,
                activity,
            ),
            turn_wait: TurnWaitAdapter::new(),
            project_execution: Mutex::new(None),
            host_extensions: Mutex::new(HashMap::new()),
            link_decisions: Mutex::new(None),
        })
    }
}
