//! Leptos components: toolbar, places panel, split panes, item views,
//! context menu, dialogs and status bar.

use crate::columns::ColumnsView;
use crate::{
    api, icons,
    state::*,
    util::{self, format_date, format_size, size_text, type_label},
};
use leptos::{ev, html, prelude::*, task::spawn_local};
use narwhal_shared::{vname, vparent, Entry};
use wasm_bindgen::JsCast;

pub(crate) fn use_st() -> AppState {
    expect_context::<AppState>()
}

// ===========================================================================
// App shell
// ===========================================================================

#[component]
pub fn App() -> impl IntoView {
    let st = AppState::new();
    provide_context(st);

    // Initial folder comes from the URL hash so links and reloads work.
    let start = st.hash_path().unwrap_or_else(|| "/".into());
    st.enter(0, start);
    st.refresh_info();

    Effect::new(move |_| st.save_prefs());
    Effect::new(move |_| {
        let i = st.active.get();
        let path = st.panes[i].path.get();
        st.info.track();
        st.sync_location(&path);
    });

    let handles = (
        window_event_listener(ev::keydown, move |e| on_key(st, e)),
        window_event_listener(ev::hashchange, move |_| {
            if let Some(path) = st.hash_path() {
                let i = st.active_index();
                if path != st.panes[i].path.get_untracked() {
                    st.navigate(i, path);
                }
            }
        }),
        window_event_listener(ev::mousedown, move |e: ev::MouseEvent| {
            // Mouse back/forward buttons, like in a desktop file manager.
            match e.button() {
                3 => st.go_back(st.active_index()),
                4 => st.go_forward(st.active_index()),
                _ => {}
            }
            let in_menu = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|el| el.closest(".menu, .hamburger").ok().flatten())
                .is_some();
            if !in_menu {
                st.menu.set(None);
                st.hamburger.set(false);
            }
        }),
        window_event_listener(ev::dragend, move |_| {
            st.dragging.set(None);
            st.drop_target.set(None);
        }),
    );
    // The listeners live for the whole lifetime of the page.
    std::mem::forget(handles);

    let on_files = move |_: ev::Event| {
        if let Some(input) = st.upload_input.get_untracked() {
            let files = input.files().map(|fl| util::upload_items(&fl)).unwrap_or_default();
            if let Some(dest) = st.upload_dest.get_untracked() {
                st.upload(dest, files);
            }
            input.set_value("");
        }
    };

    view! {
        <div class="app">
            <Toolbar/>
            <MessageBar/>
            <div class="body">
                <Show when=move || st.places_open.get()>
                    <Places/>
                </Show>
                <main class="views" class:split=move || st.split.get()>
                    <PaneView idx=0/>
                    <Show when=move || st.split.get()>
                        <PaneView idx=1/>
                    </Show>
                </main>
            </div>
            <StatusBar/>
            <ContextMenu/>
            <DialogHost/>
            <input type="file" multiple=true class="hidden-input" node_ref=st.upload_input on:change=on_files/>
        </div>
    }
}

fn on_key(st: AppState, ev: web_sys::KeyboardEvent) {
    let key = ev.key();
    let ctrl = ev.ctrl_key() || ev.meta_key();
    let shift = ev.shift_key();
    let alt = ev.alt_key();
    let in_input = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .map(|el| matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"))
        .unwrap_or(false);
    let i = st.active_index();

    if key == "Escape" {
        if st.menu.get_untracked().is_some() || st.hamburger.get_untracked() {
            st.menu.set(None);
            st.hamburger.set(false);
        } else if st.dialog.with_untracked(|d| d.is_some()) {
            st.dialog.set(None);
        } else if !in_input {
            st.clear_selection(i);
        }
        return;
    }
    if in_input || st.dialog.with_untracked(|d| d.is_some()) {
        return;
    }
    st.menu.set(None);

    let view_mode = st.view.get_untracked();
    let column_view =
        view_mode == ViewMode::Columns && st.panes[i].results.with_untracked(|r| r.is_none());
    if column_view && !ctrl && !alt && !shift {
        let moved = match key.as_str() {
            "ArrowRight" => {
                st.column_enter(i);
                true
            }
            "ArrowLeft" => {
                st.go_up(i);
                true
            }
            _ => false,
        };
        if moved {
            ev.prevent_default();
            return;
        }
    }
    let cols = if matches!(view_mode, ViewMode::Details | ViewMode::Columns) { 1 } else { grid_columns(i) };
    let mut handled = true;
    match (key.as_str(), ctrl, shift, alt) {
        ("ArrowLeft", false, false, true) => st.go_back(i),
        ("ArrowRight", false, false, true) => st.go_forward(i),
        ("ArrowUp", false, false, true) | ("Backspace", false, false, false) => st.go_up(i),
        ("F5", ..) | ("r", true, false, false) => st.refresh_all(),
        ("F2", ..) => st.start_rename(i),
        ("Delete", ..) => st.ask_delete(i),
        ("a", true, false, false) => st.select_all(i),
        ("c", true, false, false) => st.copy(i, false),
        ("x", true, false, false) => {
            if !st.read_only() {
                st.copy(i, true)
            }
        }
        ("v", true, false, false) => st.paste(i, None),
        ("F3", ..) => st.toggle_split(),
        ("F7", ..) | ("N", true, true, false) => {
            if !st.read_only() {
                st.dialog.set(Some(Dialog::NewFolder(i)))
            }
        }
        ("1", true, ..) => st.view.set(ViewMode::Icons),
        ("2", true, ..) => st.view.set(ViewMode::Compact),
        ("3", true, ..) => st.view.set(ViewMode::Details),
        ("4", true, ..) => st.view.set(ViewMode::Columns),
        ("h", true, false, false) | (".", false, false, true) => st.show_hidden.update(|v| *v = !*v),
        ("l", true, false, false) | ("F6", ..) => st.panes[i].editing_location.set(true),
        ("i", true, false, false) => st.panes[i].filter.set(Some(String::new())),
        ("f", true, false, false) => st.open_search(i),
        ("F9", ..) => st.places_open.update(|v| *v = !*v),
        ("F1", ..) => st.dialog.set(Some(Dialog::Shortcuts)),
        ("Enter", false, false, true) => {
            if let [e] = st.selected_entries(i).as_slice() {
                st.dialog.set(Some(Dialog::Properties(e.clone())));
            }
        }
        ("Enter", ..) => st.open_selected(i),
        ("=" | "+", true, ..) => st.zoom_step(true),
        ("-", true, ..) => st.zoom_step(false),
        ("ArrowDown", false, s, false) => st.move_cursor(i, cols, s),
        ("ArrowUp", false, s, false) => st.move_cursor(i, -cols, s),
        ("ArrowRight", false, s, false) if cols > 1 => st.move_cursor(i, 1, s),
        ("ArrowLeft", false, s, false) if cols > 1 => st.move_cursor(i, -1, s),
        ("Home", false, s, false) => st.move_cursor(i, i64::MIN / 2, s),
        ("End", false, s, false) => st.move_cursor(i, i64::MAX / 2, s),
        ("PageDown", false, s, false) => st.move_cursor(i, cols * 10, s),
        ("PageUp", false, s, false) => st.move_cursor(i, -cols * 10, s),
        ("Tab", false, false, false) if st.split.get_untracked() => st.active.set(1 - i),
        _ => handled = false,
    }
    if handled {
        ev.prevent_default();
    }
}

// ===========================================================================
// Toolbar
// ===========================================================================

fn tool_button(
    icon: &'static str,
    title: &'static str,
    on_click: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <button class="tb" title=title aria-label=title on:click=move |_| on_click()>
            <span class="ic" inner_html=icon></span>
        </button>
    }
}

