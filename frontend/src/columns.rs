//! Finder-style column view.
//!
//! One column per folder from the root down to the pane's current folder.
//! The selection lives in the current (right-most real) column. If exactly
//! one folder is selected, its contents appear as one more column on the
//! right; if exactly one file is selected, a preview panel does. Clicking an
//! item in any column makes that column current and selects the item.

use crate::{
    api, icons,
    state::*,
    ui::{use_st, RenameBox},
    util::{self, format_date, format_size, size_text, type_label, PreviewKind},
};
use leptos::{ev, html, prelude::*, task::spawn_local};
use narwhal_shared::Entry;

#[component]
pub fn ColumnsView(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];

    // The single selected item of the current folder, if any.
    let picked = Memo::new(move |_| {
        let sel = p.selected.get();
        if sel.len() != 1 {
            return None;
        }
        let path = p.path.get();
        st.column_entries(idx, &path)
            .and_then(|r| r.ok())
            .and_then(|v| v.into_iter().find(|e| sel.contains(&e.path)))
    });
    let folders = Memo::new(move |_| {
        let mut f = util::ancestors(&p.path.get());
        if let Some(e) = picked.get().filter(|e| e.is_dir()) {
            f.push(e.path);
        }
        f
    });

    // Keep the newest column in view.
    let strip = NodeRef::<html::Div>::new();
    Effect::new(move |_| {
        folders.track();
        picked.track();
        // Layout changes that narrow the strip.
        st.split.track();
        st.places_open.track();
        // Hold the element itself: the frame may run after this view is gone.
        if let Some(el) = strip.get_untracked() {
            request_animation_frame(move || el.set_scroll_left(el.scroll_width()));
        }
    });

    view! {
        <div class="columns" node_ref=strip role="tree">
            <For
                each=move || folders.get()
                key=|f| f.clone()
                children=move |folder: String| view! { <Column idx=idx folder=folder/> }
            />
            {move || {
                picked
                    .get()
                    .filter(|e| !e.is_dir())
                    .map(|e| view! { <FilePreview idx=idx entry=e/> })
            }}
        </div>
    }
}

/// Drag-and-drop target handlers for a folder (a folder row or a whole column).
fn drop_handlers(
    st: AppState,
    idx: usize,
    key: String,
    dest: StoredValue<String>,
    enabled: bool,
) -> (impl Fn(ev::DragEvent) + 'static, impl Fn(ev::DragEvent) + 'static) {
    let over_key = key.clone();
    let over = move |ev: ev::DragEvent| {
        if !enabled {
            return; // Bubble to the column.
        }
        let onto_self = dest.with_value(|d| {
            st.dragging.with_untracked(|g| g.as_ref().map(|g| g.contains(d)).unwrap_or(false))
        });
        if onto_self {
            return;
        }
        ev.prevent_default();
        ev.stop_propagation();
        if let Some(dt) = ev.data_transfer() {
            let internal = st.dragging.with_untracked(|d| d.is_some());
            dt.set_drop_effect(if !internal || ev.ctrl_key() { "copy" } else { "move" });
        }
        if st.drop_target.with_untracked(|d| d.as_deref() != Some(over_key.as_str())) {
            st.drop_target.set(Some(over_key.clone()));
        }
    };
    let drop = move |ev: ev::DragEvent| {
        if !enabled || st.drop_target.with_untracked(|d| d.as_deref() != Some(key.as_str())) {
            return;
        }
        ev.prevent_default();
        ev.stop_propagation();
        st.active.set(idx);
        st.handle_drop(&ev, dest.get_value());
    };
    (over, drop)
}

