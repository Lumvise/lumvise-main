use crate::config::LlmProviderKind;
use crate::error::{NeuralError, Result};
use crate::llm_providers::adapter::LlmTransportKind;
use crate::llm_providers::discovery::LlmModelDescriptor;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const CATALOG_ENV: &str = "LUMVISE_PROVIDER_MODEL_CATALOG_PATH";
const SEED: &str = include_str!("../../provider-models.toml");
const SCHEMA_VERSION: u32 = 2;

/// The inventory namespace compatible with an adapter transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmModelSource {
    Api,
    Client,
}

impl LlmModelSource {
    pub fn for_transport(transport: LlmTransportKind) -> Self {
        match transport {
            LlmTransportKind::DirectApi | LlmTransportKind::Live => Self::Api,
            LlmTransportKind::Client | LlmTransportKind::LocalProcess => Self::Client,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelInventory {
    pub models: Vec<LlmModelDescriptor>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelSources {
    pub api: Option<ProviderModelInventory>,
    pub client: Option<ProviderModelInventory>,
}

impl ProviderModelSources {
    pub fn for_source(&self, source: LlmModelSource) -> Option<&ProviderModelInventory> {
        match source {
            LlmModelSource::Api => self.api.as_ref(),
            LlmModelSource::Client => self.client.as_ref(),
        }
    }

    fn for_source_mut(&mut self, source: LlmModelSource) -> Option<&mut ProviderModelInventory> {
        match source {
            LlmModelSource::Api => self.api.as_mut(),
            LlmModelSource::Client => self.client.as_mut(),
        }
    }
}
/// The models and default exposed by the catalog's conventional source for a
/// provider. Prefer [`ProviderModelCatalog::resolve`] when the adapter
/// transport is known, because a provider may expose different API and client
/// inventories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCandidates {
    pub models: Vec<LlmModelDescriptor>,
    pub default_model: Option<String>,
}

impl ProviderModelCandidates {
    fn from_inventory(inventory: &ProviderModelInventory) -> Self {
        Self {
            models: inventory.models.clone(),
            default_model: inventory.default_model.clone(),
        }
    }
}

/// Catalog resolution is the single source of merge and selection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProviderModels {
    pub sources: ProviderModelSources,
    pub active_source: LlmModelSource,
    pub models: Vec<LlmModelDescriptor>,
    pub default_model: Option<String>,
    pub selected_model: String,
}

/// The operator-editable provider model catalog. The first process to require a
/// missing catalog seeds it from the repository-tracked TOML; later processes
/// only read the operator-owned runtime file.
#[derive(Debug, Clone)]
pub struct ProviderModelCatalog {
    path: PathBuf,
    providers: CatalogProviders,
}

impl ProviderModelCatalog {
    pub fn configured() -> Result<Self> {
        Self::new(runtime_catalog_path())
    }

    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        ensure_seed(&path)?;
        let source = fs::read_to_string(&path).map_err(|source| NeuralError::Io {
            value: path.display().to_string(),
            expected: "readable provider model catalog".to_string(),
            source,
        })?;
        let value = toml::from_str::<toml::Value>(&source)
            .map_err(|error| catalog_error(&path, format!("valid TOML: {error}")))?;
        let schema_version = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| catalog_error(&path, "integer schema_version".to_string()))?
            as u32;
        let file = match schema_version {
            SCHEMA_VERSION => toml::from_str::<CatalogFile>(&source)
                .map_err(|error| catalog_error(&path, format!("schema v2 TOML: {error}")))?,
            1 => migrate_v1(
                &path,
                toml::from_str::<LegacyCatalogFile>(&source)
                    .map_err(|error| catalog_error(&path, format!("schema v1 TOML: {error}")))?,
            )?,
            other => {
                return Err(catalog_error(
                    &path,
                    format!("unsupported schema_version {other}"),
                ));
            }
        };
        validate_catalog(&path, file)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sources(&self, kind: LlmProviderKind) -> &ProviderModelSources {
        self.providers.for_kind(kind)
    }
    /// Returns the catalog models for this provider's conventional transport.
    ///
    /// This compatibility accessor follows the provider's normal transport
    /// family. Call [`Self::resolve`] with the negotiated source when a
    /// provider supports both API and client transports.
    pub fn candidates(&self, kind: LlmProviderKind) -> ProviderModelCandidates {
        let source = conventional_source(kind);
        let inventory = self
            .sources(kind)
            .for_source(source)
            .expect("validated catalog must define the provider's conventional source");
        ProviderModelCandidates::from_inventory(inventory)
    }

    /// Merges only the selected source and selects a source-local model.
    pub fn resolve(
        &self,
        kind: LlmProviderKind,
        source: LlmModelSource,
        discovered: Option<Vec<LlmModelDescriptor>>,
        configured_model: &str,
    ) -> Result<ResolvedProviderModels> {
        let mut sources = self.sources(kind).clone();
        let inventory =
            sources
                .for_source_mut(source)
                .ok_or_else(|| NeuralError::InvalidValue {
                    value: format!("{kind:?}:{source:?}"),
                    expected: "catalog model source compatible with selected transport".to_string(),
                })?;
        if let Some(discovered) = discovered {
            inventory.models = merged_models(&inventory.models, discovered);
        }
        let selected_model =
            select_model(inventory, configured_model).ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{kind:?}:{source:?}"),
                expected: "a configured, default, or discovered model in the active source"
                    .to_string(),
            })?;
        Ok(ResolvedProviderModels {
            models: inventory.models.clone(),
            default_model: inventory.default_model.clone(),
            sources,
            active_source: source,
            selected_model,
        })
    }
}