#[component]
fn Toolbar() -> impl IntoView {
    let st = use_st();
    let pane = move || st.panes[st.active.get()];
    let can_back = move || pane().back.with(|b| !b.is_empty());
    let can_fwd = move || pane().forward.with(|f| !f.is_empty());
    let at_root = move || pane().path.with(|p| p == "/");
    let writable = move || st.info.with(|i| i.as_ref().map(|i| !i.read_only).unwrap_or(false));

    let view_btn = move |mode: ViewMode, icon: &'static str, title: &'static str| {
        view! {
            <button class="tb" class:on=move || st.view.get() == mode title=title aria-label=title
                on:click=move |_| st.view.set(mode)>
                <span class="ic" inner_html=icon></span>
            </button>
        }
    };

    view! {
        <header class="toolbar">
            <div class="group">
                <button class="tb" title="Back (Alt+Left)" aria-label="Back" disabled=move || !can_back()
                    on:click=move |_| st.go_back(st.active_index())>
                    <span class="ic" inner_html=icons::BACK></span>
                </button>
                <button class="tb" title="Forward (Alt+Right)" aria-label="Forward" disabled=move || !can_fwd()
                    on:click=move |_| st.go_forward(st.active_index())>
                    <span class="ic" inner_html=icons::FORWARD></span>
                </button>
                <button class="tb" title="Up (Alt+Up)" aria-label="Up" disabled=at_root
                    on:click=move |_| st.go_up(st.active_index())>
                    <span class="ic" inner_html=icons::UP></span>
                </button>
            </div>

            <div class="group seg">
                {view_btn(ViewMode::Icons, icons::VIEW_ICONS, "Icons (Ctrl+1)")}
                {view_btn(ViewMode::Compact, icons::VIEW_COMPACT, "Compact (Ctrl+2)")}
                {view_btn(ViewMode::Details, icons::VIEW_DETAILS, "Details (Ctrl+3)")}
                {view_btn(ViewMode::Columns, icons::VIEW_COLUMNS, "Columns (Ctrl+4)")}
            </div>

            <div class="spacer"></div>

            <Show when=writable>
                <div class="group">
                    <button class="tb labeled" title="Create a new folder (F7)"
                        on:click=move |_| st.dialog.set(Some(Dialog::NewFolder(st.active_index())))>
                        <span class="ic" inner_html=icons::NEW_FOLDER></span><span class="tl">"New Folder"</span>
                    </button>
                    <button class="tb labeled" title="Upload files into this folder"
                        on:click=move |_| st.pick_upload(st.active_index(), false)>
                        <span class="ic" inner_html=icons::UPLOAD></span><span class="tl">"Upload"</span>
                    </button>
                    <button class="tb labeled" title="Upload a folder with its contents into this folder"
                        on:click=move |_| st.pick_upload(st.active_index(), true)>
                        <span class="ic" inner_html=icons::UPLOAD_FOLDER></span><span class="tl">"Upload Folder"</span>
                    </button>
                </div>
            </Show>

            <div class="group">
                <button class="tb hide-sm" class:on=move || st.previews.get() title="Show previews" aria-label="Show previews"
                    on:click=move |_| st.previews.update(|v| *v = !*v)>
                    <span class="ic" inner_html=icons::PREVIEW></span>
                </button>
                <button class="tb labeled" class:on=move || st.split.get() title="Split view (F3)"
                    on:click=move |_| st.toggle_split()>
                    <span class="ic" inner_html=icons::SPLIT></span><span class="tl">"Split"</span>
                </button>
                <button class="tb" class:on=move || pane().search.with(|s| s.is_some()) title="Search (Ctrl+F)"
                    aria-label="Search"
                    on:click=move |_| {
                        let i = st.active_index();
                        if st.panes[i].search.with_untracked(|s| s.is_some()) { st.close_search(i) } else { st.open_search(i) }
                    }>
                    <span class="ic" inner_html=icons::SEARCH></span>
                </button>
            </div>

            <div class="hamburger">
                <button class="tb" class:on=move || st.hamburger.get() title="Menu" aria-label="Menu"
                    on:click=move |_| st.hamburger.update(|v| *v = !*v)>
                    <span class="ic" inner_html=icons::MENU></span>
                </button>
                <Show when=move || st.hamburger.get()>
                    <HamburgerMenu/>
                </Show>
            </div>
        </header>
    }
}

