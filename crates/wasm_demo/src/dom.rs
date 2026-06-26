use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    CanvasRenderingContext2d, Document as DomDocument, HtmlCanvasElement, HtmlElement, Window,
};

pub(crate) fn canvas_context(id: &str) -> Result<CanvasRenderingContext2d, JsValue> {
    let canvas = element_by_id::<HtmlCanvasElement>(id)?;
    Ok(canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("2d canvas context unavailable"))?
        .dyn_into::<CanvasRenderingContext2d>()?)
}

pub(crate) fn element_by_id<T>(id: &str) -> Result<T, JsValue>
where
    T: JsCast,
{
    dom_document()?
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))?
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str(&format!("element #{id} has unexpected type")))
}

pub(crate) fn set_text(id: &str, text: &str) {
    if let Ok(el) = element_by_id::<HtmlElement>(id)
        && el.text_content().as_deref() != Some(text)
    {
        el.set_text_content(Some(text));
    }
}

pub(crate) fn request_animation_frame(callback: &Closure<dyn FnMut(f64)>) -> Result<i32, JsValue> {
    web_window()?.request_animation_frame(callback.as_ref().unchecked_ref())
}

pub(crate) fn web_window() -> Result<Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))
}

pub(crate) fn dom_document() -> Result<DomDocument, JsValue> {
    web_window()?
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))
}

pub(crate) fn room_from_url() -> String {
    web_window()
        .ok()
        .and_then(|window| window.location().hash().ok())
        .map(|hash| hash.trim_start_matches('#').trim().to_string())
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| "demo".to_string())
}

pub(crate) fn websocket_url(room: &str) -> Result<String, JsValue> {
    let location = web_window()?.location();
    let protocol = match location.protocol()?.as_str() {
        "https:" => "wss",
        _ => "ws",
    };
    let host = location
        .hostname()
        .ok()
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "localhost".to_string());
    Ok(format!("{protocol}://{host}:3000/ws?room={room}"))
}

pub(crate) fn resume_token_from_storage(room: &str) -> String {
    web_window()
        .ok()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(&storage_key(room)).ok().flatten())
        .unwrap_or_default()
}

pub(crate) fn store_resume_token(room: &str, token: &str) {
    if token.is_empty() {
        return;
    }
    if let Ok(Some(storage)) = web_window().and_then(|window| window.local_storage()) {
        let _ = storage.set_item(&storage_key(room), token);
    }
}

fn storage_key(room: &str) -> String {
    format!("vectis.resume.{room}")
}
