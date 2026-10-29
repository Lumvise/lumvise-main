//! Owns only disposable processes, TLS material and the test access-token validator.
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use lumvise_resource_routing::{InvocationControl, auth::AuthenticatedPrincipal};
use lumvise_resource_server::{
    AccessTokenValidator, ResourceListener, ResourceServer, ServerResources,
};

pub(super) struct ProcessFixture {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<String>,
    role: String,
}

impl ProcessFixture {
    pub(super) fn launch(root: &Path, role: &str, endpoint: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "revision_network_process", "--nocapture"]);
        command
            .env("LUMVISE_REVISION_TEST_ROLE", role)
            .env("LUMVISE_REVISION_TEST_ROOT", root);
        command.env(
            "LUMVISE_REVISION_TEST_ENDPOINT",
            endpoint.unwrap_or_default(),
        );
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take();
        let output = stdout_lines(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            role: role.into(),
        }
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn expect(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let line = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "child `{}` output `{error}`; expected `{prefix}`",
                        self.role
                    )
                });
            if let Some(value) = line.strip_prefix(prefix) {
                return value.trim().to_owned();
            }
        }
    }

    pub(super) fn signal(&mut self, value: &str) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }

    pub(super) fn finish(mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "child `{}` exited `{status}`; expected success",
                    self.role
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "child `{}` did not exit; expected exit within 60 seconds",
                self.role
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn stdout_lines(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                return;
            }
        }
    });
    receiver
}

pub(super) fn event(value: &str) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

pub(super) fn wait_for_publication() {
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(
        command.trim(),
        "publish",
        "expected publication test signal"
    );
}

pub(super) fn create_certificate(root: &Path) {
    let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    std::fs::write(root.join("certificate.pem"), certificate.cert.pem()).unwrap();
    std::fs::write(
        root.join("key.pem"),
        certificate.signing_key.serialize_pem(),
    )
    .unwrap();
}

pub(super) fn start_server(root: &Path) -> (ProcessFixture, String) {
    let mut process = ProcessFixture::launch(root, "server", None);
    let port = process.expect("SERVER_READY ").parse::<u16>().unwrap();
    (process, format!("https://127.0.0.1:{port}/"))
}

struct CooperationAccessTokens;

impl AccessTokenValidator for CooperationAccessTokens {
    fn validate(
        &self,
        token: &str,
        _: &InvocationControl,
    ) -> Result<AuthenticatedPrincipal, String> {
        if !matches!(token, "client-a" | "client-b") {
            return Err(format!(
                "token `{token}`; expected client-a or client-b fixture token"
            ));
        }
        Ok(AuthenticatedPrincipal {
            issuer: "https://revision-fixture.example".into(),
            subject: token.into(),
            tenant_id: "shared-revision-tenant".into(),
            scopes: BTreeSet::from([
                "lumvise.resources".into(),
                "lumvise.resources.migrate".into(),
            ]),
        })
    }
}

pub(super) fn serve() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(serve_listener());
}

async fn serve_listener() {
    let root = std::path::PathBuf::from(std::env::var_os("LUMVISE_REVISION_TEST_ROOT").unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let listener =
        ResourceListener::new(tcp, &root.join("certificate.pem"), &root.join("key.pem")).unwrap();
    let server = Arc::new(ResourceServer::new(ServerResources::internal(
        root.join("server-state"),
        Arc::new(CooperationAccessTokens),
        None,
        None,
        None,
    )));
    event(&format!("SERVER_READY {port}"));
    let shutdown =
        tokio::task::spawn_blocking(|| std::io::stdin().read_to_end(&mut Vec::new()).unwrap());
    tokio::select! {
        result = server.serve_listener(listener) => result.unwrap(),
        result = shutdown => { result.unwrap(); }
    }
}
