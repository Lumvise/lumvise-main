//! Owns readable agent workflow guidance in Knowledge's signed descriptions.
//! Manifest builders use this table; artifact storage/transfer remain their owners' internals.

const CREATE_GUIDANCE: &str = "Search with search_knowledge/get_knowledge before creating; update an existing record when appropriate. Supply project_root and the narrowest existing semantic_element_id. Use plain titles/content and human-readable Markdown for someone outside this agent conversation: purpose/outcome first, plain language, complete sentences, short paragraphs. Separate headings, lists and paragraphs with blank lines. Explain decisions and why they matter; fence code/log excerpts; place raw IDs in metadata or labeled links. Avoid terse agent shorthand. Kinds: requirements for expected outcomes/acceptance criteria, specification for design/constraints, media for image/audio/video notes. Reports, reference notes, plans, handoffs and questions use create_document (Context). Agents owns executable missions, status, owner, blockers and evidence. Optional expires_at (RFC 3339) ends active discovery. New records start active at revision 1. Retain returned artifact_id as its exact accepted revision. Reuse operation_id with identical retry input; choose a new ID for a later action. No atomic lease/claim guarantee.";
const UPDATE_GUIDANCE: &str = "Use search_knowledge/get_knowledge first; preserve human text. Pass project_root, artifact_id and expected_revision from the original read. Reuse operation_id/input after a lost reply; on conflict re-read, merge and use a new ID. Working saves keep artifact_id. Element changes, pins and citations freeze history; retain returned IDs. Cite working dependencies with edit_version from get_knowledge. If replay reports knowledge_artifact_edit_unavailable, use get_artifact_write_receipt. Set status resolved when done, superseded with superseded_by when replaced, or archived when withdrawn. Content, tags, dependencies and metadata replace prior values: merge deliberately. Write human-readable Markdown for someone outside this agent conversation: purpose first, plain language, complete sentences, short paragraphs, headings/lists when useful, blank lines between blocks and fenced code/logs. Explain decisions; keep raw IDs in metadata/labeled links. Avoid terse agent shorthand. Complete only after acceptance criteria are verified; record commands/results and remaining limits. Updates offer no atomic lease/claim guarantee.";
const GET_GUIDANCE: &str = "Read the full artifact by artifact_id after search/list discovery. This ID-only request has no project_root field: verify the returned artifact's project_root and semantic anchor match the current checkout. The record carries status, revision and timestamps. Exact IDs may address retained history even when status is active; choose current revisions through project search/list before editing. An inactive record is not a current instruction. Reconcile requirements and specifications with current source/tests before resuming; saved completion text alone is not verification. Read reports and plans through get_document, and mission state through Agents.";
const SEARCH_GUIDANCE: &str = "Search readable project knowledge with explicit current project_root and optional narrow semantic_element_id/type filters before creating or changing a record. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Results contain IDs/titles/scores/status, not full content: follow with get_knowledge to restore requirements or specifications. Search reports and context through search_documents. Inspect vector/lexical mode; no result is not proof that related knowledge never existed.";
const LIST_GUIDANCE: &str = "Read artifacts attached to one existing semantic_element_id from the current project. This ID-only request has no project_root field: verify each returned artifact's project_root. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Inspect nearby requirements and specifications before editing. Read report/context documents separately.";
const DEPENDENTS_GUIDANCE: &str = "List artifacts with explicit dependency edges to target_kind/target_id (semantic_element or artifact). Verify returned project scope and inspect affected records before a change; an empty list says nothing about undocumented dependencies.";
const FIND_GUIDANCE: &str = "Find semantic owners for a narrow artifact anchor with explicit current project_root. This delegates to compiled Semantic and requires that plugin to be enabled/available; inspect returned IDs rather than inventing an anchor.";
const ELEMENT_GUIDANCE: &str = "Read an existing semantic_element_id for anchor inspection. This ID-only request has no project_root field; use an ID already discovered within the current project. The public element view omits project_root, so retain and verify the originating discovery scope.";

