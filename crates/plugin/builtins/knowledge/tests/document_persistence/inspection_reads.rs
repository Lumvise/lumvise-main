//! Listing a memory space costs the host a fixed number of reads, however many documents it holds.
use super::*;
use lumvise_db_core::{PersistenceResult, SemanticReadiness};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Counts what the host asks the semantic store, by operation name.
struct CountingSemantic(Arc<LocalPersistence>, Arc<Mutex<BTreeMap<String, usize>>>);

impl SemanticPersistence for CountingSemantic {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult> {
        let debug = format!("{operation:?}");
        let name = debug
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or_default();
        *self.1.lock().unwrap().entry(name.to_owned()).or_default() += 1;
        SemanticPersistence::execute(self.0.as_ref(), operation, control)
    }

    fn readiness(&self) -> PersistenceResult<SemanticReadiness> {
        SemanticPersistence::readiness(self.0.as_ref())
    }
}

fn inspect(host: &mut DocumentBoundaryHost, export: &str, input: Value) -> Value {
    host.knowledge(
        "inspect_document_space",
        json!({"selection":{"space_kind":"assistant"},"export_id":export,"input":input}),
    )
    .unwrap()["output"]
        .clone()
}

#[test]
fn listing_a_memory_space_reads_each_body_owner_once_however_many_documents_it_holds() {
    let reads = Arc::new(Mutex::new(BTreeMap::new()));
    let mut host = DocumentBoundaryHost::new();
    let semantic = Arc::new(CountingSemantic(host.persistence.clone(), reads.clone()));
    host.broker = Arc::new(
        AppCoreHostCapabilityBroker::new(semantic, host.persistence.clone())
            .with_index_client("client-a".into())
            .unwrap(),
    );
    host.invocation_context = host.invocation_context.clone().with_human_interaction();
    let root = host
        .knowledge(
            "inspect_document_space",
            json!({"selection":{"space_kind":"assistant"},"export_id":"open","input":{}}),
        )
        .unwrap()["space"]["project_root"]
        .clone();
    let mut add = |host: &mut DocumentBoundaryHost, from: usize, to: usize| {
        for index in from..to {
            inspect(
                host,
                "create_document",
                json!({"project_root":root, "document_id":format!("context:{index}"),
                    "document_kind":"context", "title":format!("Memory {index}"), "content":"Remembered"}),
            );
        }
    };
    let mut list_reads = |host: &mut DocumentBoundaryHost| {
        reads.lock().unwrap().clear();
        let listed = inspect(host, "list_documents", json!({"project_root":root}));
        let owners = reads.lock().unwrap().get("Element").copied().unwrap_or(0);
        (listed["documents"].as_array().unwrap().len(), owners)
    };

    add(&mut host, 0, 3);
    let (few_documents, few_reads) = list_reads(&mut host);
    add(&mut host, 3, 12);
    let (many_documents, many_reads) = list_reads(&mut host);

    assert_eq!((few_documents, many_documents), (3, 12));
    assert_eq!(many_reads, few_reads);
}
