//! Production composition shared by the community host and desktop adapter.
//! Both enter through `build_app_runtime`; resource and plugin internals stay here.

use super::provider_startup::{
    configured_llm_registry, persisted_app_settings, spawn_llm_provider_sync,
};
use super::resource_routing::{ResourceRouter, RoutedResources};
use crate::{AppCore, PluginProductionConfig};
use lumvise_frontend_core::FrontendCore;
use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
use lumvise_resource_routing::{ResourcePlacement, ResourceRoutingConfig};
use std::sync::Arc;

pub(super) fn build_app_runtime() -> Result<Arc<AppCore>, String> {
    let config = ResourceRoutingConfig::from_environment()
        .map_err(|error| format!("reading resource routing configuration: {error}"))?;
    let resources = ResourceRouter::build(config.clone())
        .map_err(|error| format!("building selected resource routes: {error}"))?;
    let relational = Arc::clone(&resources.relational);
    let app = Arc::new(build_selected_app(&config, resources)?);
    app.install_managed_models()
        .map_err(|error| format!("installing managed model runtime: {error}"))?;
    spawn_metrics_sampler(Arc::clone(&app));
    if config.llm_execution == ResourcePlacement::Internal {
        spawn_llm_provider_sync(Arc::clone(&app), relational);
    }
    Ok(app)
}

fn build_selected_app(
    config: &ResourceRoutingConfig,
    mut resources: RoutedResources,
) -> Result<AppCore, String> {
    if config.llm_execution == ResourcePlacement::Internal {
        resources.llms = configured_llm_registry(
            resources.relational.as_ref(),
            Arc::new(ReqwestLlmHttpClient::new()),
        )
        .map_err(|error| format!("loading configured LLM providers: {error}"))?;
    }
    let settings = persisted_app_settings(resources.relational.as_ref())
        .map_err(|error| format!("loading frontend settings: {error}"))?;
    // No catalog reconcile here: background discovery has not published a
    // provider catalog yet, and reconciling against it would erase the stored
    // assistant model (a later sync would then reinstall the provider
    // default). The real sync in `spawn_llm_provider_sync` owns the
    // reconcile once discovery produced the actual catalog.
    let app = AppCore::new_production(
        resources.semantic,
        resources.relational,
        FrontendCore::new(settings),
        resources.llms,
        configured_plugins()?,
    )
    .map_err(|error| format!("building production app core: {error}"))?;
    *app.provider_catalog
        .lock()
        .map_err(|_| "provider catalog lock poisoned")? = resources.llm_catalog;
    install_selected_speech(
        app,
        config,
        resources.speech_recognizer,
        resources.speech_synthesizer,
    )
}

fn configured_plugins() -> Result<PluginProductionConfig, String> {
    let config = PluginProductionConfig::configured();
    config
        .install_bundled_release_if_present()
        .map_err(|error| format!("installing bundled plugin release: {error}"))?;
    config
        .initialize_deny_all_policy_if_missing()
        .map_err(|error| format!("initializing plugin deny-all policy: {error}"))?;
    Ok(config)
}

fn install_selected_speech(
    app: AppCore,
    config: &ResourceRoutingConfig,
    recognizer: Option<Arc<dyn lumvise_neural_core::SpeechRecognizer>>,
    synthesizer: Option<Arc<dyn lumvise_neural_core::SpeechSynthesizer>>,
) -> Result<AppCore, String> {
    if config.speech_inference != ResourcePlacement::Centralized {
        return install_optional_voice_services(app);
    }
    let app = recognizer
        .map(|service| app.with_speech_recognizer(service))
        .ok_or("selected centralized speech route has no STT adapter")?;
    synthesizer
        .map(|service| app.with_speech_synthesizer(service))
        .ok_or_else(|| "selected centralized speech route has no TTS adapter".to_string())
}

fn install_optional_voice_services(app: AppCore) -> Result<AppCore, String> {
    #[cfg(all(feature = "assistant-e2e", feature = "desktop-app"))]
    return super::e2e_voice_adapters::install_environment_voice_adapters(app)
        .map_err(|error| format!("installing assistant-e2e voice adapters: {error}"));
    #[cfg(not(all(feature = "assistant-e2e", feature = "desktop-app")))]
    Ok(app)
}

/// Periodically pushes graph-derived observability gauges into the metrics
/// recorder. Hook-specific snapshots are emitted by the coordinator.
fn spawn_metrics_sampler(app: Arc<AppCore>) {
    let interval = crate::observability::sampler_interval();
    if interval.is_zero() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("lumvise-metrics-sampler".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(interval);
                sample_periodic_gauges(&app);
            }
        });
}

fn sample_periodic_gauges(_app: &AppCore) {}