const EXPORT_GUIDANCE: &[(&str, &str)] = &[
    (
        "manifest",
        "Inspect Knowledge's packaged export IDs; invoke only tools present in the enabled tool catalog. Agent workflow: search/read existing project knowledge, store readable anchored Requirements, Specification and Media artifacts, use documents for reports/context, re-read on resume, then record verified evidence. Other plugins are not enabled by this manifest.",
    ),
    ("create_knowledge", CREATE_GUIDANCE),
    ("knowledge.create", CREATE_GUIDANCE),
    ("update_knowledge", UPDATE_GUIDANCE),
    ("knowledge.update", UPDATE_GUIDANCE),
    ("get_knowledge", GET_GUIDANCE),
    ("knowledge.get", GET_GUIDANCE),
    ("search_knowledge", SEARCH_GUIDANCE),
    ("knowledge.search", SEARCH_GUIDANCE),
    ("list_knowledge_for_element", LIST_GUIDANCE),
    ("knowledge.list_for_element", LIST_GUIDANCE),
    ("list_knowledge_dependents", DEPENDENTS_GUIDANCE),
    ("knowledge.list_dependents", DEPENDENTS_GUIDANCE),
    ("find_semantic_elements", FIND_GUIDANCE),
    ("knowledge.find_elements", FIND_GUIDANCE),
    ("get_semantic_element", ELEMENT_GUIDANCE),
    ("knowledge.get_element", ELEMENT_GUIDANCE),
    (
        "list_knowledge",
        "List full artifacts with explicit current project_root and optional semantic_element_ids/type/tags/limit. Only active records are returned by default; include_inactive: true adds resolved, superseded, archived and expired ones. Review nearby requirements and specifications before creating duplicates; a result limit can omit records.",
    ),
    (
        "delete_knowledge",
        "Remove the exact current artifact revision from its selection with explicit project_root and expected_revision from its original read matching its stored scope; retained historical bodies are not destroyed. Keep operation_id and input unchanged when retrying the same removal. Prefer a status change (resolved, superseded or archived) when the history should stay. Read it first and inspect dependents. Preserve human guidance unless removal is intended.",
    ),
    (
        "preview_knowledge_transfer",
        "Read-only preview of eligible artifact copies into explicit destination project_root. Review source_project_root, source_artifact_id, target_semantic_element_id, exact_match/simhash_distance and already_copied before selecting transfer_ids. Content fingerprints indicate similarity, not project identity or permission; indexing does not apply transfers automatically.",
    ),
    (
        "apply_knowledge_transfer",
        "Apply only reviewed transfer_ids from a fresh preview for explicit destination project_root. Revalidates eligibility and destination scope before freezing one atomic selected dependency set; source artifacts remain. Copies preserve provenance, referenced attachments and eligible structured references. Keep operation_id for identical retries: replay returns the original full result before mutable eligibility checks. A fresh review uses a new operation_id; stale competing publications conflict. Completed copies are kept. copied_artifact_ids/already_copied_artifact_ids contain exact accepted artifact IDs, not review choices. Preview similarity is not project identity.",
    ),
    (
        "rebuild_knowledge_vectors",
        "Rebuild search vectors for existing artifacts with explicit project_root. Inspect returned job status/vectors_rebuilt, including unavailable embedding capability; this does not regenerate source knowledge or prove task completion.",
    ),
    (
        "run_cultivation",
        "Run the requested cultivation mode within explicit project_root and optional target_id/target_path. Inspect the returned run.document_ids and canonical Context documents; generated summaries do not replace human acceptance criteria or verified task evidence.",
    ),
    (
        "get_cultivation_run",
        "Read a stored run by run_id and verify its project_root/mode/document IDs. This ID-only request has no project_root field; a recorded run is not evidence that every generated document or implementation task is complete.",
    ),
    (
        "ensure_c4_nucleus",
        "Ensure a scoped C4 report for explicit project_root and a target_id or target_path. refresh/retry_failed may request generation work; inspect returned pending, failed or held state and read generated documents before relying on them. A held report preserves authored content or an intentional deletion; review its reason_code instead of overwriting or creating a parallel body.",
    ),
    (
        "debug_c4_nucleus",
        "Diagnose a scoped C4 nucleus without mutation using explicit project_root and optional target_id/target_path. Inspect functional Context documents and report state before requesting generation; diagnostics are not task completion.",
    ),
    (
        "project_knowledge",
        "Read the canonical project Knowledge projection using projectRoot and changesSince. Preserve revision/scope when consuming it; Requirements, Specification and Media remain artifacts; Report and Context bodies are documents.",
    ),
    (
        "http.knowledge.manifest",
        "List indexed project sourceId values for Knowledge HTTP consumers. Select the exact current project root; display names/fingerprints do not identify a project.",
    ),
    (
        "http.knowledge.setup",
        "Read Obsidian setup metadata for the selected sourceId query scope; this does not create artifacts or enable other plugins.",
    ),
    (
        "http.knowledge.export",
        "Export the selected sourceId project's Knowledge projection through an HTTP envelope. Retain project/revision context for subsequent reads.",
    ),
    (
        "http.knowledge.page",
        "Read one projected element page with sourceId and elementId/element_id query scope; do not confuse document hashes with project identity.",
    ),
    (
        "http.knowledge.sync",
        "Read versioned Obsidian projection changes from a schema-v1 JSON body using sourceId, appliedRevision and contentHashes. Reconcile the returned revision/change batch; it is projection synchronization, not a task ownership claim.",
    ),
    (
        "http.knowledge.c4_nucleus",
        "HTTP adapter for scoped C4 report generation. Supply current project_root and target_id/target_path in the JSON body, then inspect pending/failed generation before trusting the report.",
    ),
    (
        "http.knowledge.c4_action",
        "Read the scoped C4 action descriptor; obtaining its method/path/query does not execute generation.",
    ),
    (
        "http.knowledge.c4_debug",
        "Read scoped C4 diagnostics with project_root and target_id/target_path in the HTTP query without mutation.",
    ),
    (
        "http.knowledge.write",
        "HTTP create adapter: put the same readable title/content, explicit project_root and existing semantic anchor as create_knowledge in the JSON body; search/read existing records first.",
    ),
    ("http.knowledge.artifacts.create", CREATE_GUIDANCE),
    ("http.knowledge.artifacts.update", UPDATE_GUIDANCE),
    (
        "http.knowledge.artifacts.delete",
        "HTTP delete adapter: read the artifact first, then supply artifact_id, expected_revision and matching project_root in the JSON body. Inspect dependents before intended removal.",
    ),
    (
        "http.knowledge.resolve_target",
        "Resolve a JSON-body semantic_element_id to its public anchor view. Retain the current project discovery scope because this view omits project_root.",
    ),
    (
        "http.knowledge.events",
        "Consume Knowledge live-event pages and retain the returned cursor; this feed is not project-filtered, so verify event/artifact scope. Events prompt a fresh artifact read; they are not a task lease or authoritative completion evidence.",
    ),
    (
        "http.knowledge.projection_artifacts",
        "List stored Knowledge artifacts through the HTTP projection-artifact route. Verify each artifact's project_root before treating it as current project guidance.",
    ),
    (
        "trigger.semantic_element_upserted",
        "Runtime-delivered semantic-change trigger invalidates projections and refreshes report intents. It does not automatically apply cross-project transfers; agents should not fabricate deliveries.",
    ),
    (
        "poll_functional_artifact_generation",
        "Recurring runtime consumer stores completed functional Context documents. Inspect pending/completed generation state; it is not an implementation-task scheduler or atomic claim service.",
    ),
];

