use super::*;
use lumvise_db_core::{
    PluginDataMutation, RelationalOperation, SemanticRevisionPublicationOutcome,
};

const OWNER: &str = "builtin.knowledge";
const TABLE: &str = "citation_owner_observations";
const PUBLICATION: &str = "citation-publication";

fn citation_call(
    fixture: &AuthorityFixture,
    context: &PluginInvocationContext,
    request: Value,
) -> Result<Value, HostCapabilityError> {
    fixture.authority.invoke(
        OWNER,
        json!({"operation":"citation","request":request}),
        fixture.persistence.as_ref(),
        context,
    )
}

fn save_observation(store: &dyn RelationalPersistence, value: Value) {
    let control = InvocationControl::sixty_seconds();
    store
        .execute(
            RelationalOperation::EnsurePluginDataTable {
                plugin_id: OWNER.into(),
                table_name: TABLE.into(),
                schema: json!({}),
            },
            &control,
        )
        .unwrap();
    store
        .execute(
            RelationalOperation::ApplyMutations {
                plugin_id: OWNER.into(),
                mutations: vec![PluginDataMutation::Put {
                    table_name: TABLE.into(),
                    row_key: PUBLICATION.into(),
                    value,
                }],
            },
            &control,
        )
        .unwrap();
}

fn reader_invocation(fixture: &AuthorityFixture) -> PluginInvocationContext {
    let context = PluginInvocationContext::new(
        "citation-reader",
        "builtin.agents",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .enter_plugin("builtin.agents", "get_agent");
    fixture.bind_agent_owner(&context, "autonomous").unwrap();
    fixture.authority.invoke("builtin.agents", json!({"operation":"bind_agent","binding":{
        "agent_id":"scout","purpose":"autonomous","grants":[{"project_root":"/source","writable":false}],
        "run_id":null,"observations":[{"table":"observed_owner","row_key":"scout","expected":null}]
    }}), fixture.persistence.as_ref(), &context).unwrap();
    context.enter_plugin(OWNER, "create_document")
}

fn prepared_claim(fixture: &AuthorityFixture) -> (PluginInvocationContext, Value) {
    let record = fixture.seed_project("/source");
    adopt_source_revision(fixture, &record);
    let context = reader_invocation(fixture);
    let intent = citation_call(
        fixture,
        &context,
        json!({"operation":"intent","project_root":"agent-space://scout",
        "document_id":"decision","publication_id":PUBLICATION}),
    )
    .unwrap()["intent"]
        .clone();
    let source = citation_call(
        fixture,
        &context,
        json!({"operation":"project","project_root":"/source"}),
    )
    .unwrap();
    let observed = json!({"opaque_owner_state":"pending"});
    save_observation(fixture.persistence.as_ref(), observed.clone());
    (
        context,
        json!({"operation":"acquire","claim":{
        "project_root":"/source","project_id":source["project"]["project_id"],"source_context":"local",
        "intent":intent,"records":[{"kind":"element","record_id":record}],"artifact_edits":{}},
        "observation":{"project_root":"agent-space://scout","table":TABLE,"expected":observed}}),
    )
}

fn adopt_source_revision(fixture: &AuthorityFixture, record: &str) {
    let control = InvocationControl::sixty_seconds();
    let project = resolve_project(
        fixture.persistence.as_ref(),
        "/source",
        "fixture-authority",
        &control,
    )
    .unwrap();
    let update = serde_json::from_value(json!({"operation_id":"seed-citable-source",
        "project_root":"/source", "project_id":project.project_id,
        "selection_id":"fixture-source", "source_context":"local", "expected_generation":0,
        "mode":"pinned", "records":[{"kind":"element","record_id":record}],
        "artifact_edits":{}, "connection_ids":[], "source_anchor":null}))
    .unwrap();
    SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::SelectRevisions(update),
        &control,
    )
    .unwrap();
}

