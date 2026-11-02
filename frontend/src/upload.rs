//! Reliable uploads.
//!
//! Each file goes up in its own request, together with its size and
//! SHA-256. The server only puts a file in place when both match what it
//! received, so a cut-off or damaged upload never shows up as a file.
//! Failed attempts are retried (waiting for the network to come back if it
//! is gone), and a few files are sent at once so many small files go fast.
//! How many is a setting (`AppState::upload_parallel`) that can change
//! while an upload runs. Files added during an upload join its queue.

use crate::{api, state::*};
use futures::future::join_all;
use leptos::prelude::*;
use narwhal_shared::OpReport;
use sha2::{Digest, Sha256};
use std::{cell::Cell, cell::RefCell, rc::Rc};
use wasm_bindgen_futures::JsFuture;

/// Attempts per file before giving up.
const ATTEMPTS: u32 = 5;
/// Seconds to wait before attempt 2, 3, ….
const BACKOFF: [i32; 4] = [1, 3, 6, 12];
/// How long to wait for a lost network connection to come back.
const OFFLINE_WAIT_MS: f64 = 30.0 * 60.0 * 1000.0;
/// Bytes hashed per step (the page stays responsive in between).
const HASH_CHUNK: f64 = 8.0 * 1024.0 * 1024.0;

#[derive(Clone, Debug, PartialEq)]
pub enum FileState {
    Waiting,
    /// Computing the checksum.
    Checking,
    Sending,
    /// Waiting to try again: (next attempt, why the last one failed).
    Retrying(u32, String),
    Done,
    Failed(String),
}

pub async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = JsFuture::from(p).await;
}

/// Wait (up to a limit) while the browser says it is offline.
async fn wait_until_online() {
    let start = js_sys::Date::now();
    while !window().navigator().on_line() && js_sys::Date::now() - start < OFFLINE_WAIT_MS {
        sleep(2000).await;
    }
}

