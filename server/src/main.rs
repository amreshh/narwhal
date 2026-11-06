//! Narwhal server: a small file API over a root directory, plus the static
//! WASM frontend.

mod confined;
mod error;
mod fsops;
mod trash;

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post, put},
    Json, Router,
};
use confined::{Dir, Root};
use error::{ApiError, ApiResult};
use narwhal_shared::{
    vjoin, vname, CreateReq, DeleteReq, Entry, Info, Listing, OpReport, Place, RenameReq, SizeReport, TransferReq,
    TrashItem, TrashReq,
};
use serde::Deserialize;
use std::{
    ffi::OsString,
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
    root: Root,
    root_name: String,
    read_only: bool,
    /// Deleting moves items to the Trash.
    trash: bool,
    /// Trash items older than this are deleted for good; 0 = never.
    trash_days: u64,
    places: Vec<Place>,
    /// Default number of files the app uploads at once.
    upload_parallel: u32,
    /// Largest upload in bytes; 0 = unlimited.
    max_upload: u64,
    /// Host names the server answers to; empty = any.
    allowed_hosts: Vec<String>,
    /// Content-Security-Policy for the app's pages.
    app_csp: HeaderValue,
}

type AppState = Arc<Config>;

/// Stop at startup because of bad configuration.
fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

/// A setting, trimmed; empty counts as unset.
fn env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => fail(format!("{key} is not valid UTF-8")),
    }
}

/// A yes/no setting. Anything else stops the server instead of silently
/// meaning "no" (`NARWHAL_READ_ONLY=ture` must not leave the storage writable).
fn env_bool(key: &str, default: bool) -> bool {
    match env(key).map(|v| v.to_ascii_lowercase()).as_deref() {
        None => default,
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        Some(v) => fail(format!("{key}={v:?} is not a yes/no value; use true or false")),
    }
}

/// A number setting. An invalid one stops the server instead of silently
/// falling back to the default (`NARWHAL_MAX_UPLOAD_MB=10GB` is not "unlimited").
fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    match env(key) {
        None => default,
        Some(v) => v.parse().unwrap_or_else(|_| fail(format!("{key}={v:?} is not a valid number"))),
    }
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

