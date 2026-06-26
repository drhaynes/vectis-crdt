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

pub(crate) fn element_width(id: &str) -> Option<f64> {
    element_by_id::<HtmlElement>(id)
        .ok()
        .map(|el| f64::from(el.offset_width()))
}

pub(crate) fn element_height(id: &str) -> Option<f64> {
    element_by_id::<HtmlElement>(id)
        .ok()
        .map(|el| f64::from(el.offset_height()))
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

pub(crate) fn now() -> f64 {
    web_window()
        .ok()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or(0.0)
}
