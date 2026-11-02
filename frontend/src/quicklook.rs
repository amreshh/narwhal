//! Quick Look: Space shows the selected item large, like in Finder. While
//! it is open the arrow keys keep moving the selection, and the preview
//! follows.

use crate::{
    api,
    columns::TextPreview,
    icons,
    office::OfficePreview,
    state::*,
    ui::use_st,
    util::{self, format_date, size_text, type_label, PreviewKind},
};
use leptos::{ev, prelude::*};
use narwhal_shared::Entry;

/// How much of a text file Quick Look reads.
const TEXT_BYTES: u64 = 512 * 1024;

/// Keys that keep Quick Look open (they move the selection).
pub fn keeps_open(key: &str) -> bool {
    matches!(key, "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End" | "PageUp" | "PageDown")
}

/// The item Quick Look shows in pane `i`: the cursor item if it is
/// selected, otherwise the only selected item. With its position and the
/// number of items. Tracked.
fn shown(st: AppState, i: usize) -> Option<(usize, usize, Entry)> {
    let p = st.panes[i];
    let vis = st.visible(i);
    let sel = p.selected.get();
    let path = p
        .cursor
        .get()
        .filter(|c| sel.contains(c))
        .or_else(|| (sel.len() == 1).then(|| sel.iter().next().cloned()).flatten())?;
    let pos = vis.iter().position(|e| e.path == path)?;
    Some((pos, vis.len(), vis[pos].clone()))
}

/// Space: open Quick Look for the selection, or close it.
pub fn toggle(st: AppState) {
    if st.quicklook.get_untracked() {
        st.quicklook.set(false);
    } else if !st.in_trash(st.active_index()) && untrack(|| shown(st, st.active_index())).is_some() {
        st.quicklook.set(true);
    }
}

#[component]
pub fn QuickLook() -> impl IntoView {
    let st = use_st();
    let item = Memo::new(move |_| shown(st, st.active.get()));
    // Nothing left to show (e.g. the selection was cleared): close.
    Effect::new(move |_| {
        if item.with(|i| i.is_none()) {
            st.quicklook.set(false);
        }
    });
    let step = move |delta: i64| st.move_cursor(st.active_index(), delta, false);

    move || {
        let (pos, count, entry) = item.get()?;
        let kind = util::preview_kind(&entry);
        let url = api::download_url(&entry.path, true);
        let body = match kind {
            Some(PreviewKind::Image) => view! { <img class="ql-img" src=url alt="" draggable="false"/> }.into_any(),
            Some(PreviewKind::Video) => view! {
                <video class="ql-video" src=url controls autoplay preload="metadata"></video>
            }
            .into_any(),
            Some(PreviewKind::Audio) => view! {
                <div class="ql-info">
                    <span class="ql-icon" inner_html=icons::file_icon(&entry)></span>
                    <audio src=url controls autoplay preload="metadata"></audio>
                </div>
            }
            .into_any(),
            Some(PreviewKind::Pdf) => view! { <iframe class="ql-pdf" src=url title="PDF preview"></iframe> }.into_any(),
            Some(PreviewKind::Text) => view! {
                <div class="ql-text"><TextPreview entry=entry.clone() limit=TEXT_BYTES/></div>
            }
            .into_any(),
            Some(PreviewKind::Office) => view! {
                <div class="ql-office"><OfficePreview entry=entry.clone()/></div>
            }
            .into_any(),
            None => {
                let modified = entry.modified.map(format_date).unwrap_or_default();
                view! {
                    <div class="ql-info">
                        <span class="ql-icon" inner_html=icons::file_icon(&entry)></span>
                        <div class="ql-type">{type_label(&entry)}</div>
                        <div class="muted">{size_text(&entry)}</div>
                        <div class="muted">{format!("Modified {modified}")}</div>
                    </div>
                }
                .into_any()
            }
        };
        let (open_entry, dl_entry) = (entry.clone(), entry.clone());
        let is_file = !entry.is_dir();
        Some(view! {
            <div class="ql-backdrop" on:mousedown=move |ev: ev::MouseEvent| {
                if ev.target() == ev.current_target() {
                    st.quicklook.set(false);
                }
            }>
                <button class="ql-nav prev" title="Previous (←/↑)" aria-label="Previous"
                    disabled={pos == 0} on:click=move |_| step(-1)>
                    <span class="ic" inner_html=icons::BACK></span>
                </button>
                <div class="ql" role="dialog" aria-label=format!("Quick Look: {}", entry.name)>
                    <div class="ql-head">
                        <span class="ql-title" title=entry.path.clone()>{entry.name.clone()}</span>
                        <span class="ql-count">{format!("{} of {count}", pos + 1)}</span>
                        <button class="btn small" on:click=move |_| {
                            st.quicklook.set(false);
                            st.open(st.active_index(), &open_entry);
                        }>
                            <span class="ic" inner_html=icons::OPEN></span>"Open"
                        </button>
                        {is_file.then(|| view! {
                            <button class="btn small" on:click=move |_| st.download(std::slice::from_ref(&dl_entry))>
                                <span class="ic" inner_html=icons::DOWNLOAD></span>"Download"
                            </button>
                        })}
                        <button class="tb" title="Close (Space or Esc)" aria-label="Close"
                            on:click=move |_| st.quicklook.set(false)>
                            <span class="ic" inner_html=icons::CLOSE></span>
                        </button>
                    </div>
                    <div class="ql-body">{body}</div>
                </div>
                <button class="ql-nav next" title="Next (→/↓)" aria-label="Next"
                    disabled={pos + 1 >= count} on:click=move |_| step(1)>
                    <span class="ic" inner_html=icons::FORWARD></span>
                </button>
            </div>
        })
    }
}
