use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{Event, HtmlButtonElement, HtmlCanvasElement, PointerEvent};

use crate::browser_app::BrowserApp;
use crate::dom::{element_by_id, request_animation_frame};

pub(crate) fn bind_canvas(app: &Rc<RefCell<BrowserApp>>, id: &str) -> Result<(), JsValue> {
    let canvas = element_by_id::<HtmlCanvasElement>(id)?;

    {
        let app = Rc::clone(app);
        let canvas_for_handler = canvas.clone();
        let handler =
            Closure::<dyn FnMut(PointerEvent)>::wrap(Box::new(move |event: PointerEvent| {
                event.prevent_default();
                let _ = canvas_for_handler.set_pointer_capture(event.pointer_id());
                app.borrow_mut().pointer_down(&canvas_for_handler, &event);
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
                app.borrow_mut().pointer_move(&canvas_for_handler, &event);
            }));
        canvas.add_event_listener_with_callback("pointermove", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let app = Rc::clone(app);
        let handler =
            Closure::<dyn FnMut(PointerEvent)>::wrap(Box::new(move |event: PointerEvent| {
                event.prevent_default();
                app.borrow_mut().pointer_up();
            }));
        canvas.add_event_listener_with_callback("pointerup", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    {
        let app = Rc::clone(app);
        let handler = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |event: Event| {
            event.prevent_default();
            app.borrow_mut().pointer_cancel();
        }));
        canvas
            .add_event_listener_with_callback("pointercancel", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    Ok(())
}

pub(crate) fn bind_controls(app: &Rc<RefCell<BrowserApp>>) -> Result<(), JsValue> {
    bind_button(app, "btn-undo", |app| app.undo())?;

    {
        let button = element_by_id::<HtmlButtonElement>("btn-reconnect")?;
        let app = Rc::clone(app);
        let handler = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
            let _ = BrowserApp::connect(&app);
        }));
        button.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }

    Ok(())
}

fn bind_button<F>(app: &Rc<RefCell<BrowserApp>>, id: &str, mut f: F) -> Result<(), JsValue>
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