#[component]
fn HamburgerMenu() -> impl IntoView {
    let st = use_st();
    let i = st.active_index();
    view! {
        <div class="menu popup" role="menu">
            {mcheck(st, "Show Hidden Files", "Ctrl+H", st.show_hidden.get_untracked(), move || st.show_hidden.update(|v| *v = !*v))}
            {mcheck(st, "Show Previews", "", st.previews.get_untracked(), move || st.previews.update(|v| *v = !*v))}
            {mcheck(st, "Show Places Panel", "F9", st.places_open.get_untracked(), move || st.places_open.update(|v| *v = !*v))}
            {mcheck(st, "Split View", "F3", st.split.get_untracked(), move || st.toggle_split())}
            <div class="msep"></div>
            {mi(st, "Filter…", "Ctrl+I", icons::FILTER, move || st.panes[i].filter.set(Some(String::new())))}
            {mi(st, "Search…", "Ctrl+F", icons::SEARCH, move || st.open_search(i))}
            {mi(st, "Edit Location", "Ctrl+L", icons::RENAME, move || st.panes[i].editing_location.set(true))}
            {mi(st, "Refresh", "F5", icons::REFRESH, move || st.refresh_all())}
            <div class="msep"></div>
            {mi(st, "Keyboard Shortcuts", "F1", icons::KEYBOARD, move || st.dialog.set(Some(Dialog::Shortcuts)))}
        </div>
    }
}

#[component]
fn MessageBar() -> impl IntoView {
    let st = use_st();
    move || {
        st.message.get().map(|(kind, text)| {
            view! {
                <div class="msgbar" class:error={kind == MsgKind::Error} role="status">
                    <span class="ic" inner_html=icons::INFO></span>
                    <span class="msgtext">{text}</span>
                    {tool_button(icons::CLOSE, "Dismiss", move || st.message.set(None))}
                </div>
            }
        })
    }
}

// ===========================================================================
// Places panel
// ===========================================================================

#[component]
fn Places() -> impl IntoView {
    let st = use_st();
    let places = move || st.info.with(|i| i.as_ref().map(|i| i.places.clone()).unwrap_or_default());
    let current = move || st.panes[st.active.get()].path.get();

    view! {
        <aside class="places" aria-label="Places">
            <div class="section">"Places"</div>
            {move || {
                places()
                    .into_iter()
                    .enumerate()
                    .map(|(n, place)| {
                        let icon = if n == 0 { icons::DRIVE } else { icons::FOLDER };
                        let (p1, p2, p3, p4) = (place.path.clone(), place.path.clone(), place.path.clone(), place.path.clone());
                        let p5 = place.path.clone();
                        view! {
                            <button class="place"
                                class:current=move || current() == p1
                                class:droptarget=move || st.drop_target.with(|d| d.as_deref() == Some(p2.as_str()))
                                title=place.path.clone()
                                on:click=move |_| st.navigate(st.active_index(), p3.clone())
                                on:dragover=move |ev: ev::DragEvent| {
                                    ev.prevent_default();
                                    if st.drop_target.with_untracked(|d| d.as_deref() != Some(p4.as_str())) {
                                        st.drop_target.set(Some(p4.clone()));
                                    }
                                }
                                on:drop=move |ev: ev::DragEvent| {
                                    ev.prevent_default();
                                    st.handle_drop(&ev, p5.clone());
                                }>
                                <span class="ic" inner_html=icon></span>
                                <span class="label">{place.name}</span>
                            </button>
                        }
                    })
                    .collect_view()
            }}
            <div class="grow"></div>
            <div class="section">"Storage"</div>
            <DiskMeter/>
        </aside>
    }
}

#[component]
fn DiskMeter(#[prop(optional)] compact: bool) -> impl IntoView {
    let st = use_st();
    move || {
        st.info.with(|i| i.as_ref().and_then(|i| i.disk)).map(|d| {
            let used = d.total.saturating_sub(d.available);
            let pct = if d.total > 0 { used as f64 * 100.0 / d.total as f64 } else { 0.0 };
            let title = format!("{} free of {} ({pct:.0}% used)", format_size(d.available), format_size(d.total));
            view! {
                <div class="disk" class:compact=compact title=title>
                    <span class="disk-label">{format!("{} free", format_size(d.available))}</span>
                    <div class="meter">
                        <div class="fill" class:warn={pct > 90.0} style=format!("width:{pct:.1}%")></div>
                    </div>
                </div>
            }
        })
    }
}

// ===========================================================================
// Pane: location bar + view
// ===========================================================================

