use crate::{DbError, Result};

pub(crate) fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(DbError::invalid_value(value, expected));
    }
    Ok(())
}
