mod browser_app;
mod dom;
mod input;
mod render;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{ALICE_COLOR, BOB_COLOR, Peer};
use browser_app::BrowserApp;
use input::{bind_canvas, bind_controls, start_loop};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let app = Rc::new(RefCell::new(BrowserApp::new()?));
    bind_canvas(&app, "canvas-alice", Peer::Alice, ALICE_COLOR)?;
    bind_canvas(&app, "canvas-bob", Peer::Bob, BOB_COLOR)?;
    bind_controls(&app)?;
    start_loop(app)?;

    Ok(())
}
