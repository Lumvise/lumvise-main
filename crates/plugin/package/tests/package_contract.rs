use std::{collections::BTreeMap, fs::File, io::Write, path::Path};

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use lumvise_plugin_package::{
    ExecutionMode, ExportDescriptor, ExportSurface, HostCapabilityRequirement, HostCompatibility,
    HttpMethod, HttpStreamMode, PackageError, PluginManifest, ProtocolRange, PublisherIdentity,
    SseStreamPolicy, VerificationLimits, ViewMenuPlacement, ViewSurface, install_verified_package,
    verify_package, verify_package_with_limits,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

struct SignedPackageFixture {
    workspace: TempDir,
    signing_key: SigningKey,
    manifest: PluginManifest,
    executable: Vec<u8>,
}

const VIEW_ASSET_PATH: &str = "views/knowledge/index.html";
const VIEW_ASSET: &[u8] = b"<!doctype html><title>Knowledge</title>";

#[path = "package_contract/archive_contracts.rs"]
mod archive_contracts;
#[path = "package_contract/manifest_contracts.rs"]
mod manifest_contracts;
#[path = "package_contract/surface_contracts.rs"]
mod surface_contracts;

impl SignedPackageFixture {
    fn valid() -> Self {
        let executable = b"#!/bin/sh\nprintf knowledge-plugin".to_vec();
        let manifest = PluginManifest {
            schema_version: 1,
            publisher: PublisherIdentity {
                publisher_id: "lumvise.test".into(),
                key_id: "package.release.1".into(),
            },
            plugin_id: "knowledge".into(),
            plugin_version: "1.2.3".into(),
            protocol: ProtocolRange { min: 2, max: 4 },
            targets: BTreeMap::from([(
                "aarch64-apple-darwin".into(),
                "bin/knowledge-aarch64-apple-darwin".into(),
            )]),
            files: BTreeMap::from([
                (
                    "bin/knowledge-aarch64-apple-darwin".into(),
                    hex::encode(Sha256::digest(&executable)),
                ),
                (
                    VIEW_ASSET_PATH.into(),
                    hex::encode(Sha256::digest(VIEW_ASSET)),
                ),
            ]),
            exports: fixture_exports(),
            host_capabilities: vec![HostCapabilityRequirement {
                id: "semantic.read".into(),
                version: "^1.0".into(),
            }],
        };
        Self {
            workspace: tempfile::tempdir().expect("fixture workspace"),
            signing_key: SigningKey::from_bytes(&[7; 32]),
            manifest,
            executable,
        }
    }

    fn public_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    fn write(&self) -> std::path::PathBuf {
        self.write_with(&self.executable, &[])
    }

    fn write_with(&self, executable: &[u8], extra: &[(&str, &[u8])]) -> std::path::PathBuf {
        let manifest = serde_json::to_vec(&self.manifest).expect("canonical manifest");
        self.write_raw(&manifest, &manifest, executable, extra)
    }

    fn write_manifest(&self, manifest: &PluginManifest) -> std::path::PathBuf {
        let manifest = serde_json::to_vec(manifest).expect("canonical manifest");
        self.write_raw(&manifest, &manifest, &self.executable, &[])
    }

    fn write_unsigned_manifest_mutation(&self, manifest: &PluginManifest) -> std::path::PathBuf {
        let signed = serde_json::to_vec(&self.manifest).expect("signed manifest");
        let archive = serde_json::to_vec(manifest).expect("archive manifest");
        self.write_raw(&archive, &signed, &self.executable, &[])
    }

    fn write_raw(
        &self,
        manifest: &[u8],
        signed_manifest: &[u8],
        executable: &[u8],
        extra: &[(&str, &[u8])],
    ) -> std::path::PathBuf {
        let path = self.workspace.path().join("knowledge.lvp");
        let file = File::create(&path).expect("create package");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        let signature = self.signing_key.sign(&signature_payload(signed_manifest));

        archive
            .start_file("manifest.json", options)
            .expect("manifest entry");
        archive.write_all(manifest).expect("manifest bytes");
        archive
            .start_file("signature.ed25519", options)
            .expect("signature entry");
        archive
            .write_all(&signature.to_bytes())
            .expect("signature bytes");
        archive
            .start_file("bin/knowledge-aarch64-apple-darwin", options)
            .expect("executable entry");
        archive.write_all(executable).expect("executable bytes");
        archive
            .start_file(VIEW_ASSET_PATH, options)
            .expect("view asset entry");
        archive.write_all(VIEW_ASSET).expect("view asset bytes");
        for (name, bytes) in extra {
            archive.start_file(*name, options).expect("extra entry");
            archive.write_all(bytes).expect("extra bytes");
        }
        archive.finish().expect("finish package");
        path
    }

    fn write_with_duplicate_executable(&self) -> std::path::PathBuf {
        const ORIGINAL: &[u8] = b"bin/kn0wledge-aarch64-apple-darwin";
        const DUPLICATE: &[u8] = b"bin/knowledge-aarch64-apple-darwin";
        let path = self.write_with(
            &self.executable,
            &[(
                std::str::from_utf8(ORIGINAL).expect("ASCII fixture path"),
                &self.executable,
            )],
        );
        let archive = std::fs::read(&path).expect("read duplicate fixture");
        let mut rewritten = Vec::with_capacity(archive.len());
        let mut remaining = archive.as_slice();
        while let Some(offset) = remaining
            .windows(ORIGINAL.len())
            .position(|part| part == ORIGINAL)
        {
            rewritten.extend_from_slice(&remaining[..offset]);
            rewritten.extend_from_slice(DUPLICATE);
            remaining = &remaining[offset + ORIGINAL.len()..];
        }
        rewritten.extend_from_slice(remaining);
        std::fs::write(&path, rewritten).expect("rewrite duplicate names");
        path
    }

    fn write_with_symlink(&self) -> std::path::PathBuf {
        const LINK: &[u8] = b"bin/escape-link";
        let path = self.write_with(&self.executable, &[("bin/escape-link", b"../outside")]);
        let mut archive = std::fs::read(&path).expect("read symlink fixture");
        let name_offset = archive
            .windows(LINK.len())
            .enumerate()
            .find_map(|(offset, part)| {
                (part == LINK
                    && offset >= 46
                    && archive.get(offset - 46..offset - 42) == Some(&[0x50, 0x4b, 0x01, 0x02]))
                .then_some(offset)
            })
            .expect("central-directory link entry");
        let header = name_offset - 46;
        archive[header + 5] = 3;
        archive[header + 38..header + 42].copy_from_slice(&(0o120777_u32 << 16).to_le_bytes());
        std::fs::write(&path, archive).expect("write symlink fixture");
        path
    }
}

fn fixture_exports() -> Vec<ExportDescriptor> {
    vec![
        fixture_export(
            "knowledge.search",
            ExportSurface::McpTool,
            ExecutionMode::Foreground,
        ),
        fixture_export(
            "knowledge.scoped",
            ExportSurface::ScopedMcpTool {
                scope: "assistant_session".into(),
            },
            ExecutionMode::Foreground,
        ),
        fixture_export(
            "knowledge.http",
            ExportSurface::HttpRoute {
                method: HttpMethod::Post,
                path_template: "/api/knowledge/{report_id}".into(),
                stream_mode: HttpStreamMode::ServerSentEvents,
                sse_policy: Some(test_sse_policy()),
            },
            ExecutionMode::Foreground,
        ),
        fixture_export(
            "knowledge.view",
            ExportSurface::View {
                view_id: "knowledge.report".into(),
                surface: ViewSurface::Fullscreen,
                asset_path: VIEW_ASSET_PATH.into(),
                content_security_policy: "default-src 'none'; script-src 'self'".into(),
                allowed_host_apis: vec!["knowledge.read".into()],
                menu_placement: Some(ViewMenuPlacement::DesktopSettings),
            },
            ExecutionMode::Foreground,
        ),
        fixture_export(
            "knowledge.refresh",
            ExportSurface::RecurringTask {
                interval_seconds: 600,
                delivery: test_background_policy(),
            },
            ExecutionMode::Background,
        ),
        fixture_export(
            "knowledge.storage",
            ExportSurface::StorageTrigger {
                event_kinds: vec!["semantic.element.upserted".into()],
                entity_kinds: vec!["semantic_element".into()],
                delivery: test_background_policy(),
            },
            ExecutionMode::Background,
        ),
        fixture_export(
            "knowledge.command",
            ExportSurface::Command,
            ExecutionMode::Foreground,
        ),
    ]
}

fn test_background_policy() -> lumvise_plugin_package::BackgroundDeliveryPolicy {
    lumvise_plugin_package::BackgroundDeliveryPolicy {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 10_000,
        dead_letter_max_entries: 100,
        dead_letter_retention_seconds: 86_400,
    }
}

fn test_sse_policy() -> SseStreamPolicy {
    SseStreamPolicy {
        max_events_per_poll: 100,
        poll_interval_ms: 100,
        heartbeat_interval_ms: 5_000,
        max_backoff_ms: 2_000,
    }
}

fn fixture_export(id: &str, surface: ExportSurface, execution: ExecutionMode) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: id.into(),
        name: id.replace('.', " "),
        surface,
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: serde_json::json!({"type": "object"}),
        admission: None,
        execution,
    }
}

