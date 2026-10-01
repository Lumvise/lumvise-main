//! Catalog of Host Capabilities implemented by App Core.
//!
//! This module owns each stable identity, its actual implementation version,
//! and its first dispatch boundary. Capability-specific behavior remains in the
//! owning adapter modules; callers should use the lookup functions here.

use std::collections::BTreeMap;

use semver::Version;

pub(super) const PLUGIN_STORAGE_CAPABILITY: &str = "storage.plugin";
pub(super) const SEMANTIC_STORAGE: &str = "storage.semantic";
pub(super) const SEMANTIC_SNAPSHOT_CAPABILITY: &str = "semantic.snapshot";
pub(super) const NEURAL_EMBED_CAPABILITY: &str = "neural.embed";
pub(super) const PLUGIN_INVOKE_CAPABILITY: &str = "plugin.invoke";
pub(super) const FRONTEND_ACTION: &str = "frontend.action";
pub(super) const FRONTEND_CANVAS: &str = "frontend.canvas";
pub(super) const NEURAL_LLM: &str = "neural.llm";
pub(super) const SPEECH_TO_TEXT: &str = "modalities.speech_to_text";
pub(super) const TEXT_TO_SPEECH: &str = "modalities.text_to_speech";
pub(super) const BACKGROUND_JOB: &str = "runtime.background_job";
pub(super) const TURN_WAIT: &str = "runtime.turn_wait";
pub(super) const PROJECT_EXECUTION: &str = "runtime.project_execution";
pub(super) const SCOPED_MCP: &str = "runtime.scoped_mcp";
pub(super) const EXCLUSIVE_LANE: &str = "runtime.exclusive_lane";
pub(super) const ASSISTANT_ENGINE_AVAILABILITY: &str = "runtime.assistant_engine_availability";
/// Reports whether the speech dependencies a voice session needs are installed,
/// so a missing selection is startup-detectable instead of only surfacing
/// mid-turn (issue #92).
pub(super) const SPEECH_AVAILABILITY: &str = "runtime.speech_availability";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HostCapabilityRoute {
    PluginStorage,
    SemanticStorage,
    SemanticSnapshot,
    ProjectSource,
    NeuralEmbed,
    PluginInvokeAuthorization,
    Services,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct HostCapabilityDefinition {
    pub(super) id: &'static str,
    pub(super) route: HostCapabilityRoute,
    major: u64,
    minor: u64,
    patch: u64,
}

impl HostCapabilityDefinition {
    pub(super) fn version(&self) -> Version {
        Version::new(self.major, self.minor, self.patch)
    }
}

const HOST_CAPABILITIES: [HostCapabilityDefinition; 19] = [
    capability(
        PLUGIN_STORAGE_CAPABILITY,
        HostCapabilityRoute::PluginStorage,
    ),
    semantic_capability(),
    capability("project.source", HostCapabilityRoute::ProjectSource),
    capability(
        SEMANTIC_SNAPSHOT_CAPABILITY,
        HostCapabilityRoute::SemanticSnapshot,
    ),
    capability(NEURAL_EMBED_CAPABILITY, HostCapabilityRoute::NeuralEmbed),
    capability(
        PLUGIN_INVOKE_CAPABILITY,
        HostCapabilityRoute::PluginInvokeAuthorization,
    ),
    capability(FRONTEND_ACTION, HostCapabilityRoute::Services),
    capability(FRONTEND_CANVAS, HostCapabilityRoute::Services),
    capability(NEURAL_LLM, HostCapabilityRoute::Services),
    capability(SPEECH_TO_TEXT, HostCapabilityRoute::Services),
    capability(TEXT_TO_SPEECH, HostCapabilityRoute::Services),
    capability(BACKGROUND_JOB, HostCapabilityRoute::Services),
    capability(TURN_WAIT, HostCapabilityRoute::Services),
    capability(PROJECT_EXECUTION, HostCapabilityRoute::Services),
    capability(SCOPED_MCP, HostCapabilityRoute::Services),
    capability(EXCLUSIVE_LANE, HostCapabilityRoute::Services),
    capability(ASSISTANT_ENGINE_AVAILABILITY, HostCapabilityRoute::Services),
    capability(SPEECH_AVAILABILITY, HostCapabilityRoute::Services),
    capability("runtime.audio_session", HostCapabilityRoute::Services),
];

const fn capability(id: &'static str, route: HostCapabilityRoute) -> HostCapabilityDefinition {
    HostCapabilityDefinition {
        id,
        route,
        major: 1,
        minor: 0,
        patch: 0,
    }
}

const fn semantic_capability() -> HostCapabilityDefinition {
    HostCapabilityDefinition {
        id: SEMANTIC_STORAGE,
        route: HostCapabilityRoute::SemanticStorage,
        major: 1,
        minor: 3,
        patch: 0,
    }
}

pub(super) fn host_capability_definition(id: &str) -> Option<&'static HostCapabilityDefinition> {
    HOST_CAPABILITIES
        .iter()
        .find(|definition| definition.id == id)
}

