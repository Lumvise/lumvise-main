use super::*;

struct FakeInstalledCatalog {
    _directory: tempfile::TempDir,
    path: PathBuf,
    original: String,
}

impl FakeInstalledCatalog {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-models.toml");
        let mut file: CatalogFile = toml::from_str(SEED).unwrap();
        for inventory in [
            file.providers.claude.client.as_mut().unwrap(),
            file.providers.codex.client.as_mut().unwrap(),
            file.providers.gemini.client.as_mut().unwrap(),
            file.providers.z_ai.api.as_mut().unwrap(),
        ] {
            inventory.default = Some("operator-choice".into());
            inventory.models = vec![RawModel {
                id: "operator-choice".into(),
                display_name: "Operator label".into(),
            }];
        }
        let original = toml::to_string(&file).unwrap();
        fs::write(&path, &original).unwrap();
        Self {
            _directory: directory,
            path,
            original,
        }
    }
}

#[test]
fn default_install_gains_current_choices_without_rewriting_or_changing_defaults() {
    let installed = FakeInstalledCatalog::new();
    let mut catalog = load_catalog_selection(installed.path.clone(), None).unwrap();
    catalog.append_bundled_choices().unwrap();
    for (kind, source, latest) in [
        (
            LlmProviderKind::Claude,
            LlmModelSource::Client,
            "claude-sonnet-5-5",
        ),
        (
            LlmProviderKind::Codex,
            LlmModelSource::Client,
            "gpt-6.1-sol",
        ),
        (
            LlmProviderKind::Gemini,
            LlmModelSource::Client,
            "gemini-3.8-flash",
        ),
        (LlmProviderKind::Zai, LlmModelSource::Api, "glm-5.2"),
    ] {
        let resolved = catalog
            .resolve(kind, source, None, "provider-default")
            .unwrap();
        assert_eq!(resolved.selected_model, "operator-choice");
        assert_eq!(resolved.models[0].display_name, "Operator label");
        assert_eq!(
            resolved
                .models
                .iter()
                .filter(|model| model.id == latest)
                .count(),
            1
        );
    }
    assert_eq!(
        fs::read_to_string(&installed.path).unwrap(),
        installed.original
    );
}

#[test]
fn explicitly_selected_catalog_remains_exact() {
    let installed = FakeInstalledCatalog::new();
    let catalog = load_catalog_selection(
        PathBuf::from("unused-default"),
        Some(installed.path.clone()),
    )
    .unwrap();
    let choices = catalog.candidates(LlmProviderKind::Claude);
    assert_eq!(choices.models.len(), 1);
    assert_eq!(choices.models[0].id, "operator-choice");
    assert_eq!(
        fs::read_to_string(&installed.path).unwrap(),
        installed.original
    );
}

#[test]
fn live_inventory_replaces_updated_fallbacks_and_retains_saved_selection() {
    let installed = FakeInstalledCatalog::new();
    let catalog = load_catalog_selection(installed.path.clone(), None).unwrap();
    let resolved = catalog
        .resolve(
            LlmProviderKind::Codex,
            LlmModelSource::Client,
            Some(vec![LlmModelDescriptor {
                id: "live-model".into(),
                display_name: "Live label".into(),
            }]),
            "saved-selection",
        )
        .unwrap();
    assert_eq!(
        resolved
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["live-model", "saved-selection"]
    );
    assert_eq!(resolved.selected_model, "saved-selection");
    assert!(resolved.sources.api.is_none());
}

#[test]
fn bundled_client_choices_never_cross_into_api_inventory() {
    let installed = FakeInstalledCatalog::new();
    let mut catalog = ProviderModelCatalog::new(&installed.path).unwrap();
    catalog.providers.claude.api = Some(ProviderModelInventory {
        default_model: Some("private-api-model".into()),
        models: vec![LlmModelDescriptor {
            id: "private-api-model".into(),
            display_name: "Private API".into(),
        }],
    });
    catalog.append_bundled_choices().unwrap();
    let api = catalog
        .sources(LlmProviderKind::Claude)
        .api
        .as_ref()
        .unwrap();
    assert_eq!(api.models.len(), 1);
    assert_eq!(api.models[0].id, "private-api-model");
}