#[component]
fn Column(idx: usize, folder: String) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let f = StoredValue::new(folder.clone());

    let entries = Memo::new(move |_| f.with_value(|f| st.column_entries(idx, f)));
    let is_current = move || p.path.with(|c| f.with_value(|f| c == f));
    // The entry of this column that leads to the current folder.
    let trail = Memo::new(move |_| {
        let cur = p.path.get();
        f.with_value(|f| util::child_on_path(f, &cur))
    });

    // Load the listing if nobody has yet. The current folder is loaded by
    // the pane itself.
    Effect::new(move |_| {
        if entries.with(|e| e.is_none()) && !is_current() {
            f.with_value(|f| st.ensure_listing(f));
        }
    });

    let on_click = move |ev: ev::MouseEvent| {
        ev.stop_propagation();
        st.active.set(idx);
        if !is_current() {
            st.navigate(idx, f.get_value());
        } else {
            st.clear_selection(idx);
        }
    };
    let on_menu = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        ev.stop_propagation();
        st.active.set(idx);
        if !is_current() {
            st.navigate(idx, f.get_value());
        }
        st.clear_selection(idx);
        st.menu.set(Some(Menu { x: ev.client_x(), y: ev.client_y(), pane: idx, target: None }));
    };
    let key = format!("col:{folder}");
    let (on_dragover, on_drop) = drop_handlers(st, idx, key.clone(), f, true);
    let is_drop = move || st.drop_target.with(|d| d.as_deref() == Some(key.as_str()));

    let rows = move || match entries.get() {
        None => view! { <div class="col-note">"Loading…"</div> }.into_any(),
        Some(Err(e)) => view! { <div class="col-note error">{e}</div> }.into_any(),
        Some(Ok(v)) if v.is_empty() => {
            let filtered = is_current() && p.filter.with(|t| t.as_deref().is_some_and(|t| !t.is_empty()));
            let text = if filtered { "No items match the filter" } else { "Empty folder" };
            view! { <div class="col-note">{text}</div> }.into_any()
        }
        Some(Ok(v)) => v
            .into_iter()
            .enumerate()
            .map(|(pos, entry)| view! { <ColumnRow idx=idx folder=f trail=trail pos=pos entry=entry/> })
            .collect_view()
            .into_any(),
    };

    view! {
        <div class="column" class:current=is_current class:droptarget=is_drop
            role="group" aria-label=folder.clone()
            on:click=on_click on:contextmenu=on_menu on:dragover=on_dragover on:drop=on_drop>
            {rows}
        </div>
    }
}

#[component]
fn ColumnRow(
    idx: usize,
    folder: StoredValue<String>,
    trail: Memo<Option<String>>,
    pos: usize,
    entry: Entry,
) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let path = StoredValue::new(entry.path.clone());
    let is_dir = entry.is_dir();

    let is_current = move || p.path.with(|c| folder.with_value(|f| c == f));
    let selected = move || is_current() && path.with_value(|x| p.selected.with(|s| s.contains(x)));
    let on_trail = move || path.with_value(|x| trail.with(|t| t.as_deref() == Some(x.as_str())));
    let is_cursor = move || {
        is_current() && st.active.get() == idx && path.with_value(|x| p.cursor.with(|c| c.as_deref() == Some(x.as_str())))
    };
    let is_cut = move || {
        path.with_value(|x| st.clip.with(|c| c.as_ref().is_some_and(|c| c.cut && c.paths.contains(x))))
    };
    let is_drop = move || path.with_value(|x| st.drop_target.with(|d| d.as_deref() == Some(x.as_str())));
    let renaming = move || {
        path.with_value(|x| st.renaming.with(|r| matches!(r, Some((i, rp)) if *i == idx && rp == x)))
    };

    let on_click = move |ev: ev::MouseEvent| {
        ev.stop_propagation();
        st.active.set(idx);
        let target = path.get_value();
        if !is_current() {
            st.navigate(idx, folder.get_value());
            st.select_only(idx, &target);
        } else if ev.shift_key() {
            st.select_range(idx, &target);
        } else if ev.ctrl_key() || ev.meta_key() {
            st.toggle(idx, &target);
        } else {
            st.select_only(idx, &target);
        }
    };
    let open_entry = entry.clone();
    let on_dbl = move |ev: ev::MouseEvent| {
        ev.stop_propagation();
        st.open(idx, &open_entry);
    };
    let menu_entry = entry.clone();
    let on_menu = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        ev.stop_propagation();
        st.active.set(idx);
        let target = path.get_value();
        if !is_current() {
            st.navigate(idx, folder.get_value());
            st.select_only(idx, &target);
        } else if !p.selected.with_untracked(|s| s.contains(&target)) {
            st.select_only(idx, &target);
        }
        st.menu.set(Some(Menu { x: ev.client_x(), y: ev.client_y(), pane: idx, target: Some(menu_entry.clone()) }));
    };
    let on_dragstart = move |ev: ev::DragEvent| {
        let target = path.get_value();
        // Outside the current column, drag just this item (without
        // navigating, which would rebuild the columns under the pointer).
        let dragged = if is_current() {
            if !p.selected.with_untracked(|s| s.contains(&target)) {
                st.select_only(idx, &target);
            }
            st.selected_paths(idx)
        } else {
            vec![target.clone()]
        };
        st.dragging.set(Some(dragged));
        if let Some(dt) = ev.data_transfer() {
            let _ = dt.set_data("text/plain", &target);
            dt.set_effect_allowed("copyMove");
        }
    };
    let (on_dragover, on_drop) = drop_handlers(st, idx, entry.path.clone(), path, is_dir);

    let name = entry.name.clone();
    let label = move || {
        if renaming() {
            view! { <RenameBox idx=idx path=path.get_value() name=name.clone()/> }.into_any()
        } else {
            view! { <span class="cname">{name.clone()}</span> }.into_any()
        }
    };

    view! {
        <div class="crow" role="treeitem"
            class:selected=selected class:trail=on_trail class:cursor=is_cursor class:cut=is_cut
            class:droptarget=is_drop class:dim={entry.hidden}
            aria-selected=move || if selected() || on_trail() { "true" } else { "false" }
            data-pos=move || is_current().then(|| pos.to_string())
            title=entry.path.clone()
            draggable=move || if renaming() { "false" } else { "true" }
            on:click=on_click on:dblclick=on_dbl on:contextmenu=on_menu
            on:dragstart=on_dragstart on:dragover=on_dragover on:drop=on_drop>
            <span class="cicon" inner_html=icons::file_icon(&entry)></span>
            {label}
            {is_dir.then(|| view! { <span class="chev ic" inner_html=icons::CHEVRON></span> })}
        </div>
    }
}

