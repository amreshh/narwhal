//! Application state (all reactive signals) and the actions that mutate it.

use crate::{api, util};
use leptos::{html, prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use narwhal_shared::{vname, vparent, Entry, Info, OpReport, TransferOp};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use wasm_bindgen::JsCast;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ViewMode {
    Icons,
    Compact,
    Details,
    Columns,
}

/// A cached folder listing, used by the column view.
#[derive(Clone, Debug, PartialEq)]
pub enum Col {
    Loading,
    Ready(Vec<Entry>),
    Failed(String),
}

const LISTING_CACHE_MAX: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MsgKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub paths: Vec<String>,
    pub cut: bool,
}

#[derive(Clone, Debug)]
pub struct Menu {
    pub x: i32,
    pub y: i32,
    pub pane: usize,
    /// None = clicked on empty space in the view.
    pub target: Option<Entry>,
}

#[derive(Clone, Debug)]
pub enum Dialog {
    NewFolder(usize),
    NewFile(usize),
    ConfirmDelete(Vec<String>),
    Properties(Entry),
    Shortcuts,
}

#[derive(Clone, Debug)]
pub struct Upload {
    pub label: String,
    pub progress: f64,
}

pub const ZOOM_STEPS: [u32; 9] = [32, 48, 64, 80, 96, 112, 128, 160, 192];

/// One file view. Two exist so split view (F3) works like Dolphin's.
#[derive(Clone, Copy)]
pub struct Pane {
    pub path: RwSignal<String>,
    pub back: RwSignal<Vec<String>>,
    pub forward: RwSignal<Vec<String>>,
    pub entries: RwSignal<Vec<Entry>>,
    pub loading: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub selected: RwSignal<HashSet<String>>,
    /// Range-selection anchor.
    pub anchor: RwSignal<Option<String>>,
    /// Keyboard focus item.
    pub cursor: RwSignal<Option<String>>,
    /// Some(text) while the filter bar is open.
    pub filter: RwSignal<Option<String>>,
    /// Some(query) while the search bar is open.
    pub search: RwSignal<Option<String>>,
    /// Search results replace the folder listing while present.
    pub results: RwSignal<Option<Vec<Entry>>>,
    pub editing_location: RwSignal<bool>,
    /// The folder whose listing is currently in `entries`.
    pub loaded: RwSignal<String>,
    load_seq: RwSignal<u32>,
}