#[component]
fn PaneView(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let items = Memo::new(move |_| st.visible(idx));
    let pane_key = format!("pane:{idx}");

    let on_bg_click = move |ev: ev::MouseEvent| {
        st.active.set(idx);
        if !(ev.ctrl_key() || ev.shift_key() || ev.meta_key()) {
            st.clear_selection(idx);
        }
    };
    let on_bg_menu = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        st.active.set(idx);
        st.clear_selection(idx);
        st.menu.set(Some(Menu { x: ev.client_x(), y: ev.client_y(), pane: idx, target: None }));
    };
    let key1 = pane_key.clone();
    let on_dragover = move |ev: ev::DragEvent| {
        ev.prevent_default();
        if let Some(dt) = ev.data_transfer() {
            let internal = st.dragging.with_untracked(|d| d.is_some());
            dt.set_drop_effect(if !internal || ev.ctrl_key() { "copy" } else { "move" });
        }
        if st.drop_target.with_untracked(|d| d.as_deref() != Some(key1.as_str())) {
            st.drop_target.set(Some(key1.clone()));
        }
    };
    let on_dragleave = move |ev: ev::DragEvent| {
        // relatedTarget is null when the pointer leaves the window.
        if ev.related_target().is_none() {
            st.drop_target.set(None);
        }
    };
    let on_drop = move |ev: ev::DragEvent| {
        ev.prevent_default();
        st.active.set(idx);
        st.handle_drop(&ev, p.path.get_untracked());
    };
    let key2 = pane_key.clone();
    let is_drop = move || st.drop_target.with(|d| d.as_deref() == Some(key2.as_str()));
    // Search results are a flat list, so they're shown as details instead.
    // A memo, so it only notifies when the answer actually changes.
    let column_view = Memo::new(move |_| st.view.get() == ViewMode::Columns && p.results.with(|r| r.is_none()));

    let list = move || {
        items
            .get()
            .into_iter()
            .enumerate()
            .map(|(pos, entry)| view! { <Item idx=idx pos=pos entry=entry/> })
            .collect_view()
    };

    view! {
        <section class="pane" class:active=move || st.active.get() == idx data-pane=idx.to_string()
            on:mousedown=move |_| st.active.set(idx)>
            <LocationBar idx=idx/>
            <Show when=move || p.search.with(|s| s.is_some())>
                <SearchBar idx=idx/>
            </Show>
            <div class="loadbar" class:on=move || p.loading.get()></div>
            <div class="view" class:droptarget=is_drop class:cols=column_view tabindex="-1"
                style=move || format!("--icon:{}px", st.zoom.get())
                on:click=on_bg_click on:contextmenu=on_bg_menu
                on:dragover=on_dragover on:dragleave=on_dragleave on:drop=on_drop>
                {move || match st.view.get() {
                    ViewMode::Columns if column_view.get() => view! { <ColumnsView idx=idx/> }.into_any(),
                    ViewMode::Details | ViewMode::Columns => view! {
                        <DetailsHeader/>
                        <div class="rows" role="grid">{list}</div>
                    }.into_any(),
                    mode => view! {
                        <div class="grid" class:compact={mode == ViewMode::Compact} role="grid">{list}</div>
                    }.into_any(),
                }}
                {move || {
                    if column_view.get() {
                        return None; // Each column shows its own state.
                    }
                    let empty = items.with(|v| v.is_empty());
                    if let Some(err) = p.error.get() {
                        Some(view! { <div class="placeholder error">{err}</div> }.into_any())
                    } else if empty && p.loading.get() {
                        Some(view! { <div class="placeholder">"Loading…"</div> }.into_any())
                    } else if empty {
                        let text = if p.results.with(|r| r.is_some()) {
                            "No matching files found"
                        } else if p.filter.with(|f| f.as_deref().map(|f| !f.is_empty()).unwrap_or(false)) {
                            "No items match the filter"
                        } else {
                            "This folder is empty"
                        };
                        Some(view! { <div class="placeholder">{text}</div> }.into_any())
                    } else {
                        None
                    }
                }}
            </div>
            <Show when=move || p.filter.with(|f| f.is_some())>
                <FilterBar idx=idx/>
            </Show>
        </section>
    }
}

