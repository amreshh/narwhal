//! The Trash view, shown when a pane's location is `TRASH`. Its selection
//! uses the pane's `selected`/`anchor`/`cursor`, holding Trash ids instead
//! of paths.

use crate::{
    icons,
    state::*,
    ui::{mi, use_st},
    util::{self, format_date, size_text},
};
use leptos::{ev, prelude::*};
use narwhal_shared::{vparent, TrashItem};
use std::collections::HashSet;

fn items(st: AppState) -> Vec<TrashItem> {
    st.trash.with_untracked(|t| t.as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default())
}

fn ids(st: AppState) -> Vec<String> {
    items(st).into_iter().map(|t| t.id).collect()
}

pub fn selected_ids(st: AppState, i: usize) -> Vec<String> {
    let sel = st.panes[i].selected.get_untracked();
    ids(st).into_iter().filter(|id| sel.contains(id)).collect()
}

fn select_only(st: AppState, i: usize, id: &str) {
    let p = st.panes[i];
    p.selected.set(HashSet::from([id.to_string()]));
    p.anchor.set(Some(id.to_string()));
    p.cursor.set(Some(id.to_string()));
}

fn select_range(st: AppState, i: usize, id: &str) {
    let p = st.panes[i];
    let all = ids(st);
    let anchor = p.anchor.get_untracked().unwrap_or_else(|| id.to_string());
    match (all.iter().position(|x| *x == anchor), all.iter().position(|x| x == id)) {
        (Some(a), Some(b)) => {
            let (lo, hi) = (a.min(b), a.max(b));
            p.selected.set(all[lo..=hi].iter().cloned().collect());
            p.cursor.set(Some(id.to_string()));
        }
        _ => select_only(st, i, id),
    }
}

pub fn select_all(st: AppState, i: usize) {
    st.panes[i].selected.set(ids(st).into_iter().collect());
}

/// Keyboard navigation in the Trash list.
pub fn move_cursor(st: AppState, i: usize, delta: i64, extend: bool) {
    let all = ids(st);
    if all.is_empty() {
        return;
    }
    let p = st.panes[i];
    let cur = p.cursor.get_untracked().and_then(|c| all.iter().position(|x| *x == c));
    let next = match cur {
        None => 0,
        Some(c) => (c as i64 + delta).clamp(0, all.len() as i64 - 1) as usize,
    };
    if extend {
        select_range(st, i, &all[next]);
    } else {
        select_only(st, i, &all[next]);
    }
    scroll_item_into_view(i, next);
}

/// Context menu for the Trash (on an item or on empty space).
pub fn trash_menu(st: AppState, i: usize) -> AnyView {
    let sel = selected_ids(st, i);
    let n = sel.len();
    let any = !items(st).is_empty();
    let sel2 = sel.clone();
    view! {
        {(n > 0).then(|| mi(st, "Restore", "", icons::RESTORE, move || st.restore(sel.clone())))}
        {(n > 0).then(|| mi(st, "Delete Permanently", "Del", icons::TRASH,
            move || st.dialog.set(Some(Dialog::ConfirmPurge(sel2.clone())))))}
        {(n > 0).then(|| view! { <div class="msep"></div> })}
        {mi(st, "Select All", "Ctrl+A", icons::SELECT_ALL, move || select_all(st, i))}
        {mi(st, "Refresh", "F5", icons::REFRESH, move || st.refresh_all())}
        {any.then(|| view! { <div class="msep"></div> })}
        {any.then(|| mi(st, "Empty Trash", "", icons::TRASH, move || st.dialog.set(Some(Dialog::ConfirmEmptyTrash))))}
    }
    .into_any()
}

