//! Previews of office documents (Word, Excel, PowerPoint, OpenDocument).
//! The file is read here in the browser by `narwhal-office`, so untrusted
//! documents are never parsed on the server.

use crate::api;
use leptos::{prelude::*, task::spawn_local};
use narwhal_office::{Block, Preview, Sheet, Slide, Span, MAX_COLS, MAX_ROWS};
use narwhal_shared::Entry;
use std::sync::Arc;

/// Largest file that is downloaded for a preview.
const MAX_BYTES: u64 = 64 << 20;

#[component]
pub fn OfficePreview(entry: Entry) -> impl IntoView {
    // None while loading.
    let state = RwSignal::new(None::<Result<Arc<Preview>, String>>);
    if entry.size > MAX_BYTES {
        state.set(Some(Err("This file is too large to preview".into())));
    } else {
        let path = entry.path.clone();
        spawn_local(async move {
            let res = match api::file_bytes(&path).await {
                Ok(bytes) => narwhal_office::parse(&bytes).map(Arc::new).map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let _ = state.try_set(Some(res));
        });
    }
    move || match state.get() {
        None => view! { <div class="col-note">"Loading…"</div> }.into_any(),
        Some(Err(e)) => view! { <div class="col-note">{e}</div> }.into_any(),
        Some(Ok(p)) => match &*p {
            Preview::Document { blocks, cut } => document(blocks, *cut),
            Preview::Sheets(_) => view! { <Sheets preview=p.clone()/> }.into_any(),
            Preview::Slides { slides, cut } => presentation(slides, *cut),
        },
    }
}

fn spans(spans: &[Span]) -> AnyView {
    spans
        .iter()
        .map(|s| view! { <span class:b={s.bold} class:i={s.italic} class:u={s.underline}>{s.text.clone()}</span> })
        .collect_view()
        .into_any()
}

/// A document heading, one level down: the preview is inside the app's page.
fn heading(level: u8, s: &[Span]) -> AnyView {
    let (class, body) = (format!("oh oh{level}"), spans(s));
    match level {
        1 => view! { <h2 class=class>{body}</h2> }.into_any(),
        2 => view! { <h3 class=class>{body}</h3> }.into_any(),
        3 => view! { <h4 class=class>{body}</h4> }.into_any(),
        4 => view! { <h5 class=class>{body}</h5> }.into_any(),
        _ => view! { <h6 class=class>{body}</h6> }.into_any(),
    }
}

fn note(text: String) -> impl IntoView {
    view! { <div class="onote">{text}</div> }
}

fn document(blocks: &[Block], cut: bool) -> AnyView {
    if blocks.is_empty() {
        return view! { <div class="col-note">"This document has no text to show"</div> }.into_any();
    }
    let body = blocks
        .iter()
        .map(|b| match b {
            Block::Heading(level, s) => heading(*level, s),
            Block::Paragraph(s) => view! { <p>{spans(s)}</p> }.into_any(),
            Block::ListItem(level, s) => view! { <p class="oli" style=format!("--lvl:{level}")>{spans(s)}</p> }.into_any(),
            Block::Table(rows) => view! {
                <table class="otable">
                    <tbody>
                        {rows.iter().map(|r| view! {
                            <tr>{r.iter().map(|c| view! { <td>{c.clone()}</td> }).collect_view()}</tr>
                        }).collect_view()}
                    </tbody>
                </table>
            }
            .into_any(),
        })
        .collect_view();
    view! {
        <div class="office odoc">{body}</div>
        {cut.then(|| note("Only the beginning of the document is shown.".into()))}
    }
    .into_any()
}

/// `0` -> `A`, `27` -> `AB`.
fn column_name(mut c: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap_or_default()
}

/// Numbers and percentages are right-aligned, as in a spreadsheet.
fn numeric(text: &str) -> bool {
    let t = text.trim_end_matches('%');
    !t.is_empty() && t.parse::<f64>().is_ok()
}

fn sheet_table(s: &Sheet) -> AnyView {
    let Some(width) = s.rows.first().map(Vec::len).filter(|w| *w > 0) else {
        return view! { <div class="col-note">"This sheet is empty"</div> }.into_any();
    };
    view! {
        <div class="ogrid">
            <table>
                <thead>
                    <tr><th></th>{(0..width).map(|c| view! { <th>{column_name(c)}</th> }).collect_view()}</tr>
                </thead>
                <tbody>
                    {s.rows.iter().enumerate().map(|(r, row)| view! {
                        <tr>
                            <th>{r + 1}</th>
                            {row.iter().map(|c| view! {
                                // Long texts are cut off in the cell: show them whole on hover.
                                <td class:num={numeric(c)} title=(c.chars().count() > 30).then(|| c.clone())>{c.clone()}</td>
                            }).collect_view()}
                        </tr>
                    }).collect_view()}
                </tbody>
            </table>
        </div>
        {s.cut.then(|| note(format!("Showing the first {MAX_ROWS} rows and {MAX_COLS} columns.")))}
    }
    .into_any()
}

#[component]
fn Sheets(preview: Arc<Preview>) -> impl IntoView {
    let current = RwSignal::new(0usize);
    let names: Vec<String> = match &*preview {
        Preview::Sheets(s) => s.iter().map(|s| s.name.clone()).collect(),
        _ => Vec::new(),
    };
    let preview = StoredValue::new(preview);
    let tabs = (names.len() > 1).then(|| {
        view! {
            <div class="otabs" role="tablist">
                {names.into_iter().enumerate().map(|(i, name)| view! {
                    <button class="otab" role="tab" class:on=move || current.get() == i
                        aria-selected=move || if current.get() == i { "true" } else { "false" }
                        on:click=move |_| current.set(i)>{name}</button>
                }).collect_view()}
            </div>
        }
    });
    view! {
        <div class="office osheets">
            {tabs}
            {move || preview.with_value(|p| match &**p {
                Preview::Sheets(s) => s.get(current.get()).map(sheet_table),
                _ => None,
            })}
        </div>
    }
}

fn presentation(slides: &[Slide], cut: bool) -> AnyView {
    if slides.is_empty() {
        return view! { <div class="col-note">"This presentation has no slides"</div> }.into_any();
    }
    view! {
        <ol class="office oslides">
            {slides.iter().enumerate().map(|(i, s)| view! {
                <li class="oslide">
                    <span class="onum">{i + 1}</span>
                    <div class="otext">
                        {(!s.title.is_empty()).then(|| view! { <p class="otitle">{s.title.clone()}</p> })}
                        {s.lines.iter().map(|l| view! { <p>{l.clone()}</p> }).collect_view()}
                        {(s.title.is_empty() && s.lines.is_empty()).then(|| view! { <p class="muted">"No text on this slide"</p> })}
                    </div>
                </li>
            }).collect_view()}
        </ol>
        {cut.then(|| note("Only the first slides are shown.".into()))}
    }
    .into_any()
}
