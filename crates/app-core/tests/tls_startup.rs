//! Exercise process-level TLS setup through the public AppCore constructor.
use lumvise_app_core::AppCore;
use std::process::{Command, Output};

#[test]
fn fresh_app_core_supports_live_tls_with_both_crypto_backends() {
    const CHILD_FLAG: &str = "LUMVISE_TLS_STARTUP_TEST_CHILD";
    if std::env::var_os(CHILD_FLAG).is_some() {
        assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        let _app = AppCore::in_memory().expect("in-memory AppCore startup");
        let _tls = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        return;
    }
    let output = isolated_tls_probe(CHILD_FLAG);
    assert!(
        output.status.success(),
        "fresh-process TLS failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn isolated_tls_probe(child_flag: &str) -> Output {
    // A child prevents another parallel test from preselecting Rustls's process default.
    Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "fresh_app_core_supports_live_tls_with_both_crypto_backends",
            "--nocapture",
        ])
        .env(child_flag, "1")
        .output()
        .expect("fresh TLS test process")
}
