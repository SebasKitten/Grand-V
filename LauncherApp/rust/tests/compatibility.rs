use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use gta_browser_launcher::{
    config::{self, Settings},
    server::{self, Running},
    storage, tls,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    Connector, connect_async, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest},
};

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("mirror/playgta5.com");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("launcher.html"), b"<h1>Fixture</h1>").unwrap();
    std::fs::write(root.join("data/test.bin"), b"0123456789").unwrap();
    std::fs::write(root.join("test.wasm"), b"wasm").unwrap();
    temp
}
fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn settings() -> Settings {
    Settings {
        port: port(),
        ..Default::default()
    }
}
async fn start(folder: &std::path::Path, settings: Settings, code: &str) -> Running {
    server::start_with_options(
        folder,
        settings,
        code,
        server::StartOptions {
            loopback_only: true,
            advertise: false,
        },
    )
    .await
    .unwrap()
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn base(host: &Running) -> String {
    format!("http://localhost:{}", host.port())
}
async fn ws(
    host: &Running,
    origin: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut req = format!("ws://localhost:{}/relay", host.port())
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("Origin", origin.parse().unwrap());
    connect_async(req).await.unwrap().0
}
async fn send_json<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, v: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}
async fn receive<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Message
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
fn hello(kind: &str, key: &str) -> Value {
    json!({"type":kind,"protocol":1,"key":key,"room":"friends","build":"fixture-1","map":"gta5"})
}

#[tokio::test]
async fn tunnel_origin_updates_share_links_and_websocket_checks() {
    let temp = fixture();
    let mut settings = settings();
    settings.mode = "external".into();
    settings.public_origin = "https://pending.invalid".into();
    settings.cloudflare_tunnel = true;
    settings.multiplayer = true;
    let mut host = start(temp.path(), settings, "tunnel-code").await;
    let origin = "https://fixture.trycloudflare.com";
    host.set_public_origin(origin).unwrap();
    assert_eq!(host.url(), format!("{origin}/launcher.html"));
    assert_eq!(host.share_url(), host.url());
    assert_eq!(host.settings.bind_ip(), std::net::Ipv4Addr::LOCALHOST);
    let saved = Settings::load(temp.path()).unwrap();
    assert_eq!(saved.public_origin, origin);
    assert!(saved.cloudflare_tunnel);
    let mut socket = ws(&host, origin).await;
    send_json(&mut socket, hello("check", "tunnel-code")).await;
    assert!(receive(&mut socket).await.is_text());
    socket.close(None).await.unwrap();
    let mut req = format!("ws://localhost:{}/relay", host.port())
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", "https://pending.invalid".parse().unwrap());
    assert!(connect_async(req).await.is_err());
    host.stop().await;
}

#[test]
fn connection_limit_settings() {
    let mut settings: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings.connection_limit(), 0);
    settings.max_connections = 4096;
    settings.https = true;
    assert_eq!(settings.connection_limit(), 4096);
    settings.cloudflare_tunnel = true;
    settings.mode = "external".into();
    assert_eq!(settings.connection_limit(), 200);
    let temp = tempfile::tempdir().unwrap();
    settings.save(temp.path()).unwrap();
    let mut saved = Settings::load(temp.path()).unwrap();
    assert_eq!(saved.connection_limit(), 200);
    saved.cloudflare_tunnel = false;
    assert_eq!(saved.connection_limit(), 4096);
    saved.max_connections = 0;
    assert_eq!(saved.connection_limit(), 0);
}