fn conventional_source(kind: LlmProviderKind) -> LlmModelSource {
    match kind {
        LlmProviderKind::Cerebras
        | LlmProviderKind::OpenAiCompatible
        | LlmProviderKind::OpenAiRealtime
        | LlmProviderKind::OpenRouter
        | LlmProviderKind::Zai => LlmModelSource::Api,
        LlmProviderKind::Claude
        | LlmProviderKind::Codex
        | LlmProviderKind::Gemini
        | LlmProviderKind::Local => LlmModelSource::Client,
    }
}

fn select_model(inventory: &ProviderModelInventory, configured_model: &str) -> Option<String> {
    let configured_model = configured_model.trim();
    inventory
        .models
        .iter()
        .find(|model| configured_model != "provider-default" && model.id == configured_model)
        .or_else(|| {
            inventory
                .default_model
                .as_deref()
                .and_then(|default| inventory.models.iter().find(|model| model.id == default))
        })
        .or_else(|| inventory.models.first())
        .map(|model| model.id.clone())
}

fn merged_models(
    declared: &[LlmModelDescriptor],
    discovered: Vec<LlmModelDescriptor>,
) -> Vec<LlmModelDescriptor> {
    let declared_ids: BTreeSet<_> = declared.iter().map(|model| model.id.as_str()).collect();
    let mut merged = declared.to_vec();
    let mut live_only = BTreeSet::new();
    for model in discovered {
        if !model.id.trim().is_empty() && !declared_ids.contains(model.id.as_str()) {
            live_only.insert(model.id);
        }
    }
    merged.extend(live_only.into_iter().map(|id| LlmModelDescriptor {
        display_name: id.clone(),
        id,
    }));
    merged
}

fn runtime_catalog_path() -> PathBuf {
    if let Some(path) = env::var_os(CATALOG_ENV).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".lumvise/config/provider-models.toml"))
        .unwrap_or_else(|| PathBuf::from(".lumvise/config/provider-models.toml"))
}

fn ensure_seed(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| NeuralError::Io {
            value: parent.display().to_string(),
            expected: "provider model catalog directory".to_string(),
            source,
        })?;
    }
    write_new(path, SEED)
}

fn write_new(path: &Path, contents: &str) -> Result<()> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(source) => {
            return Err(NeuralError::Io {
                value: path.display().to_string(),
                expected: "new provider model catalog".to_string(),
                source,
            });
        }
    };
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| NeuralError::Io {
            value: path.display().to_string(),
            expected: "durably written provider model catalog".to_string(),
            source,
        })
}

fn migrate_v1(path: &Path, legacy: LegacyCatalogFile) -> Result<CatalogFile> {
    let migrated = CatalogFile {
        schema_version: SCHEMA_VERSION,
        providers: RawCatalogProviders {
            cerebras: RawProviderSources::api(legacy.providers.cerebras),
            claude: RawProviderSources::client(legacy.providers.claude),
            codex: RawProviderSources::client(legacy.providers.codex),
            gemini: RawProviderSources::client(legacy.providers.gemini),
            openai_realtime: RawProviderSources::api(legacy.providers.openai_realtime),
            openrouter: RawProviderSources::api(legacy.providers.openrouter),
            z_ai: RawProviderSources::api(legacy.providers.z_ai),
            custom_openai: default_custom_openai_sources(),
            local: RawProviderSources::client(legacy.providers.local),
        },
    };
    // Validate before changing the operator-owned file, then atomically replace it.
    let validated = validate_catalog(path, migrated.clone())?;
    let rendered = toml::to_string_pretty(&migrated).map_err(|error| {
        catalog_error(path, format!("serializable schema v2 migration: {error}"))
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".provider-models-{}.tmp", std::process::id()));
    write_new(&temporary, &rendered)?;
    fs::rename(&temporary, path).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "atomically migrated schema v2 provider model catalog".to_string(),
        source,
    })?;
    drop(validated);
    Ok(migrated)
}

