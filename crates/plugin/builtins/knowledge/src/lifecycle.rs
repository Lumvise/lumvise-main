//! Shared Knowledge record write stamps and discovery lifecycle rules.

use chrono::{DateTime, Utc};
use lumvise_contracts::{KnowledgeArtifactLifecycleV2 as Lifecycle, KnowledgeArtifactStatusV2};

pub(crate) fn stamp(current: &mut Lifecycle, previous: Option<&Lifecycle>, now: DateTime<Utc>) {
    let now = now.to_rfc3339();
    current.revision = previous.map_or(0, |previous| previous.revision) + 1;
    current.created_at = match previous {
        Some(previous) => previous.created_at.clone(),
        None => Some(now.clone()),
    };
    current.status_changed_at = match previous {
        Some(previous) if previous.status != current.status => Some(now.clone()),
        Some(previous) => previous.status_changed_at.clone(),
        None => current.status_changed_at.clone(),
    };
    current.updated_at = Some(now);
}

pub(crate) fn is_active(lifecycle: &Lifecycle, now: DateTime<Utc>) -> bool {
    let expired = lifecycle.expires_at.as_deref().is_some_and(|expiry| {
        DateTime::parse_from_rfc3339(expiry).is_ok_and(|expiry| expiry <= now)
    });
    lifecycle.status == KnowledgeArtifactStatusV2::Active && !expired
}
