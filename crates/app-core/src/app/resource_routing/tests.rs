use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use lumvise_db_core::{LocalPersistence, RelationalPersistence, SemanticPersistence};
use lumvise_neural_core::{
    LlmConfigurationRequirement, LlmProviderAvailability, LlmProviderCatalog, LlmProviderKind,
    LlmProviderRegistry, LlmProviderStatus, LlmProviderSync, NeuralError, ProviderModelSources,
    speech::{SpeechRecognizer, SpeechSynthesizer},
    text2voice::{Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEventSink},
    voice2text::{Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEventSink},
};
use lumvise_resource_routing::{InvocationControl, ResourcePlacement, ResourceRoutingConfig};

use super::{ResourceAdapterFactory, ResourceRouter};

const INTERNAL_LLM: usize = 0;
const CENTRAL_LLM: usize = 1;
const INTERNAL_SPEECH: usize = 2;
const CENTRAL_SPEECH: usize = 3;
const INTERNAL_SEMANTIC: usize = 4;
const CENTRAL_SEMANTIC: usize = 5;
const INTERNAL_RELATIONAL: usize = 6;
const CENTRAL_RELATIONAL: usize = 7;

#[derive(Default)]
struct SilentSpeech;

impl SpeechRecognizer for SilentSpeech {
    fn warmup(&self) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
    fn transcribe(
        &self,
        _: &Voice2TextRequest,
        _: &InvocationControl,
    ) -> lumvise_neural_core::Result<Voice2TextResponse> {
        Err(NeuralError::ProviderFailed {
            provider_id: "fake".into(),
            message: "not invoked".into(),
        })
    }
    fn stream_with_events(
        &self,
        _: &Voice2TextRequest,
        _: &InvocationControl,
        _: &mut Voice2TextStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Err(NeuralError::ProviderFailed {
            provider_id: "fake".into(),
            message: "not invoked".into(),
        })
    }
}

impl SpeechSynthesizer for SilentSpeech {
    fn warmup(&self) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
    fn synthesize(
        &self,
        _: &Text2VoiceRequest,
        _: &InvocationControl,
    ) -> lumvise_neural_core::Result<Text2VoiceResponse> {
        Err(NeuralError::ProviderFailed {
            provider_id: "fake".into(),
            message: "not invoked".into(),
        })
    }
    fn stream_with_events(
        &self,
        _: &Text2VoiceRequest,
        _: &InvocationControl,
        _: &mut Text2VoiceStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Err(NeuralError::ProviderFailed {
            provider_id: "fake".into(),
            message: "not invoked".into(),
        })
    }
}

struct NamedFakes {
    calls: [AtomicUsize; 8],
    semantic: Arc<dyn SemanticPersistence>,
    relational: Arc<dyn RelationalPersistence>,
}

impl NamedFakes {
    fn new(root: &std::path::Path) -> Self {
        let persistence = Arc::new(LocalPersistence::open(root).unwrap());
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence;
        Self {
            calls: std::array::from_fn(|_| AtomicUsize::new(0)),
            semantic,
            relational,
        }
    }
    fn selected(&self, index: usize) {
        self.calls[index].fetch_add(1, Ordering::SeqCst);
    }
    fn calls(&self, index: usize) -> usize {
        self.calls[index].load(Ordering::SeqCst)
    }
}

fn fake_llm_sync(provider_id: &str) -> LlmProviderSync {
    LlmProviderSync {
        registry: LlmProviderRegistry::empty(),
        catalog: LlmProviderCatalog {
            providers: vec![LlmProviderStatus {
                provider_id: provider_id.to_string(),
                kind: LlmProviderKind::Local,
                state: LlmProviderAvailability::MissingConfiguration {
                    requirement: LlmConfigurationRequirement::Configuration,
                },
                model_sources: ProviderModelSources {
                    api: None,
                    client: None,
                },
                active_source: None,
                selected_transport: None,
            }],
        },
    }
}