pub(super) fn description(id: &str) -> &'static str {
    if id == crate::documents::maintenance::EXPORT_ID {
        return "Pure captured-document provenance for Agents maintenance. Inspect body-bound causal paths or prepare metadata for the proposed content. Performs no storage reads or writes and grants no permission; publication still requires the original exact editing base and authority.";
    }
    if id == crate::documents::delegation::EXPORT_ID {
        return "Internal Agents maintenance command. The host binds one enabled permission revision, exact destination and named inputs to the authenticated executor. Content updates require the original editing base and unchanged generated-content ownership; no other documents, source writes or arbitrary exports are granted.";
    }
    if id == crate::documents::inspection::EXPORT_ID {
        return "Native human inspection of one persistent Assistant, Agent or project-shared MCP memory space. Reuses document editing bases and receipts. HTTP/MCP callers cannot acquire this authority.";
    }
    if let Some(description) = crate::publication_journal::description(id) {
        return description;
    }
    if let Some(description) = crate::documents::description(id) {
        return description;
    }
    EXPORT_GUIDANCE
        .iter()
        .find(|(export_id, _)| *export_id == id)
        .map(|(_, guidance)| *guidance)
        .unwrap_or_else(|| {
            panic!("missing Knowledge guidance for `{id}`; expected a signed export ID")
        })
}