fn http_export_mut(manifest: &mut PluginManifest) -> &mut ExportDescriptor {
    manifest
        .exports
        .iter_mut()
        .find(|export| matches!(export.surface, ExportSurface::HttpRoute { .. }))
        .expect("HTTP fixture export")
}

fn view_export_mut(manifest: &mut PluginManifest) -> &mut ExportDescriptor {
    manifest
        .exports
        .iter_mut()
        .find(|export| matches!(export.surface, ExportSurface::View { .. }))
        .expect("View fixture export")
}

fn recurring_export_mut(manifest: &mut PluginManifest) -> &mut ExportDescriptor {
    manifest
        .exports
        .iter_mut()
        .find(|export| matches!(export.surface, ExportSurface::RecurringTask { .. }))
        .expect("recurring fixture export")
}

fn storage_export_mut(manifest: &mut PluginManifest) -> &mut ExportDescriptor {
    manifest
        .exports
        .iter_mut()
        .find(|export| matches!(export.surface, ExportSurface::StorageTrigger { .. }))
        .expect("storage fixture export")
}

fn signature_payload(manifest: &[u8]) -> Vec<u8> {
    [b"LUMVISE_PLUGIN_PACKAGE_V1\0".as_slice(), manifest].concat()
}

fn compatible_host() -> HostCompatibility {
    HostCompatibility::new(3, "aarch64-apple-darwin")
}

#[cfg(unix)]
fn is_read_only(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("installed metadata")
        .permissions()
        .mode()
        & 0o222
        == 0
}

#[cfg(not(unix))]
fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .expect("installed metadata")
        .permissions()
        .readonly()
}
