//! Deadline policy for source-checkpoint operations; persisted rows and CAS rules stay unchanged.
use lumvise_resource_routing::InvocationControl;

#[derive(Clone, Copy, Default)]
pub(super) enum IndexPublicationInvocations {
    #[default]
    Inherited,
    AppOwned,
}

impl IndexPublicationInvocations {
    pub(super) fn operation(self, inherited: &InvocationControl) -> InvocationControl {
        match self {
            Self::Inherited => inherited.child(),
            Self::AppOwned => InvocationControl::sixty_seconds(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn only_app_owned_operations_receive_an_independent_sixty_second_budget() {
        let parent = InvocationControl::sixty_seconds();
        parent.cancel();
        assert!(
            IndexPublicationInvocations::Inherited
                .operation(&parent)
                .is_cancelled()
        );
        let operation = IndexPublicationInvocations::AppOwned.operation(&parent);
        assert!(!operation.is_cancelled());
        assert!(operation.remaining() > Duration::from_secs(59));
        assert!(operation.remaining() <= Duration::from_secs(60));
    }
}
