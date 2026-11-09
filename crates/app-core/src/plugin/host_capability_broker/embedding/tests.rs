use super::*;
use crate::plugin::host_capability_broker::tests::{app_with_default_broker, request};
use lumvise_neural_core::text2vector::{
    TextChunkPlan, TextChunkPlanRequest, TextChunkPlanner, TextChunkRange,
};
use lumvise_plugin_runtime::HostCapabilityBroker;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

struct ProfileCounterService {
    profile: &'static str,
    calls: AtomicUsize,
    barrier: Option<Arc<Barrier>>,
}

impl ProfileCounterService {
    fn new(profile: &'static str) -> Arc<Self> {
        Arc::new(Self {
            profile,
            calls: AtomicUsize::new(0),
            barrier: None,
        })
    }
}

impl TextChunkPlanner for ProfileCounterService {
    fn embedding_profile_fingerprint(&self) -> lumvise_neural_core::Result<String> {
        if let Some(barrier) = &self.barrier {
            barrier.wait();
            barrier.wait();
        }
        Ok(self.profile.into())
    }

    fn plan_text_chunks(
        &self,
        request: &TextChunkPlanRequest,
    ) -> lumvise_neural_core::Result<TextChunkPlan> {
        Ok(TextChunkPlan {
            engine_id: self.profile.into(),
            model: None,
            profile_fingerprint: self.profile.into(),
            ranges: vec![TextChunkRange {
                start_byte: 0,
                end_byte: request.text.len(),
                token_count: 3,
            }],
        })
    }
}

impl ArtifactTextVectorizer for ProfileCounterService {
    fn vectorize_artifact_text(
        &self,
        _: &str,
        _: TextEmbeddingTask,
    ) -> lumvise_db_core::Result<lumvise_db_core::ArtifactTextVector> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(lumvise_db_core::ArtifactTextVector {
            engine_id: self.profile.into(),
            model: None,
            dimensions: 2,
            vector: vec![1.0, 0.0],
            normalized: true,
            metadata: json!({}),
        })
    }
}

#[test]
fn profile_mismatch_is_rejected_before_inference() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let service = ProfileCounterService::new("profile-a");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(service.clone()));
    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["secret source"], "expected_profile_fingerprint": "profile-b"}),
        ))
        .unwrap_err();
    assert!(error.to_string().contains("embedding_profile_conflict"));
    assert!(!error.to_string().contains("secret source"));
    assert_eq!(service.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn concurrent_replacement_keeps_guard_and_inference_on_selected_service() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let old = Arc::new(ProfileCounterService {
        profile: "old",
        calls: AtomicUsize::new(0),
        barrier: Some(barrier.clone()),
    });
    let new = ProfileCounterService::new("new");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(old.clone()));
    std::thread::scope(|scope| {
        let call = scope.spawn(|| {
            broker.invoke(request(
                "plugin.semantic",
                NEURAL_EMBED_CAPABILITY,
                json!({"texts": ["source"], "expected_profile_fingerprint": "old"}),
            ))
        });
        barrier.wait();
        *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(new.clone()));
        barrier.wait();
        let result = call.join().unwrap().unwrap();
        assert_eq!(result["engine_id"], "old");
        assert_eq!(result["profile_fingerprint"], "old");
    });
    assert_eq!(old.calls.load(Ordering::SeqCst), 1);
    assert_eq!(new.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn chunk_capability_returns_ranges_without_embedding_or_source_echo() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let service = ProfileCounterService::new("selected");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(service.clone()));
    let result = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_CHUNK_TEXT_CAPABILITY,
            json!({"text": "秘密 source", "task": "document", "max_tokens": 1024}),
        ))
        .unwrap();
    assert_eq!(
        result["ranges"],
        json!([{"start_byte": 0, "end_byte": 13, "token_count": 3}])
    );
    assert!(!result.to_string().contains("秘密"));
    assert_eq!(service.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn malformed_chunk_and_embed_errors_do_not_echo_source() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    for (capability, input) in [
        (
            NEURAL_CHUNK_TEXT_CAPABILITY,
            json!({"text": "secret source", "max_tokens": 0}),
        ),
        (
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["secret source"], "unexpected": true}),
        ),
    ] {
        let error = broker
            .invoke(request("plugin.semantic", capability, input))
            .unwrap_err();
        assert!(!error.to_string().contains("secret source"));
        assert!(error.to_string().contains("invalid_host_capability_input"));
    }
}

#[test]
fn large_text_ranges_and_large_aggregate_are_embedded_without_byte_budgets() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let service = ProfileCounterService::new("selected");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(service.clone()));
    let texts = vec!["x".repeat(40000); 8];
    let output = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": texts, "expected_profile_fingerprint": "selected"}),
        ))
        .unwrap();
    assert_eq!(output["vectors"].as_array().unwrap().len(), 8);
    assert_eq!(service.calls.load(Ordering::SeqCst), 8);
}

#[test]
fn unplanned_backend_rejects_guard_and_chunk_capability_explicitly() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let service = ProfileCounterService::new("unplanned");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::unplanned(service.clone()));
    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_EMBED_CAPABILITY,
            json!({"texts": ["body"], "expected_profile_fingerprint": "unplanned"}),
        ))
        .unwrap_err();
    assert!(error.to_string().contains("unsupported_embedding_profile"));
    let error = broker
        .invoke(request(
            "plugin.semantic",
            NEURAL_CHUNK_TEXT_CAPABILITY,
            json!({"text": "body", "task": "document", "max_tokens": 1024}),
        ))
        .unwrap_err();
    assert!(error.to_string().contains("unsupported_text_chunk_planner"));
    assert_eq!(service.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn malformed_empty_and_whitespace_inputs_are_refused_before_inference() {
    let app = app_with_default_broker();
    let broker = app.default_plugin_host_capability_broker().unwrap();
    let service = ProfileCounterService::new("selected");
    *broker.vectorizer.lock().unwrap() = Some(PluginEmbeddingService::planned(service.clone()));
    for texts in [
        json!([]),
        json!([""]),
        json!([" \n\t"]),
        json!([3]),
        json!("body"),
    ] {
        let error = broker
            .invoke(request(
                "plugin.semantic",
                NEURAL_EMBED_CAPABILITY,
                json!({"texts": texts}),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("invalid_host_capability_input"));
    }
    assert_eq!(service.calls.load(Ordering::SeqCst), 0);
}
