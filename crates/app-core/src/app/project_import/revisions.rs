//! Import orchestration uses the common producer owner; Semantic keeps normalization.
use super::*;
use crate::index_publication::IndexSourceCheckout;

pub(super) fn prepare_checkout(
    service: &ProjectFolderImportService,
    session: &mut ProjectImportSession,
    root: &Path,
    route: &str,
    control: &InvocationControl,
) -> Result<(), HttpResponse> {
    if session.source_checkout.is_some() {
        return Ok(());
    }
    let client = service
        .index_publication_client
        .get()
        .ok_or_else(|| source_failure("missing persisted startup client identity"))?;
    let checkout = IndexSourceCheckout::open(
        service.semantic.clone(),
        service.relational.clone(),
        client,
        root,
        route,
        PROVIDER_INSTANCE_ID,
        control,
    )
    .map_err(source_failure)?
    .for_background_job();
    session.projection =
        SemanticIndexProjection::with_source(root, PROVIDER_INSTANCE_ID, checkout.source().clone())
            .map_err(scan_failure)?;
    session.source_checkout = Some(checkout);
    Ok(())
}

pub(super) fn source_failure(error: impl std::fmt::Display) -> HttpResponse {
    error_response(
        "500 Internal Server Error",
        format!("source publication failed: {error}"),
    )
}