/// Allow as many open files as the system lets this process have: every
/// connection uses one, and so does each folder level of a copy or delete.
fn raise_open_file_limit() {
    use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};
    if let Rlimit { current: Some(current), maximum: Some(max) } = getrlimit(Resource::Nofile) {
        if current < max {
            let _ = setrlimit(Resource::Nofile, Rlimit { current: Some(max), maximum: Some(max) });
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();
    raise_open_file_limit();

    let root_raw = env("NARWHAL_ROOT").unwrap_or_else(|| "/data".into());
    let root = Root::open(Path::new(&root_raw), trash::DIR)
        .unwrap_or_else(|e| fail(format!("NARWHAL_ROOT {root_raw:?} is not usable: {e}")));
    let static_dir = PathBuf::from(env("NARWHAL_STATIC_DIR").unwrap_or_else(|| "./static".into()));
    let bind: SocketAddr = env("NARWHAL_BIND")
        .unwrap_or_else(|| "[::]:8080".into())
        .parse()
        .unwrap_or_else(|_| fail("NARWHAL_BIND must be host:port, e.g. [::]:8080 or 0.0.0.0:8080"));

    let cfg = Arc::new(Config {
        root,
        root_name: env("NARWHAL_ROOT_NAME").unwrap_or_else(|| "Storage".into()),
        read_only: env_bool("NARWHAL_READ_ONLY", false),
        trash: env_bool("NARWHAL_TRASH", true),
        trash_days: env_num("NARWHAL_TRASH_DAYS", 30),
        places: env("NARWHAL_PLACES").map(|p| parse_places(&p)).unwrap_or_default(),
        max_upload: env_num("NARWHAL_MAX_UPLOAD_MB", 0u64).saturating_mul(1024 * 1024),
        upload_parallel: env_num("NARWHAL_UPLOAD_PARALLEL", 3u32).clamp(1, 16),
        allowed_hosts: env("NARWHAL_ALLOWED_HOSTS")
            .map(|v| {
                v.split(',')
                    .map(|h| host_without_port(h.trim()).to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        app_csp: app_csp(&static_dir),
    });

    let listener = listen(bind).unwrap_or_else(|e| fail(format!("cannot listen on {bind}: {e}")));
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| bind.to_string());
    let stack = if local.starts_with('[') { "IPv4 + IPv6" } else { "IPv4 only" };

    tracing::info!(
        root = %cfg.root.path().display(),
        static_dir = %static_dir.display(),
        read_only = cfg.read_only,
        trash = cfg.trash,
        trash_days = cfg.trash_days,
        "starting narwhal on {local} ({stack})"
    );
    if static_dir.canonicalize().is_ok_and(|s| s.starts_with(cfg.root.path())) && !cfg.read_only {
        tracing::warn!("NARWHAL_STATIC_DIR is inside NARWHAL_ROOT: anyone using the app can change the app itself");
    }

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
        .route("/api/trash", get(trash_list).post(trash_move))
        .route("/api/trash/restore", post(trash_restore))
        .route("/api/trash/purge", post(trash_purge))
        .route("/api/trash/empty", post(trash_empty))
        .route("/api/upload", put(upload))
        .route("/api/{*rest}", any(|| async { ApiError::not_found("Unknown API endpoint") }))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(cfg.clone());

    // The SPA: hashed assets from trunk, index.html as fallback.
    let spa = Compression::new(ServeDir::new(&static_dir).fallback(ServeFile::new(static_dir.join("index.html"))));

    if cfg.trash && !cfg.read_only && cfg.trash_days > 0 {
        let c = cfg.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                let c = c.clone();
                if let Ok(rep) = blocking(move || Ok(trash::expire(&c.root, c.trash_days))).await {
                    if rep.done > 0 || !rep.errors.is_empty() {
                        tracing::info!(removed = rep.done, errors = ?rep.errors, "emptied old Trash items");
                    }
                }
            }
        });
    }

    let app =
        api.fallback_service(spa).layer(middleware::from_fn_with_state(cfg, guard)).layer(TraceLayer::new_for_http());

    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await.expect("server error");
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
// Request guard and security headers
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
///   submitting a form to this server with the user's cookies.
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
        let same = matches!((origin_host, host), (Some(o), Some(h)) if o.eq_ignore_ascii_case(h));
        if !same {
            return Err("Cross-site requests are not allowed");
        }
    }
    Ok(())
}

/// Standard base64 (padded), for the hashes in a Content-Security-Policy.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// `'sha256-…'` sources for the inline scripts of a page (Trunk's loader),
/// so the policy can allow exactly those and no others.
fn inline_script_hashes(html: &str) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        rest = &rest[start..];
        let (Some(tag_end), Some(close)) = (rest.find('>'), rest.find("</script>")) else { break };
        if tag_end < close && !rest[..tag_end].contains("src=") {
            // Browsers hash the text with line breaks normalized to \n.
            let body = rest[tag_end + 1..close].replace("\r\n", "\n").replace('\r', "\n");
            out.push(format!("'sha256-{}'", base64(&Sha256::digest(body.as_bytes()))));
        }
        rest = &rest[close + "</script>".len()..];
    }
    out
}

/// The app's Content-Security-Policy: only its own scripts (and its loader)
/// run, and it talks to nothing but this server. Inline styles are allowed,
/// as the UI sets `style` attributes.
fn app_csp(static_dir: &Path) -> HeaderValue {
    let index = static_dir.join("index.html");
    let hashes = match std::fs::read_to_string(&index) {
        Ok(html) => inline_script_hashes(&html).join(" "),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", index.display());
            String::new()
        }
    };
    let csp = format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' {hashes}; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    HeaderValue::from_str(&csp).unwrap_or_else(|_| HeaderValue::from_static("frame-ancestors 'none'"))
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
    let is_api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    // The app must not be framed by other sites (clickjacking).
    headers.entry(header::X_FRAME_OPTIONS).or_insert(HeaderValue::from_static("DENY"));
    headers.entry(header::X_CONTENT_TYPE_OPTIONS).or_insert(HeaderValue::from_static("nosniff"));
    // Folder and file names are in the API's URLs: never send them elsewhere.
    headers.entry(header::REFERRER_POLICY).or_insert(HeaderValue::from_static("no-referrer"));
    if !is_api {
        headers.entry(header::CONTENT_SECURITY_POLICY).or_insert(cfg.app_csp.clone());
    }
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
struct UploadQuery {
    /// Folder to upload into.
    #[serde(default = "root_path")]
    path: String,
    /// The file's path below that folder (`a.jpg` or `photos/2024/a.jpg`).
    rel: String,
    /// Exact size in bytes.
    size: u64,
    /// Hex SHA-256 of the content.
    sha256: String,
    /// A repeat of an attempt that may have arrived: reuse an identical file.
    #[serde(default)]
    retry: bool,
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
    blocking(move || {
        Ok(Info {
            name: cfg.root_name.clone(),
            read_only: cfg.read_only,
            disk: fsops::disk_info(&cfg.root),
            places: fsops::places(&cfg.root, &cfg.root_name, &cfg.places),
            trash: trash_enabled(&cfg),
            trash_items: if trash_enabled(&cfg) { trash::count(&cfg.root) } else { 0 },
            upload_parallel: cfg.upload_parallel,
        })
    })
    .await
    .map(Json)
}

