use crate::{AppCoreError, Result};
use lumvise_db_core::{RelationalOperation, RelationalPersistence, RelationalResult};
use lumvise_resource_routing::InvocationControl;
#[cfg(any(feature = "desktop-app", test))]
use serde_json::json;

const PROVIDER_API_KEYS_SCOPE: &str = "llm_provider_api_keys";
const PROVIDER_ENDPOINTS_SCOPE: &str = "llm_provider_endpoints";

/// Stores a local API key for a supported desktop LLM provider.
///
/// # Example
///
/// ```ignore
/// set_provider_api_key(&db, "cerebras", "csk-...")?;
/// ```
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn set_provider_api_key(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
    api_key: &str,
) -> Result<()> {
    let provider_id = supported_provider_id(provider_id)?;
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(AppCoreError::invalid_value(
            provider_id,
            "non-empty provider API key",
        ));
    }
    match relational.execute(
        RelationalOperation::SetPersistentSetting {
            scope: PROVIDER_API_KEYS_SCOPE.to_string(),
            key: provider_id.to_string(),
            value: json!(api_key),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::PersistentSettingUpdated(_) => Ok(()),
        other => Err(AppCoreError::unsupported(
            "set provider API key",
            format!("persistent setting update, got {other:?}"),
        )),
    }
}

