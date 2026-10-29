//! Canonical project-qualified document references. Knowledge resolves targets;
//! shared revision policy alone selects families and retains exact evidence.
mod schema;
use super::{KnowledgeArtifactV2, KnowledgeDocument, KnowledgeProjectIdentity, SemanticElementV2};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A target in its authoritative project, independent of the citing document's owner.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeQualifiedCitation {
    pub project: KnowledgeProjectIdentity,
    pub target: KnowledgeCitationTarget,
}

/// Reads a target through Knowledge and discovers its authoritative project identity.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCitationReadRequest {
    pub project_root: String,
    pub target: KnowledgeCitationTarget,
}

/// An authorized read and its exact citation; reading alone does not retain evidence.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCitationRead {
    pub reference: KnowledgeQualifiedCitation,
    pub state: KnowledgeCitationReadState,
    pub body: Option<KnowledgeCitationBody>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeCitationReadState {
    Available,
    Missing,
    Pruned,
}

/// Uses the existing public records instead of exposing Knowledge's storage representation.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(tag = "target_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KnowledgeCitationBody {
    SemanticElement { element: SemanticElementV2 },
    Artifact { artifact: KnowledgeArtifactV2 },
    Document { document: KnowledgeDocument },
}

/// Preserves target categories while making selection behavior explicit.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "target_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KnowledgeCitationTarget {
    SemanticElement {
        semantic_element_id: String,
        selection: KnowledgeRevisionCitationSelection,
    },
    Artifact {
        artifact_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit_version: Option<u64>,
        selection: KnowledgeRevisionCitationSelection,
    },
    Document {
        document_id: String,
        selection: KnowledgeDocumentCitationSelection,
    },
}

impl KnowledgeCitationTarget {
    /// Reads the declared category and ID without resolving a family or granting access.
    /// Example: `citation.target.identity()` supplies a validation label, never a body lookup.
    pub fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::SemanticElement {
                semantic_element_id,
                ..
            } => ("semantic_element", semantic_element_id),
            Self::Artifact { artifact_id, .. } => ("artifact", artifact_id),
            Self::Document { document_id, .. } => ("document", document_id),
        }
    }
}

/// Exact evidence never substitutes a current family member.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum KnowledgeRevisionCitationSelection {
    Selected {
        selection_id: String,
        source_context: String,
    },
    Exact {
        source_context: String,
    },
}

/// A document's logical identity and exact accepted locator have distinct meanings.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum KnowledgeDocumentCitationSelection {
    Current,
    Exact {
        semantic_element_id: String,
        source_context: String,
    },
}

impl<'de> Deserialize<'de> for KnowledgeDocumentCitationSelection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde's tagged unit variant ignores extra fields; an empty struct enforces the contract.
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum ClosedSelection {
            Current {},
            Exact {
                semantic_element_id: String,
                source_context: String,
            },
        }
        Ok(match ClosedSelection::deserialize(deserializer)? {
            ClosedSelection::Current {} => Self::Current,
            ClosedSelection::Exact {
                semantic_element_id,
                source_context,
            } => Self::Exact {
                semantic_element_id,
                source_context,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::KnowledgeDocumentReference;
    use serde_json::json;

    #[test]
    fn shared_manifest_shapes_preserve_closed_qualified_and_selection_variants() {
        let qualified = super::KnowledgeQualifiedCitation::reference_schema();
        assert_eq!(qualified["additionalProperties"], false);
        assert_eq!(
            qualified["properties"]["project"],
            super::KnowledgeProjectIdentity::input_schema()
        );
        let targets = super::KnowledgeCitationTarget::input_schema();
        assert_eq!(qualified["properties"]["target"], targets);
        let variants = targets["oneOf"].as_array().unwrap();
        assert_eq!(variants.len(), 3);
        for variant in variants {
            assert_eq!(variant["additionalProperties"], false);
            assert!(
                variant["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("selection"))
            );
        }
    }

    #[test]
    fn qualified_reference_preserves_category_project_and_exact_original_token() {
        let value = json!({"target_kind":"qualified","project":{"project_root":"/source","project_id":"lineage",
            "storage_authority":"store"},"target":{"target_kind":"artifact","artifact_id":"a1","edit_version":7,
            "selection":{"mode":"exact","source_context":"git:main"}}});
        let reference: KnowledgeDocumentReference = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reference).unwrap(), value);
        let mut ambiguous = value;
        ambiguous["target"]["selection"]["selection_id"] = json!("working");
        assert!(serde_json::from_value::<KnowledgeDocumentReference>(ambiguous).is_err());
    }

    #[test]
    fn exact_document_requires_its_accepted_locator_and_current_has_no_locator() {
        let exact = json!({"mode":"exact","source_context":"local"});
        assert!(
            serde_json::from_value::<super::KnowledgeDocumentCitationSelection>(exact).is_err()
        );
        let current = json!({"mode":"current","semantic_element_id":"old"});
        assert!(
            serde_json::from_value::<super::KnowledgeDocumentCitationSelection>(current).is_err()
        );
    }

    #[test]
    fn target_identity_is_available_without_interpreting_its_selection() {
        for value in [
            json!({"target_kind":"semantic_element","semantic_element_id":"id","selection":{"mode":"exact","source_context":"local"}}),
            json!({"target_kind":"artifact","artifact_id":"id","selection":{"mode":"selected","selection_id":"working","source_context":"local"}}),
            json!({"target_kind":"document","document_id":"id","selection":{"mode":"current"}}),
        ] {
            let target: super::KnowledgeCitationTarget =
                serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                target.identity(),
                (value["target_kind"].as_str().unwrap(), "id")
            );
        }
    }
}