async fn list(State(cfg): State<AppState>, Query(q): Query<PathQuery>) -> ApiResult<Json<Listing>> {
    blocking(move || fsops::list_dir(&cfg.root, &q.path)).await.map(Json)
}

async fn search(State(cfg): State<AppState>, Query(q): Query<SearchQuery>) -> ApiResult<Json<Vec<Entry>>> {
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

fn trash_enabled(cfg: &Config) -> bool {
    cfg.trash && !cfg.read_only
}

fn ensure_trash(cfg: &Config) -> ApiResult<()> {
    ensure_writable(cfg)?;
    if cfg.trash {
        Ok(())
    } else {
        Err(ApiError::forbidden("The Trash is turned off"))
    }
}

async fn trash_list(State(cfg): State<AppState>) -> ApiResult<Json<Vec<TrashItem>>> {
    ensure_trash(&cfg)?;
    blocking(move || trash::list(&cfg.root)).await.map(Json)
}

async fn trash_move(State(cfg): State<AppState>, Json(req): Json<DeleteReq>) -> ApiResult<Json<OpReport>> {
    ensure_trash(&cfg)?;
    blocking(move || Ok(trash::trash(&cfg.root, &req.paths))).await.map(Json)
}

async fn trash_restore(State(cfg): State<AppState>, Json(req): Json<TrashReq>) -> ApiResult<Json<OpReport>> {
    ensure_trash(&cfg)?;
    blocking(move || Ok(trash::restore(&cfg.root, &req.ids))).await.map(Json)
}

async fn trash_purge(State(cfg): State<AppState>, Json(req): Json<TrashReq>) -> ApiResult<Json<OpReport>> {
    ensure_trash(&cfg)?;
    blocking(move || Ok(trash::purge(&cfg.root, &req.ids))).await.map(Json)
}

async fn trash_empty(State(cfg): State<AppState>) -> ApiResult<Json<OpReport>> {
    ensure_trash(&cfg)?;
    blocking(move || Ok(trash::empty(&cfg.root))).await.map(Json)
}

async fn transfer(State(cfg): State<AppState>, Json(req): Json<TransferReq>) -> ApiResult<Json<OpReport>> {
    ensure_writable(&cfg)?;
    blocking(move || fsops::transfer(&cfg.root, &req)).await.map(Json)
}

/// Streams a file with Range/ETag support. `inline=true` is used for
/// previews; those responses are sandboxed so uploaded HTML/SVG cannot run
/// script against this origin.
async fn download(State(cfg): State<AppState>, Query(q): Query<DownloadQuery>, req: Request) -> ApiResult<Response> {
    let name = vname(&fsops::normalize_virtual(&q.path)?).to_string();
    let path = q.path.clone();
    let (file, _) = blocking(move || fsops::open_file(&cfg.root, &path)).await?;
    // Decided by name, not by the response: a 304 (cache revalidation) has
    // no Content-Type, and the browser applies its headers to the cached copy.
    let is_pdf = Path::new(&name).extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    let mime = mime_guess::from_path(&name).first_or_octet_stream();

    // Serve the file that was just opened and checked: it is reopened
    // through its descriptor, not looked up by its path again.
    let res = match ServeFile::new_with_mime(confined::fd_path(&file), &mime).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    drop(file);
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
    if q.inline && is_pdf {
        // Browsers' PDF viewers refuse to run in a sandboxed document, and
        // run PDF scripts in their own isolated viewer anyway. Allow the app
        // (only) to frame it for the preview panel.
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("frame-ancestors 'self'"));
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    } else {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    }
    Ok(res)
}