impl Pane {
    fn new(path: &str) -> Self {
        Self {
            path: RwSignal::new(path.to_string()),
            back: RwSignal::new(Vec::new()),
            forward: RwSignal::new(Vec::new()),
            entries: RwSignal::new(Vec::new()),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
            selected: RwSignal::new(HashSet::new()),
            anchor: RwSignal::new(None),
            cursor: RwSignal::new(None),
            filter: RwSignal::new(None),
            search: RwSignal::new(None),
            results: RwSignal::new(None),
            editing_location: RwSignal::new(false),
            loaded: RwSignal::new(String::new()),
            load_seq: RwSignal::new(0),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Prefs {
    view: ViewMode,
    zoom: u32,
    hidden: bool,
    previews: bool,
    places: bool,
    sort: SortKey,
    asc: bool,
}

const PREFS_KEY: &str = "narwhal.prefs";

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

#[derive(Clone, Copy)]
pub struct AppState {
    pub panes: [Pane; 2],
    pub active: RwSignal<usize>,
    pub split: RwSignal<bool>,
    pub view: RwSignal<ViewMode>,
    pub zoom: RwSignal<u32>,
    pub sort: RwSignal<(SortKey, bool)>,
    pub show_hidden: RwSignal<bool>,
    pub previews: RwSignal<bool>,
    pub places_open: RwSignal<bool>,
    pub info: RwSignal<Option<Info>>,
    pub clip: RwSignal<Option<Clip>>,
    pub menu: RwSignal<Option<Menu>>,
    pub hamburger: RwSignal<bool>,
    pub dialog: RwSignal<Option<Dialog>>,
    pub renaming: RwSignal<Option<(usize, String)>>,
    pub message: RwSignal<Option<(MsgKind, String)>>,
    pub upload: RwSignal<Option<Upload>>,
    pub upload_dest: RwSignal<Option<String>>,
    pub upload_input: NodeRef<html::Input>,
    /// Paths being dragged from inside the app.
    pub dragging: RwSignal<Option<Vec<String>>>,
    /// Folder path (or "pane:N") currently highlighted as a drop target.
    pub drop_target: RwSignal<Option<String>>,
    /// Folder listings for the column view, shared by both panes.
    pub listings: RwSignal<HashMap<String, Col>>,
    listings_gen: StoredValue<u32>,
}

impl AppState {
    pub fn new() -> Self {
        let prefs: Option<Prefs> = storage()
            .and_then(|s| s.get_item(PREFS_KEY).ok().flatten())
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let (view, zoom, hidden, previews, places, sort) = match prefs {
            Some(p) => (p.view, p.zoom, p.hidden, p.previews, p.places, (p.sort, p.asc)),
            None => (ViewMode::Icons, 64, false, true, true, (SortKey::Name, true)),
        };
        Self {
            panes: [Pane::new("/"), Pane::new("/")],
            active: RwSignal::new(0),
            split: RwSignal::new(false),
            view: RwSignal::new(view),
            zoom: RwSignal::new(zoom.clamp(32, 192)),
            sort: RwSignal::new(sort),
            show_hidden: RwSignal::new(hidden),
            previews: RwSignal::new(previews),
            places_open: RwSignal::new(places),
            info: RwSignal::new(None),
            clip: RwSignal::new(None),
            menu: RwSignal::new(None),
            hamburger: RwSignal::new(false),
            dialog: RwSignal::new(None),
            renaming: RwSignal::new(None),
            message: RwSignal::new(None),
            upload: RwSignal::new(None),
            upload_dest: RwSignal::new(None),
            upload_input: NodeRef::new(),
            dragging: RwSignal::new(None),
            drop_target: RwSignal::new(None),
            listings: RwSignal::new(HashMap::new()),
            listings_gen: StoredValue::new(0),
        }
    }

    /// Persist view preferences (call from an Effect; reads are tracked).
    pub fn save_prefs(&self) {
        let (sort, asc) = self.sort.get();
        let prefs = Prefs {
            view: self.view.get(),
            zoom: self.zoom.get(),
            hidden: self.show_hidden.get(),
            previews: self.previews.get(),
            places: self.places_open.get(),
            sort,
            asc,
        };
        if let (Some(s), Ok(raw)) = (storage(), serde_json::to_string(&prefs)) {
            let _ = s.set_item(PREFS_KEY, &raw);
        }
    }

    pub fn read_only(&self) -> bool {
        self.info.with_untracked(|i| i.as_ref().map(|i| i.read_only).unwrap_or(false))
    }

    pub fn active_index(&self) -> usize {
        self.active.get_untracked()
    }

    // -- messages -----------------------------------------------------------

    pub fn notify(&self, kind: MsgKind, text: impl Into<String>) {
        let text = text.into();
        self.message.set(Some((kind, text.clone())));
        if kind == MsgKind::Info {
            let msg = self.message;
            set_timeout(
                move || {
                    let still_same = msg.with_untracked(|m| m.as_ref().map(|(_, t)| *t == text).unwrap_or(false));
                    if still_same {
                        msg.set(None);
                    }
                },
                Duration::from_secs(4),
            );
        }
    }

    pub fn error(&self, text: impl Into<String>) {
        self.notify(MsgKind::Error, text)
    }

    fn report(&self, rep: &OpReport, success: Option<String>) {
        if rep.errors.is_empty() {
            if let Some(s) = success {
                self.notify(MsgKind::Info, s);
            }
        } else {
            let mut text = rep.errors.iter().take(3).cloned().collect::<Vec<_>>().join(" · ");
            if rep.errors.len() > 3 {
                text.push_str(&format!(" · and {} more", rep.errors.len() - 3));
            }
            self.error(text);
        }
    }

    // -- loading & navigation ----------------------------------------------

    pub fn refresh_info(&self) {
        let info = self.info;
        let st = *self;
        spawn_local(async move {
            match api::info().await {
                Ok(i) => info.set(Some(i)),
                Err(e) => st.error(format!("Could not reach the server: {e}")),
            }
        });
    }

    pub fn load(&self, i: usize) {
        let p = self.panes[i];
        let path = p.path.get_untracked();
        let seq = p.load_seq.get_untracked().wrapping_add(1);
        p.load_seq.set(seq);
        p.loading.set(true);
        let st = *self;
        spawn_local(async move {
            let res = api::list(&path).await;
            if p.load_seq.get_untracked() != seq {
                return; // A newer navigation superseded this one.
            }
            p.loading.set(false);
            match res {
                Ok(listing) => {
                    let existing: HashSet<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
                    p.selected.update(|s| s.retain(|x| existing.contains(x.as_str())));
                    p.error.set(None);
                    if listing.path != path {
                        p.path.set(listing.path.clone());
                    }
                    st.cache_listing(listing.path.clone(), Col::Ready(listing.entries.clone()));
                    p.loaded.set(listing.path);
                    p.entries.set(listing.entries);
                }
                Err(e) => {
                    p.entries.set(Vec::new());
                    p.loaded.set(path);
                    p.error.set(Some(e));
                }
            }
        });
    }

    pub fn refresh_all(&self) {
        self.refresh_listings();
        self.load(0);
        if self.split.get_untracked() {
            self.load(1);
        }
        self.refresh_info();
        for i in 0..2 {
            if self.panes[i].results.with_untracked(|r| r.is_some()) {
                self.run_search(i);
            }
        }
    }

    /// Show `path` in pane `i` without touching history.
    pub fn enter(&self, i: usize, path: String) {
        let p = self.panes[i];
        p.path.set(path);
        p.selected.set(HashSet::new());
        p.anchor.set(None);
        p.cursor.set(None);
        // Only notify when something changes; views rebuild on these.
        if p.results.with_untracked(|r| r.is_some()) {
            p.results.set(None);
        }
        if p.search.with_untracked(|r| r.is_some()) {
            p.search.set(None);
        }
        if p.filter.with_untracked(|r| r.is_some()) {
            p.filter.set(None);
        }
        p.editing_location.set(false);
        self.renaming.set(None);
        self.load(i);
    }

    pub fn navigate(&self, i: usize, path: String) {
        let p = self.panes[i];
        let current = p.path.get_untracked();
        if current != path {
            p.back.update(|b| {
                b.push(current);
                if b.len() > 100 {
                    b.remove(0);
                }
            });
            p.forward.set(Vec::new());
        }
        self.enter(i, path);
    }

    pub fn go_back(&self, i: usize) {
        let p = self.panes[i];
        let Some(prev) = p.back.with_untracked(|b| b.last().cloned()) else { return };
        let current = p.path.get_untracked();
        p.back.update(|b| {
            b.pop();
        });
        p.forward.update(|f| f.push(current.clone()));
        self.enter(i, prev);
        self.select_only(i, &current);
    }

    pub fn go_forward(&self, i: usize) {
        let p = self.panes[i];
        let Some(next) = p.forward.with_untracked(|f| f.last().cloned()) else { return };
        let current = p.path.get_untracked();
        p.forward.update(|f| {
            f.pop();
        });
        p.back.update(|b| b.push(current));
        self.enter(i, next);
    }

    /// Go to the parent folder and select the folder we came from.
    pub fn go_up(&self, i: usize) {
        let current = self.panes[i].path.get_untracked();
        if current == "/" {
            return;
        }
        self.navigate(i, vparent(&current));
        self.select_only(i, &current);
    }

    pub fn toggle_split(&self) {
        if self.split.get_untracked() {
            if self.active_index() == 1 {
                let path = self.panes[1].path.get_untracked();
                self.enter(0, path);
            }
            self.active.set(0);
            self.split.set(false);
        } else {
            let path = self.panes[self.active_index()].path.get_untracked();
            self.enter(1, path);
            self.split.set(true);
        }
    }

    pub fn open_in_other(&self, i: usize, path: String) {
        let other = 1 - i;
        if !self.split.get_untracked() {
            self.split.set(true);
            self.enter(other, path);
        } else {
            self.navigate(other, path);
        }
        self.active.set(other);
    }

    pub fn open(&self, i: usize, e: &Entry) {
        if e.is_dir() {
            self.navigate(i, e.path.clone());
        } else {
            let _ = window().open_with_url_and_target(&api::download_url(&e.path, true), "_blank");
        }
    }

    pub fn open_selected(&self, i: usize) {
        let sel = self.selected_entries(i);
        if let Some(dir) = sel.iter().find(|e| e.is_dir()) {
            self.navigate(i, dir.path.clone());
        } else {
            for e in sel.iter().take(10) {
                self.open(i, e);
            }
        }
    }

    pub fn download(&self, entries: &[Entry]) {
        let doc = document();
        for e in entries.iter().filter(|e| !e.is_dir()) {
            if let Ok(el) = doc.create_element("a") {
                let a: web_sys::HtmlAnchorElement = el.unchecked_into();
                a.set_href(&api::download_url(&e.path, false));
                a.set_download(&e.name);
                a.click();
            }
        }
    }

    pub fn copy_location(&self, path: &str) {
        let clipboard = window().navigator().clipboard();
        let _ = clipboard.write_text(path);
        self.notify(MsgKind::Info, format!("Copied “{path}” to the clipboard"));
    }

    /// Current path from the URL hash (`#/some/folder`).
    pub fn hash_path(&self) -> Option<String> {
        let hash = window().location().hash().ok()?;
        let raw = hash.strip_prefix('#')?;
        let decoded: String = js_sys::decode_uri_component(raw).ok()?.into();
        decoded.starts_with('/').then_some(decoded)
    }

    /// Mirror the active pane's folder in the URL hash and page title.
    pub fn sync_location(&self, path: &str) {
        let encoded: String = js_sys::encode_uri(path).into();
        if self.hash_path().as_deref() != Some(path) {
            let _ = window().location().replace(&format!("#{encoded}"));
        }
        let name = if path == "/" {
            self.info.with_untracked(|i| i.as_ref().map(|i| i.name.clone())).unwrap_or_else(|| "Storage".into())
        } else {
            vname(path).to_string()
        };
        document().set_title(&format!("{name} — Narwhal"));
    }

    // -- folder listings (column view) -----------------------------------------

    fn cache_listing(&self, path: String, col: Col) {
        self.listings.update(|m| {
            if m.len() >= LISTING_CACHE_MAX {
                m.clear();
            }
            m.insert(path, col);
        });
    }

    /// Fetch `path` into the listing cache unless it is there already.
    pub fn ensure_listing(&self, path: &str) {
        if self.listings.with_untracked(|m| m.contains_key(path)) {
            return;
        }
        self.cache_listing(path.to_string(), Col::Loading);
        self.fetch_listing(path);
    }

    /// After a change: forget cached folders that aren't on screen, and
    /// re-fetch the ones that are, keeping the old rows until the new arrive.
    fn refresh_listings(&self) {
        self.listings_gen.update_value(|g| *g = g.wrapping_add(1));
        let mut shown = HashSet::new();
        let mut current = HashSet::new();
        let panes = if self.split.get_untracked() { 2 } else { 1 };
        for i in 0..panes {
            let p = self.panes[i];
            let path = p.path.get_untracked();
            shown.extend(util::ancestors(&path));
            if let [one] = self.selected_paths(i).as_slice() {
                shown.insert(one.clone());
            }
            current.insert(path);
        }
        self.listings.update(|m| m.retain(|k, _| shown.contains(k)));
        let cached: Vec<String> = self.listings.with_untracked(|m| m.keys().cloned().collect());
        for path in cached.into_iter().filter(|k| !current.contains(k)) {
            self.fetch_listing(&path);
        }
    }

    fn fetch_listing(&self, path: &str) {
        let st = *self;
        let path = path.to_string();
        let gen = self.listings_gen.get_value();
        spawn_local(async move {
            let col = match api::list(&path).await {
                Ok(l) => Col::Ready(l.entries),
                Err(e) => Col::Failed(e),
            };
            // A refresh in the meantime invalidated this request.
            if st.listings_gen.try_get_value() == Some(gen) {
                st.cache_listing(path, col);
            }
        });
    }

    /// Raw listing of `folder`: the pane's own data if it is showing that
    /// folder, otherwise the cache. None while loading.
    fn listing_for(&self, i: usize, folder: &str) -> Option<Result<Vec<Entry>, String>> {
        let p = self.panes[i];
        if p.loaded.with(|l| l == folder) && p.path.with(|c| c == folder) {
            return Some(match p.error.get() {
                Some(e) => Err(e),
                None => Ok(p.entries.get()),
            });
        }
        match self.listings.with(|m| m.get(folder).cloned()) {
            Some(Col::Ready(v)) => Some(Ok(v)),
            Some(Col::Failed(e)) => Some(Err(e)),
            Some(Col::Loading) | None => None,
        }
    }

    /// Sorted, hidden-filtered contents of one column. The pane's current
    /// folder also gets the filter bar applied (it is `visible`).
    pub fn column_entries(&self, i: usize, folder: &str) -> Option<Result<Vec<Entry>, String>> {
        if self.panes[i].path.with(|c| c == folder) {
            return match self.listing_for(i, folder)? {
                Ok(_) => Some(Ok(self.visible(i))),
                Err(e) => Some(Err(e)),
            };
        }
        let show_hidden = self.show_hidden.get();
        let (key, asc) = self.sort.get();
        self.listing_for(i, folder).map(|r| {
            r.map(|mut v| {
                v.retain(|e| show_hidden || !e.hidden);
                sort_entries(&mut v, key, asc);
                v
            })
        })
    }

    /// Column view: step into the selected folder and select its first item.
    pub fn column_enter(&self, i: usize) {
        let sel = self.selected_entries(i);
        let [dir] = sel.as_slice() else { return };
        if !dir.is_dir() {
            return;
        }
        let path = dir.path.clone();
        self.navigate(i, path.clone());
        let st = *self;
        let first = untrack(move || st.column_entries(i, &path))
            .and_then(|r| r.ok())
            .and_then(|v| v.into_iter().next());
        if let Some(first) = first {
            self.select_only(i, &first.path);
        }
    }

    // -- the visible list ------------------------------------------------------

    /// Entries shown in pane `i`: hidden/filter applied, sorted folders-first.
    pub fn visible(&self, i: usize) -> Vec<Entry> {
        let p = self.panes[i];
        let show_hidden = self.show_hidden.get();
        let (key, asc) = self.sort.get();
        let filter = p.filter.get().unwrap_or_default().to_lowercase();
        let mut v = match p.results.get() {
            Some(r) => r,
            None => {
                let path = p.path.get();
                self.listing_for(i, &path).and_then(|r| r.ok()).unwrap_or_default()
            }
        };
        v.retain(|e| (show_hidden || !e.hidden) && (filter.is_empty() || e.name.to_lowercase().contains(&filter)));
        sort_entries(&mut v, key, asc);
        v
    }

    pub fn visible_untracked(&self, i: usize) -> Vec<Entry> {
        let st = *self;
        untrack(move || st.visible(i))
    }

    pub fn selected_entries(&self, i: usize) -> Vec<Entry> {
        let vis = self.visible_untracked(i);
        self.panes[i].selected.with_untracked(|s| vis.into_iter().filter(|e| s.contains(&e.path)).collect())
    }

    pub fn selected_paths(&self, i: usize) -> Vec<String> {
        self.selected_entries(i).into_iter().map(|e| e.path).collect()
    }

    // -- selection ------------------------------------------------------------

    pub fn select_only(&self, i: usize, path: &str) {
        let p = self.panes[i];
        p.selected.set(HashSet::from([path.to_string()]));
        p.anchor.set(Some(path.to_string()));
        p.cursor.set(Some(path.to_string()));
    }

    pub fn toggle(&self, i: usize, path: &str) {
        let p = self.panes[i];
        p.selected.update(|s| {
            if !s.remove(path) {
                s.insert(path.to_string());
            }
        });
        p.anchor.set(Some(path.to_string()));
        p.cursor.set(Some(path.to_string()));
    }

    pub fn select_range(&self, i: usize, path: &str) {
        let p = self.panes[i];
        let vis = self.visible_untracked(i);
        let anchor = p.anchor.get_untracked().unwrap_or_else(|| path.to_string());
        let a = vis.iter().position(|e| e.path == anchor);
        let b = vis.iter().position(|e| e.path == path);
        match (a, b) {
            (Some(a), Some(b)) => {
                let (lo, hi) = (a.min(b), a.max(b));
                p.selected.set(vis[lo..=hi].iter().map(|e| e.path.clone()).collect());
                p.cursor.set(Some(path.to_string()));
            }
            _ => self.select_only(i, path),
        }
    }

    pub fn select_all(&self, i: usize) {
        let all = self.visible_untracked(i).into_iter().map(|e| e.path).collect();
        self.panes[i].selected.set(all);
    }

    pub fn clear_selection(&self, i: usize) {
        let p = self.panes[i];
        if p.selected.with_untracked(|s| !s.is_empty()) {
            p.selected.set(HashSet::new());
        }
    }

    /// Keyboard navigation: move the cursor by `delta` positions.
    pub fn move_cursor(&self, i: usize, delta: i64, extend: bool) {
        let p = self.panes[i];
        let vis = self.visible_untracked(i);
        if vis.is_empty() {
            return;
        }
        let cur = p.cursor.get_untracked().and_then(|c| vis.iter().position(|e| e.path == c));
        let next = match cur {
            None => 0,
            Some(c) => (c as i64 + delta).clamp(0, vis.len() as i64 - 1) as usize,
        };
        let path = vis[next].path.clone();
        if extend {
            self.select_range(i, &path);
        } else {
            self.select_only(i, &path);
        }
        scroll_item_into_view(i, next);
    }

    // -- file operations --------------------------------------------------------

    pub fn copy(&self, i: usize, cut: bool) {
        let paths = self.selected_paths(i);
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.clip.set(Some(Clip { paths, cut }));
        let verb = if cut { "Cut" } else { "Copied" };
        self.notify(MsgKind::Info, format!("{verb} {} — paste with Ctrl+V", util::plural(n, "item", "items")));
    }

    pub fn paste(&self, i: usize, dest: Option<String>) {
        let Some(clip) = self.clip.get_untracked() else { return };
        let dest = dest.unwrap_or_else(|| self.panes[i].path.get_untracked());
        if clip.cut {
            self.clip.set(None);
        }
        let op = if clip.cut { TransferOp::Move } else { TransferOp::Copy };
        self.transfer(clip.paths, dest, op);
    }

    pub fn transfer(&self, paths: Vec<String>, dest: String, op: TransferOp) {
        if self.read_only() || paths.is_empty() {
            return;
        }
        let st = *self;
        let n = paths.len();
        spawn_local(async move {
            match api::transfer(paths, &dest, op).await {
                Ok(rep) => {
                    let verb = if op == TransferOp::Copy { "Copied" } else { "Moved" };
                    let place = if dest == "/" { "the root folder".to_string() } else { format!("“{}”", vname(&dest)) };
                    st.report(&rep, Some(format!("{verb} {} to {place}", util::plural(n, "item", "items"))));
                }
                Err(e) => st.error(e),
            }
            st.refresh_all();
        });
    }

    pub fn ask_delete(&self, i: usize) {
        if self.read_only() {
            return;
        }
        let paths = self.selected_paths(i);
        if !paths.is_empty() {
            self.dialog.set(Some(Dialog::ConfirmDelete(paths)));
        }
    }

    pub fn delete(&self, paths: Vec<String>) {
        let st = *self;
        let n = paths.len();
        spawn_local(async move {
            match api::delete(paths).await {
                Ok(rep) => st.report(&rep, Some(format!("Deleted {}", util::plural(n, "item", "items")))),
                Err(e) => st.error(e),
            }
            st.refresh_all();
        });
    }

    pub fn mkdir(&self, i: usize, name: String) {
        self.create(i, name, true)
    }

    pub fn touch(&self, i: usize, name: String) {
        self.create(i, name, false)
    }

    fn create(&self, i: usize, name: String, folder: bool) {
        let st = *self;
        let dir = self.panes[i].path.get_untracked();
        spawn_local(async move {
            let res = if folder { api::mkdir(&dir, &name).await } else { api::touch(&dir, &name).await };
            match res {
                Ok(path) => {
                    st.select_only(i, &path);
                    st.refresh_all();
                }
                Err(e) => st.error(e),
            }
        });
    }

    pub fn start_rename(&self, i: usize) {
        if self.read_only() {
            return;
        }
        let sel = self.selected_paths(i);
        if sel.len() == 1 {
            self.renaming.set(Some((i, sel[0].clone())));
        }
    }

    pub fn rename(&self, i: usize, path: String, new_name: String) {
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() || new_name == vname(&path) {
            return;
        }
        let st = *self;
        spawn_local(async move {
            match api::rename(&path, &new_name).await {
                Ok(new_path) => {
                    st.select_only(i, &new_path);
                    st.refresh_all();
                }
                Err(e) => st.error(e),
            }
        });
    }

    /// Open the browser's picker for files, or for a whole folder.
    pub fn pick_upload(&self, i: usize, folder: bool) {
        if self.read_only() {
            return;
        }
        self.upload_dest.set(Some(self.panes[i].path.get_untracked()));
        if let Some(input) = self.upload_input.get_untracked() {
            input.set_webkitdirectory(folder);
            input.click();
        }
    }

    /// Upload files, each given with its path relative to `dest`.
    pub fn upload(&self, dest: String, files: Vec<(String, web_sys::File)>) {
        if files.is_empty() {
            return;
        }
        if self.read_only() {
            self.error("This storage is read-only");
            return;
        }
        let tops: HashSet<&str> = files.iter().map(|(p, _)| p.split('/').next().unwrap_or("")).collect();
        let label = match (files.len(), tops.len()) {
            (1, _) => vname(&files[0].0).to_string(),
            (n, 1) if files[0].0.contains('/') => format!("{} ({n} files)", files[0].0.split('/').next().unwrap_or("")),
            (n, _) => format!("{n} files"),
        };
        let up = self.upload;
        up.set(Some(Upload { label, progress: 0.0 }));
        let st = *self;
        spawn_local(async move {
            let res = api::upload(&dest, files, move |f| {
                up.update(|u| {
                    if let Some(u) = u {
                        u.progress = f;
                    }
                })
            })
            .await;
            up.set(None);
            match res {
                Ok(rep) => {
                    let msg = format!("Uploaded {}", util::plural(rep.done as usize, "file", "files"));
                    st.report(&rep, Some(msg));
                }
                Err(e) => st.error(e),
            }
            st.refresh_all();
        });
    }

    /// Handle a drop of internal items (move, or copy with Ctrl) or of files
    /// from the desktop (upload) onto folder `dest`.
    pub fn handle_drop(&self, ev: &web_sys::DragEvent, dest: String) {
        self.drop_target.set(None);
        let copy = ev.ctrl_key();
        if let Some(paths) = self.dragging.get_untracked() {
            self.dragging.set(None);
            let paths: Vec<String> =
                paths.into_iter().filter(|p| *p != dest && (copy || vparent(p) != dest)).collect();
            let op = if copy { TransferOp::Copy } else { TransferOp::Move };
            self.transfer(paths, dest, op);
            return;
        }
        let Some(dt) = ev.data_transfer() else { return };
        // Folders can only be read through entries; fall back to the plain
        // file list in browsers without them.
        let entries = util::dropped_entries(&dt);
        if entries.is_empty() {
            let files = dt.files().map(|fl| util::upload_items(&fl)).unwrap_or_default();
            self.upload(dest, files);
            return;
        }
        let st = *self;
        spawn_local(async move {
            match util::read_entries(entries).await {
                Ok(files) if files.is_empty() => st.error("Nothing to upload: the dropped folders are empty"),
                Ok(files) => st.upload(dest, files),
                Err(e) => st.error(e),
            }
        });
    }

    // -- search ----------------------------------------------------------------

    pub fn open_search(&self, i: usize) {
        let p = self.panes[i];
        if p.search.with_untracked(|s| s.is_none()) {
            p.search.set(Some(String::new()));
        }
    }

    pub fn run_search(&self, i: usize) {
        let p = self.panes[i];
        let q = p.search.get_untracked().unwrap_or_default();
        if q.trim().is_empty() {
            p.results.set(None);
            return;
        }
        let path = p.path.get_untracked();
        let st = *self;
        p.loading.set(true);
        spawn_local(async move {
            match api::search(&path, &q).await {
                Ok(r) => {
                    p.selected.set(HashSet::new());
                    p.results.set(Some(r));
                }
                Err(e) => st.error(e),
            }
            p.loading.set(false);
        });
    }

    pub fn close_search(&self, i: usize) {
        let p = self.panes[i];
        p.search.set(None);
        if p.results.with_untracked(|r| r.is_some()) {
            p.results.set(None);
        }
    }

    pub fn zoom_step(&self, up: bool) {
        let z = self.zoom.get_untracked();
        let next = if up {
            ZOOM_STEPS.iter().copied().find(|s| *s > z).unwrap_or(192)
        } else {
            ZOOM_STEPS.iter().rev().copied().find(|s| *s < z).unwrap_or(32)
        };
        self.zoom.set(next);
    }
}

/// Number of columns in the icon grid of pane `i` (1 for details view).
pub fn grid_columns(i: usize) -> i64 {
    document()
        .query_selector(&format!(".pane[data-pane='{i}'] .grid"))
        .ok()
        .flatten()
        .and_then(|el| window().get_computed_style(&el).ok().flatten())
        .and_then(|cs| cs.get_property_value("grid-template-columns").ok())
        .map(|v| v.split_whitespace().count().max(1) as i64)
        .unwrap_or(1)
}

pub fn scroll_item_into_view(i: usize, pos: usize) {
    if let Ok(Some(el)) = document().query_selector(&format!(".pane[data-pane='{i}'] [data-pos='{pos}']")) {
        let opts = web_sys::ScrollIntoViewOptions::new();
        opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
        el.scroll_into_view_with_scroll_into_view_options(&opts);
    }
}

/// Folders first, then by `key`.
pub fn sort_entries(v: &mut [Entry], key: SortKey, asc: bool) {
    v.sort_by(|a, b| {
        b.is_dir().cmp(&a.is_dir()).then_with(|| {
            let o = match key {
                SortKey::Name => util::natural_cmp(&a.name, &b.name),
                SortKey::Size => {
                    let (sa, sb) = if a.is_dir() {
                        (a.children.unwrap_or(0) as u64, b.children.unwrap_or(0) as u64)
                    } else {
                        (a.size, b.size)
                    };
                    sa.cmp(&sb).then_with(|| util::natural_cmp(&a.name, &b.name))
                }
                SortKey::Modified => a.modified.cmp(&b.modified),
                SortKey::Type => util::type_label(a)
                    .cmp(&util::type_label(b))
                    .then_with(|| util::natural_cmp(&a.name, &b.name)),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        })
    });
}