#[component]
fn LocationBar(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let root_name = move || {
        st.info.with(|i| i.as_ref().map(|i| i.name.clone())).unwrap_or_else(|| "Storage".into())
    };
    let crumbs = move || {
        let path = p.path.get();
        let mut acc = String::new();
        path.split('/')
            .filter(|s| !s.is_empty())
            .map(|seg| {
                acc.push('/');
                acc.push_str(seg);
                (seg.to_string(), acc.clone())
            })
            .collect::<Vec<_>>()
    };
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if p.editing_location.get() {
            if let Some(el) = input.get() {
                let _ = el.focus();
                el.select();
            }
        }
    });

    view! {
        <div class="location" title="Click to edit the location (Ctrl+L)"
            on:click=move |_| p.editing_location.set(true)>
            {move || {
                if p.editing_location.get() {
                    view! {
                        <input class="loc-input" node_ref=input spellcheck="false" prop:value=move || p.path.get()
                            on:click=|ev: ev::MouseEvent| ev.stop_propagation()
                            on:keydown=move |ev: ev::KeyboardEvent| match ev.key().as_str() {
                                "Enter" => {
                                    let v = event_target_value(&ev);
                                    let v = if v.starts_with('/') { v } else { format!("/{v}") };
                                    p.editing_location.set(false);
                                    st.navigate(idx, v);
                                }
                                "Escape" => p.editing_location.set(false),
                                _ => {}
                            }
                            on:blur=move |_| p.editing_location.set(false)/>
                    }
                    .into_any()
                } else {
                    let list = crumbs();
                    let last = list.len();
                    view! {
                        <nav class="crumbs" aria-label="Location">
                            <button class="crumb root" class:current={last == 0}
                                on:click=move |ev: ev::MouseEvent| {
                                    ev.stop_propagation();
                                    st.navigate(idx, "/".into());
                                }>
                                <span class="ic" inner_html=icons::DRIVE></span>
                                <span>{root_name}</span>
                            </button>
                            {list
                                .into_iter()
                                .enumerate()
                                .map(|(n, (name, path))| {
                                    view! {
                                        <span class="sep ic" inner_html=icons::CHEVRON></span>
                                        <button class="crumb" class:current={n + 1 == last}
                                            on:click=move |ev: ev::MouseEvent| {
                                                ev.stop_propagation();
                                                st.navigate(idx, path.clone());
                                            }>
                                            {name}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </nav>
                    }
                    .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn SearchBar(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    });
    let count = move || {
        p.results.with(|r| r.as_ref().map(|r| util::plural(r.len(), "result", "results")))
    };
    view! {
        <div class="bar searchbar">
            <span class="ic" inner_html=icons::SEARCH></span>
            <input node_ref=input placeholder="Search file names in this folder and below — press Enter"
                prop:value=move || p.search.get().unwrap_or_default()
                on:input=move |ev: ev::Event| p.search.set(Some(event_target_value(&ev)))
                on:keydown=move |ev: ev::KeyboardEvent| match ev.key().as_str() {
                    "Enter" => st.run_search(idx),
                    "Escape" => st.close_search(idx),
                    _ => {}
                }/>
            <span class="hint">{count}</span>
            {tool_button(icons::CLOSE, "Close search", move || st.close_search(idx))}
        </div>
    }
}

#[component]
fn FilterBar(idx: usize) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    });
    view! {
        <div class="bar filterbar">
            <span class="ic" inner_html=icons::FILTER></span>
            <span class="hint">"Filter:"</span>
            <input node_ref=input placeholder="Type to filter the current view"
                prop:value=move || p.filter.get().unwrap_or_default()
                on:input=move |ev: ev::Event| p.filter.set(Some(event_target_value(&ev)))
                on:keydown=move |ev: ev::KeyboardEvent| {
                    if ev.key() == "Escape" {
                        p.filter.set(None);
                    }
                }/>
            {tool_button(icons::CLOSE, "Close filter", move || p.filter.set(None))}
        </div>
    }
}

#[component]
fn DetailsHeader() -> impl IntoView {
    let st = use_st();
    let head = move |key: SortKey, label: &'static str, class: &'static str| {
        view! {
            <button class=format!("hcol {class}")
                on:click=move |ev: ev::MouseEvent| {
                    ev.stop_propagation();
                    st.sort.update(|(k, asc)| {
                        if *k == key {
                            *asc = !*asc;
                        } else {
                            *k = key;
                            *asc = true;
                        }
                    });
                }>
                <span>{label}</span>
                <span class="arrow">
                    {move || {
                        let (k, asc) = st.sort.get();
                        if k != key { "" } else if asc { "▴" } else { "▾" }
                    }}
                </span>
            </button>
        }
    };
    view! {
        <div class="dheader" on:click=|ev: ev::MouseEvent| ev.stop_propagation()>
            {head(SortKey::Name, "Name", "name")}
            {head(SortKey::Size, "Size", "size")}
            {head(SortKey::Modified, "Modified", "date")}
            {head(SortKey::Type, "Type", "type")}
        </div>
    }
}

// ===========================================================================
// A single file/folder
// ===========================================================================

#[component]
fn Item(idx: usize, pos: usize, entry: Entry) -> impl IntoView {
    let st = use_st();
    let p = st.panes[idx];
    let path = entry.path.clone();
    let is_dir = entry.is_dir();

    let selected = {
        let path = path.clone();
        move || p.selected.with(|s| s.contains(&path))
    };
    let is_cursor = {
        let path = path.clone();
        move || p.cursor.with(|c| c.as_deref() == Some(path.as_str())) && st.active.get() == idx
    };
    let is_cut = {
        let path = path.clone();
        move || st.clip.with(|c| c.as_ref().map(|c| c.cut && c.paths.contains(&path)).unwrap_or(false))
    };
    let is_drop = {
        let path = path.clone();
        move || st.drop_target.with(|d| d.as_deref() == Some(path.as_str()))
    };
    let renaming = {
        let path = path.clone();
        move || st.renaming.with(|r| matches!(r, Some((i, rp)) if *i == idx && *rp == path))
    };

    let on_click = {
        let path = path.clone();
        move |ev: ev::MouseEvent| {
            ev.stop_propagation();
            st.active.set(idx);
            if ev.shift_key() {
                st.select_range(idx, &path);
            } else if ev.ctrl_key() || ev.meta_key() {
                st.toggle(idx, &path);
            } else {
                st.select_only(idx, &path);
            }
        }
    };
    let on_dbl = {
        let entry = entry.clone();
        move |ev: ev::MouseEvent| {
            ev.stop_propagation();
            st.open(idx, &entry);
        }
    };
    let on_menu = {
        let entry = entry.clone();
        move |ev: ev::MouseEvent| {
            ev.prevent_default();
            ev.stop_propagation();
            st.active.set(idx);
            if !p.selected.with_untracked(|s| s.contains(&entry.path)) {
                st.select_only(idx, &entry.path);
            }
            st.menu.set(Some(Menu { x: ev.client_x(), y: ev.client_y(), pane: idx, target: Some(entry.clone()) }));
        }
    };
    let on_dragstart = {
        let path = path.clone();
        move |ev: ev::DragEvent| {
            if !p.selected.with_untracked(|s| s.contains(&path)) {
                st.select_only(idx, &path);
            }
            st.dragging.set(Some(st.selected_paths(idx)));
            if let Some(dt) = ev.data_transfer() {
                let _ = dt.set_data("text/plain", &path);
                dt.set_effect_allowed("copyMove");
            }
        }
    };
    let on_dragover = {
        let path = path.clone();
        move |ev: ev::DragEvent| {
            if !is_dir {
                return; // Let it bubble: dropping on a file targets the pane's folder.
            }
            let onto_self = st.dragging.with_untracked(|d| d.as_ref().map(|d| d.contains(&path)).unwrap_or(false));
            if onto_self {
                return;
            }
            ev.prevent_default();
            ev.stop_propagation();
            if let Some(dt) = ev.data_transfer() {
                let internal = st.dragging.with_untracked(|d| d.is_some());
                dt.set_drop_effect(if !internal || ev.ctrl_key() { "copy" } else { "move" });
            }
            if st.drop_target.with_untracked(|d| d.as_deref() != Some(path.as_str())) {
                st.drop_target.set(Some(path.clone()));
            }
        }
    };
    let on_drop = {
        let path = path.clone();
        move |ev: ev::DragEvent| {
            if !is_dir || st.drop_target.with_untracked(|d| d.as_deref() != Some(path.as_str())) {
                return;
            }
            ev.prevent_default();
            ev.stop_propagation();
            st.active.set(idx);
            st.handle_drop(&ev, path.clone());
        }
    };

    let icon_svg = icons::file_icon(&entry);
    let thumb = util::is_thumbnailable(&entry).then(|| api::download_url(&entry.path, true));
    let icon = move || {
        let use_thumb = st.previews.get() && st.view.get() == ViewMode::Icons;
        match (&thumb, use_thumb) {
            (Some(url), true) => view! {
                <img class="thumb" loading="lazy" decoding="async" draggable="false" src=url.clone() alt=""/>
            }
            .into_any(),
            _ => view! { <span class="fi-wrap" inner_html=icon_svg.clone()></span> }.into_any(),
        }
    };

    let name = entry.name.clone();
    let rename_path = path.clone();
    let renaming2 = renaming.clone();
    let label = move || {
        if renaming() {
            view! { <RenameBox idx=idx path=rename_path.clone() name=name.clone()/> }.into_any()
        } else {
            view! { <span class="name">{name.clone()}</span> }.into_any()
        }
    };
    let subtitle = p
        .results
        .with_untracked(|r| r.is_some())
        .then(|| view! { <span class="where">{vparent(&entry.path)}</span> });

    view! {
        <div class="item" role="gridcell"
            class:selected=selected class:cursor=is_cursor class:cut=is_cut class:droptarget=is_drop
            class:dim={entry.hidden}
            aria-selected=move || if p.selected.with(|s| s.contains(&entry.path)) { "true" } else { "false" }
            data-pos=pos.to_string() title=path.clone()
            draggable=move || if renaming2() { "false" } else { "true" }
            on:click=on_click on:dblclick=on_dbl on:contextmenu=on_menu
            on:dragstart=on_dragstart on:dragover=on_dragover on:drop=on_drop>
            <div class="icon">{icon}</div>
            <div class="label">{label}{subtitle}</div>
            <span class="col size">{size_text(&entry)}</span>
            <span class="col date">{entry.modified.map(format_date).unwrap_or_default()}</span>
            <span class="col type">{type_label(&entry)}</span>
        </div>
    }
}

#[component]
pub fn RenameBox(idx: usize, path: String, name: String) -> impl IntoView {
    let st = use_st();
    let input = NodeRef::<html::Input>::new();
    let stem = util::stem_utf16_len(&name);
    let initial = name.clone();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            el.set_value(&initial);
            let _ = el.focus();
            let _ = el.set_selection_range(0, stem);
        }
    });
    // Enter and blur both commit; make sure only the first one counts. A plain
    // Rc (not a reactive value) because blur also fires while the input is
    // being removed, after this component's reactive scope is disposed.
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let done_esc = done.clone();
    let commit = move |value: String| {
        if done.replace(true) {
            return;
        }
        st.renaming.set(None);
        st.rename(idx, path.clone(), value);
    };
    let commit2 = commit.clone();
    view! {
        <input class="rename" node_ref=input value=name spellcheck="false"
            on:click=|ev: ev::MouseEvent| ev.stop_propagation()
            on:dblclick=|ev: ev::MouseEvent| ev.stop_propagation()
            on:mousedown=|ev: ev::MouseEvent| ev.stop_propagation()
            on:keydown=move |ev: ev::KeyboardEvent| {
                ev.stop_propagation();
                match ev.key().as_str() {
                    "Enter" => commit(event_target_value(&ev)),
                    "Escape" => {
                        done_esc.set(true);
                        st.renaming.set(None);
                    }
                    _ => {}
                }
            }
            on:blur=move |ev: ev::FocusEvent| commit2(event_target_value(&ev))/>
    }
}