/// How much of a text file the preview reads.
const TEXT_PREVIEW_BYTES: u64 = 64 * 1024;

/// Details of the selected file, shown as the last column, with its
/// contents for the kinds the browser can show (images, media, PDF, text).
#[component]
fn FilePreview(idx: usize, entry: Entry) -> impl IntoView {
    let st = use_st();
    let kind = util::preview_kind(&entry).filter(|_| st.previews.get_untracked());
    let url = api::download_url(&entry.path, true);
    let icon = || view! { <span class="fi-wrap" inner_html=icons::file_icon(&entry)></span> };
    let wide = matches!(kind, Some(PreviewKind::Video | PreviewKind::Pdf | PreviewKind::Text));
    let big = match kind {
        Some(PreviewKind::Image) => view! { <div class="big"><img src=url alt="" draggable="false"/></div> }.into_any(),
        Some(PreviewKind::Video) => view! {
            <div class="big media"><video src=url controls preload="metadata"></video></div>
        }
        .into_any(),
        Some(PreviewKind::Audio) => view! {
            <div class="big">{icon()}</div>
            <audio class="media" src=url controls preload="metadata"></audio>
        }
        .into_any(),
        Some(PreviewKind::Pdf) => view! {
            <div class="big media"><iframe class="pdf" src=url title="PDF preview"></iframe></div>
        }
        .into_any(),
        Some(PreviewKind::Text) => view! { <TextPreview entry=entry.clone()/> }.into_any(),
        None => view! { <div class="big">{icon()}</div> }.into_any(),
    };
    let size = format!("{} ({} bytes)", format_size(entry.size), entry.size);
    let modified = entry.modified.map(format_date).unwrap_or_else(|| "Unknown".into());
    let perms = entry.mode.map(|m| util::mode_string(m, false)).unwrap_or_default();
    let open_entry = entry.clone();
    let dl_entry = entry.clone();

    view! {
        <aside class="preview" class:wide=wide aria-label="Preview"
            on:click=|ev: ev::MouseEvent| ev.stop_propagation()
            on:contextmenu=|ev: ev::MouseEvent| ev.stop_propagation()>
            {big}
            <h3>{entry.name.clone()}</h3>
            <div class="muted">{type_label(&entry)}{" — "}{size_text(&entry)}</div>
            <dl>
                <dt>"Size"</dt><dd>{size}</dd>
                <dt>"Modified"</dt><dd>{modified}</dd>
                <dt>"Permissions"</dt><dd class="mono">{perms}</dd>
            </dl>
            <div class="actions">
                <button class="btn" on:click=move |_| st.open(idx, &open_entry)>
                    <span class="ic" inner_html=icons::OPEN></span>"Open"
                </button>
                <button class="btn" on:click=move |_| st.download(std::slice::from_ref(&dl_entry))>
                    <span class="ic" inner_html=icons::DOWNLOAD></span>"Download"
                </button>
            </div>
        </aside>
    }
}

/// The start of a text file, read with a range request.
#[component]
fn TextPreview(entry: Entry) -> impl IntoView {
    // None while loading; Some(Err) for binary or unreadable files.
    let text = RwSignal::new(None::<Result<String, String>>);
    if entry.size == 0 {
        text.set(Some(Ok(String::new())));
    } else {
        let path = entry.path.clone();
        spawn_local(async move {
            let res = api::head_bytes(&path, TEXT_PREVIEW_BYTES)
                .await
                .and_then(|b| util::preview_text(&b).ok_or_else(|| "Binary file — no text preview".into()));
            let _ = text.try_set(Some(res));
        });
    }
    let cut = entry.size > TEXT_PREVIEW_BYTES;
    move || match text.get() {
        None => view! { <pre class="textprev muted">"Loading…"</pre> }.into_any(),
        Some(Err(e)) => view! { <div class="col-note">{e}</div> }.into_any(),
        Some(Ok(t)) if t.is_empty() => view! { <div class="col-note">"Empty file"</div> }.into_any(),
        Some(Ok(t)) => view! {
            <pre class="textprev">{t}{cut.then_some("\n…")}</pre>
        }
        .into_any(),
    }
}
