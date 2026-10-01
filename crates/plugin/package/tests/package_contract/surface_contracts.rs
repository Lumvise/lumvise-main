use super::*;

#[test]
fn verified_package_preserves_signed_http_surface_metadata() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let package = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .expect("valid signed package");
    let http = package
        .exports()
        .iter()
        .find(|export| export.id == "knowledge.http")
        .expect("signed HTTP export");

    assert!(matches!(
        http.surface,
        ExportSurface::HttpRoute {
            method: HttpMethod::Post,
            ref path_template,
            stream_mode: HttpStreamMode::ServerSentEvents,
            ref sse_policy,
        } if path_template == "/api/knowledge/{report_id}" && sse_policy == &Some(test_sse_policy())
    ));
}

#[test]
fn sse_route_without_signed_delivery_policy_is_rejected() {
    let mut fixture = SignedPackageFixture::valid();
    let ExportSurface::HttpRoute { sse_policy, .. } =
        &mut http_export_mut(&mut fixture.manifest).surface
    else {
        panic!("HTTP fixture export")
    };
    *sse_policy = None;

    let error = match verify_package(&fixture.write(), &fixture.public_key(), &compatible_host()) {
        Ok(_) => panic!("missing SSE policy accepted"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        PackageError::InvalidSsePolicy {
            field: "sse_policy",
            ..
        }
    ));
}

#[test]
fn recurring_task_requires_background_execution_and_bounded_policy() {
    let mut fixture = SignedPackageFixture::valid();
    let task = recurring_export_mut(&mut fixture.manifest);
    task.execution = ExecutionMode::Foreground;

    let error = verify_package(&fixture.write(), &fixture.public_key(), &compatible_host())
        .err()
        .expect("foreground recurring task rejected");

    assert!(
        matches!(error, PackageError::InvalidBackgroundExecution(id) if id == "knowledge.refresh")
    );
}

#[test]
fn storage_trigger_rejects_duplicate_event_filter() {
    let mut fixture = SignedPackageFixture::valid();
    let ExportSurface::StorageTrigger { event_kinds, .. } =
        &mut storage_export_mut(&mut fixture.manifest).surface
    else {
        panic!("storage fixture export")
    };
    event_kinds.push(event_kinds[0].clone());

    let error = verify_package(&fixture.write(), &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate event kind rejected");

    assert!(matches!(
        error,
        PackageError::InvalidStorageTriggerFilter {
            field: "event_kinds",
            ..
        }
    ));
}

#[test]
fn verified_package_preserves_signed_view_surface_metadata() {
    let fixture = SignedPackageFixture::valid();
    let package = verify_package(&fixture.write(), &fixture.public_key(), &compatible_host())
        .expect("valid signed package");
    let view = package
        .exports()
        .iter()
        .find(|export| export.id == "knowledge.view")
        .expect("signed View export");

    assert!(matches!(
        view.surface,
        ExportSurface::View {
            ref view_id,
            surface: ViewSurface::Fullscreen,
            ref asset_path,
            ref content_security_policy,
            ref allowed_host_apis,
            menu_placement: Some(ViewMenuPlacement::DesktopSettings),
        } if view_id == "knowledge.report"
            && asset_path == VIEW_ASSET_PATH
            && content_security_policy == "default-src 'none'; script-src 'self'"
            && allowed_host_apis == &["knowledge.read"]
    ));
}

#[test]
fn view_menu_placement_is_optional_for_existing_signed_manifests() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = serde_json::to_value(&fixture.manifest).expect("manifest value");
    let view = manifest["exports"]
        .as_array_mut()
        .expect("export array")
        .iter_mut()
        .find(|export| export["kind"] == "view")
        .expect("View export");
    view.as_object_mut()
        .expect("View object")
        .remove("menu_placement");

    let decoded: PluginManifest = serde_json::from_value(manifest).expect("legacy View manifest");
    let view = decoded
        .exports
        .iter()
        .find(|export| matches!(export.surface, ExportSurface::View { .. }))
        .expect("View export");
    assert!(matches!(
        view.surface,
        ExportSurface::View {
            menu_placement: None,
            ..
        }
    ));
}

#[test]
fn verified_package_preserves_signed_scoped_mcp_surface() {
    let fixture = SignedPackageFixture::valid();
    let package = verify_package(&fixture.write(), &fixture.public_key(), &compatible_host())
        .expect("valid scoped package");
    let scoped = package
        .exports()
        .iter()
        .find(|export| export.id == "knowledge.scoped")
        .expect("scoped export");
    assert!(matches!(&scoped.surface,
        ExportSurface::ScopedMcpTool { scope } if scope == "assistant_session"));
}