#[component]
pub fn TrashView(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let has_sel = move || p.selected.with(|s| !s.is_empty());
    let has_items = move || st.trash.with(|t| matches!(t, Some(Ok(v)) if !v.is_empty()));

    let rows = move || match st.trash.get() {
        None => view! { <div class="placeholder">"Loading…"</div> }.into_any(),
        Some(Err(e)) => view! { <div class="placeholder error">{e}</div> }.into_any(),
        Some(Ok(v)) if v.is_empty() => view! { <div class="placeholder">"The Trash is empty"</div> }.into_any(),
        Some(Ok(v)) => v
            .into_iter()
            .enumerate()
            .map(|(pos, item)| view! { <TrashRow idx=idx pos=pos item=item/> })
            .collect_view()
            .into_any(),
    };

    view! {
        <div class="trashhead" on:click=|ev: ev::MouseEvent| ev.stop_propagation()>
        <div class="trashbar">
            <span class="hint">"Items here can be restored to where they were deleted from."</span>
            <div class="spacer"></div>
            <button class="btn" disabled=move || !has_sel()
                on:click=move |_| st.restore(selected_ids(st, idx))>
                <span class="ic" inner_html=icons::RESTORE></span>"Restore"
            </button>
            <button class="btn" disabled=move || !has_sel()
                on:click=move |_| st.ask_delete(idx, true)>
                <span class="ic" inner_html=icons::TRASH></span>"Delete Permanently"
            </button>
            <button class="btn" disabled=move || !has_items()
                on:click=move |_| st.dialog.set(Some(Dialog::ConfirmEmptyTrash))>
                "Empty Trash"
            </button>
        </div>
        <div class="dheader trash">
            <span class="hcol name">"Name"</span>
            <span class="hcol where">"Original Location"</span>
            <span class="hcol date">"Deleted"</span>
            <span class="hcol size">"Size"</span>
        </div>
        </div>
        <div class="rows trash" role="grid">{rows}</div>
    }
}

#[component]
fn TrashRow(idx: usize, pos: usize, item: TrashItem) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let id = StoredValue::new(item.id.clone());
    let selected = move || id.with_value(|id| p.selected.with(|s| s.contains(id)));
    let is_cursor =
        move || st.active.get() == idx && id.with_value(|id| p.cursor.with(|c| c.as_deref() == Some(id.as_str())));

    let on_click = move |ev: ev::MouseEvent| {
        ev.stop_propagation();
        st.active.set(idx);
        let id = id.get_value();
        if ev.shift_key() {
            select_range(st, idx, &id);
        } else if ev.ctrl_key() || ev.meta_key() {
            p.selected.update(|s| {
                if !s.remove(&id) {
                    s.insert(id.clone());
                }
            });
            p.anchor.set(Some(id.clone()));
            p.cursor.set(Some(id));
        } else {
            select_only(st, idx, &id);
        }
    };
    let on_dbl = move |ev: ev::MouseEvent| {
        ev.stop_propagation();
        st.restore(vec![id.get_value()]);
    };
    let on_menu = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        ev.stop_propagation();
        st.active.set(idx);
        if !selected() {
            select_only(st, idx, &id.get_value());
        }
        st.menu.set(Some(Menu { x: ev.client_x(), y: ev.client_y(), pane: idx, target: None }));
    };

    let e = &item.entry;
    let origin = vparent(&e.path);
    let origin_label = if origin == "/" {
        st.info.with_untracked(|i| i.as_ref().map(|i| i.name.clone())).unwrap_or("/".into())
    } else {
        origin.clone()
    };
    view! {
        <div class="item" role="gridcell" class:selected=selected class:cursor=is_cursor class:dim={e.hidden}
            aria-selected=move || if selected() { "true" } else { "false" }
            data-pos=pos.to_string() title=format!("{} — double-click to restore", e.path)
            on:click=on_click on:dblclick=on_dbl on:contextmenu=on_menu>
            <div class="icon"><span class="fi-wrap" inner_html=icons::file_icon(e)></span></div>
            <div class="label"><span class="name">{e.name.clone()}</span></div>
            <span class="col where" title=origin>{origin_label}</span>
            <span class="col date">{format_date(item.deleted)}</span>
            <span class="col size">{size_text(e)}</span>
        </div>
    }
}

/// Status bar text for the Trash.
pub fn summary(st: AppState, i: usize) -> String {
    let all = st.trash.with(|t| t.as_ref().and_then(|r| r.as_ref().ok()).map(|v| v.len()).unwrap_or(0));
    let n = st.panes[i].selected.with(|s| s.len());
    if n > 0 {
        format!("{n} of {} selected", util::plural(all, "item", "items"))
    } else {
        format!("{} in the Trash", util::plural(all, "item", "items"))
    }
}
