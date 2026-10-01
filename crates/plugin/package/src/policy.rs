/// Resource limits applied before archive or entry bytes are allocated.
#[derive(Clone, Copy, Debug)]
pub struct VerificationLimits {
    /// Maximum compressed `.lvp` file size.
    pub max_archive_bytes: u64,
    /// Maximum uncompressed size of any single ZIP entry.
    pub max_entry_bytes: u64,
    /// Maximum combined uncompressed size of all ZIP entries.
    pub max_total_uncompressed_bytes: u64,
}

impl Default for VerificationLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 256 * 1024 * 1024,
            max_entry_bytes: 128 * 1024 * 1024,
            max_total_uncompressed_bytes: 512 * 1024 * 1024,
        }
    }
}
