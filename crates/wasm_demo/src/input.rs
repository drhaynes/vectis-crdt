use std::cell::RefCell;
use std::rc::Rc;

use app_core::Peer;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{Event, HtmlButtonElement, HtmlCanvasElement, HtmlInputElement, PointerEvent};

use crate::browser_app::BrowserApp;
use crate::dom::{element_by_id, request_animation_frame};

pub(crate) fn bind_canvas(
    app: &Rc<RefCell<BrowserApp>>,
    id: &str,
    peer: Peer,
    color: u32,
) -> Result<(), JsValue> {
    let canvas = element_by_id::<HtmlCanvasElement>(id)?;

    {
        let app = Rc::clone(app);
        let canvas_for_handler = canvas.clone();
        let handler =
            Closure::<dyn FnMut(PointerEvent)>::wrap(Box::new(move |event: PointerEvent| {
                event.prevent_default();
                let _ = canvas_for_handler.set_pointer_capture(event.pointer_id());
                app.borrow_mut()
                    .pointer_down(peer, &canvas_for_handler, &event, color);
            }));
        canvas.add_event_listener_with_callback("pointerdown", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let app = Rc::clone(app);
        let canvas_for_handler = canvas.clone();
        let handler =
            Closure::<dyn FnMut(PointerEvent)>::wrap(Box::new(move |event: PointerEvent| {
                event.prevent_default();
                app.borrow_mut()
                    .pointer_move(peer, &canvas_for_handler, &event);
            }));
        canvas.add_event_listener_with_callback("pointermove", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let app = Rc::clone(app);
        let handler =
            Closure::<dyn FnMut(PointerEvent)>::wrap(Box::new(move |event: PointerEvent| {
                event.prevent_default();
                app.borrow_mut().pointer_up(peer);
            }));
        canvas.add_event_listener_with_callback("pointerup", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let app = Rc::clone(app);
        let handler = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |event: Event| {
            event.prevent_default();
            app.borrow_mut().pointer_cancel(peer);
        }));
        canvas
            .add_event_listener_with_callback("pointercancel", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    Ok(())
}

pub(crate) fn bind_controls(app: &Rc<RefCell<BrowserApp>>) -> Result<(), JsValue> {
    {
        let slider = element_by_id::<HtmlInputElement>("delay-slider")?;
        let slider_for_handler = slider.clone();
        let app = Rc::clone(app);
        let handler = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
            let delay = slider_for_handler.value().parse::<u32>().unwrap_or(500);
            app.borrow_mut().set_network_delay(delay);
        }));
        slider.add_event_listener_with_callback("input", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    bind_button(app, "btn-disconnect", |app| app.toggle_disconnect())?;
    bind_button(app, "btn-sync", |app| app.reconnect_and_sync())?;
    bind_button(app, "btn-undo-alice", |app| app.undo(Peer::Alice))?;
    bind_button(app, "btn-undo-bob", |app| app.undo(Peer::Bob))?;
    bind_button(app, "btn-clear", |app| app.clear_all())?;

    Ok(())
}

pub(crate) fn bind_button<F>(
    app: &Rc<RefCell<BrowserApp>>,
    id: &str,
    mut f: F,
) -> Result<(), JsValue>
where
    F: 'static + FnMut(&mut BrowserApp),
{
    let button = element_by_id::<HtmlButtonElement>(id)?;
    let app = Rc::clone(app);
    let handler = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
        f(&mut app.borrow_mut());
    }));
    button.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
    handler.forget();
    Ok(())
}

pub(crate) fn start_loop(app: Rc<RefCell<BrowserApp>>) -> Result<(), JsValue> {
    let raf = Rc::new(RefCell::new(None::<Closure<dyn FnMut(f64)>>));
    let raf_for_closure = Rc::clone(&raf);

    *raf_for_closure.borrow_mut() = Some(Closure::wrap(Box::new(move |time: f64| {
        app.borrow_mut().frame(time);
        if let Some(callback) = raf.borrow().as_ref() {
            let _ = request_animation_frame(callback);
        }
    }) as Box<dyn FnMut(f64)>));

    if let Some(callback) = raf_for_closure.borrow().as_ref() {
        request_animation_frame(callback)?;
    }

    Ok(())
}