/// Clears a local API key by replacing its persisted value with JSON null.
///
/// The persistent-settings contract has no delete operation; null is the
/// durable absence marker and `stored_provider_api_key` treats it as
/// unconfigured. The secret value is never returned to callers.
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn clear_provider_api_key(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
) -> Result<()> {
    let provider_id = supported_provider_id(provider_id)?;
    match relational.execute(
        RelationalOperation::SetPersistentSetting {
            scope: PROVIDER_API_KEYS_SCOPE.to_string(),
            key: provider_id.to_string(),
            value: json!(null),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::PersistentSettingUpdated(_) => Ok(()),
        other => Err(AppCoreError::unsupported(
            "clear provider API key",
            format!("persistent setting update, got {other:?}"),
        )),
    }
}

/// Reads a local API key for a supported desktop LLM provider.
pub(crate) fn stored_provider_api_key(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
) -> Result<Option<String>> {
    let provider_id = supported_provider_id(provider_id)?;
    let result = relational.execute(
        RelationalOperation::GetPersistentSetting {
            scope: PROVIDER_API_KEYS_SCOPE.to_string(),
            key: provider_id.to_string(),
        },
        &InvocationControl::sixty_seconds(),
    )?;
    let RelationalResult::PersistentSetting(record) = result else {
        return Err(AppCoreError::unsupported(
            "get provider API key",
            "persistent setting result",
        ));
    };
    Ok(record.and_then(|record| {
        record
            .value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }))
}

/// Stores the base URL of a user-defined OpenAI-compatible endpoint.
///
/// Only `custom_openai` accepts an endpoint; the value is validated as an
/// http(s) URL without embedded credentials before it is persisted.
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn set_provider_endpoint(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
    endpoint: &str,
) -> Result<String> {
    let provider_id = supported_provider_id(provider_id)?;
    if provider_id != "custom_openai" {
        return Err(AppCoreError::invalid_value(
            provider_id,
            "provider id custom_openai; only the custom OpenAI-compatible provider accepts an endpoint",
        ));
    }
    let endpoint = validated_provider_endpoint(endpoint)?;
    match relational.execute(
        RelationalOperation::SetPersistentSetting {
            scope: PROVIDER_ENDPOINTS_SCOPE.to_string(),
            key: provider_id.to_string(),
            value: json!(endpoint),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::PersistentSettingUpdated(_) => Ok(endpoint),
        other => Err(AppCoreError::unsupported(
            "set provider endpoint",
            format!("persistent setting update, got {other:?}"),
        )),
    }
}

/// Clears a stored endpoint by replacing its persisted value with JSON null.
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn clear_provider_endpoint(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
) -> Result<()> {
    let provider_id = supported_provider_id(provider_id)?;
    if provider_id != "custom_openai" {
        return Err(AppCoreError::invalid_value(
            provider_id,
            "endpoint configuration is only supported for custom_openai",
        ));
    }
    match relational.execute(
        RelationalOperation::SetPersistentSetting {
            scope: PROVIDER_ENDPOINTS_SCOPE.to_string(),
            key: provider_id.to_string(),
            value: json!(null),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        RelationalResult::PersistentSettingUpdated(_) => Ok(()),
        other => Err(AppCoreError::unsupported(
            "clear provider endpoint",
            format!("persistent setting update, got {other:?}"),
        )),
    }
}

/// Reads the stored endpoint for a supported desktop LLM provider.
pub(crate) fn stored_provider_endpoint(
    relational: &dyn RelationalPersistence,
    provider_id: &str,
) -> Result<Option<String>> {
    let provider_id = supported_provider_id(provider_id)?;
    let result = relational.execute(
        RelationalOperation::GetPersistentSetting {
            scope: PROVIDER_ENDPOINTS_SCOPE.to_string(),
            key: provider_id.to_string(),
        },
        &InvocationControl::sixty_seconds(),
    )?;
    let RelationalResult::PersistentSetting(record) = result else {
        return Err(AppCoreError::unsupported(
            "get provider endpoint",
            "persistent setting result",
        ));
    };
    Ok(record.and_then(|record| {
        record
            .value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }))
}

/// Validates and trims a user-supplied endpoint URL. Accepts only absolute
/// http/https URLs whose authority carries no embedded credentials.
#[cfg(any(feature = "desktop-app", test))]
fn validated_provider_endpoint(endpoint: &str) -> Result<String> {
    let trimmed = endpoint.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(AppCoreError::invalid_value(
            trimmed,
            "absolute endpoint URL starting with http:// or https://",
        ));
    };
    if scheme != "http" && scheme != "https" {
        return Err(AppCoreError::invalid_value(
            scheme,
            "endpoint scheme http or https",
        ));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(AppCoreError::invalid_value(
            trimmed,
            "endpoint URL with a non-empty host",
        ));
    }
    if authority.contains('@') {
        return Err(AppCoreError::invalid_value(
            trimmed,
            "endpoint URL without embedded user credentials",
        ));
    }
    Ok(trimmed.to_string())
}

/// Lists configured/not-configured status for every LLM Provider that
/// accepts a local API key. Never returns the stored secret value.
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn provider_credential_status(
    relational: &dyn RelationalPersistence,
) -> Result<serde_json::Value> {
    const PROVIDERS: [(&str, &str); 6] = [
        ("cerebras", "Cerebras"),
        ("openrouter", "OpenRouter"),
        ("z_ai", "Z.AI GLM"),
        ("custom_openai", "Custom (OpenAI-compatible)"),
        ("gemini", "Gemini Live"),
        ("openai_realtime", "OpenAI Realtime"),
    ];
    let mut statuses = Vec::with_capacity(PROVIDERS.len());
    for (provider_id, label) in PROVIDERS {
        let configured = stored_provider_api_key(relational, provider_id)?.is_some();
        let mut status = serde_json::Map::new();
        status.insert("providerId".into(), json!(provider_id));
        status.insert("label".into(), json!(label));
        status.insert("configured".into(), json!(configured));
        // Only the custom OpenAI-compatible provider stores an endpoint URL;
        // the key is optional there, so the renderer needs endpoint presence.
        if provider_id == "custom_openai" {
            let endpoint_configured = stored_provider_endpoint(relational, provider_id)?.is_some();
            status.insert("endpointConfigured".into(), json!(endpoint_configured));
        }
        statuses.push(serde_json::Value::Object(status));
    }
    Ok(json!(statuses))
}

fn supported_provider_id(provider_id: &str) -> Result<&'static str> {
    match provider_id.trim() {
        "cerebras" => Ok("cerebras"),
        "openrouter" => Ok("openrouter"),
        "z_ai" => Ok("z_ai"),
        "custom_openai" => Ok("custom_openai"),
        "gemini" => Ok("gemini"),
        "openai_realtime" => Ok("openai_realtime"),
        other => Err(AppCoreError::invalid_value(
            other,
            "provider id cerebras, openrouter, or z_ai; gemini or openai_realtime",
        )),
    }
}

#[cfg(test)]
mod tests {
    use lumvise_db_core::{LocalPersistence, RelationalPersistence};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn provider_api_key_round_trips_through_local_settings() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        set_provider_api_key(relational.as_ref(), "cerebras", " fake-key ").unwrap();

        assert_eq!(
            stored_provider_api_key(relational.as_ref(), "cerebras")
                .unwrap()
                .as_deref(),
            Some("fake-key")
        );
    }

    #[test]
    fn provider_api_key_rejects_unknown_provider() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        let error = set_provider_api_key(relational.as_ref(), "codex", "fake-key").unwrap_err();

        assert!(error.to_string().contains("cerebras, openrouter, or z_ai"));
    }

    #[test]
    fn provider_endpoint_round_trips_through_local_settings() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        let stored = set_provider_endpoint(
            relational.as_ref(),
            "custom_openai",
            " http://localhost:11434/v1 ",
        )
        .unwrap();

        assert_eq!(stored, "http://localhost:11434/v1");
        assert_eq!(
            stored_provider_endpoint(relational.as_ref(), "custom_openai")
                .unwrap()
                .as_deref(),
            Some("http://localhost:11434/v1")
        );
    }

    #[test]
    fn provider_endpoint_clear_replaces_value_with_null() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        set_provider_endpoint(
            relational.as_ref(),
            "custom_openai",
            "https://gateway.internal/v1",
        )
        .unwrap();
        clear_provider_endpoint(relational.as_ref(), "custom_openai").unwrap();

        assert_eq!(
            stored_provider_endpoint(relational.as_ref(), "custom_openai").unwrap(),
            None
        );
    }

    #[test]
    fn provider_endpoint_rejects_invalid_urls() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        for endpoint in [
            "ftp://localhost:11434/v1",
            "localhost:11434/v1",
            "http://",
            "https://user:pass@host/v1",
            "   ",
        ] {
            let error =
                set_provider_endpoint(relational.as_ref(), "custom_openai", endpoint).unwrap_err();
            assert!(
                !error.to_string().is_empty(),
                "endpoint {endpoint:?} must be rejected"
            );
        }
    }

    #[test]
    fn provider_endpoint_rejects_providers_other_than_custom_openai() {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let relational: Arc<dyn RelationalPersistence> = persistence;

        let error = set_provider_endpoint(
            relational.as_ref(),
            "cerebras",
            "https://api.cerebras.ai/v1",
        )
        .unwrap_err();

        assert!(error.to_string().contains("custom_openai"));
        assert_eq!(
            stored_provider_endpoint(relational.as_ref(), "cerebras").unwrap(),
            None
        );
    }
}