impl ResourceAdapterFactory for NamedFakes {
    type Error = std::io::Error;
    fn internal_llms(&self) -> Result<LlmProviderSync, Self::Error> {
        self.selected(INTERNAL_LLM);
        Ok(fake_llm_sync("fake-internal"))
    }
    fn centralized_llms(&self) -> Result<LlmProviderSync, Self::Error> {
        self.selected(CENTRAL_LLM);
        Ok(fake_llm_sync("fake-central"))
    }
    fn internal_speech(
        &self,
    ) -> Result<
        (
            Option<Arc<dyn SpeechRecognizer>>,
            Option<Arc<dyn SpeechSynthesizer>>,
        ),
        Self::Error,
    > {
        self.selected(INTERNAL_SPEECH);
        Ok((None, None))
    }
    fn centralized_speech(
        &self,
    ) -> Result<(Arc<dyn SpeechRecognizer>, Arc<dyn SpeechSynthesizer>), Self::Error> {
        self.selected(CENTRAL_SPEECH);
        Ok((Arc::new(SilentSpeech), Arc::new(SilentSpeech)))
    }
    fn internal_semantic(&self) -> Result<Arc<dyn SemanticPersistence>, Self::Error> {
        self.selected(INTERNAL_SEMANTIC);
        Ok(self.semantic.clone())
    }
    fn centralized_semantic(&self) -> Result<Arc<dyn SemanticPersistence>, Self::Error> {
        self.selected(CENTRAL_SEMANTIC);
        Ok(self.semantic.clone())
    }
    fn internal_relational(&self) -> Result<Arc<dyn RelationalPersistence>, Self::Error> {
        self.selected(INTERNAL_RELATIONAL);
        Ok(self.relational.clone())
    }
    fn centralized_relational(&self) -> Result<Arc<dyn RelationalPersistence>, Self::Error> {
        self.selected(CENTRAL_RELATIONAL);
        Ok(self.relational.clone())
    }
}

fn route(central: bool) -> ResourcePlacement {
    if central {
        ResourcePlacement::Centralized
    } else {
        ResourcePlacement::Internal
    }
}

fn assert_selection(llm: bool, speech: bool, semantic: bool, relational: bool) {
    let temp = tempfile::tempdir().unwrap();
    let fakes = NamedFakes::new(&temp.path().join("selection.sqlite"));
    let config = ResourceRoutingConfig {
        llm_execution: route(llm),
        speech_inference: route(speech),
        graph_persistence: route(semantic),
        sql_persistence: route(relational),
        central: None,
    };
    let resources = ResourceRouter::build_with_factories(&config, &fakes).unwrap();
    for (internal, central, selected_central) in [
        (INTERNAL_LLM, CENTRAL_LLM, llm),
        (INTERNAL_SPEECH, CENTRAL_SPEECH, speech),
        (INTERNAL_SEMANTIC, CENTRAL_SEMANTIC, semantic),
        (INTERNAL_RELATIONAL, CENTRAL_RELATIONAL, relational),
    ] {
        assert_eq!(fakes.calls(internal), usize::from(!selected_central));
        assert_eq!(fakes.calls(central), usize::from(selected_central));
    }
    assert_eq!(resources.llm_catalog.providers.len(), 1);
    assert_eq!(
        resources.llm_catalog.providers[0].provider_id,
        if llm { "fake-central" } else { "fake-internal" }
    );
}

macro_rules! placement_case {
    ($name:ident, $llm:expr, $speech:expr, $semantic:expr, $relational:expr) => {
        #[test]
        fn $name() {
            assert_selection($llm, $speech, $semantic, $relational);
        }
    };
}

placement_case!(
    internal_internal_internal_internal,
    false,
    false,
    false,
    false
);
placement_case!(
    central_internal_internal_internal,
    true,
    false,
    false,
    false
);
placement_case!(
    internal_central_internal_internal,
    false,
    true,
    false,
    false
);
placement_case!(central_central_internal_internal, true, true, false, false);
placement_case!(
    internal_internal_central_internal,
    false,
    false,
    true,
    false
);
placement_case!(central_internal_central_internal, true, false, true, false);
placement_case!(internal_central_central_internal, false, true, true, false);
placement_case!(central_central_central_internal, true, true, true, false);
placement_case!(
    internal_internal_internal_central,
    false,
    false,
    false,
    true
);
placement_case!(central_internal_internal_central, true, false, false, true);
placement_case!(internal_central_internal_central, false, true, false, true);
placement_case!(central_central_internal_central, true, true, false, true);
placement_case!(internal_internal_central_central, false, false, true, true);
placement_case!(central_internal_central_central, true, false, true, true);
placement_case!(internal_central_central_central, false, true, true, true);
placement_case!(central_central_central_central, true, true, true, true);

