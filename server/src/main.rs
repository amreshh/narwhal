//! Narwhal server: a small file API over a root directory, plus the static
//! WASM frontend.

mod error;
mod fsops;

use axum::{
    body::Body,
    extract::{multipart::Field, DefaultBodyLimit, Multipart, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use error::{ApiError, ApiResult};
use serde::Deserialize;
use narwhal_shared::{
    CreateReq, DeleteReq, Entry, Info, Listing, OpReport, Place, RenameReq, SizeReport,
    TransferReq,
};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::{
    compression::Compression,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

struct Config {
    root: PathBuf,
    root_name: String,
    read_only: bool,
    places: Vec<Place>,
    /// Host names the server answers to; empty = any.
    allowed_hosts: Vec<String>,
}

type AppState = Arc<Config>;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_bool(key: &str) -> bool {
    matches!(env(key).as_deref().map(str::to_lowercase).as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// `NARWHAL_PLACES="Documents=/docs,Media=/media"`
fn parse_places(raw: &str) -> Vec<Place> {
    raw.split(',')
        .filter_map(|item| {
            let (name, path) = item.split_once('=')?;
            let (name, path) = (name.trim(), path.trim());
            (!name.is_empty() && path.starts_with('/'))
                .then(|| Place { name: name.to_string(), path: path.to_string() })
        })
        .collect()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let root_raw = env("NARWHAL_ROOT").unwrap_or_else(|| "/data".into());
    let root = match Path::new(&root_raw).canonicalize() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("NARWHAL_ROOT {root_raw:?} is not usable: {e}");
            std::process::exit(1);
        }
    };
    let static_dir = PathBuf::from(env("NARWHAL_STATIC_DIR").unwrap_or_else(|| "./static".into()));
    let bind: SocketAddr = match env("NARWHAL_BIND").unwrap_or_else(|| "[::]:8080".into()).parse() {
        Ok(a) => a,
        Err(_) => {
            eprintln!("NARWHAL_BIND must be host:port, e.g. [::]:8080 or 0.0.0.0:8080");
            std::process::exit(1);
        }
    };
    let max_upload_mb: u64 = env("NARWHAL_MAX_UPLOAD_MB").and_then(|v| v.parse().ok()).unwrap_or(0);

    let cfg = Arc::new(Config {
        root,
        root_name: env("NARWHAL_ROOT_NAME").unwrap_or_else(|| "Storage".into()),
        read_only: env_bool("NARWHAL_READ_ONLY"),
        places: env("NARWHAL_PLACES").map(|p| parse_places(&p)).unwrap_or_default(),
        allowed_hosts: env("NARWHAL_ALLOWED_HOSTS")
            .map(|v| {
                v.split(',')
                    .map(|h| host_without_port(h.trim()).to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    });

    let listener = match listen(bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {bind}: {e}");
            std::process::exit(1);
        }
    };
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| bind.to_string());
    let stack = if local.starts_with('[') { "IPv4 + IPv6" } else { "IPv4 only" };

    tracing::info!(
        root = %cfg.root.display(),
        static_dir = %static_dir.display(),
        read_only = cfg.read_only,
        "starting narwhal on {local} ({stack})"
    );

    let body_limit = if max_upload_mb == 0 {
        DefaultBodyLimit::disable()
    } else {
        DefaultBodyLimit::max((max_upload_mb * 1024 * 1024) as usize)
    };

    let api = Router::new()
        .route("/api/info", get(info))
        .route("/api/list", get(list))
        .route("/api/download", get(download))
        .route("/api/search", get(search))
        .route("/api/size", get(size))
        .route("/api/mkdir", post(mkdir))
        .route("/api/touch", post(touch))
        .route("/api/rename", post(rename))
        .route("/api/delete", post(delete))
        .route("/api/transfer", post(transfer))
        .route("/api/upload", post(upload))
        .route("/api/{*rest}", any(|| async { ApiError::not_found("Unknown API endpoint") }))
        .route("/healthz", get(|| async { "ok" }))
        .layer(body_limit)
        .with_state(cfg.clone());

    // The SPA: hashed assets from trunk, index.html as fallback.
    let spa = Compression::new(
        ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html"))),
    );

    let app = api
        .fallback_service(spa)
        .layer(middleware::from_fn_with_state(cfg, guard))
        .layer(TraceLayer::new_for_http());

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Open the listening socket.
///
/// `[::]:PORT` (the default) means "every address": a dual-stack socket that
/// accepts both IPv4 and IPv6, so `localhost` works whichever family it
/// resolves to. Rootless Podman, for example, forwards a connection to the
/// container over the same family it arrived on, and an IPv4-only server then
/// shows up as "connection reset". If the host or pod has IPv6 disabled, it
/// falls back to IPv4 on the same port. Any other address is bound exactly.
fn listen(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};

    fn open(addr: SocketAddr, dual_stack: bool) -> std::io::Result<std::net::TcpListener> {
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        if dual_stack {
            socket.set_only_v6(false)?;
        }
        socket.set_reuse_address(true)?;
        socket.bind(&SockAddr::from(addr))?;
        socket.listen(1024)?;
        socket.set_nonblocking(true)?;
        Ok(socket.into())
    }

    let std_listener = match addr {
        SocketAddr::V6(v6) if v6.ip().is_unspecified() => open(addr, true).or_else(|e| {
            let v4 = SocketAddr::from(([0, 0, 0, 0], addr.port()));
            eprintln!("IPv6 is unavailable ({e}); listening on {v4} instead");
            open(v4, false)
        })?,
        _ => open(addr, false)?,
    };
    tokio::net::TcpListener::from_std(std_listener)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}

// ---------------------------------------------------------------------------
// Request guard
// ---------------------------------------------------------------------------

/// `example.com:8080` -> `example.com`, `[::1]:8080` -> `[::1]`.
fn host_without_port(host: &str) -> &str {
    if host.starts_with('[') {
        return host.find(']').map_or(host, |i| &host[..=i]);
    }
    match host.rsplit_once(':') {
        Some((h, port)) if !h.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

/// Decide whether a request may proceed.
///
/// - With `allowed_hosts` set, other host names are refused. This stops DNS
///   rebinding, where a malicious site points its own domain at this server.
/// - Writes (anything but GET/HEAD/OPTIONS) must come from this app's own
///   origin. This stops cross-site request forgery, e.g. a page elsewhere
///   posting a multipart upload with the user's cookies.
fn check_request(
    method: &Method,
    host: Option<&str>,
    headers: &HeaderMap,
    allowed_hosts: &[String],
) -> Result<(), &'static str> {
    if !allowed_hosts.is_empty() {
        let name = host.map(|h| host_without_port(h).to_ascii_lowercase()).unwrap_or_default();
        if !allowed_hosts.contains(&name) {
            return Err("Unknown host name");
        }
    }
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let header = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    // Modern browsers say where a request comes from.
    match header(header::HeaderName::from_static("sec-fetch-site")) {
        Some("same-origin" | "none") => return Ok(()),
        Some(_) => return Err("Cross-site requests are not allowed"),
        None => {}
    }
    // Older browsers: the Origin, if sent, must match the Host. Non-browser
    // clients (curl, scripts) send neither header and are let through.
    if let Some(origin) = header(header::ORIGIN) {
        let origin_host = origin.split_once("://").map(|(_, rest)| rest);
        if origin_host.is_none() || origin_host != host {
            return Err("Cross-site requests are not allowed");
        }
    }
    Ok(())
}

async fn guard(State(cfg): State<AppState>, req: Request, next: Next) -> Response {
    // Kubelet probes use the pod IP as host name.
    if req.uri().path() != "/healthz" {
        // HTTP/2 carries the host in the URI authority instead of a Host header.
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .or_else(|| req.uri().authority().map(|a| a.as_str()));
        if let Err(msg) = check_request(req.method(), host, req.headers(), &cfg.allowed_hosts) {
            tracing::warn!(method = %req.method(), uri = %req.uri(), host = ?host, "refused: {msg}");
            return ApiError::forbidden(msg).into_response();
        }
    }
    let mut res = next.run(req).await;
    // The app must not be framed by other sites (clickjacking).
    res.headers_mut()
        .entry(header::X_FRAME_OPTIONS)
        .or_insert(HeaderValue::from_static("DENY"));
    res
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn root_path() -> String {
    "/".into()
}

#[derive(Deserialize)]
struct PathQuery {
    #[serde(default = "root_path")]
    path: String,
}

#[derive(Deserialize)]
struct DownloadQuery {
    path: String,
    #[serde(default)]
    inline: bool,
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default = "root_path")]
    path: String,
    q: String,
    limit: Option<usize>,
}

async fn blocking<T, F>(f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ApiResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await?
}

fn ensure_writable(cfg: &Config) -> ApiResult<()> {
    if cfg.read_only {
        Err(ApiError::forbidden("This storage is read-only"))
    } else {
        Ok(())
    }
}

async fn info(State(cfg): State<AppState>) -> ApiResult<Json<Info>> {
    let c = cfg.clone();
    blocking(move || {
        Ok(Info {
            name: c.root_name.clone(),
            read_only: c.read_only,
            disk: fsops::disk_info(&c.root),
            places: fsops::places(&c.root, &c.root_name, &c.places),
        })
    })
    .await
    .map(Json)
}

async fn list(State(cfg): State<AppState>, Query(q): Query<PathQuery>) -> ApiResult<Json<Listing>> {
    blocking(move || fsops::list_dir(&cfg.root, &q.path)).await.map(Json)
}

async fn search(
    State(cfg): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Vec<Entry>>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    blocking(move || fsops::search(&cfg.root, &q.path, &q.q, limit)).await.map(Json)
}

async fn size(State(cfg): State<AppState>, Query(q): Query<PathQuery>) -> ApiResult<Json<SizeReport>> {
    blocking(move || fsops::size_of(&cfg.root, &q.path)).await.map(Json)
}

async fn mkdir(State(cfg): State<AppState>, Json(req): Json<CreateReq>) -> ApiResult<Json<String>> {
    ensure_writable(&cfg)?;
    blocking(move || fsops::create_dir(&cfg.root, &req)).await.map(Json)
}

async fn touch(State(cfg): State<AppState>, Json(req): Json<CreateReq>) -> ApiResult<Json<String>> {
    ensure_writable(&cfg)?;
    blocking(move || fsops::create_file(&cfg.root, &req)).await.map(Json)
}

async fn rename(State(cfg): State<AppState>, Json(req): Json<RenameReq>) -> ApiResult<Json<String>> {
    ensure_writable(&cfg)?;
    blocking(move || fsops::rename(&cfg.root, &req)).await.map(Json)
}

async fn delete(State(cfg): State<AppState>, Json(req): Json<DeleteReq>) -> ApiResult<Json<OpReport>> {
    ensure_writable(&cfg)?;
    blocking(move || Ok(fsops::delete(&cfg.root, &req.paths))).await.map(Json)
}

async fn transfer(
    State(cfg): State<AppState>,
    Json(req): Json<TransferReq>,
) -> ApiResult<Json<OpReport>> {
    ensure_writable(&cfg)?;
    blocking(move || fsops::transfer(&cfg.root, &req)).await.map(Json)
}

/// Streams a file with Range/ETag support. `inline=true` is used for
/// previews; those responses are sandboxed so uploaded HTML/SVG cannot run
/// script against this origin.
async fn download(
    State(cfg): State<AppState>,
    Query(q): Query<DownloadQuery>,
    req: Request,
) -> ApiResult<Response> {
    let path = fsops::resolve(&cfg.root, &q.path)?;
    let meta = tokio::fs::metadata(&path).await?;
    if !meta.is_file() {
        return Err(ApiError::bad_request("Only files can be downloaded"));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let res = match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut res = res.map(Body::new);

    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if q.inline { "inline" } else { "attachment" },
        percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC)
    );
    let headers = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    Ok(res)
}

/// Deletes an upload's temp file when dropped, unless disarmed. This also
/// covers a request that is cancelled mid-upload (its future is dropped).
struct TempGuard(Option<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

async fn upload(
    State(cfg): State<AppState>,
    Query(q): Query<PathQuery>,
    mut multipart: Multipart,
) -> ApiResult<Json<OpReport>> {
    ensure_writable(&cfg)?;
    let dir = fsops::resolve(&cfg.root, &q.path)?;
    if !tokio::fs::metadata(&dir).await?.is_dir() {
        return Err(ApiError::bad_request("Upload target is not a folder"));
    }

    let mut report = OpReport::default();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        let Some(raw) = field.file_name().map(str::to_owned) else { continue };
        let name = match fsops::sanitize_name(&raw) {
            Ok(n) => n,
            Err(e) => {
                report.errors.push(format!("{raw}: {}", e.message));
                continue;
            }
        };
        // Write to a hidden temp file first so readers never see partial files.
        let d = dir.clone();
        let (file, tmp) = match blocking(move || fsops::create_temp(&d).map_err(ApiError::from)).await {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(format!("{name}: {}", e.message));
                continue;
            }
        };
        let mut tmp_guard = TempGuard(Some(tmp.clone()));
        let result = async {
            write_field(&mut field, tokio::fs::File::from_std(file)).await?;
            // Never replaces an existing file, even one created meanwhile.
            let d = dir.clone();
            let n = name.clone();
            blocking(move || fsops::rename_unique(&tmp, &d, &n).map_err(ApiError::from)).await
        }
        .await;
        match result {
            Ok(_) => {
                tmp_guard.0 = None;
                report.done += 1;
            }
            Err(e) => report.errors.push(format!("{name}: {}", e.message)),
        }
    }
    Ok(Json(report))
}

async fn write_field(field: &mut Field<'_>, mut file: tokio::fs::File) -> ApiResult<()> {
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_parse() {
        let p = parse_places("Docs=/docs, Media = /media ,bad,x=relative");
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].name, "Media");
        assert_eq!(p[1].path, "/media");
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(k, v)| (header::HeaderName::from_static(k), HeaderValue::from_static(v)))
            .collect()
    }

    #[test]
    fn host_names() {
        assert_eq!(host_without_port("example.com:8080"), "example.com");
        assert_eq!(host_without_port("example.com"), "example.com");
        assert_eq!(host_without_port("[::1]:8080"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("::1"), "::1");
    }

    #[test]
    fn cross_site_writes_are_refused() {
        let host = Some("files.example.com");
        let post = &Method::POST;
        // Same origin, by fetch metadata or by Origin header.
        assert!(check_request(post, host, &headers(&[("sec-fetch-site", "same-origin")]), &[]).is_ok());
        assert!(check_request(post, host, &headers(&[("origin", "https://files.example.com")]), &[]).is_ok());
        // Non-browser clients send neither.
        assert!(check_request(post, host, &headers(&[]), &[]).is_ok());
        // Another site.
        assert!(check_request(post, host, &headers(&[("sec-fetch-site", "cross-site")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("sec-fetch-site", "same-site")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("origin", "https://evil.example")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("origin", "null")]), &[]).is_err());
        // Reads are not affected.
        assert!(check_request(&Method::GET, host, &headers(&[("sec-fetch-site", "cross-site")]), &[]).is_ok());
    }

    #[test]
    fn unknown_hosts_are_refused_when_configured() {
        let allowed = vec!["files.example.com".to_string(), "localhost".to_string()];
        let get = &Method::GET;
        assert!(check_request(get, Some("files.example.com"), &headers(&[]), &allowed).is_ok());
        assert!(check_request(get, Some("localhost:8080"), &headers(&[]), &allowed).is_ok());
        assert!(check_request(get, Some("FILES.example.com:443"), &headers(&[]), &allowed).is_ok());
        assert!(check_request(get, Some("rebind.evil.example"), &headers(&[]), &allowed).is_err());
        assert!(check_request(get, None, &headers(&[]), &allowed).is_err());
        // Without a list, any host is accepted.
        assert!(check_request(get, Some("anything"), &headers(&[]), &[]).is_ok());
    }
}
