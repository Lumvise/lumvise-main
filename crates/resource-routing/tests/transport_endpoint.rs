use lumvise_resource_routing::Http2CentralTransport;
use url::Url;

#[test]
fn endpoint_uses_existing_tls_without_oidc_configuration() {
    assert!(
        Http2CentralTransport::from_endpoint(Url::parse("https://localhost:443").unwrap(), None)
            .is_ok()
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ca.pem");
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(&path, certificate.cert.pem()).unwrap();
    assert!(
        Http2CentralTransport::from_endpoint(
            Url::parse("https://localhost:443").unwrap(),
            Some(&path)
        )
        .is_ok()
    );
    std::fs::write(&path, "not a certificate").unwrap();
    assert!(
        Http2CentralTransport::from_endpoint(
            Url::parse("https://localhost:443").unwrap(),
            Some(&path)
        )
        .is_err()
    );
}

#[test]
fn endpoint_rejects_ambiguous_routes_and_never_echoes_url_credentials() {
    for endpoint in [
        "http://localhost",
        "https://localhost/path",
        "https://localhost/?query=1",
        "https://localhost/#fragment",
        "https://user:secret@localhost/",
    ] {
        let error = Http2CentralTransport::from_endpoint(Url::parse(endpoint).unwrap(), None)
            .err()
            .expect("invalid origin rejected");
        assert!(!error.to_string().contains("secret"));
    }
}
