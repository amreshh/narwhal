//! Reliable uploads.
//!
//! Each file goes up in its own request, together with its size and
//! SHA-256. The server only puts a file in place when both match what it
//! received, so a cut-off or damaged upload never shows up as a file.
//! Failed attempts are retried (waiting for the network to come back if it
//! is gone), and a few files are sent at once so many small files go fast.
//! How many is a setting (`AppState::upload_parallel`) that can change
//! while an upload runs. Files added during an upload join its queue, and
//! any file can be cancelled until it has arrived.

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
    /// Stopped by the user before it arrived.
    Cancelled,
}

impl FileState {
    /// Not finished yet, so it can still be cancelled.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Waiting | Self::Checking | Self::Sending | Self::Retrying(..))
    }

    /// Being checked, sent, or about to be retried.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Sending | Self::Retrying(..))
    }
}

/// Stops one file's upload: before it starts, while it is checked or sent,
/// or while it waits to be retried.
#[derive(Clone, Default)]
pub struct Cancel(Rc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    cancelled: Cell<bool>,
    /// The request sending the file right now, if any.
    request: RefCell<Option<web_sys::XmlHttpRequest>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.cancelled.set(true);
        // Taken out first: abort() runs event handlers right away.
        let request = self.0.request.borrow_mut().take();
        if let Some(xhr) = request {
            let _ = xhr.abort();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.get()
    }

    /// Set by `api::upload_file` while its request runs.
    pub fn set_request(&self, xhr: Option<web_sys::XmlHttpRequest>) {
        *self.0.request.borrow_mut() = xhr;
    }
}

/// Why a file was not uploaded.
enum Stop {
    Failed(String),
    Cancelled,
}

pub async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = JsFuture::from(p).await;
}

/// Wait `ms` milliseconds, or less if the file is cancelled meanwhile.
async fn pause(ms: i32, cancel: &Cancel) -> Result<(), Stop> {
    let end = js_sys::Date::now() + f64::from(ms);
    loop {
        if cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let left = end - js_sys::Date::now();
        if left <= 0.0 {
            return Ok(());
        }
        sleep(left.min(250.0) as i32).await;
    }
}

/// Wait (up to a limit) while the browser says it is offline.
async fn wait_until_online(cancel: &Cancel) -> Result<(), Stop> {
    let start = js_sys::Date::now();
    while !window().navigator().on_line() && js_sys::Date::now() - start < OFFLINE_WAIT_MS {
        pause(2000, cancel).await?;
    }
    Ok(())
}

/// Hex SHA-256 of a file, read in chunks so huge files don't fill memory.
async fn sha256(file: &web_sys::File, cancel: &Cancel) -> Result<String, Stop> {
    let unreadable = || Stop::Failed("Could not read the file. Was it moved or changed?".to_string());
    let size = file.size();
    let mut hasher = Sha256::new();
    let mut pos = 0.0;
    loop {
        if cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
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
    cancel: Cancel,
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
        q.borrow_mut().extend(files.into_iter().map(|(rel, file)| Job {
            dest: dest.to_string(),
            rel,
            file,
            cancel: Cancel::default(),
        }))
    });
}

/// Cancel file `n` of the running upload (if it hasn't finished).
pub fn cancel(st: AppState, n: usize) {
    if let Some(c) = QUEUE.with(|q| q.borrow().get(n).map(|j| j.cancel.clone())) {
        c.cancel();
    }
    st.upload.update(|u| {
        if let Some(u) = u {
            u.cancel(n);
        }
    });
}

/// Cancel every file of the running upload that hasn't finished.
pub fn cancel_all(st: AppState) {
    let tokens: Vec<Cancel> = QUEUE.with(|q| q.borrow().iter().map(|j| j.cancel.clone()).collect());
    for c in &tokens {
        c.cancel();
    }
    st.upload.update(|u| {
        if let Some(u) = u {
            for n in 0..u.files.len() {
                u.cancel(n);
            }
        }
    });
}

/// What an upload did: the files that arrived and failed, and how many
/// were cancelled.
#[derive(Default)]
pub struct Outcome {
    pub report: OpReport,
    pub cancelled: usize,
}

