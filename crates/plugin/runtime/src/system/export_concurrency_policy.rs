//! Per-export concurrency policies for controlled parallelism.

use std::collections::HashMap;

/// Defines how many concurrent invocations an export can handle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExportConcurrencyPolicy {
    /// Only one invocation can execute at a time (default, maintains backward compat).
    #[default]
    Serial,
    /// Up to N concurrent invocations can execute in parallel.
    Parallel(u32),
}

impl ExportConcurrencyPolicy {
    /// Returns the maximum concurrent slots for this policy.
    pub(crate) fn max_concurrent(&self) -> u32 {
        match self {
            Self::Serial => 1,
            Self::Parallel(n) => *n,
        }
    }
}

/// Registry of export-level concurrency policies.
#[derive(Clone, Debug, Default)]
pub struct ExportConcurrencyRegistry {
    policies: HashMap<String, ExportConcurrencyPolicy>,
}

impl ExportConcurrencyRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Sets the concurrency policy for an export (export_id only).
    pub fn set_policy(&mut self, export_key: String, policy: ExportConcurrencyPolicy) {
        self.policies.insert(export_key, policy);
    }

    /// Gets the concurrency policy for an export, defaulting to Serial.
    pub fn get_policy(&self, export_key: &str) -> ExportConcurrencyPolicy {
        self.policies.get(export_key).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_serial() {
        let policy = ExportConcurrencyPolicy::default();
        assert_eq!(policy.max_concurrent(), 1);
    }

    #[test]
    fn parallel_policy_respects_concurrency_limit() {
        let policy = ExportConcurrencyPolicy::Parallel(4);
        assert_eq!(policy.max_concurrent(), 4);
    }

    #[test]
    fn registry_returns_set_policy() {
        let mut registry = ExportConcurrencyRegistry::new();
        registry.set_policy(
            "plugin::export".to_string(),
            ExportConcurrencyPolicy::Parallel(2),
        );
        assert_eq!(
            registry.get_policy("plugin::export"),
            ExportConcurrencyPolicy::Parallel(2)
        );
    }

    #[test]
    fn registry_defaults_to_serial() {
        let registry = ExportConcurrencyRegistry::new();
        assert_eq!(
            registry.get_policy("unknown::export"),
            ExportConcurrencyPolicy::Serial
        );
    }
}
