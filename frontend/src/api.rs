//! Thin client for the Narwhal HTTP API. URLs are relative so the app works
//! behind any ingress path prefix.

use crate::upload::Cancel;
use gloo_net::http::{Request, Response};
use narwhal_shared::{
    CreateReq, DeleteReq, Entry, ErrorBody, Info, Listing, OpReport, RenameReq, SizeReport, TransferOp, TransferReq,
    TrashItem, TrashReq,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::{ProgressEvent, XmlHttpRequest};

pub type ApiResult<T> = Result<T, String>;

fn enc(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

fn js_err(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

async fn parse<T: DeserializeOwned>(resp: Response) -> ApiResult<T> {
    if resp.ok() {
        resp.json::<T>().await.map_err(|e| e.to_string())
    } else {
        let status = resp.status();
        match resp.json::<ErrorBody>().await {
            Ok(b) => Err(b.error),
            Err(_) => Err(format!("Request failed (HTTP {status})")),
        }
    }
}

async fn get<T: DeserializeOwned>(url: &str) -> ApiResult<T> {
    let resp = Request::get(url).send().await.map_err(|e| e.to_string())?;
    parse(resp).await
}

async fn post<B: Serialize, T: DeserializeOwned>(url: &str, body: &B) -> ApiResult<T> {
    let resp = Request::post(url).json(body).map_err(|e| e.to_string())?.send().await.map_err(|e| e.to_string())?;
    parse(resp).await
}

pub async fn info() -> ApiResult<Info> {
    get("api/info").await
}

pub async fn list(path: &str) -> ApiResult<Listing> {
    get(&format!("api/list?path={}", enc(path))).await
}

pub async fn search(path: &str, q: &str) -> ApiResult<Vec<Entry>> {
    get(&format!("api/search?path={}&q={}", enc(path), enc(q))).await
}

pub async fn size(path: &str) -> ApiResult<SizeReport> {
    get(&format!("api/size?path={}", enc(path))).await
}

pub async fn mkdir(dir: &str, name: &str) -> ApiResult<String> {
    post("api/mkdir", &CreateReq { path: dir.into(), name: name.into() }).await
}

pub async fn touch(dir: &str, name: &str) -> ApiResult<String> {
    post("api/touch", &CreateReq { path: dir.into(), name: name.into() }).await
}

pub async fn rename(path: &str, new_name: &str) -> ApiResult<String> {
    post("api/rename", &RenameReq { path: path.into(), new_name: new_name.into() }).await
}

pub async fn delete(paths: Vec<String>) -> ApiResult<OpReport> {
    post("api/delete", &DeleteReq { paths }).await
}

pub async fn transfer(paths: Vec<String>, dest: &str, op: TransferOp) -> ApiResult<OpReport> {
    post("api/transfer", &TransferReq { paths, dest: dest.into(), op }).await
}

pub async fn trash_list() -> ApiResult<Vec<TrashItem>> {
    get("api/trash").await
}

/// Move items to the Trash; the report's `ids` are their Trash ids.
pub async fn trash(paths: Vec<String>) -> ApiResult<OpReport> {
    post("api/trash", &DeleteReq { paths }).await
}

/// Put Trash items back; the report's `ids` are the restored paths.
pub async fn trash_restore(ids: Vec<String>) -> ApiResult<OpReport> {
    post("api/trash/restore", &TrashReq { ids }).await
}

pub async fn trash_purge(ids: Vec<String>) -> ApiResult<OpReport> {
    post("api/trash/purge", &TrashReq { ids }).await
}

pub async fn trash_empty() -> ApiResult<OpReport> {
    post("api/trash/empty", &()).await
}

pub fn download_url(path: &str, inline: bool) -> String {
    format!("api/download?path={}{}", enc(path), if inline { "&inline=true" } else { "" })
}

/// The first `n` bytes of a file (fewer if it is shorter).
pub async fn head_bytes(path: &str, n: u64) -> ApiResult<Vec<u8>> {
    let resp = Request::get(&download_url(path, true))
        .header("Range", &format!("bytes=0-{}", n.saturating_sub(1)))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(format!("Could not read the file (HTTP {})", resp.status()));
    }
    let mut bytes = resp.binary().await.map_err(|e| e.to_string())?;
    bytes.truncate(n as usize);
    Ok(bytes)
}

/// A whole file's content (for previews that need all of it).
pub async fn file_bytes(path: &str) -> ApiResult<Vec<u8>> {
    let resp = Request::get(&download_url(path, true)).send().await.map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(format!("Could not read the file (HTTP {})", resp.status()));
    }
    resp.binary().await.map_err(|e| e.to_string())
}

/// Why sending a file failed.
pub enum SendError {
    /// Worth another try: the connection broke, the server was briefly
    /// unavailable, or the data arrived damaged (`damaged`).
    Retry { msg: String, damaged: bool },
    /// Trying again won't help (e.g. read-only storage, invalid name).
    Fatal(String),
    /// The user cancelled it.
    Cancelled,
}

/// How long an upload may go without progress before it is given up on.
const STALL_MS: f64 = 60_000.0;

/// Upload one file to `dir` (as `rel`, which may include folders) with its
/// SHA-256; the server keeps it only if it arrives intact. Uses XHR for
/// upload progress (fetch has none). `on_progress` gets the bytes sent;
/// `cancel` aborts the request.
pub async fn upload_file(
    dir: &str,
    rel: &str,
    file: &web_sys::File,
    sha256: &str,
    retry: bool,
    cancel: &Cancel,
    on_progress: impl Fn(f64) + 'static,
) -> Result<String, SendError> {
    if cancel.is_cancelled() {
        return Err(SendError::Cancelled);
    }
    let fatal = |e: JsValue| SendError::Fatal(js_err(e));
    let url = format!(
        "api/upload?path={}&rel={}&size={}&sha256={sha256}{}",
        enc(dir),
        enc(rel),
        file.size() as u64,
        if retry { "&retry=true" } else { "" }
    );
    let xhr = XmlHttpRequest::new().map_err(fatal)?;
    xhr.open_with_async("PUT", &url, true).map_err(fatal)?;
    xhr.set_request_header("Content-Type", "application/octet-stream").map_err(fatal)?;

    // Watchdog: a connection can hang without failing. Abort if nothing
    // moves for a while; it is retried like any broken connection.
    let last = Rc::new(Cell::new(js_sys::Date::now()));
    let stalled = Rc::new(Cell::new(false));
    let progress = {
        let last = last.clone();
        Closure::<dyn FnMut(ProgressEvent)>::new(move |e: ProgressEvent| {
            last.set(js_sys::Date::now());
            on_progress(e.loaded());
        })
    };
    // Once everything is sent the server still checks and syncs the file,
    // which can take a moment without progress events.
    let sent = {
        let last = last.clone();
        Closure::<dyn FnMut()>::new(move || last.set(f64::INFINITY))
    };
    let up = xhr.upload().map_err(fatal)?;
    up.set_onprogress(Some(progress.as_ref().unchecked_ref()));
    up.set_onload(Some(sent.as_ref().unchecked_ref()));
    let watchdog = {
        let (xhr, stalled) = (xhr.clone(), stalled.clone());
        leptos::prelude::set_interval_with_handle(
            move || {
                if js_sys::Date::now() - last.get() > STALL_MS {
                    stalled.set(true);
                    let _ = xhr.abort();
                }
            },
            std::time::Duration::from_secs(5),
        )
        .ok()
    };

    let finished = js_sys::Promise::new(&mut |resolve, reject| {
        xhr.set_onload(Some(&resolve));
        xhr.set_onerror(Some(&reject));
        xhr.set_onabort(Some(&reject));
    });
    cancel.set_request(Some(xhr.clone()));
    let outcome = match xhr.send_with_opt_blob(Some(file)) {
        Ok(()) => wasm_bindgen_futures::JsFuture::from(finished).await.map(|_| ()),
        Err(e) => Err(e),
    };
    cancel.set_request(None);
    if let Some(w) = watchdog {
        w.clear();
    }
    // Unhook the closures before they are freed, so no late event calls them.
    up.set_onprogress(None);
    up.set_onload(None);
    drop((progress, sent));
    if outcome.is_err() {
        if cancel.is_cancelled() {
            return Err(SendError::Cancelled);
        }
        let msg = if stalled.get() { "The connection stalled" } else { "The connection was interrupted" };
        return Err(SendError::Retry { msg: msg.into(), damaged: false });
    }

    let status = xhr.status().unwrap_or(0);
    let text = xhr.response_text().ok().flatten().unwrap_or_default();
    if (200..300).contains(&status) {
        return serde_json::from_str(&text).map_err(|e| SendError::Retry { msg: e.to_string(), damaged: false });
    }
    let msg = serde_json::from_str::<ErrorBody>(&text)
        .map(|b| b.error)
        .unwrap_or_else(|_| format!("Upload failed (HTTP {status})"));
    Err(match status {
        422 => SendError::Retry { msg, damaged: true },
        0 | 408 | 429 | 500..=599 => SendError::Retry { msg, damaged: false },
        _ => SendError::Fatal(msg),
    })
}