#[test]
fn scoped_mcp_surface_rejects_empty_uppercase_and_repeated_separator_scopes() {
    for scope in ["", "AssistantSession", "assistant__session"] {
        let fixture = SignedPackageFixture::valid();
        let mut manifest = fixture.manifest.clone();
        let scoped = manifest
            .exports
            .iter_mut()
            .find(|export| export.id == "knowledge.scoped")
            .expect("scoped fixture");
        scoped.surface = ExportSurface::ScopedMcpTool {
            scope: scope.into(),
        };
        let path = fixture.write_manifest(&manifest);
        let error = verify_package(&path, &fixture.public_key(), &compatible_host())
            .err()
            .expect("invalid scope rejected");
        assert!(matches!(error, PackageError::InvalidScopedMcpScope { .. }));
    }
}

#[test]
fn http_surface_missing_method_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = serde_json::to_value(&fixture.manifest).expect("manifest value");
    let exports = manifest["exports"].as_array_mut().expect("export array");
    let http = exports
        .iter_mut()
        .find(|export| export["kind"] == "http_route")
        .expect("HTTP export");
    http.as_object_mut().expect("HTTP object").remove("method");
    let bytes = serde_json::to_vec(&manifest).expect("invalid manifest bytes");
    let path = fixture.write_raw(&bytes, &bytes, &fixture.executable, &[]);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("missing HTTP method rejected");

    assert!(matches!(error, PackageError::InvalidManifest(_)));
}

#[test]
fn view_surface_missing_content_security_policy_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = serde_json::to_value(&fixture.manifest).expect("manifest value");
    let exports = manifest["exports"].as_array_mut().expect("export array");
    let view = exports
        .iter_mut()
        .find(|export| export["kind"] == "view")
        .expect("View export");
    view.as_object_mut()
        .expect("View object")
        .remove("content_security_policy");
    let bytes = serde_json::to_vec(&manifest).expect("invalid manifest bytes");
    let path = fixture.write_raw(&bytes, &bytes, &fixture.executable, &[]);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("missing View CSP rejected");

    assert!(matches!(error, PackageError::InvalidManifest(_)));
}

#[test]
fn multiline_view_content_security_policy_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let ExportSurface::View {
        content_security_policy,
        ..
    } = &mut view_export_mut(&mut manifest).surface
    else {
        panic!("expected View export");
    };
    *content_security_policy = "default-src 'none'\nscript-src *".into();
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("multiline View CSP rejected");

    assert!(matches!(
        error,
        PackageError::InvalidViewContentSecurityPolicy { .. }
    ));
}

#[test]
fn relative_http_path_template_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let ExportSurface::HttpRoute { path_template, .. } =
        &mut http_export_mut(&mut manifest).surface
    else {
        panic!("expected HTTP export");
    };
    *path_template = "api/knowledge".into();
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("relative HTTP path rejected");

    assert!(matches!(
        error,
        PackageError::InvalidHttpPathTemplate { .. }
    ));
}

#[test]
fn traversing_view_asset_path_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let ExportSurface::View { asset_path, .. } = &mut view_export_mut(&mut manifest).surface else {
        panic!("expected View export");
    };
    *asset_path = "../views/knowledge/index.html".into();
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("traversing View asset rejected");

    assert!(matches!(error, PackageError::UnsafePath(path) if path.contains("..")));
}

#[test]
fn unsigned_view_asset_path_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let ExportSurface::View { asset_path, .. } = &mut view_export_mut(&mut manifest).surface else {
        panic!("expected View export");
    };
    *asset_path = "views/knowledge/missing.html".into();
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("unsigned View asset rejected");

    assert!(matches!(error, PackageError::UnsignedViewAsset { .. }));
}

#[test]
fn duplicate_http_method_and_path_are_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let mut duplicate = http_export_mut(&mut manifest).clone();
    duplicate.id = "knowledge.http.duplicate".into();
    duplicate.name = "Duplicate HTTP route".into();
    manifest.exports.push(duplicate);
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate HTTP route rejected");

    assert!(matches!(error, PackageError::DuplicateHttpRoute { .. }));
}

#[test]
fn duplicate_view_ids_are_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    let mut duplicate = view_export_mut(&mut manifest).clone();
    duplicate.id = "knowledge.view.duplicate".into();
    duplicate.name = "Duplicate View".into();
    manifest.exports.push(duplicate);
    let path = fixture.write_manifest(&manifest);

    let error = verify_package(&path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate View id rejected");

    assert!(matches!(error, PackageError::DuplicateViewId(_)));
}
