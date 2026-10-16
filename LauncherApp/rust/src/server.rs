use crate::{
    config::{self, Settings},
    relay::Relay,
    storage,
};
use anyhow::{Result, bail};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Extension, FromRequestParts, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use bytes::Bytes;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde_json::json;
use std::{
    collections::HashMap,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Connection {
    ip: IpAddr,
    secure: bool,
    port: u16,
    cancel: CancellationToken,
    _slot: Arc<ConnectionSlot>,
}
struct ConnectionSlot(Arc<AtomicUsize>);
impl ConnectionSlot {
    fn acquire(active: &Arc<AtomicUsize>, limit: usize) -> Option<Arc<Self>> {
        active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                if limit == 0 || count < limit {
                    count.checked_add(1)
                } else {
                    None
                }
            })
            .ok()?;
        Some(Arc::new(Self(active.clone())))
    }
}
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Client {
    seen: Instant,
    scheduled: Instant,
}

#[cfg(test)]
mod connection_limit_tests {
    use super::*;

    #[test]
    fn limits_release_only_after_last_owner_and_allow_unlimited() {
        let active = Arc::new(AtomicUsize::new(0));
        let slot = ConnectionSlot::acquire(&active, 1).unwrap();
        let retained = slot.clone();
        assert!(ConnectionSlot::acquire(&active, 1).is_none());
        drop(slot);
        assert!(ConnectionSlot::acquire(&active, 1).is_none());
        drop(retained);
        let slots: Vec<_> = (0..512)
            .map(|_| ConnectionSlot::acquire(&active, 512).unwrap())
            .collect();
        assert!(ConnectionSlot::acquire(&active, 512).is_none());
        let extra = ConnectionSlot::acquire(&active, 0).unwrap();
        drop((slots, extra));
        assert_eq!(active.load(Ordering::Relaxed), 0);
        assert!(ConnectionSlot::acquire(&active, usize::MAX).is_some());
    }
}
struct Limits {
    clients: Mutex<HashMap<IpAddr, Client>>,
    maximum: usize,
    rate: f64,
}
impl Limits {
    fn admit(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|_, c| {
            now.duration_since(c.seen) <= Duration::from_secs(120) || c.scheduled > now
        });
        if !clients.contains_key(&ip) && self.maximum > 0 && clients.len() >= self.maximum {
            return false;
        }
        clients
            .entry(ip)
            .and_modify(|c| c.seen = now)
            .or_insert(Client {
                seen: now,
                scheduled: now,
            });
        true
    }
    fn reserve(&self, ip: IpAddr, size: usize) -> Duration {
        if self.rate == 0. {
            return Duration::ZERO;
        }
        let now = Instant::now();
        let mut clients = self.clients.lock().unwrap();
        let c = clients.entry(ip).or_insert(Client {
            seen: now,
            scheduled: now,
        });
        c.seen = now;
        // Reject impractically small rates before Duration conversion in start().
        c.scheduled = c.scheduled.max(now) + Duration::from_secs_f64(size as f64 / self.rate);
        c.scheduled.saturating_duration_since(now)
    }
}
struct Rebind {
    port: u16,
    answer: oneshot::Sender<std::result::Result<(), String>>,
}
struct Shared {
    folder: PathBuf,
    root: PathBuf,
    settings: Mutex<Settings>,
    limits: Limits,
    saves: tokio::sync::Mutex<()>,
    log: tokio::sync::Mutex<()>,
    relay: Option<Relay>,
    token: String,
    port: AtomicU16,
    rebind: mpsc::Sender<Rebind>,
    certificate: Option<Vec<u8>>,
    fingerprint: String,
    stop: CancellationToken,
    allowed_hosts: Vec<String>,
    upgrades: tokio_util::task::TaskTracker,
}
pub struct Running {
    state: Arc<Shared>,
    task: Option<JoinHandle<()>>,
    pub settings: Settings,
    pub code: String,
    pub discovery_error: Option<String>,
}
impl Running {
    /// Replace the temporary origin after a launcher-owned Quick Tunnel is allocated.
    pub fn set_public_origin(&mut self, origin: &str) -> Result<()> {
        if self.settings.mode != "external" || self.settings.https {
            bail!("Tunnel requires a loopback HTTP backend");
        }
        let origin = config::origin(origin, true)?;
        let mut settings = self.settings.clone();
        settings.public_origin = origin;
        settings.save(&self.state.folder)?;
        *self.state.settings.lock().unwrap() = settings.clone();
        self.settings = settings;
        Ok(())
    }
    pub fn port(&self) -> u16 {
        self.state.port.load(Ordering::Relaxed)
    }
    pub fn url(&self) -> String {
        if !self.settings.public_origin.is_empty() {
            format!(
                "{}/launcher.html",
                self.settings.public_origin.trim_end_matches('/')
            )
        } else if self.settings.mode == "device" {
            format!(
                "{}://localhost:{}/launcher.html#local-launch={}",
                self.settings.scheme(),
                self.port(),
                self.state.token
            )
        } else {
            format!(
                "{}://localhost:{}/launcher.html",
                self.settings.scheme(),
                self.port()
            )
        }
    }
    pub fn share_url(&self) -> String {
        if !self.settings.public_origin.is_empty() {
            return format!(
                "{}/launcher.html",
                self.settings.public_origin.trim_end_matches('/')
            );
        }
        let host = config::addresses()
            .first()
            .map(ToString::to_string)
            .unwrap_or_else(|| "127.0.0.1".into());
        format!(
            "{}://{host}:{}/launcher.html",
            self.settings.scheme(),
            self.port()
        )
    }
    pub fn fingerprint(&self) -> &str {
        &self.state.fingerprint
    }
    pub fn request_stop(&self) {
        self.state.stop.cancel();
    }
    pub async fn stop(mut self) {
        self.state.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}

pub struct StartOptions {
    /// Restrict even LAN/public fixtures to loopback; never expands a production bind address.
    pub loopback_only: bool,
    pub advertise: bool,
}
impl Default for StartOptions {
    fn default() -> Self {
        Self {
            loopback_only: false,
            advertise: true,
        }
    }
}
pub async fn start(folder: &Path, settings: Settings, requested_code: &str) -> Result<Running> {
    start_with_options(folder, settings, requested_code, StartOptions::default()).await
}
pub async fn start_with_options(
    folder: &Path,
    mut settings: Settings,
    requested_code: &str,
    options: StartOptions,
) -> Result<Running> {
    let folder = config::resolve_folder(folder)?;
    if settings.mode == "lan"
        && (!settings.https || settings.tls_cert.is_empty() || settings.tls_key.is_empty())
    {
        let path = folder.clone();
        let generated = tokio::task::spawn_blocking(move || crate::tls::generate(&path)).await??;
        settings.tls_cert = generated.0;
        settings.tls_key = generated.1;
        settings.https = true;
    }
    if settings.mode == "external" {
        settings.public_origin = config::origin(&settings.public_origin, true)?;
    }
    settings.validate()?;
    if settings.speed_mib > 0. && settings.speed_mib < 0.000001 {
        bail!("Speed must be zero or at least 0.000001 MiB/s");
    }
    let tls = if settings.https {
        Some(crate::tls::load(
            Path::new(&settings.tls_cert),
            Path::new(&settings.tls_key),
        )?)
    } else {
        None
    };
    let (certificate, fingerprint) = if settings.mode == "lan"
        && Path::new(&settings.tls_cert)
            .file_name()
            .is_some_and(|n| n == "lan-server.pem")
    {
        let (der, fp) = crate::tls::public_root(Path::new(&settings.tls_cert))?;
        (Some(der), fp)
    } else {
        (None, String::new())
    };
    // Bind before persisting anything: a busy listener leaves the old launcher/settings intact.
    let bind_ip = if options.loopback_only {
        std::net::Ipv4Addr::LOCALHOST
    } else {
        settings.bind_ip()
    };
    let listener = TcpListener::bind((bind_ip, settings.port)).await?;
    crate::diagnostics::record(
        "Server",
        format!(
            "Listening on {}://{bind_ip}:{}; audience={}, multiplayer={}",
            settings.scheme(),
            settings.port,
            settings.mode,
            settings.multiplayer
        ),
    );
    let code = if settings.multiplayer {
        config::save_code(&folder, requested_code)?
    } else {
        requested_code.to_string()
    };
    settings.save(&folder)?;
    let stop = CancellationToken::new();
    let (rebind_tx, mut rebind_rx) = mpsc::channel::<Rebind>(1);
    let mut allowed_hosts = vec!["localhost".into(), "127.0.0.1".into()];
    allowed_hosts.extend(config::addresses().into_iter().map(|ip| ip.to_string()));
    let state = Arc::new(Shared {
        root: folder.join("mirror/playgta5.com"),
        folder,
        settings: Mutex::new(settings.clone()),
        limits: Limits {
            clients: Default::default(),
            maximum: settings.max_clients,
            rate: settings.speed_mib * 1048576.,
        },
        saves: Default::default(),
        log: Default::default(),
        relay: settings.multiplayer.then(|| Relay::new(code.clone())),
        token: config::random_token(),
        port: AtomicU16::new(settings.port),
        rebind: rebind_tx,
        certificate,
        fingerprint,
        stop: stop.clone(),
        allowed_hosts,
        upgrades: Default::default(),
    });
    let router = Router::new().fallback(handle).with_state(state.clone());
    let (port_tx, port_rx) = watch::channel(settings.port);
    let mut discovery_error = None;
    let discovery_task = if settings.mode == "lan" && options.advertise && !options.loopback_only {
        let fp = state.fingerprint.clone();
        let cancel = stop.clone();
        let secure = settings.https;
        match tokio::net::UdpSocket::bind("0.0.0.0:8014").await {
            Ok(socket) => Some(tokio::spawn(async move {
                crate::discovery::advertise(socket, port_rx, secure, fp, cancel).await
            })),
            Err(e) => {
                discovery_error = Some(e.to_string());
                crate::diagnostics::record("Discovery", format!("Unavailable: {e}"));
                None
            }
        }
    } else {
        None
    };
    let task_state = state.clone();
    let connection_limit = settings.connection_limit();
    let task = tokio::spawn(async move {
        let mut listener = listener;
        let slots = Arc::new(AtomicUsize::new(0));
        let mut connections = JoinSet::new();
        let mut generation = CancellationToken::new();
        loop {
            tokio::select! {
                biased;
                _=stop.cancelled()=>break,
                Some(command)=rebind_rx.recv()=>{
                    let result=async {
                        let saved=task_state.settings.lock().unwrap().clone();
                        let replacement=TcpListener::bind((bind_ip,command.port)).await.map_err(|e|e.to_string())?;
                        let mut updated=saved;updated.port=command.port;
                        updated.save(&task_state.folder).map_err(|e|e.to_string())?;
                        *task_state.settings.lock().unwrap()=updated;
                        listener=replacement;task_state.port.store(command.port,Ordering::Relaxed);let _=port_tx.send(command.port);
                        crate::diagnostics::record("Server",format!("Listener changed to port {}",command.port));
                        let old=std::mem::replace(&mut generation,CancellationToken::new());
                        connections.spawn(async move {tokio::time::sleep(Duration::from_secs(2)).await;old.cancel();});
                        Ok(())
                    }.await;
                    let _=command.answer.send(result);
                }
                accepted=listener.accept()=>{
                    let Ok((stream,peer))=accepted else {continue;};
                    let Some(slot)=ConnectionSlot::acquire(&slots, connection_limit) else {
                        // No task is created beyond the socket ceiling.
                        let _=stream.try_write(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\nRetry-After: 1\r\n\r\n");continue;
                    };
                    let router=router.clone();let state=task_state.clone();let tls=tls.clone();let host_stop=stop.clone();let old_stop=generation.clone();
                    connections.spawn(async move {
                        let port=stream.local_addr().map_or(state.port.load(Ordering::Relaxed),|a|a.port());
                        let context=Connection{ip:peer.ip(),secure:false,port,cancel:old_stop.clone(),_slot:slot};
                        tokio::select! {
                            _=host_stop.cancelled()=>{},
                            _=old_stop.cancelled()=>{},
                            _=connection(stream,state,router,tls,context)=>{},
                        }
                    });
                }
                Some(_)=connections.join_next(),if !connections.is_empty()=>{},
            }
        }
        generation.cancel();
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        task_state.upgrades.close();
        task_state.upgrades.wait().await;
        if let Some(task) = discovery_task {
            let _ = task.await;
        }
        crate::diagnostics::record(
            "Server",
            "Stopped; owned connections and relay sessions closed",
        );
    });
    // Allow UDP bind to report failure without blocking startup on discovery.
    Ok(Running {
        state,
        task: Some(task),
        settings,
        code,
        discovery_error,
    })
}
async fn connection(
    stream: TcpStream,
    state: Arc<Shared>,
    router: Router,
    tls: Option<tokio_rustls::TlsAcceptor>,
    mut context: Connection,
) {
    if let Some(tls) = tls {
        let mut first = [0u8; 1];
        if !matches!(
            tokio::time::timeout(Duration::from_secs(10), stream.peek(&mut first)).await,
            Ok(Ok(1))
        ) {
            return;
        }
        if state.certificate.is_some() && first[0].is_ascii_uppercase() {
            let _ = serve(stream, router, context).await;
        } else {
            match tokio::time::timeout(Duration::from_secs(10), tls.accept(stream)).await {
                Ok(Ok(secure)) => {
                    context.secure = true;
                    let _ = serve(secure, router, context).await;
                }
                Ok(Err(error)) => crate::diagnostics::record(
                    "TLS",
                    format!("Handshake failed from {}: {error}", context.ip),
                ),
                Err(_) => crate::diagnostics::record(
                    "TLS",
                    format!("Handshake timed out from {}", context.ip),
                ),
            }
        }
    } else {
        let _ = serve(stream, router, context).await;
    }
}
async fn serve<I>(
    io: I,
    router: Router,
    connection: Connection,
) -> std::result::Result<(), hyper::Error>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = TowerToHyperService::new(router.layer(Extension(connection)));
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .max_buf_size(32768);
    builder
        .serve_connection(TokioIo::new(io), service)
        .with_upgrades()
        .await
}
fn response(status: StatusCode, body: impl Into<Body>, mime: &str) -> Response {
    let mut r = Response::new(body.into());
    *r.status_mut() = status;
    r.headers_mut()
        .insert("Content-Type", HeaderValue::from_str(mime).unwrap());
    r
}
fn failure(status: StatusCode, error: impl ToString) -> Response {
    response(status, error.to_string(), "text/plain; charset=utf-8")
}
fn isolation(r: &mut Response) {
    for (key, value) in [
        ("Cross-Origin-Opener-Policy", "same-origin"),
        ("Cross-Origin-Embedder-Policy", "require-corp"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
        ("Accept-Ranges", "bytes"),
    ] {
        r.headers_mut().insert(key, HeaderValue::from_static(value));
    }
}
fn header<'a>(h: &'a HeaderMap, key: &str) -> &'a str {
    h.get(key).and_then(|h| h.to_str().ok()).unwrap_or("")
}
async fn handle(
    State(state): State<Arc<Shared>>,
    Extension(conn): Extension<Connection>,
    req: Request<Body>,
) -> Response {
    let request_label = format!("{} {} {}", conn.ip, req.method(), req.uri().path());
    let settings = state.settings.lock().unwrap().clone();
    let mut r = if settings.mode == "lan" && !config::private(conn.ip) {
        failure(StatusCode::FORBIDDEN, "LAN mode accepts private peers only")
    } else if !state.limits.admit(conn.ip) {
        let mut r = failure(StatusCode::SERVICE_UNAVAILABLE, "Maximum clients reached");
        r.headers_mut()
            .insert("Retry-After", HeaderValue::from_static("120"));
        r
    } else {
        match tokio::time::timeout(
            Duration::from_secs(30),
            route(state.clone(), conn.clone(), settings, req),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                crate::diagnostics::record("Error", format!("{request_label}: {e:#}"));
                failure(StatusCode::BAD_REQUEST, e)
            }
            Err(_) => failure(StatusCode::REQUEST_TIMEOUT, "Request timed out"),
        }
    };
    crate::diagnostics::record("HTTP", format!("{request_label} -> {}", r.status()));
    isolation(&mut r);
    // Throttle body chunks across every request from the same IP, without buffering asset files.
    if state.limits.rate > 0. && r.status() != StatusCode::SWITCHING_PROTOCOLS {
        let (parts, body) = r.into_parts();
        let stream = async_stream::stream! {
            let mut frames=body.into_data_stream();
            use futures_util::StreamExt;
            while let Some(chunk)=frames.next().await {
                let chunk=match chunk {Ok(c)=>c,Err(e)=>{yield Err::<Bytes,std::io::Error>(std::io::Error::other(e));return;}};
                for piece in chunk.chunks(16384) {
                    let delay=state.limits.reserve(conn.ip,piece.len());
                    tokio::select! {_=state.stop.cancelled()=>{return;},_=tokio::time::sleep(delay)=>{}}
                    yield Ok::<Bytes,std::io::Error>(Bytes::copy_from_slice(piece));
                }
            }
        };
        r = Response::from_parts(parts, Body::from_stream(stream));
    }
    r
}
async fn route(
    state: Arc<Shared>,
    conn: Connection,
    settings: Settings,
    req: Request<Body>,
) -> Result<Response> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let head = method == Method::HEAD;
    let plaintext = settings.https && !conn.secure;
    if plaintext && method != Method::GET && !head {
        return Ok(failure(StatusCode::NOT_FOUND, "Not found"));
    }
    if path == "/__lan/certificate.cer" || (!plaintext && path == "/__lan/certificate-info") {
        if req.uri().query().is_some() || state.certificate.is_none() {
            return Ok(failure(StatusCode::NOT_FOUND, "Not found"));
        }
        if method != Method::GET && !head {
            return Ok(failure(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET or HEAD required",
            ));
        }
        let info = path.ends_with("-info");
        let body = if info {
            serde_json::to_vec(&json!({"url":"/__lan/certificate.cer","sha256":state.fingerprint}))?
        } else {
            state.certificate.clone().unwrap()
        };
        let size = body.len();
        let mut r = response(
            StatusCode::OK,
            if head { Vec::new() } else { body },
            if info {
                "application/json"
            } else {
                "application/pkix-cert"
            },
        );
        r.headers_mut().insert("Content-Length", size.into());
        r.headers_mut()
            .insert("Cache-Control", HeaderValue::from_static("no-store"));
        if !info {
            r.headers_mut().insert(
                "Content-Disposition",
                HeaderValue::from_static("attachment; filename=\"lan-root-ca.cer\""),
            );
        }
        if plaintext {
            r.headers_mut()
                .insert("Connection", HeaderValue::from_static("close"));
        }
        return Ok(r);
    }
    if plaintext {
        let host = settings.bind_ip();
        let host = if host.is_unspecified() {
            config::addresses()
                .first()
                .map(ToString::to_string)
                .unwrap_or_else(|| "127.0.0.1".into())
        } else {
            host.to_string()
        };
        let mut r = response(StatusCode::FOUND, Body::empty(), "text/plain");
        r.headers_mut().insert(
            "Location",
            format!("https://{host}:{}/launcher.html", conn.port).parse()?,
        );
        r.headers_mut()
            .insert("Connection", HeaderValue::from_static("close"));
        return Ok(r);
    }
    if path == "/relay" {
        if method != Method::GET || req.uri().query().is_some() {
            return Ok(failure(StatusCode::BAD_REQUEST, "Invalid relay URL"));
        }
        if !base64::engine::general_purpose::STANDARD
            .decode(header(req.headers(), "Sec-WebSocket-Key"))
            .is_ok_and(|key| key.len() == 16)
        {
            return Ok(failure(StatusCode::BAD_REQUEST, "Invalid WebSocket key"));
        }
        let supplied = header(req.headers(), "Origin");
        let host = header(req.headers(), "Host");
        let expected = if settings.public_origin.is_empty() {
            format!("{}://{host}", settings.scheme())
        } else {
            settings.public_origin.clone()
        };
        let allowed = if settings.public_origin.is_empty() {
            state
                .allowed_hosts
                .iter()
                .any(|h| host == format!("{h}:{}", conn.port))
        } else {
            true
        };
        if !allowed || supplied != expected || config::origin(supplied, false).is_err() {
            return Ok(failure(
                StatusCode::FORBIDDEN,
                "WebSocket origin does not match game website",
            ));
        }
        if req.headers().contains_key("Transfer-Encoding")
            || !["", "0"].contains(&header(req.headers(), "Content-Length"))
        {
            return Ok(failure(StatusCode::BAD_REQUEST, "Invalid WebSocket body"));
        }
        let Some(relay) = state.relay.clone() else {
            return Ok(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Multiplayer is disabled",
            ));
        };
        let (mut parts, _) = req.into_parts();
        let ws = WebSocketUpgrade::from_request_parts(&mut parts, &())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let stop = state.stop.clone();
        let limits_state = state.clone();
        let tracker = state.upgrades.clone();
        // Upgrade tasks are owned through the cancellation token, including already upgraded connections.
        return Ok(ws.max_message_size(2048).max_frame_size(2048).on_upgrade(move |socket|async move {
            let task=tracker.spawn(async move {
                let _slot=conn._slot;
                let refresh=async move {
                    loop {tokio::time::sleep(Duration::from_secs(30)).await;if !limits_state.limits.admit(conn.ip) {return;}}
                };
                tokio::select! {_=stop.cancelled()=>{},_=conn.cancel.cancelled()=>{},_=relay.handle(socket)=>{},_=refresh=>{}}
            });let _=task.await;
        }).into_response());
    }
    if path == "/__local/port" && method == Method::POST {
        if settings.mode != "device" || !settings.public_origin.is_empty() || !conn.ip.is_loopback()
        {
            return Ok(failure(StatusCode::NOT_FOUND, "Not found"));
        }
        let expected = format!("{}://localhost:{}", settings.scheme(), conn.port);
        if header(req.headers(), "Host") != format!("localhost:{}", conn.port)
            || header(req.headers(), "Origin") != expected
            || !bool::from(
                header(req.headers(), "X-Launcher-Token")
                    .as_bytes()
                    .ct_eq(state.token.as_bytes()),
            )
        {
            return Ok(failure(
                StatusCode::FORBIDDEN,
                "Invalid launcher capability",
            ));
        }
        let bytes = to_bytes(req.into_body(), 256).await?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        let port = v["port"]
            .as_u64()
            .filter(|p| (1024..=65535).contains(p))
            .ok_or_else(|| anyhow::anyhow!("Invalid port"))? as u16;
        if port != state.port.load(Ordering::Relaxed) {
            let (answer, receiver) = oneshot::channel();
            state
                .rebind
                .try_send(Rebind { port, answer })
                .map_err(|_| anyhow::anyhow!("Port change already pending"))?;
            receiver.await?.map_err(anyhow::Error::msg)?;
        }
        return Ok(response(StatusCode::OK,json!({"url":format!("{}://localhost:{port}/launcher.html#local-launch={}",settings.scheme(),state.token)}).to_string(),"application/json"));
    }
    if path.starts_with("/userdata/") {
        let _lock = state.saves.lock().await;
        match method {
            Method::PUT => {
                let mtime = header(req.headers(), "X-Userdata-Mtime").parse().ok();
                let body = to_bytes(req.into_body(), storage::MAX_FILE).await?;
                let folder = state.folder.clone();
                tokio::task::spawn_blocking(move || storage::put(&folder, &path, &body, mtime))
                    .await??;
                return Ok(response(
                    StatusCode::NO_CONTENT,
                    Body::empty(),
                    "text/plain",
                ));
            }
            Method::DELETE => {
                let folder = state.folder.clone();
                tokio::task::spawn_blocking(move || storage::delete(&folder, &path)).await??;
                return Ok(response(
                    StatusCode::NO_CONTENT,
                    Body::empty(),
                    "text/plain",
                ));
            }
            Method::GET => {
                let parts: Vec<_> = path.split('/').collect();
                if parts.len() != 3 {
                    bail!("Invalid restore URL");
                }
                let id = parts[2].to_string();
                let folder = state.folder.clone();
                let result =
                    tokio::task::spawn_blocking(move || storage::restore(&folder, &id)).await??;
                return Ok(response(
                    StatusCode::OK,
                    result.to_string(),
                    "application/json",
                ));
            }
            _ => {
                return Ok(failure(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "Unsupported userdata method",
                ));
            }
        }
    }
    if path == "/log" && method == Method::POST {
        let body = to_bytes(req.into_body(), 256 * 1024).await?;
        let text = String::from_utf8_lossy(&body).replace('\0', "");
        crate::diagnostics::record("Game", &text);
        let _lock = state.log.lock().await;
        let path = state.folder.join("server.log");
        if path.metadata().is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
            let rotated = state.folder.join("server.log.1");
            if rotated.exists() {
                std::fs::remove_file(&rotated)?;
            }
            std::fs::rename(&path, rotated)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        return Ok(response(
            StatusCode::NO_CONTENT,
            Body::empty(),
            "text/plain",
        ));
    }
    if path == "/data/batch" && method == Method::POST {
        let gzip = req.uri().query().is_some_and(|q| {
            url::form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "gz" && v == "1")
        });
        let body = to_bytes(req.into_body(), 1024 * 1024).await?;
        let runs: Vec<(String, u64, u64)> = serde_json::from_slice(&body)?;
        if runs.len() > 1000 {
            bail!("Too many batch runs");
        }
        let root = state.root.join("data");
        let mut selected = Vec::new();
        let mut total = 0u64;
        for (name, start, end) in runs {
            if end < start {
                bail!("Invalid range");
            }
            let file = storage::safe_target(&root, &name)?;
            let size = tokio::fs::metadata(&file).await?.len();
            let length = end.saturating_add(1).min(size).saturating_sub(start);
            total = total
                .checked_add(length)
                .ok_or_else(|| anyhow::anyhow!("Batch overflow"))?;
            if total > 64 * 1024 * 1024 {
                bail!("Batch exceeds 64 MiB");
            }
            selected.push((file, start, length));
        }
        let lengths = selected
            .iter()
            .map(|(_, _, n)| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut body = Vec::with_capacity(total as usize);
        for (path, start, length) in selected {
            let mut file = tokio::fs::File::open(path).await?;
            file.seek(std::io::SeekFrom::Start(start)).await?;
            file.take(length).read_to_end(&mut body).await?;
        }
        if gzip {
            body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(&body)?;
                Ok(encoder.finish()?)
            })
            .await??;
        }
        let length = body.len();
        let mut r = response(StatusCode::OK, body, "application/octet-stream");
        r.headers_mut().insert("Content-Length", length.into());
        r.headers_mut().insert("X-Run-Lengths", lengths.parse()?);
        if gzip {
            r.headers_mut()
                .insert("Content-Encoding", HeaderValue::from_static("gzip"));
        }
        return Ok(r);
    }
    if method != Method::GET && !head {
        return Ok(failure(StatusCode::NOT_FOUND, "Not found"));
    }
    let decoded = percent_encoding::percent_decode_str(&path).decode_utf8()?;
    let rel = decoded.trim_start_matches('/');
    let mut file_path = if rel.is_empty() {
        state.root.clone()
    } else {
        storage::safe_target(&state.root, rel)?
    };
    if file_path.is_dir() {
        file_path = storage::safe_target(
            &state.root,
            &format!(
                "{}index.html",
                if rel.is_empty() {
                    String::new()
                } else {
                    format!("{}/", rel.trim_end_matches('/'))
                }
            ),
        )?;
    }
    if !file_path.is_file() {
        return Ok(failure(StatusCode::NOT_FOUND, "Not found"));
    }
    let mut file = tokio::fs::File::open(&file_path).await?;
    let size = file.metadata().await?.len();
    let range = header(req.headers(), "Range");
    let (start, end, status) = if range.is_empty() {
        (0, size, StatusCode::OK)
    } else {
        match parse_range(range, size) {
            Some((start, length)) => (start, start + length, StatusCode::PARTIAL_CONTENT),
            None => {
                let mut r = response(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    Body::empty(),
                    "text/plain",
                );
                r.headers_mut()
                    .insert("Content-Range", format!("bytes */{size}").parse()?);
                return Ok(r);
            }
        }
    };
    let length = end - start;
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let body = if head {
        Body::empty()
    } else {
        Body::from_stream(async_stream::stream! {
            let mut remaining=length;let mut buffer=vec![0;64*1024];
            while remaining>0 {let limit=(remaining as usize).min(buffer.len());let n=match file.read(&mut buffer[..limit]).await {Ok(n)=>n,Err(e)=>{yield Err::<Bytes,std::io::Error>(e);return;}};if n==0 {yield Err::<Bytes,std::io::Error>(std::io::Error::new(std::io::ErrorKind::UnexpectedEof,"Asset truncated"));return;}remaining-=n as u64;yield Ok::<Bytes,std::io::Error>(Bytes::copy_from_slice(&buffer[..n]));}
        })
    };
    let mime = match file_path.extension().and_then(|p| p.to_str()) {
        Some("wasm") => "application/wasm".to_string(),
        Some("js") => "text/javascript".to_string(),
        _ => mime_guess::from_path(&file_path)
            .first_or_octet_stream()
            .to_string(),
    };
    let mut r = response(status, body, &mime);
    r.headers_mut().insert("Content-Length", length.into());
    if status == StatusCode::PARTIAL_CONTENT {
        r.headers_mut().insert(
            "Content-Range",
            format!("bytes {start}-{}/{size}", end - 1).parse()?,
        );
    }
    Ok(r)
}
pub fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let range = value.strip_prefix("bytes=")?;
    let (a, b) = range.split_once('-')?;
    if size == 0 {
        return None;
    }
    if a.is_empty() {
        let suffix = b.parse::<u64>().ok()?.min(size);
        return (suffix > 0).then_some((size - suffix, suffix));
    }
    let start = a.parse::<u64>().ok()?;
    let end = if b.is_empty() {
        size - 1
    } else {
        b.parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && end >= start).then(|| (start, end - start + 1))
}