/// Upload everything in the queue, including files added while this runs.
pub async fn run(st: AppState) -> Outcome {
    let next = Rc::new(Cell::new(0usize));
    let active = Rc::new(Cell::new(0usize));
    let out = Rc::new(RefCell::new(Outcome::default()));
    loop {
        // Enough workers for the highest setting; the ones above the
        // current setting wait their turn.
        let workers = (0..MAX_PARALLEL).map(|_| worker(st, next.clone(), active.clone(), out.clone()));
        join_all(workers).await;
        // Files may have been added just as the last worker stopped.
        if next.get() >= queued() {
            break;
        }
    }
    QUEUE.with(|q| q.borrow_mut().clear());
    out.take()
}

async fn worker(st: AppState, next: Rc<Cell<usize>>, active: Rc<Cell<usize>>, out: Rc<RefCell<Outcome>>) {
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
        let Some(job) = QUEUE.with(|q| q.borrow().get(n).cloned()) else { break };
        // Cancelled while waiting: skip it without taking a slot.
        if job.cancel.is_cancelled() {
            next.set(n + 1);
            out.borrow_mut().cancelled += 1;
            continue;
        }
        if active.get() >= untrack(|| st.upload_parallel()) as usize {
            sleep(250).await;
            continue;
        }
        next.set(n + 1);
        active.set(active.get() + 1);
        match send_one(st, n, &job).await {
            Ok(path) => {
                let r = &mut out.borrow_mut().report;
                r.done += 1;
                r.ids.push(path);
            }
            Err(Stop::Failed(e)) => out.borrow_mut().report.errors.push(format!("{}: {e}", job.rel)),
            Err(Stop::Cancelled) => out.borrow_mut().cancelled += 1,
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
async fn send_one(st: AppState, n: usize, job: &Job) -> Result<String, Stop> {
    let res = send_with_retries(st, n, job).await;
    match &res {
        Ok(_) => {
            set_loaded(st, n, job.file.size());
            set_state(st, n, FileState::Done);
        }
        Err(Stop::Failed(msg)) => {
            set_loaded(st, n, 0.0);
            set_state(st, n, FileState::Failed(msg.clone()));
        }
        Err(Stop::Cancelled) => st.upload.update(|u| {
            if let Some(u) = u {
                u.cancel(n);
            }
        }),
    }
    res
}

async fn send_with_retries(st: AppState, n: usize, job: &Job) -> Result<String, Stop> {
    let Job { dest, rel, file, cancel } = job;
    set_state(st, n, FileState::Checking);
    let mut sha = sha256(file, cancel).await?;
    for attempt in 1..=ATTEMPTS {
        set_state(st, n, FileState::Sending);
        set_loaded(st, n, 0.0);
        // After a failed attempt the file may have arrived anyway (only
        // the answer got lost); the server then reuses it.
        let res = api::upload_file(dest, rel, file, &sha, attempt > 1, cancel, move |b| set_loaded(st, n, b)).await;
        let (msg, damaged) = match res {
            Ok(path) => return Ok(path),
            Err(api::SendError::Cancelled) => return Err(Stop::Cancelled),
            Err(api::SendError::Fatal(msg)) => return Err(Stop::Failed(msg)),
            Err(api::SendError::Retry { msg, damaged }) => (msg, damaged),
        };
        set_loaded(st, n, 0.0);
        if attempt == ATTEMPTS {
            return Err(Stop::Failed(format!("{msg} (gave up after {ATTEMPTS} attempts)")));
        }
        set_state(st, n, FileState::Retrying(attempt + 1, msg));
        pause(BACKOFF[(attempt - 1) as usize] * 1000, cancel).await?;
        wait_until_online(cancel).await?;
        if damaged {
            // The file may have changed on disk since it was hashed.
            set_state(st, n, FileState::Checking);
            sha = sha256(file, cancel).await?;
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

/// How many files of an upload are in each final or troubled state.
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub done: usize,
    pub failed: usize,
    pub retrying: usize,
    pub cancelled: usize,
}

pub fn counts(u: &Upload) -> Counts {
    u.states.iter().fold(Counts::default(), |mut c, s| {
        match s {
            FileState::Done => c.done += 1,
            FileState::Failed(_) => c.failed += 1,
            FileState::Retrying(..) => c.retrying += 1,
            FileState::Cancelled => c.cancelled += 1,
            _ => {}
        }
        c
    })
}