fn catalog_error(path: &Path, detail: String) -> NeuralError {
    NeuralError::InvalidValue {
        value: path.display().to_string(),
        expected: format!("provider model catalog with schema_version {SCHEMA_VERSION}: {detail}"),
    }
}

fn validate_catalog(path: &Path, file: CatalogFile) -> Result<ProviderModelCatalog> {
    if file.schema_version != SCHEMA_VERSION {
        return Err(catalog_error(
            path,
            format!("unsupported schema_version {}", file.schema_version),
        ));
    }
    Ok(ProviderModelCatalog {
        path: path.to_path_buf(),
        providers: CatalogProviders {
            cerebras: validate_provider(path, "cerebras", file.providers.cerebras)?,
            claude: validate_provider(path, "claude", file.providers.claude)?,
            codex: validate_provider(path, "codex", file.providers.codex)?,
            gemini: validate_provider(path, "gemini", file.providers.gemini)?,
            openai_realtime: validate_provider(
                path,
                "openai_realtime",
                file.providers.openai_realtime,
            )?,
            openrouter: validate_provider(path, "openrouter", file.providers.openrouter)?,
            z_ai: validate_provider(path, "z_ai", file.providers.z_ai)?,
            custom_openai: validate_provider(path, "custom_openai", file.providers.custom_openai)?,
            local: validate_provider(path, "local", file.providers.local)?,
        },
    })
}

fn validate_provider(
    path: &Path,
    provider_id: &str,
    raw: RawProviderSources,
) -> Result<ProviderModelSources> {
    if raw.api.is_none() && raw.client.is_none() {
        return Err(catalog_error(
            path,
            format!("providers.{provider_id} must declare api and/or client"),
        ));
    }
    Ok(ProviderModelSources {
        api: raw
            .api
            .map(|source| validate_inventory(path, provider_id, "api", source))
            .transpose()?,
        client: raw
            .client
            .map(|source| validate_inventory(path, provider_id, "client", source))
            .transpose()?,
    })
}