#[test]
fn validation_and_existing_formats() {
    assert!(config::access_code("  GTA5  ").unwrap() == "GTA5");
    assert!(config::access_code("a\nbc").is_err());
    assert!(storage::valid_path("/userdata/profiles/0/SGTA50000"));
    for p in [
        "/userdata/../escape",
        "/userdata/CON.bin",
        "/userdata/test. ",
        "/userdata/a\\b",
        "/userdata/a:b",
        "/userdata/.tombstones.json",
    ] {
        assert!(!storage::valid_path(p), "{p}");
    }
    assert!(config::origin("https://example.com/", true).is_ok());
    assert!(config::origin("https://user@example.com", true).is_err());
    assert_eq!(
        config::join_url("https://example.com").unwrap(),
        "https://example.com/launcher.html"
    );
    for (r, want) in [
        ("bytes=1-3", Some((1, 3))),
        ("bytes=-3", Some((7, 3))),
        ("bytes=7-", Some((7, 3))),
        ("bytes=11-12", None),
        ("bytes=0-1,3-4", None),
        ("bytes=-0", None),
    ] {
        assert_eq!(server::parse_range(r, 10), want);
    }
    assert_eq!(server::parse_range("bytes=0-0", 0), None);
    let temp = fixture();
    std::fs::write(
        temp.path().join("launcher-settings.json"),
        r#"{"port":8123,"mode":"device","future_option":42}"#,
    )
    .unwrap();
    let s = Settings::load(temp.path()).unwrap();
    s.save(temp.path()).unwrap();
    let v: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("launcher-settings.json")).unwrap())
            .unwrap();
    assert_eq!(v["future_option"], 42);
    config::save_code(temp.path(), "  GTA5 ").unwrap();
    assert_eq!(config::load_code(temp.path()).unwrap(), "GTA5");
}
#[tokio::test]
async fn ranges_batch_gzip_and_headers() {
    let temp = fixture();
    let host = start(temp.path(), settings(), "").await;
    let c = client();
    let url = base(&host);
    let r = c
        .get(format!("{url}/data/test.bin"))
        .header("Range", "bytes=2-5")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["Content-Range"], "bytes 2-5/10");
    assert_eq!(r.headers()["Cross-Origin-Opener-Policy"], "same-origin");
    assert_eq!(r.headers()["Cross-Origin-Embedder-Policy"], "require-corp");
    assert_eq!(r.bytes().await.unwrap(), "2345");
    let r = c
        .head(format!("{url}/data/test.bin"))
        .header("Range", "bytes=-3")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["Content-Length"], "3");
    assert!(r.bytes().await.unwrap().is_empty());
    let r = c
        .get(format!("{url}/data/test.bin"))
        .header("Range", "bytes=10-")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["Content-Range"], "bytes */10");
    let r = c.get(format!("{url}/test.wasm")).send().await.unwrap();
    assert_eq!(r.headers()["Content-Type"], "application/wasm");
    let r = c
        .post(format!("{url}/data/batch?gz=1"))
        .json(&json!([["test.bin", 1, 2], ["test.bin", 7, 20]]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["X-Run-Lengths"], "2,3");
    assert_eq!(r.headers()["Content-Encoding"], "gzip");
    let bytes = r.bytes().await.unwrap();
    let mut reader = flate2::read::GzDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut out).unwrap();
    assert_eq!(out, b"12789");
    assert_eq!(
        c.post(format!("{url}/data/batch"))
            .json(&json!([["../../launcher-settings.json", 0, 10]]))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        c.post(format!("{url}/log"))
            .body("Fixture log")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("server.log")).unwrap(),
        "Fixture log\n"
    );
    host.stop().await;
}
#[tokio::test]
async fn save_restore_delete_rewrite_and_limits() {
    let temp = fixture();
    let mut s = settings();
    let host = start(temp.path(), s.clone(), "").await;
    let c = client();
    let url = base(&host);
    let profile = "a".repeat(32);
    let path = format!(
        "/userdata/{profile}/{}",
        URL_SAFE_NO_PAD.encode("/userdata/profiles/0/SGTA50000")
    );
    assert_eq!(
        c.put(format!("{url}{path}"))
            .header("X-Userdata-Mtime", "1700000000123")
            .body("save bytes")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let result: Value = c
        .get(format!("{url}/userdata/{profile}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["files"][0]["data"], "c2F2ZSBieXRlcw==");
    assert_eq!(result["files"][0]["mtime"], 1700000000123u64);
    assert_eq!(
        c.delete(format!("{url}{path}"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    host.stop().await;
    s.port = port();
    let host = start(temp.path(), s, "").await;
    let url = base(&host);
    let result: Value = c
        .get(format!("{url}/userdata/{profile}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(result["files"].as_array().unwrap().is_empty());
    assert!(result["tombstones"][0]["mtime"].as_u64().unwrap() > 0);
    assert_eq!(
        c.put(format!("{url}{path}"))
            .body("new bytes")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let result: Value = c
        .get(format!("{url}/userdata/{profile}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(result["tombstones"].as_array().unwrap().is_empty());
    let bad = format!(
        "{url}/userdata/{profile}/{}",
        URL_SAFE_NO_PAD.encode("/userdata/../outside")
    );
    assert_eq!(c.put(bad).body("bad").send().await.unwrap().status(), 400);
    assert_eq!(
        c.put(format!("{url}{path}"))
            .body(vec![0; storage::MAX_FILE + 1])
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    host.stop().await;
}
#[tokio::test]
async fn capability_port_change_preserves_busy_listener_and_shutdown() {
    let temp = fixture();
    let host = start(temp.path(), settings(), "").await;
    let c = client();
    let url = base(&host);
    let token = host
        .url()
        .split("local-launch=")
        .nth(1)
        .unwrap()
        .to_string();
    let endpoint = format!("{url}/__local/port");
    assert_eq!(
        c.post(&endpoint)
            .json(&json!({"port":port()}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    assert_eq!(
        c.post(&endpoint)
            .header("Origin", &url)
            .header("X-Launcher-Token", &token)
            .json(&json!({"port":busy_port}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        c.get(format!("{url}/launcher.html"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let replacement = port();
    let result: Value = c
        .post(&endpoint)
        .header("Origin", &url)
        .header("X-Launcher-Token", token)
        .json(&json!({"port":replacement}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        result["url"]
            .as_str()
            .unwrap()
            .contains(&replacement.to_string())
    );
    assert_eq!(host.port(), replacement);
    assert_eq!(Settings::load(temp.path()).unwrap().port, replacement);
    assert_eq!(
        c.get(format!("{}/launcher.html", base(&host)))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let old_port = url.rsplit(':').next().unwrap().parse::<u16>().unwrap();
    host.stop().await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", replacement))
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", old_port))
            .await
            .is_err()
    );
    assert!(busy.local_addr().is_ok());
}
#[tokio::test]
async fn aggregate_bandwidth_and_ip_admission() {
    let temp = fixture();
    std::fs::write(
        temp.path().join("mirror/playgta5.com/payload.bin"),
        vec![0; 65536],
    )
    .unwrap();
    let mut s = settings();
    s.speed_mib = 0.125;
    s.max_clients = 1;
    let host = start(temp.path(), s, "").await;
    let url = format!("http://127.0.0.1:{}/payload.bin", host.port());
    let c = client();
    let before = Instant::now();
    let fetch = || async {
        c.get(&url)
            .header("Range", "bytes=0-32767")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    };
    let (a, b) = tokio::join!(fetch(), fetch());
    assert_eq!(a.len() + b.len(), 65536);
    assert!(before.elapsed() >= Duration::from_millis(450));
    let other = reqwest::Client::builder()
        .no_proxy()
        .local_address("127.0.0.2".parse::<std::net::IpAddr>().unwrap())
        .build()
        .unwrap();
    assert_eq!(other.get(&url).send().await.unwrap().status(), 503);
    host.stop().await;
}
#[tokio::test]
async fn relay_preflight_capacity_pose_identity_and_cleanup() {
    let temp = fixture();
    let mut s = settings();
    s.multiplayer = true;
    let host = start(temp.path(), s, "GTA5").await;
    let origin = base(&host);
    let mut probe = ws(&host, &origin).await;
    send_json(&mut probe, hello("check", "GTA5")).await;
    let reply: Value = serde_json::from_str(receive(&mut probe).await.to_text().unwrap()).unwrap();
    assert_eq!(reply, json!({"type":"checked","players":0,"capacity":2}));
    let mut bad = ws(&host, &origin).await;
    send_json(&mut bad, hello("check", "gta5")).await;
    assert!(
        matches!(receive(&mut bad).await,Message::Close(Some(c)) if u16::from(c.code)==1008 && c.reason=="Join key rejected")
    );
    let mut a = ws(&host, &origin).await;
    send_json(&mut a, hello("join", "GTA5")).await;
    let welcome: Value = serde_json::from_str(receive(&mut a).await.to_text().unwrap()).unwrap();
    let id = welcome["id"].clone();
    assert_eq!(welcome["type"], "welcome");
    let mut b = ws(&host, &origin).await;
    send_json(&mut b, hello("join", "GTA5")).await;
    let welcome: Value = serde_json::from_str(receive(&mut b).await.to_text().unwrap()).unwrap();
    assert_eq!(welcome["peers"], json!([id]));
    assert!(receive(&mut a).await.to_text().unwrap().contains("joined"));
    let mut full = ws(&host, &origin).await;
    send_json(&mut full, hello("check", "GTA5")).await;
    assert!(matches!(receive(&mut full).await,Message::Close(Some(c)) if c.reason=="Room full"));
    send_json(&mut a,json!({"type":"pose","seq":1,"model":123,"position":[1,2,3],"rotation":[0,90,0],"id":"forged"})).await;
    let pose: Value = serde_json::from_str(receive(&mut b).await.to_text().unwrap()).unwrap();
    assert_eq!(pose["id"], id);
    assert_eq!(pose["seq"], 1);
    send_json(
        &mut a,
        json!({"type":"pose","seq":2,"model":123,"position":[99999,2,3],"rotation":[0,0,0]}),
    )
    .await;
    assert!(matches!(receive(&mut a).await,Message::Close(Some(c)) if u16::from(c.code)==1008));
    assert!(receive(&mut b).await.to_text().unwrap().contains("left"));
    host.stop().await;
    assert!(!matches!(
        tokio::time::timeout(Duration::from_secs(2), b.next()).await,
        Err(_)
    ));
}
#[tokio::test]
async fn origin_rejection_and_public_proxy_configuration() {
    let temp = fixture();
    let mut s = settings();
    s.mode = "external".into();
    s.public_origin = "https://school.example".into();
    s.multiplayer = true;
    let host = start(temp.path(), s, "GTA5").await;
    assert_eq!(host.url(), "https://school.example/launcher.html");
    let mut req = format!("ws://localhost:{}/relay", host.port())
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", "https://evil.example".parse().unwrap());
    assert!(connect_async(req).await.is_err());
    let mut good = ws(&host, "https://school.example").await;
    send_json(&mut good, hello("check", "GTA5")).await;
    assert!(
        receive(&mut good)
            .await
            .to_text()
            .unwrap()
            .contains("checked")
    );
    assert_eq!(
        client()
            .post(format!("{}/__local/port", base(&host)))
            .json(&json!({"port":port()}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    host.stop().await;
}
#[tokio::test]
async fn verified_https_wss_certificate_bootstrap_and_stable_root() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = fixture();
    let first = tls::generate(temp.path()).unwrap();
    let root_path = temp.path().join("lan-tls/lan-root-ca.cer");
    let original = std::fs::read(&root_path).unwrap();
    let key = std::fs::read(temp.path().join("lan-tls/lan-root-ca.key")).unwrap();
    tls::generate(temp.path()).unwrap();
    assert_eq!(original, std::fs::read(&root_path).unwrap());
    assert_eq!(
        key,
        std::fs::read(temp.path().join("lan-tls/lan-root-ca.key")).unwrap()
    );
    let mut s = settings();
    s.mode = "lan".into();
    s.https = true;
    s.tls_cert = first.0;
    s.tls_key = first.1;
    s.multiplayer = true;
    let leaf = std::fs::read(&s.tls_cert).unwrap();
    let host = start(temp.path(), s, "GTA5").await;
    assert_eq!(leaf, std::fs::read(&host.settings.tls_cert).unwrap());
    let c = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_der(&original).unwrap())
        .build()
        .unwrap();
    let url = format!("https://localhost:{}", host.port());
    assert_eq!(
        c.get(format!("{url}/launcher.html"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let r = c
        .get(format!("{url}/__lan/certificate.cer"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.bytes().await.unwrap().as_ref(), &original);
    let info: Value = c
        .get(format!("{url}/__lan/certificate-info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["sha256"], host.fingerprint());
    let r = client()
        .get(format!("{}/__lan/certificate.cer", base(&host)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["Connection"], "close");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &original);
    let raw = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    assert_eq!(
        raw.get(format!("{}/launcher.html", base(&host)))
            .send()
            .await
            .unwrap()
            .status(),
        302
    );
    assert_eq!(
        raw.post(format!("{}/data/batch", base(&host)))
            .body("[]")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(original.clone().into()).unwrap();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut request = format!("wss://localhost:{}/relay", host.port())
        .into_client_request()
        .unwrap();
    request.headers_mut().insert("Origin", url.parse().unwrap());
    let (mut socket, _) = connect_async_tls_with_config(
        request,
        None,
        false,
        Some(Connector::Rustls(Arc::new(config))),
    )
    .await
    .unwrap();
    send_json(&mut socket, hello("check", "GTA5")).await;
    assert!(
        receive(&mut socket)
            .await
            .to_text()
            .unwrap()
            .contains("checked")
    );
    host.stop().await;
}
#[test]
fn old_tombstone_list_is_preserved_as_timestamp_zero() {
    let temp = fixture();
    let id = "a".repeat(32);
    let root = temp.path().join("userdata").join(&id);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(".tombstones.json"), r#"["save.bin"]"#).unwrap();
    std::fs::write(root.join("save.bin"), "deleted file").unwrap();
    let v = storage::restore(temp.path(), &id).unwrap();
    assert_eq!(v["files"], json!([]));
    assert_eq!(
        v["tombstones"],
        json!([{"path":"/userdata/save.bin","mtime":0}])
    );
}
#[tokio::test]
async fn discovery_nonce_protocol_and_socket_cleanup() {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::watch::channel(8123u16);
    let stop = tokio_util::sync::CancellationToken::new();
    let cancel = stop.clone();
    let task = tokio::spawn(async move {
        gta_browser_launcher::discovery::advertise(socket, rx, true, "fingerprint".into(), cancel)
            .await
    });
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client
        .send_to(
            json!({"marker":"GTA5LAN1","nonce":"abcdefghijklmnop"})
                .to_string()
                .as_bytes(),
            ("127.0.0.1", port),
        )
        .await
        .unwrap();
    let mut buf = [0; 1024];
    let (n, _) = tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let v: Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["port"], 8123);
    assert_eq!(v["nonce"], "abcdefghijklmnop");
    assert_eq!(v["https"], true);
    assert!(v.get("key").is_none());
    tx.send(8124).unwrap();
    stop.cancel();
    task.await.unwrap().unwrap();
    assert!(
        tokio::net::UdpSocket::bind(("127.0.0.1", port))
            .await
            .is_ok()
    );
}