/// Deletes an upload's temp file when dropped, unless disarmed. This also
/// covers a request that is cancelled mid-upload (its future is dropped).
struct TempGuard {
    dir: Arc<Dir>,
    name: Option<OsString>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(name) = self.name.take() {
            let _ = self.dir.unlink(&name);
        }
    }
}

/// How long an upload may go without receiving any data.
const UPLOAD_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// Uploads one file sent as the raw request body, and only puts it in place
/// if it arrived complete: the size and SHA-256 the client announced must
/// match what was received. Until then it is a hidden temp file, which is
/// deleted if anything goes wrong (including the client going away).
/// Returns the file's virtual path.
async fn upload(State(cfg): State<AppState>, Query(q): Query<UploadQuery>, body: Body) -> ApiResult<Json<String>> {
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};

    ensure_writable(&cfg)?;
    let expected = q.sha256.to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("Invalid checksum"));
    }
    if cfg.max_upload > 0 && q.size > cfg.max_upload {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "The file is larger than the upload limit"));
    }
    let (subdirs, name) = fsops::split_upload_path(&q.rel)?;
    let vdir = subdirs.iter().fold(fsops::normalize_virtual(&q.path)?, |acc, d| vjoin(&acc, d));
    let base = q.path.clone();
    let dir = Arc::new(blocking(move || fsops::make_dirs(&cfg.root, &base, &subdirs)).await?);
    let d = dir.clone();
    let (file, tmp) = blocking(move || fsops::create_temp(&d).map_err(ApiError::from)).await?;
    let mut guard = TempGuard { dir: dir.clone(), name: Some(tmp.clone()) };

    // A damaged or cut-off upload is worth retrying, so it gets 422.
    let damaged = |m: &str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, m);
    let mut file = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut stream = body.into_data_stream();
    loop {
        let next = tokio::time::timeout(UPLOAD_STALL, stream.next())
            .await
            .map_err(|_| ApiError::new(StatusCode::REQUEST_TIMEOUT, "The upload stalled"))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| damaged("The upload was interrupted"))?;
        received += chunk.len() as u64;
        if received > q.size {
            return Err(damaged("More data arrived than announced"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    if received != q.size {
        return Err(damaged("The upload was incomplete"));
    }
    if fsops::hex(&hasher.finalize()) != expected {
        return Err(damaged("The checksum does not match: the file was damaged on the way"));
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    let size = q.size;
    let final_name = blocking(move || {
        let name = OsString::from(name);
        if q.retry {
            if let Some(existing) = fsops::find_identical(&dir, &name, size, &expected) {
                let _ = dir.unlink(&tmp);
                return Ok(existing);
            }
        }
        // Never replaces an existing file, even one created meanwhile.
        fsops::rename_unique(&dir, &tmp, &dir, &name).map_err(ApiError::from)
    })
    .await?;
    guard.name = None;
    Ok(Json(vjoin(&vdir, &final_name.to_string_lossy())))
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
        pairs.iter().map(|(k, v)| (header::HeaderName::from_static(k), HeaderValue::from_static(v))).collect()
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
        assert!(check_request(
            post,
            Some("Files.Example.com"),
            &headers(&[("origin", "https://files.example.com")]),
            &[]
        )
        .is_ok());
        // Non-browser clients send neither.
        assert!(check_request(post, host, &headers(&[]), &[]).is_ok());
        // Another site.
        assert!(check_request(post, host, &headers(&[("sec-fetch-site", "cross-site")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("sec-fetch-site", "same-site")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("origin", "https://evil.example")]), &[]).is_err());
        assert!(check_request(post, host, &headers(&[("origin", "null")]), &[]).is_err());
        assert!(check_request(post, None, &headers(&[("origin", "https://files.example.com")]), &[]).is_err());
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

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn only_inline_scripts_are_hashed() {
        let html = "<script src=\"x.js\"></script>\n<script type=\"module\">\r\nrun();\n</script>";
        // printf '\nrun();\n' | openssl dgst -sha256 -binary | base64
        assert_eq!(inline_script_hashes(html), ["'sha256-8Nic6TNoKbU2fN/Yduzc28MgjFgcikm6fSU0x0dR3EU='"]);
        assert!(inline_script_hashes("<p>no scripts</p>").is_empty());
    }
}