fn validate_inventory(
    path: &Path,
    provider_id: &str,
    source: &str,
    raw: RawProvider,
) -> Result<ProviderModelInventory> {
    // `local` declares no static client models; `custom_openai` has no static
    // inventory at all — every model comes from `{endpoint}/models` discovery.
    if raw.models.is_empty() && provider_id != "local" && provider_id != "custom_openai" {
        return Err(catalog_error(
            path,
            format!("providers.{provider_id}.{source}.models must not be empty"),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut models = Vec::with_capacity(raw.models.len());
    for model in raw.models {
        if model.id.trim().is_empty()
            || model.id != model.id.trim()
            || model.id == "provider-default"
        {
            return Err(catalog_error(
                path,
                format!("providers.{provider_id}.{source}.models contains an invalid id"),
            ));
        }
        if model.display_name.trim().is_empty() || model.display_name != model.display_name.trim() {
            return Err(catalog_error(
                path,
                format!(
                    "providers.{provider_id}.{source}.models.{}.display_name must be non-empty and trimmed",
                    model.id
                ),
            ));
        }
        if !ids.insert(model.id.clone()) {
            return Err(catalog_error(
                path,
                format!(
                    "providers.{provider_id}.{source}.models has duplicate id `{}`",
                    model.id
                ),
            ));
        }
        models.push(LlmModelDescriptor {
            id: model.id,
            display_name: model.display_name,
        });
    }
    if let Some(default) = &raw.default {
        if default.trim().is_empty() || default != default.trim() || !ids.contains(default) {
            return Err(catalog_error(
                path,
                format!("providers.{provider_id}.{source}.default must name a declared model"),
            ));
        }
    }
    Ok(ProviderModelInventory {
        models,
        default_model: raw.default,
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    schema_version: u32,
    providers: RawCatalogProviders,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogProviders {
    cerebras: RawProviderSources,
    claude: RawProviderSources,
    codex: RawProviderSources,
    gemini: RawProviderSources,
    openai_realtime: RawProviderSources,
    openrouter: RawProviderSources,
    z_ai: RawProviderSources,
    // Older operator-owned v2 catalogs predate the user-defined endpoint
    // provider; the default below gives them an empty discovery-only
    // inventory instead of failing the whole catalog parse.
    #[serde(default = "default_custom_openai_sources")]
    custom_openai: RawProviderSources,
    local: RawProviderSources,
}

// The custom OpenAI-compatible provider declares no static models; its
// inventory comes entirely from `{endpoint}/models` discovery.
fn default_custom_openai_sources() -> RawProviderSources {
    RawProviderSources::api(RawProvider {
        default: None,
        models: Vec::new(),
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProviderSources {
    #[serde(skip_serializing_if = "Option::is_none")]
    api: Option<RawProvider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<RawProvider>,
}

impl RawProviderSources {
    fn api(provider: RawProvider) -> Self {
        Self {
            api: Some(provider),
            client: None,
        }
    }
    fn client(provider: RawProvider) -> Self {
        Self {
            api: None,
            client: Some(provider),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    models: Vec<RawModel>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    id: String,
    display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCatalogFile {
    schema_version: u32,
    providers: LegacyRawCatalogProviders,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRawCatalogProviders {
    cerebras: RawProvider,
    claude: RawProvider,
    codex: RawProvider,
    gemini: RawProvider,
    openai_realtime: RawProvider,
    openrouter: RawProvider,
    z_ai: RawProvider,
    local: RawProvider,
}

#[derive(Debug, Clone)]
struct CatalogProviders {
    cerebras: ProviderModelSources,
    claude: ProviderModelSources,
    codex: ProviderModelSources,
    gemini: ProviderModelSources,
    openai_realtime: ProviderModelSources,
    openrouter: ProviderModelSources,
    z_ai: ProviderModelSources,
    custom_openai: ProviderModelSources,
    local: ProviderModelSources,
}

impl CatalogProviders {
    fn for_kind(&self, kind: LlmProviderKind) -> &ProviderModelSources {
        match kind {
            LlmProviderKind::Cerebras => &self.cerebras,
            LlmProviderKind::Claude => &self.claude,
            LlmProviderKind::Codex => &self.codex,
            LlmProviderKind::Gemini => &self.gemini,
            LlmProviderKind::OpenAiRealtime => &self.openai_realtime,
            LlmProviderKind::OpenRouter => &self.openrouter,
            LlmProviderKind::Zai => &self.z_ai,
            LlmProviderKind::OpenAiCompatible => &self.custom_openai,
            LlmProviderKind::Local => &self.local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_catalog_migrates_atomically_to_v2_with_dual_providers_client_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-models.toml");
        fs::write(
            &path,
            SEED.replace("[providers.custom_openai.api]\nmodels = []\n", "")
                .replace("schema_version = 2", "schema_version = 1")
                .replace(".api", "")
                .replace(".client", ""),
        )
        .unwrap();
        let catalog = ProviderModelCatalog::new(&path).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("schema_version = 2")
        );
        assert!(catalog.sources(LlmProviderKind::Codex).client.is_some());
        assert!(catalog.sources(LlmProviderKind::Codex).api.is_none());
        assert!(catalog.sources(LlmProviderKind::OpenRouter).api.is_some());
    }

    #[test]
    fn resolution_is_source_local_catalog_first_and_deterministic() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = ProviderModelCatalog::new(directory.path().join("catalog.toml")).unwrap();
        let resolved = catalog
            .resolve(
                LlmProviderKind::OpenRouter,
                LlmModelSource::Api,
                Some(vec![
                    LlmModelDescriptor {
                        id: "live-z".into(),
                        display_name: "ignored".into(),
                    },
                    LlmModelDescriptor {
                        id: "openai/gpt-4.1".into(),
                        display_name: "wrong".into(),
                    },
                    LlmModelDescriptor {
                        id: "live-a".into(),
                        display_name: "ignored".into(),
                    },
                ]),
                "provider-default",
            )
            .unwrap();
        assert_eq!(resolved.selected_model, "openai/gpt-4.1-mini");
        assert_eq!(
            resolved
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "openai/gpt-4.1-mini",
                "openai/gpt-4.1",
                "anthropic/claude-sonnet-4",
                "google/gemini-2.5-pro",
                "live-a",
                "live-z"
            ]
        );
        assert!(resolved.sources.client.is_none());
    }

    #[test]
    fn candidates_uses_the_provider_conventional_source() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = ProviderModelCatalog::new(directory.path().join("catalog.toml")).unwrap();

        let candidates = catalog.candidates(LlmProviderKind::Claude);

        assert_eq!(candidates.default_model.as_deref(), Some("sonnet"));
        assert_eq!(
            candidates
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["opus", "sonnet", "haiku"]
        );
    }

    #[test]
    fn schema_v2_is_not_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.toml");
        fs::write(&path, SEED).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        ProviderModelCatalog::new(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }
}
