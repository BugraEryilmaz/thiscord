use axum::{Router, extract::ConnectInfo, routing::get};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use thiscord_backend::tls::Tls;

struct Files(PathBuf);
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("cert.pem"));
        let _ = std::fs::remove_file(self.0.join("key.pem"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[tokio::test]
async fn https_validates_hostname_preserves_peer_and_survives_failed_reload() {
    let files =
        Files(std::env::temp_dir().join(format!("thiscord-tls-test-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&files.0).unwrap();
    // Ephemeral self-signed fixture; no production certificates or keys are copied.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pem = cert.cert.pem();
    let certificate = files.0.join("cert.pem");
    let key = files.0.join("key.pem");
    std::fs::write(&certificate, &pem).unwrap();
    std::fs::write(&key, cert.signing_key.serialize_pem()).unwrap();
    let tls = Tls::from_paths(certificate.clone(), key.clone())
        .await
        .unwrap();
    let app = Router::new().route(
        "/",
        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
            assert!(peer.ip().is_loopback());
            "TLS works"
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = axum_server::Handle::new();
    let server = axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls.config.clone())
        .unwrap()
        .handle(handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>());
    let task = tokio::spawn(server);
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
        .resolve("localhost", address)
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://localhost:{}/", address.port());
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "TLS works"
    );
    assert!(
        client
            .get(format!("https://{address}/"))
            .send()
            .await
            .is_err(),
        "Hostname checks must stay enabled"
    );
    assert!(
        client
            .get(format!("http://{address}/"))
            .send()
            .await
            .is_err(),
        "TLS port must reject plaintext HTTP"
    );
    tls.reload().await.unwrap();
    std::fs::write(&key, "not a private key").unwrap();
    assert!(tls.reload().await.is_err());
    assert_eq!(client.get(&url).send().await.unwrap().status(), 200);
    assert!(Tls::from_paths(certificate, key).await.is_err());
    handle.graceful_shutdown(Some(Duration::from_secs(1)));
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