// ===========================================================================
// Context menu
// ===========================================================================

fn mi(st: AppState, label: &'static str, keys: &'static str, icon: &'static str, f: impl Fn() + 'static) -> impl IntoView {
    view! {
        <button class="mi" role="menuitem"
            on:click=move |ev: ev::MouseEvent| {
                ev.stop_propagation();
                st.menu.set(None);
                st.hamburger.set(false);
                f();
            }>
            <span class="ic" inner_html=icon></span>
            <span class="ml">{label}</span>
            <span class="ks">{keys}</span>
        </button>
    }
}

fn mcheck(st: AppState, label: &'static str, keys: &'static str, checked: bool, f: impl Fn() + 'static) -> impl IntoView {
    mi(st, label, keys, if checked { icons::CHECK } else { icons::EMPTY }, f)
}

#[component]
fn ContextMenu() -> impl IntoView {
    let st = use_st();
    move || {
        st.menu.get().map(|m| {
            let ro = st.read_only();
            let w = window().inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1024.0) as i32;
            let h = window().inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(768.0) as i32;
            let x = m.x.min(w - 250).max(4);
            let y = m.y.min(h - 420).max(4);
            let body = match m.target {
                Some(e) => item_menu(st, m.pane, e, ro),
                None => background_menu(st, m.pane, ro),
            };
            view! {
                <div class="menu ctx" role="menu" style=format!("left:{x}px;top:{y}px")
                    on:contextmenu=|ev: ev::MouseEvent| ev.prevent_default()>
                    {body}
                </div>
            }
        })
    }
}

fn item_menu(st: AppState, i: usize, e: Entry, ro: bool) -> AnyView {
    let sel = st.selected_entries(i);
    let n = sel.len();
    let any_file = sel.iter().any(|x| !x.is_dir());
    let has_clip = st.clip.with_untracked(|c| c.is_some());
    let is_dir = e.is_dir();
    let (e1, e2, e3) = (e.clone(), e.clone(), e.clone());
    let p_other = e.path.clone();
    let p_paste = e.path.clone();

    let open = view! {
        {mi(st, if is_dir { "Open" } else { "Open in New Tab" }, "Enter", icons::OPEN, move || st.open(i, &e1))}
        {is_dir.then(|| mi(st, "Open in Split View", "", icons::SPLIT, move || st.open_in_other(i, p_other.clone())))}
        {any_file.then(|| mi(st, "Download", "", icons::DOWNLOAD, move || st.download(&sel)))}
    };
    let edit = view! {
        <div class="msep"></div>
        {(!ro).then(|| mi(st, "Cut", "Ctrl+X", icons::CUT, move || st.copy(i, true)))}
        {mi(st, "Copy", "Ctrl+C", icons::COPY, move || st.copy(i, false))}
        {(!ro && has_clip && is_dir).then(|| mi(st, "Paste Into Folder", "", icons::PASTE, move || st.paste(i, Some(p_paste.clone()))))}
        <div class="msep"></div>
        {(!ro && n == 1).then(|| mi(st, "Rename…", "F2", icons::RENAME, move || st.start_rename(i)))}
        {(!ro).then(|| mi(st, "Delete", "Del", icons::TRASH, move || st.ask_delete(i)))}
    };
    let more = view! {
        <div class="msep"></div>
        {mi(st, "Copy Location", "", icons::LINK, move || st.copy_location(&e2.path))}
        {(n == 1).then(|| mi(st, "Properties", "Alt+Enter", icons::INFO, move || st.dialog.set(Some(Dialog::Properties(e3.clone())))))}
    };
    view! { {open}{edit}{more} }.into_any()
}