#[cfg(test)]
pub(super) fn host_capability_definitions() -> &'static [HostCapabilityDefinition] {
    &HOST_CAPABILITIES
}

/// Returns the actual versions implemented by App Core.
///
/// # Example
/// ```
/// let versions = lumvise_app_core::compiled_host_capability_versions();
/// assert_eq!(versions["neural.llm"], semver::Version::new(1, 0, 0));
/// ```
pub fn compiled_host_capability_versions() -> BTreeMap<&'static str, Version> {
    HOST_CAPABILITIES
        .iter()
        .map(|definition| (definition.id, definition.version()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn catalog_contains_each_host_capability_once() {
        let identities = HOST_CAPABILITIES
            .iter()
            .map(|definition| definition.id)
            .collect::<BTreeSet<_>>();

        assert_eq!(identities.len(), 19);
        assert_eq!(identities, expected_identities());
    }

    #[test]
    fn catalog_publishes_actual_versions() {
        let versions = compiled_host_capability_versions();

        assert_eq!(versions.len(), 19);
        assert_eq!(versions[SEMANTIC_STORAGE], Version::new(1, 3, 0));
        assert!(
            versions
                .iter()
                .filter(|(id, _)| **id != SEMANTIC_STORAGE)
                .all(|(_, version)| version == &Version::new(1, 0, 0))
        );
    }

    #[test]
    fn lookup_returns_the_owned_dispatch_route() {
        for (id, route) in expected_routes() {
            assert_route(id, route);
        }
        assert!(host_capability_definition("missing.capability").is_none());
    }

    fn assert_route(id: &str, expected: HostCapabilityRoute) {
        let definition = host_capability_definition(id).expect("catalog capability");

        assert_eq!(definition.route, expected);
        let expected_version = if id == SEMANTIC_STORAGE {
            Version::new(1, 3, 0)
        } else {
            Version::new(1, 0, 0)
        };
        assert_eq!(definition.version(), expected_version);
    }

    fn expected_identities() -> BTreeSet<&'static str> {
        expected_routes().into_iter().map(|(id, _)| id).collect()
    }

    fn expected_routes() -> [(&'static str, HostCapabilityRoute); 19] {
        [
            (
                PLUGIN_STORAGE_CAPABILITY,
                HostCapabilityRoute::PluginStorage,
            ),
            (SEMANTIC_STORAGE, HostCapabilityRoute::SemanticStorage),
            ("project.source", HostCapabilityRoute::ProjectSource),
            (
                SEMANTIC_SNAPSHOT_CAPABILITY,
                HostCapabilityRoute::SemanticSnapshot,
            ),
            (NEURAL_EMBED_CAPABILITY, HostCapabilityRoute::NeuralEmbed),
            (
                PLUGIN_INVOKE_CAPABILITY,
                HostCapabilityRoute::PluginInvokeAuthorization,
            ),
            (FRONTEND_ACTION, HostCapabilityRoute::Services),
            (FRONTEND_CANVAS, HostCapabilityRoute::Services),
            (NEURAL_LLM, HostCapabilityRoute::Services),
            (SPEECH_TO_TEXT, HostCapabilityRoute::Services),
            (TEXT_TO_SPEECH, HostCapabilityRoute::Services),
            (BACKGROUND_JOB, HostCapabilityRoute::Services),
            (TURN_WAIT, HostCapabilityRoute::Services),
            (PROJECT_EXECUTION, HostCapabilityRoute::Services),
            (SCOPED_MCP, HostCapabilityRoute::Services),
            (EXCLUSIVE_LANE, HostCapabilityRoute::Services),
            (ASSISTANT_ENGINE_AVAILABILITY, HostCapabilityRoute::Services),
            (SPEECH_AVAILABILITY, HostCapabilityRoute::Services),
            ("runtime.audio_session", HostCapabilityRoute::Services),
        ]
    }
}
