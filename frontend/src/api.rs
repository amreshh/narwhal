//! Thin client for the Narwhal HTTP API. URLs are relative so the app works
//! behind any ingress path prefix.

use gloo_net::http::{Request, Response};
use serde::{de::DeserializeOwned, Serialize};
use narwhal_shared::{
    CreateReq, DeleteReq, Entry, ErrorBody, Info, Listing, OpReport, RenameReq, SizeReport,
    TransferOp, TransferReq,
};
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::{FormData, ProgressEvent, XmlHttpRequest};

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
    let resp = Request::post(url)
        .json(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
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

pub fn download_url(path: &str, inline: bool) -> String {
    format!("api/download?path={}{}", enc(path), if inline { "&inline=true" } else { "" })
}

/// Upload files with XHR so we get progress events (fetch has none for uploads).
/// Each file comes with its path relative to `dir`; for files inside folders
/// (`photos/2024/a.jpg`) the server creates the folders.
pub async fn upload(
    dir: &str,
    files: Vec<(String, web_sys::File)>,
    on_progress: impl Fn(f64) + 'static,
) -> ApiResult<OpReport> {
    let form = FormData::new().map_err(js_err)?;
    for (rel, f) in &files {
        if rel.contains('/') {
            form.append_with_str("path", rel).map_err(js_err)?;
        }
        form.append_with_blob_and_filename("file", f, &f.name()).map_err(js_err)?;
    }
    let xhr = XmlHttpRequest::new().map_err(js_err)?;
    xhr.open_with_async("POST", &format!("api/upload?path={}", enc(dir)), true)
        .map_err(js_err)?;

    let progress = Closure::<dyn FnMut(ProgressEvent)>::new(move |e: ProgressEvent| {
        if e.length_computable() && e.total() > 0.0 {
            on_progress(e.loaded() / e.total());
        }
    });
    xhr.upload()
        .map_err(js_err)?
        .set_onprogress(Some(progress.as_ref().unchecked_ref()));

    let finished = js_sys::Promise::new(&mut |resolve, reject| {
        xhr.set_onload(Some(&resolve));
        xhr.set_onerror(Some(&reject));
        xhr.set_onabort(Some(&reject));
    });
    xhr.send_with_opt_form_data(Some(&form)).map_err(js_err)?;
    let outcome = wasm_bindgen_futures::JsFuture::from(finished).await;
    drop(progress);
    outcome.map_err(|_| "Upload failed: the connection was interrupted".to_string())?;

    let status = xhr.status().map_err(js_err)?;
    let text = xhr.response_text().map_err(js_err)?.unwrap_or_default();
    if (200..300).contains(&status) {
        serde_json::from_str(&text).map_err(|e| e.to_string())
    } else {
        Err(serde_json::from_str::<ErrorBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| format!("Upload failed (HTTP {status})")))
    }
}