fn background_menu(st: AppState, i: usize, ro: bool) -> AnyView {
    let has_clip = st.clip.with_untracked(|c| c.is_some());
    let view_mode = st.view.get_untracked();
    let (sort, _) = st.sort.get_untracked();
    let set_sort = move |k: SortKey| {
        st.sort.update(|(key, asc)| {
            if *key == k {
                *asc = !*asc;
            } else {
                *key = k;
                *asc = true;
            }
        })
    };

    let create = view! {
        {(!ro).then(|| mi(st, "New Folder…", "F7", icons::NEW_FOLDER, move || st.dialog.set(Some(Dialog::NewFolder(i)))))}
        {(!ro).then(|| mi(st, "New Text File…", "", icons::NEW_FILE, move || st.dialog.set(Some(Dialog::NewFile(i)))))}
        {(!ro).then(|| mi(st, "Upload Files…", "", icons::UPLOAD, move || st.pick_upload(i, false)))}
        {(!ro).then(|| mi(st, "Upload Folder…", "", icons::UPLOAD_FOLDER, move || st.pick_upload(i, true)))}
        {(!ro && has_clip).then(|| mi(st, "Paste", "Ctrl+V", icons::PASTE, move || st.paste(i, None)))}
        {(!ro).then(|| view! { <div class="msep"></div> })}
        {mi(st, "Select All", "Ctrl+A", icons::SELECT_ALL, move || st.select_all(i))}
        {mi(st, "Refresh", "F5", icons::REFRESH, move || st.refresh_all())}
    };
    let modes = view! {
        <div class="msep"></div>
        <div class="mlabel">"View Mode"</div>
        {mcheck(st, "Icons", "Ctrl+1", view_mode == ViewMode::Icons, move || st.view.set(ViewMode::Icons))}
        {mcheck(st, "Compact", "Ctrl+2", view_mode == ViewMode::Compact, move || st.view.set(ViewMode::Compact))}
        {mcheck(st, "Details", "Ctrl+3", view_mode == ViewMode::Details, move || st.view.set(ViewMode::Details))}
        {mcheck(st, "Columns", "Ctrl+4", view_mode == ViewMode::Columns, move || st.view.set(ViewMode::Columns))}
    };
    let sorting = view! {
        <div class="mlabel">"Sort By"</div>
        {mcheck(st, "Name", "", sort == SortKey::Name, move || set_sort(SortKey::Name))}
        {mcheck(st, "Size", "", sort == SortKey::Size, move || set_sort(SortKey::Size))}
        {mcheck(st, "Modified", "", sort == SortKey::Modified, move || set_sort(SortKey::Modified))}
        {mcheck(st, "Type", "", sort == SortKey::Type, move || set_sort(SortKey::Type))}
        <div class="msep"></div>
        {mcheck(st, "Show Hidden Files", "Ctrl+H", st.show_hidden.get_untracked(), move || st.show_hidden.update(|v| *v = !*v))}
    };
    view! { {create}{modes}{sorting} }.into_any()
}

// ===========================================================================
// Dialogs
// ===========================================================================

#[component]
fn DialogHost() -> impl IntoView {
    let st = use_st();
    move || {
        st.dialog.get().map(|d| {
            let body = match d {
                Dialog::NewFolder(i) => view! {
                    <NameDialog i=i folder=true title="Create New Folder" label="Folder name" initial="New Folder"/>
                }
                .into_any(),
                Dialog::NewFile(i) => view! {
                    <NameDialog i=i folder=false title="Create New Text File" label="File name" initial="New Text File.txt"/>
                }
                .into_any(),
                Dialog::ConfirmDelete(paths) => view! { <DeleteDialog paths=paths/> }.into_any(),
                Dialog::Properties(entry) => view! { <PropertiesDialog entry=entry/> }.into_any(),
                Dialog::Shortcuts => view! { <ShortcutsDialog/> }.into_any(),
            };
            view! {
                <div class="overlay"
                    on:mousedown=move |ev: ev::MouseEvent| {
                        if ev.target() == ev.current_target() {
                            st.dialog.set(None);
                        }
                    }>
                    {body}
                </div>
            }
        })
    }
}

#[component]
fn NameDialog(
    i: usize,
    folder: bool,
    title: &'static str,
    label: &'static str,
    initial: &'static str,
) -> impl IntoView {
    let st = use_st();
    let value = RwSignal::new(initial.to_string());
    let input = NodeRef::<html::Input>::new();
    let stem = util::stem_utf16_len(initial);
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            // Set the value before selecting, so typing replaces the default name.
            el.set_value(&value.get_untracked());
            let _ = el.focus();
            let _ = el.set_selection_range(0, stem);
        }
    });
    let submit = move || {
        let name = value.get_untracked().trim().to_string();
        if name.is_empty() {
            return;
        }
        st.dialog.set(None);
        if folder {
            st.mkdir(i, name);
        } else {
            st.touch(i, name);
        }
    };
    view! {
        <div class="dialog" role="dialog" aria-label=title>
            <h2>{title}</h2>
            <label class="field">
                <span>{label}</span>
                <input node_ref=input spellcheck="false" prop:value=move || value.get()
                    on:input=move |ev: ev::Event| value.set(event_target_value(&ev))
                    on:keydown=move |ev: ev::KeyboardEvent| {
                        if ev.key() == "Enter" {
                            submit();
                        }
                    }/>
            </label>
            <div class="buttons">
                <button class="btn primary" disabled=move || value.with(|v| v.trim().is_empty())
                    on:click=move |_| submit()>"OK"</button>
                <button class="btn" on:click=move |_| st.dialog.set(None)>"Cancel"</button>
            </div>
        </div>
    }
}

#[component]
fn DeleteDialog(paths: Vec<String>) -> impl IntoView {
    let st = use_st();
    let what = match paths.as_slice() {
        [one] => format!("“{}”", vname(one)),
        many => format!("these {} items", many.len()),
    };
    let paths = StoredValue::new(paths);
    let confirm = NodeRef::<html::Button>::new();
    Effect::new(move |_| {
        if let Some(b) = confirm.get() {
            let _ = b.focus();
        }
    });
    view! {
        <div class="dialog" role="alertdialog" aria-label="Delete permanently">
            <h2>"Delete Permanently"</h2>
            <p>{format!("Do you really want to permanently delete {what}?")}</p>
            <p class="muted">"This action cannot be undone."</p>
            <div class="buttons">
                <button class="btn danger" node_ref=confirm
                    on:click=move |_| {
                        st.dialog.set(None);
                        st.delete(paths.get_value());
                    }>
                    <span class="ic" inner_html=icons::TRASH></span>"Delete"
                </button>
                <button class="btn" on:click=move |_| st.dialog.set(None)>"Cancel"</button>
            </div>
        </div>
    }
}