#[test]
fn mixed_internal_graph_and_llm_share_one_relational_composition() {
    let config = ResourceRoutingConfig {
        llm_execution: ResourcePlacement::Internal,
        speech_inference: ResourcePlacement::Internal,
        graph_persistence: ResourcePlacement::Internal,
        sql_persistence: ResourcePlacement::Centralized,
        central: None,
    };
    assert!(super::requires_local_persistence(&config));
}

struct ReadyStorageClient {
    requests: std::sync::Mutex<Vec<lumvise_resource_routing::protocol::ReadinessRequestV1>>,
    unavailable: bool,
}

impl lumvise_resource_routing::ResourceInvocationClient for ReadyStorageClient {
    fn readiness(
        &self,
        request: &lumvise_resource_routing::protocol::ReadinessRequestV1,
        _: &InvocationControl,
    ) -> Result<
        lumvise_resource_routing::protocol::ReadinessResponseV1,
        lumvise_resource_routing::transport::TransportError,
    > {
        self.requests.lock().unwrap().push(request.clone());
        Ok(lumvise_resource_routing::protocol::ReadinessResponseV1 {
            capabilities: request
                .requested_capabilities
                .iter()
                .map(
                    |capability| lumvise_resource_routing::protocol::CapabilityReadinessEntryV1 {
                        capability: *capability,
                        status: i32::from(self.unavailable),
                    },
                )
                .collect(),
            ..Default::default()
        })
    }
    fn invoke(
        &self,
        _: &[lumvise_resource_routing::protocol::InvocationEnvelopeV1],
        _: &InvocationControl,
    ) -> Result<
        Vec<lumvise_resource_routing::protocol::InvocationEnvelopeV1>,
        lumvise_resource_routing::transport::TransportError,
    > {
        panic!("startup must not issue a persistence operation")
    }
    fn cancel(
        &self,
        _: &lumvise_resource_routing::protocol::InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> Result<
        lumvise_resource_routing::protocol::InvocationTerminalV1,
        lumvise_resource_routing::transport::TransportError,
    > {
        panic!("startup must not issue cancellation")
    }
}

#[test]
fn injected_storage_retains_local_compute_and_needs_no_local_database() {
    let client = Arc::new(ReadyStorageClient {
        requests: Default::default(),
        unavailable: false,
    });
    let (config, resources) =
        ResourceRouter::build_selection(super::AppResourceSelection::RemoteStorage {
            client: client.clone(),
            client_instance_id: "durable-client".into(),
        })
        .unwrap();
    assert!(!super::requires_local_persistence(&config));
    assert_eq!(config.llm_execution, ResourcePlacement::Internal);
    assert_eq!(config.speech_inference, ResourcePlacement::Internal);
    assert!(resources.llm_catalog.providers.is_empty());
    assert!(resources.speech_recognizer.is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].client_instance_id, "durable-client");
    assert_eq!(
        requests[0].requested_capabilities,
        vec![
            super::ResourceCapabilityV1::GraphPersistence as i32,
            super::ResourceCapabilityV1::SqlPersistence as i32
        ]
    );
}

#[test]
fn injected_unavailable_storage_fails_without_local_fallback() {
    let client = Arc::new(ReadyStorageClient {
        requests: Default::default(),
        unavailable: true,
    });
    let result = ResourceRouter::build_selection(super::AppResourceSelection::RemoteStorage {
        client: client.clone(),
        client_instance_id: "durable-client".into(),
    });
    assert!(
        result
            .err()
            .unwrap()
            .contains("graph_persistence route is not ready")
    );
    let empty = ResourceRouter::build_selection(super::AppResourceSelection::RemoteStorage {
        client,
        client_instance_id: String::new(),
    });
    assert!(
        empty
            .err()
            .unwrap()
            .contains("expected stable client identity")
    );
}
