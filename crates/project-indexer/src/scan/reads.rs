use super::*;
use crate::SourceParseInput;
use sha2::{Digest, Sha256};

struct PendingFile {
    file: ScannedFile,
    bytes: Option<Vec<u8>>,
    changed: bool,
}

impl<S: ProjectSource, P: ProjectFileParser> ProjectIndexer<S, P> {
    pub(super) fn prepare_batch(
        &mut self,
        entries: &[SourceEntry],
        scan: &mut PreparedProjectScan,
    ) -> Result<(), ScanError> {
        let pending: Vec<_> = entries
            .iter()
            .map(|entry| self.prepare_entry(entry.clone(), scan))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let inputs: Vec<_> = pending
            .iter()
            .filter_map(|pending| {
                pending.bytes.as_ref().map(|bytes| SourceParseInput {
                    path: &pending.file.entry.path,
                    bytes,
                })
            })
            .collect();
        let parsed = self.parser.parse_batch(&inputs)?;
        if parsed.len() != inputs.len() {
            return Err(invalid(
                parsed.len().to_string(),
                format!("expected {} ordered parser results", inputs.len()),
            ));
        }
        scan.metrics.files_parsed += parsed.len();
        let mut parsed = parsed.into_iter();
        for mut pending in pending {
            if pending.bytes.is_some() {
                pending.file.parsed =
                    Arc::new(parsed.next().expect("validated parser result count"));
            }
            scan.retain(Arc::new(pending.file), pending.changed);
        }
        Ok(())
    }

    fn prepare_entry(
        &self,
        entry: SourceEntry,
        scan: &mut PreparedProjectScan,
    ) -> Result<Option<PendingFile>, ScanError> {
        let old = self.files.get(&entry.path);
        if let Some(old) = old.filter(|old| freshness_matches(old, &entry)) {
            if scan.full_snapshot {
                scan.retain(Arc::clone(old), true);
            }
            return Ok(None);
        }
        let (file, bytes) = self.read_entry(entry, old.map(Arc::as_ref), &mut scan.metrics)?;
        let changed =
            old.is_none_or(|old| old.entry.kind != file.entry.kind || old.digest != file.digest);
        Ok(Some(PendingFile {
            file,
            bytes,
            changed,
        }))
    }

    fn read_entry(
        &self,
        mut entry: SourceEntry,
        old: Option<&ScannedFile>,
        metrics: &mut ScanMetrics,
    ) -> Result<(ScannedFile, Option<Vec<u8>>), ScanError> {
        if entry.kind == SourceKind::Directory {
            return Ok((
                ScannedFile {
                    entry,
                    digest: [0; 32],
                    fingerprint: None,
                    byte_len: 0,
                    parsed: Arc::new(ParsedFile::default()),
                },
                None,
            ));
        }
        let read = self.source.read_file(&entry.path)?;
        entry.stamp = read.stamp;
        metrics.files_read += 1;
        metrics.bytes_read += read.bytes.len();
        let digest = Sha256::digest(&read.bytes).into();
        let fingerprint = old
            .filter(|old| old.digest == digest)
            .and_then(|old| old.fingerprint.clone())
            .unwrap_or_else(|| crate::SourceFingerprint::file(&entry.path, &read.bytes));
        let cached = old.filter(|old| old.entry.kind == SourceKind::File && old.digest == digest);
        let parsed = cached
            .map(|old| Arc::clone(&old.parsed))
            .unwrap_or_default();
        let file = ScannedFile {
            entry,
            digest,
            fingerprint: Some(fingerprint),
            byte_len: read.bytes.len() as u64,
            parsed,
        };
        Ok((file, cached.is_none().then_some(read.bytes)))
    }
}
