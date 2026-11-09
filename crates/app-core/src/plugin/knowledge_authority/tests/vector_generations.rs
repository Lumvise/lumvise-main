//! A surviving conversation must not deliver a write acknowledgement after its grant changes.
use super::*;
use lumvise_db_core::{
    ElementTextVectorGenerationKey as Key, ElementTextVectorGenerationState as State,
    ElementTextVectorGenerationStatus as Status, SemanticProjectAccess, SemanticReadiness,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct FakeGenerationSemanticPersistence {
    persistence: Arc<LocalPersistence>,
    writable: AtomicBool,
    revoke_on_write: bool,
    completed: AtomicUsize,
}

impl FakeGenerationSemanticPersistence {
    fn new(fixture: &AuthorityFixture, writable: bool, revoke_on_write: bool) -> Self {
        fixture.seed_project("/remote");
        Self {
            persistence: fixture.persistence.clone(),
            writable: AtomicBool::new(writable),
            revoke_on_write,
            completed: AtomicUsize::new(0),
        }
    }
}

impl SemanticPersistence for FakeGenerationSemanticPersistence {
    fn readiness(&self) -> lumvise_db_core::Result<SemanticReadiness> {
        SemanticPersistence::readiness(self.persistence.as_ref())
    }

    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        match operation {
            SemanticOperation::ProjectAccess { project_root } if project_root == "/remote" => Ok(
                SemanticResult::ProjectAccess(SemanticProjectAccess::Member {
                    project_id: "remote-lineage".into(),
                    storage_authority: "https://app.lumvise.com".into(),
                    writable: self.writable.load(Ordering::SeqCst),
                }),
            ),
            SemanticOperation::BeginElementTextVectorGeneration { spec } => self.complete(spec.key),
            SemanticOperation::StageElementTextVectorChunk { key, .. }
            | SemanticOperation::CommitElementTextVectorGeneration { key }
            | SemanticOperation::AbortElementTextVectorGeneration { key } => self.complete(key),
            other => SemanticPersistence::execute(self.persistence.as_ref(), other, control),
        }
    }
}

impl FakeGenerationSemanticPersistence {
    fn complete(&self, key: Key) -> lumvise_db_core::Result<SemanticResult> {
        self.completed.fetch_add(1, Ordering::SeqCst);
        if self.revoke_on_write {
            self.writable.store(false, Ordering::SeqCst);
        }
        Ok(SemanticResult::ElementTextVectorGeneration(Status {
            key,
            state: State::Committed,
            next_chunk: 1,
            chunk_count: 1,
            published_count: 1,
        }))
    }
}

fn generation_input(operation: &str, root: &str) -> Value {
    let key =
        json!({"project_root":root,"semantic_element_id":"section","generation_id":"generation"});
    match operation {
        "begin_element_text_vector_generation" => json!({"operation":operation,"spec":{"key":key,
            "content_fingerprint":"fingerprint","engine_id":"fixture","model":null,
            "profile_fingerprint":"profile","dimensions":2,"chunk_count":1,"chunk_plan_fingerprint":"plan"}}),
        "stage_element_text_vector_chunk" => {
            json!({"operation":operation,"key":key,"chunk":0,"vector":[1.0,0.0]})
        }
        operation => json!({"operation":operation,"key":key}),
    }
}

const OPERATIONS: [&str; 4] = [
    "begin_element_text_vector_generation",
    "stage_element_text_vector_chunk",
    "commit_element_text_vector_generation",
    "abort_element_text_vector_generation",
];

fn invoke_generation(
    fixture: &AuthorityFixture,
    source: &FakeGenerationSemanticPersistence,
    operation: &str,
    root: &str,
) -> Result<Value, HostCapabilityError> {
    crate::plugin::semantic_storage_capability::invoke_controlled(
        source,
        &crate::plugin::semantic_snapshot_writes::SemanticSnapshotWrites::new(),
        "builtin.semantic",
        generation_input(operation, root),
        &invocation("generation-session", 1),
        &fixture.authority,
    )
}

#[test]
fn generation_status_requires_the_same_writable_root_before_and_after_io() {
    for operation in OPERATIONS {
        let fixture = AuthorityFixture::new();
        fixture.bind("generation-session", 1, true).unwrap();
        let source = FakeGenerationSemanticPersistence::new(&fixture, true, false);
        let status = invoke_generation(&fixture, &source, operation, "/remote").unwrap();
        assert_eq!(status["state"], "committed");
        assert_eq!(status["published_count"], 1);
        assert_eq!(source.completed.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn readonly_or_foreign_generation_root_is_denied_before_underlying_write() {
    for operation in OPERATIONS {
        for (writable, root) in [(false, "/remote"), (true, "/foreign")] {
            let fixture = AuthorityFixture::new();
            fixture.bind("generation-session", 1, true).unwrap();
            let source = FakeGenerationSemanticPersistence::new(&fixture, writable, false);
            assert!(invoke_generation(&fixture, &source, operation, root).is_err());
            assert_eq!(source.completed.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn generation_succeeds_underneath_but_revoked_write_grant_blocks_status_in_live_session() {
    for operation in OPERATIONS {
        let fixture = AuthorityFixture::new();
        fixture.bind("generation-session", 1, true).unwrap();
        let source = FakeGenerationSemanticPersistence::new(&fixture, true, true);
        let error = invoke_generation(&fixture, &source, operation, "/remote")
            .unwrap_err()
            .to_string();
        assert!(error.contains("authorized project"), "{error}");
        assert_eq!(
            source.completed.load(Ordering::SeqCst),
            1,
            "operation must succeed before post-I/O denial"
        );
        let surviving = fixture
            .authority
            .current(
                &source,
                &invocation("generation-session", 1),
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
        let grant = surviving
            .project_grants
            .iter()
            .find(|grant| grant.project.project_root == "/remote")
            .unwrap();
        assert!(
            !grant.writable,
            "session survives with a read-only project grant"
        );
    }
}
