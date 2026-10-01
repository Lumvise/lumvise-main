use lumvise_resource_server::{
    OidcAccessTokenValidator, ResourceServer, ResourceServerConfig, ResourceServerError,
    ServerResources,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = ResourceServerConfig::from_environment()?;

    // OidcAccessTokenValidator::discover calls reqwest::blocking, which
    // creates/drops its own Tokio runtime internally.  Running it directly
    // inside a #[tokio::main] context panics with "Cannot drop a runtime in
    // a context where blocking is not allowed".  Move it to a blocking thread
    // via spawn_blocking so the inner runtime lifecycle is safe.
    let oidc_config = config.oidc.clone();
    let validator =
        tokio::task::spawn_blocking(move || OidcAccessTokenValidator::discover(&oidc_config))
            .await
            .map_err(|join_error| {
                ResourceServerError::AuthenticationSetup(format!(
                    "OIDC discovery task panicked: {join_error}"
                ))
            })??;
    let access_tokens = Arc::new(validator);

    // This is the only neural composition root for the central server. It
    // accepts server-owned configuration and therefore cannot recurse through
    // desktop or centralized adapters.
    let resources = ServerResources::internal_from_neural_configuration(
        config.data_dir.clone(),
        access_tokens,
        &config.neural,
    )?;
    Arc::new(ResourceServer::new(resources))
        .serve(&config)
        .await?;
    Ok(())
}