/// Hex SHA-256 of a file, read in chunks so huge files don't fill memory.
async fn sha256(file: &web_sys::File) -> Result<String, String> {
    let unreadable = || "Could not read the file. Was it moved or changed?".to_string();
    let size = file.size();
    let mut hasher = Sha256::new();
    let mut pos = 0.0;
    loop {
        let end = (pos + HASH_CHUNK).min(size);
        let part = file.slice_with_f64_and_f64(pos, end).map_err(|_| unreadable())?;
        let buf = JsFuture::from(part.array_buffer()).await.map_err(|_| unreadable())?;
        hasher.update(js_sys::Uint8Array::new(&buf).to_vec());
        pos = end;
        if pos >= size {
            break;
        }
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A file waiting in (or taken from) the queue. Its index matches the
/// running `Upload`'s lists.
#[derive(Clone)]
struct Job {
    dest: String,
    rel: String,
    file: web_sys::File,
}

thread_local! {
    /// Every file of the running upload, in order. Grows when files are
    /// added meanwhile; cleared when the upload ends.
    static QUEUE: RefCell<Vec<Job>> = const { RefCell::new(Vec::new()) };
}

fn queued() -> usize {
    QUEUE.with(|q| q.borrow().len())
}

/// Add files (each with its path relative to `dest`) to the queue. The
/// caller adds them to the `Upload` state at the same time.
pub fn enqueue(dest: &str, files: Vec<(String, web_sys::File)>) {
    QUEUE.with(|q| {
        q.borrow_mut().extend(files.into_iter().map(|(rel, file)| Job { dest: dest.to_string(), rel, file }))
    });
}

/// Upload everything in the queue, including files added while this runs.
pub async fn run(st: AppState) -> OpReport {
    let next = Rc::new(Cell::new(0usize));
    let active = Rc::new(Cell::new(0usize));
    let report = Rc::new(RefCell::new(OpReport::default()));
    loop {
        // Enough workers for the highest setting; the ones above the
        // current setting wait their turn.
        let workers = (0..MAX_PARALLEL).map(|_| worker(st, next.clone(), active.clone(), report.clone()));
        join_all(workers).await;
        // Files may have been added just as the last worker stopped.
        if next.get() >= queued() {
            break;
        }
    }
    QUEUE.with(|q| q.borrow_mut().clear());
    report.take()
}

async fn worker(st: AppState, next: Rc<Cell<usize>>, active: Rc<Cell<usize>>, report: Rc<RefCell<OpReport>>) {
    loop {
        let n = next.get();
        if n >= queued() {
            // Stay around while others work: more files may be added.
            if active.get() == 0 {
                break;
            }
            sleep(250).await;
            continue;
        }
        if active.get() >= untrack(|| st.upload_parallel()) as usize {
            sleep(250).await;
            continue;
        }
        let Some(job) = QUEUE.with(|q| q.borrow().get(n).cloned()) else { break };
        next.set(n + 1);
        active.set(active.get() + 1);
        match send_one(st, &job.dest, n, &job.rel, &job.file).await {
            Ok(path) => {
                let mut r = report.borrow_mut();
                r.done += 1;
                r.ids.push(path);
            }
            Err(e) => report.borrow_mut().errors.push(format!("{}: {e}", job.rel)),
        }
        active.set(active.get() - 1);
    }
}

fn set_state(st: AppState, n: usize, state: FileState) {
    st.upload.update(|u| {
        if let Some(u) = u {
            u.set_state(n, state);
        }
    });
}

fn set_loaded(st: AppState, n: usize, bytes: f64) {
    st.upload.update(|u| {
        if let Some(u) = u {
            u.set_loaded(n, bytes);
        }
    });
}

/// One file, with retries. Returns its path on the server.
async fn send_one(st: AppState, dest: &str, n: usize, rel: &str, file: &web_sys::File) -> Result<String, String> {
    set_state(st, n, FileState::Checking);
    let mut sha = sha256(file).await.inspect_err(|e| set_state(st, n, FileState::Failed(e.clone())))?;
    for attempt in 1..=ATTEMPTS {
        set_state(st, n, FileState::Sending);
        set_loaded(st, n, 0.0);
        // After a failed attempt the file may have arrived anyway (only
        // the answer got lost); the server then reuses it.
        let res = api::upload_file(dest, rel, file, &sha, attempt > 1, move |b| set_loaded(st, n, b)).await;
        match res {
            Ok(path) => {
                set_loaded(st, n, file.size());
                set_state(st, n, FileState::Done);
                return Ok(path);
            }
            Err(api::SendError::Fatal(msg)) => {
                set_loaded(st, n, 0.0);
                set_state(st, n, FileState::Failed(msg.clone()));
                return Err(msg);
            }
            Err(api::SendError::Retry { msg, damaged }) => {
                set_loaded(st, n, 0.0);
                if attempt == ATTEMPTS {
                    let msg = format!("{msg} (gave up after {ATTEMPTS} attempts)");
                    set_state(st, n, FileState::Failed(msg.clone()));
                    return Err(msg);
                }
                set_state(st, n, FileState::Retrying(attempt + 1, msg));
                sleep(BACKOFF[(attempt - 1) as usize] * 1000).await;
                wait_until_online().await;
                if damaged {
                    // The file may have changed on disk since it was hashed.
                    set_state(st, n, FileState::Checking);
                    sha = sha256(file).await.inspect_err(|e| set_state(st, n, FileState::Failed(e.clone())))?;
                }
            }
        }
    }
    unreachable!("the last attempt always returns")
}

/// Ask before leaving the page while an upload is running.
pub fn guard_unload(st: AppState) {
    let handle = window_event_listener(leptos::ev::beforeunload, move |ev: web_sys::BeforeUnloadEvent| {
        if st.upload.with_untracked(|u| u.is_some()) {
            ev.prevent_default();
            ev.set_return_value("An upload is still running");
        }
    });
    std::mem::forget(handle);
}

/// Status line counts: (finished, failed, retrying).
pub fn counts(u: &Upload) -> (usize, usize, usize) {
    u.states.iter().fold((0, 0, 0), |(d, f, r), s| match s {
        FileState::Done => (d + 1, f, r),
        FileState::Failed(_) => (d, f + 1, r),
        FileState::Retrying(..) => (d, f, r + 1),
        _ => (d, f, r),
    })
}
