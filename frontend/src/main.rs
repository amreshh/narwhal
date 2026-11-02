mod api;
mod columns;
mod icons;
mod office;
mod quicklook;
mod state;
mod trash;
mod ui;
mod upload;
mod util;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(ui::App);
}