#[test]
fn read_only_agent_can_retain_an_observed_citation_but_cannot_manage_source_pins() {
    let fixture = AuthorityFixture::new();
    let (context, request) = prepared_claim(&fixture);
    let first = citation_call(&fixture, &context, request.clone()).unwrap();
    assert_eq!(first["publication"]["outcome"], "accepted");
    assert_eq!(
        citation_call(&fixture, &context, request.clone()).unwrap(),
        first
    );
    let source = &request["claim"];
    let update = serde_json::from_value(json!({"operation_id":"general-pin","project_root":"/source",
        "project_id":source["project_id"],"selection_id":"arbitrary","source_context":"local","expected_generation":0,
        "mode":"pinned","records":source["records"],"artifact_edits":{},"connection_ids":[],"source_anchor":null})).unwrap();
    let access = semantic_access::KnowledgeSemanticAccess {
        semantic: fixture.persistence.as_ref(),
        authority: &fixture.authority,
        context: &context,
        control: &InvocationControl::sixty_seconds(),
    };
    assert!(
        access
            .execute(SemanticOperation::SelectRevisions(update))
            .is_err()
    );
    let released = citation_call(&fixture, &context, json!({"operation":"release","claim":{
        "project_root":"/source","project_id":source["project_id"],"intent":source["intent"],"expected_generation":1},
        "observation":request["observation"]})).unwrap();
    assert_eq!(
        released["publication"]["value"]["accepted_scope"]["records"],
        json!([])
    );
}

#[test]
fn claim_requires_signed_owner_destination_and_unchanged_journal_observation() {
    let fixture = AuthorityFixture::new();
    let (context, request) = prepared_claim(&fixture);
    let other = context
        .clone()
        .enter_plugin("builtin.semantic", "select_revisions");
    assert!(citation_call(&fixture, &other, request.clone()).is_err());
    let mut forged = request.clone();
    forged["claim"]["intent"]["space_id"] = json!("another-space");
    assert!(citation_call(&fixture, &context, forged).is_err());
    save_observation(
        fixture.persistence.as_ref(),
        json!({"opaque_owner_state":"changed"}),
    );
    assert!(citation_call(&fixture, &context, request).is_err());
}

struct JournalChangesDuringAcquisition {
    store: Arc<LocalPersistence>,
    accepted: std::sync::atomic::AtomicBool,
}

impl SemanticPersistence for JournalChangesDuringAcquisition {
    fn readiness(&self) -> lumvise_db_core::PersistenceResult<lumvise_db_core::SemanticReadiness> {
        SemanticPersistence::readiness(self.store.as_ref())
    }

    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::PersistenceResult<SemanticResult> {
        let acquisition = matches!(operation, SemanticOperation::AcquireCitationRetention(_));
        let result = SemanticPersistence::execute(self.store.as_ref(), operation, control)?;
        if acquisition {
            self.accepted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            save_observation(
                self.store.as_ref(),
                json!({"opaque_owner_state":"changed during I/O"}),
            );
        }
        Ok(result)
    }
}

#[test]
fn journal_change_during_source_acceptance_prevents_acknowledging_protected_evidence() {
    let fixture = AuthorityFixture::new();
    let (context, request) = prepared_claim(&fixture);
    let source = JournalChangesDuringAcquisition {
        store: fixture.persistence.clone(),
        accepted: std::sync::atomic::AtomicBool::new(false),
    };
    assert!(
        fixture
            .authority
            .invoke(
                OWNER,
                json!({"operation":"citation","request":request.clone()}),
                &source,
                &context
            )
            .is_err()
    );
    assert!(source.accepted.load(std::sync::atomic::Ordering::SeqCst));
    let claim = serde_json::from_value(request["claim"].clone()).unwrap();
    let replay = SemanticPersistence::execute(
        fixture.persistence.as_ref(),
        SemanticOperation::AcquireCitationRetention(claim),
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert!(matches!(
        replay,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    ));
}