#[component]
fn PropertiesDialog(entry: Entry) -> impl IntoView {
    let st = use_st();
    let size = RwSignal::new(None::<String>);
    if entry.is_dir() {
        let path = entry.path.clone();
        spawn_local(async move {
            let text = match api::size(&path).await {
                Ok(r) => format!(
                    "{} ({} bytes) · {}, {}",
                    format_size(r.bytes),
                    r.bytes,
                    util::plural(r.files as usize, "file", "files"),
                    util::plural(r.dirs as usize, "folder", "folders")
                ),
                Err(e) => e,
            };
            let _ = size.try_set(Some(text));
        });
    } else {
        size.set(Some(format!("{} ({} bytes)", format_size(entry.size), entry.size)));
    }
    let perms = entry.mode.map(|m| util::mode_string(m, entry.is_dir())).unwrap_or_default();
    let modified = entry.modified.map(format_date).unwrap_or_else(|| "Unknown".into());
    let location = vparent(&entry.path);
    view! {
        <div class="dialog props" role="dialog" aria-label="Properties">
            <div class="props-head">
                <span class="bigicon" inner_html=icons::file_icon(&entry)></span>
                <h2>{entry.name.clone()}</h2>
            </div>
            <dl>
                <dt>"Type"</dt><dd>{type_label(&entry)}</dd>
                <dt>"Location"</dt><dd class="mono">{location}</dd>
                <dt>"Size"</dt><dd>{move || size.get().unwrap_or_else(|| "Calculating…".into())}</dd>
                <dt>"Modified"</dt><dd>{modified}</dd>
                <dt>"Permissions"</dt><dd class="mono">{perms}</dd>
            </dl>
            <div class="buttons">
                <button class="btn primary" on:click=move |_| st.dialog.set(None)>"Close"</button>
            </div>
        </div>
    }
}

#[component]
fn ShortcutsDialog() -> impl IntoView {
    let st = use_st();
    const KEYS: &[(&str, &str)] = &[
        ("Double-click / Enter", "Open folder or file"),
        ("Backspace / Alt+Up", "Go up"),
        ("Alt+Left / Alt+Right", "Back / forward"),
        ("Arrows, Home, End", "Move selection (Shift extends)"),
        ("Ctrl+A", "Select all"),
        ("Ctrl+C / Ctrl+X / Ctrl+V", "Copy / cut / paste"),
        ("F2", "Rename"),
        ("Delete", "Delete"),
        ("F7", "New folder"),
        ("F3 / Tab", "Split view / switch pane"),
        ("Ctrl+1 / 2 / 3 / 4", "Icons / compact / details / columns"),
        ("Left / Right (columns)", "Parent folder / into selected folder"),
        ("Ctrl+ + / Ctrl+ −", "Zoom"),
        ("Ctrl+H or Alt+.", "Show hidden files"),
        ("Ctrl+L", "Edit location"),
        ("Ctrl+I / Ctrl+F", "Filter / search"),
        ("F9", "Toggle places panel"),
        ("Alt+Enter", "Properties"),
        ("Drag + Ctrl", "Copy instead of move"),
    ];
    view! {
        <div class="dialog wide" role="dialog" aria-label="Keyboard shortcuts">
            <h2>"Keyboard Shortcuts"</h2>
            <table class="keys">
                {KEYS.iter().map(|(k, d)| view! { <tr><td><kbd>{*k}</kbd></td><td>{*d}</td></tr> }).collect_view()}
            </table>
            <div class="buttons">
                <button class="btn primary" on:click=move |_| st.dialog.set(None)>"Close"</button>
            </div>
        </div>
    }
}

// ===========================================================================
// Status bar
// ===========================================================================

#[component]
fn StatusBar() -> impl IntoView {
    let st = use_st();
    let summary = Memo::new(move |_| {
        let i = st.active.get();
        let p = st.panes[i];
        let vis = st.visible(i);
        let sel: Vec<Entry> = p.selected.with(|s| vis.iter().filter(|e| s.contains(&e.path)).cloned().collect());
        match sel.as_slice() {
            [one] => format!("{} ({})", one.name, size_text(one)),
            [] => {
                let dirs = vis.iter().filter(|e| e.is_dir()).count();
                let files = vis.len() - dirs;
                let bytes: u64 = vis.iter().map(|e| e.size).sum();
                let mut s = format!("{}, {}", util::plural(dirs, "folder", "folders"), util::plural(files, "file", "files"));
                if files > 0 {
                    s.push_str(&format!(" ({})", format_size(bytes)));
                }
                s
            }
            many => {
                let bytes: u64 = many.iter().map(|e| e.size).sum();
                format!("{} items selected ({})", many.len(), format_size(bytes))
            }
        }
    });

    view! {
        <footer class="statusbar">
            <div class="status-text">
                {move || match st.upload.get() {
                    Some(u) => view! {
                        <span class="upload">
                            <span>{format!("Uploading {}…", u.label)}</span>
                            <progress max="1" value=u.progress.to_string()></progress>
                            <span>{format!("{:.0}%", u.progress * 100.0)}</span>
                        </span>
                    }
                    .into_any(),
                    None => view! { <span>{move || summary.get()}</span> }.into_any(),
                }}
            </div>
            <Show when=move || matches!(st.view.get(), ViewMode::Icons | ViewMode::Compact)>
                <label class="zoom" title="Zoom (Ctrl + / Ctrl −)">
                    <input type="range" min="32" max="192" step="8" aria-label="Zoom"
                        prop:value=move || st.zoom.get().to_string()
                        on:input=move |ev: ev::Event| {
                            if let Ok(v) = event_target_value(&ev).parse::<u32>() {
                                st.zoom.set(v);
                            }
                        }/>
                </label>
            </Show>
            <DiskMeter compact=true/>
        </footer>
    }
}
