use std::cell::RefCell;
use std::rc::Rc;

use app_core::{ClientApp, ClientEvent};
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    Blob, CanvasRenderingContext2d, Event, FileReader, HtmlCanvasElement, MessageEvent,
    PointerEvent, WebSocket,
};

use crate::dom::{canvas_context, room_from_url, websocket_url};
use crate::{render, ui};

pub(crate) struct BrowserApp {
    app: ClientApp,
    ctx: CanvasRenderingContext2d,
    ws: Option<WebSocket>,
}

impl BrowserApp {
    pub(crate) fn new() -> Result<Self, JsValue> {
        let room = room_from_url();
        Ok(Self {
            app: ClientApp::new(room),
            ctx: canvas_context("canvas-main")?,
            ws: None,
        })
    }

    pub(crate) fn connect(app: &Rc<RefCell<Self>>) -> Result<(), JsValue> {
        if let Some(ws) = app.borrow_mut().ws.take() {
            let _ = ws.close();
        }

        let url = websocket_url(app.borrow().app.room())?;
        let ws = WebSocket::new(&url)?;

        {
            let app = Rc::clone(app);
            let onopen = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
                app.borrow_mut().handle_open();
            }));
            ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
            onopen.forget();
        }

        {
            let app = Rc::clone(app);
            let onmessage =
                Closure::<dyn FnMut(MessageEvent)>::wrap(Box::new(move |event: MessageEvent| {
                    if let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() {
                        let bytes = Uint8Array::new(&buffer).to_vec();
                        app.borrow_mut().handle_frame(&bytes);
                    } else if let Ok(blob) = event.data().dyn_into::<Blob>() {
                        read_blob_frame(Rc::clone(&app), blob);
                    }
                }));
            ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget();
        }

        {
            let app = Rc::clone(app);
            let onclose = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
                app.borrow_mut().handle_close();
            }));
            ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
            onclose.forget();
        }

        {
            let app = Rc::clone(app);
            let onerror = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
                app.borrow_mut().handle_close();
            }));
            ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
            onerror.forget();
        }

        app.borrow_mut().ws = Some(ws);
        Ok(())
    }

    pub(crate) fn frame(&mut self, _now_ms: f64) {
        render::render_app(&self.ctx, &self.app);
        ui::update_stats(&self.app);
        ui::update_controls(&self.app);
        ui::render_log(&self.app);
    }

    pub(crate) fn pointer_down(&mut self, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        self.app.begin_stroke(render::pointer_pos(canvas, event));
    }

    pub(crate) fn pointer_move(&mut self, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        self.app.extend_stroke(render::pointer_pos(canvas, event));
    }

    pub(crate) fn pointer_up(&mut self) {
        let events = self.app.end_stroke();
        self.send_events(events);
    }

    pub(crate) fn pointer_cancel(&mut self) {
        self.app.cancel_stroke();
    }

    pub(crate) fn undo(&mut self) {
        let events = self.app.undo();
        self.send_events(events);
    }

    fn handle_open(&mut self) {
        self.app.set_connected(true);
        let events = self.app.hello_frame();
        self.send_events(events);
    }

    fn handle_frame(&mut self, bytes: &[u8]) {
        self.app.receive_frame(bytes);
    }

    fn handle_close(&mut self) {
        self.app.set_connected(false);
        self.ws = None;
    }

    fn send_events(&mut self, events: Vec<ClientEvent>) {
        for event in events {
            match event {
                ClientEvent::SendFrame(frame) => {
                    if let Some(ws) = &self.ws {
                        let _ = ws.send_with_u8_array(&frame);
                    }
                }
            }
        }
    }
}

fn read_blob_frame(app: Rc<RefCell<BrowserApp>>, blob: Blob) {
    let Ok(reader) = FileReader::new() else {
        return;
    };
    let reader_for_load = reader.clone();
    let onload = Closure::<dyn FnMut(Event)>::wrap(Box::new(move |_| {
        let Ok(result) = reader_for_load.result() else {
            return;
        };
        if let Ok(buffer) = result.dyn_into::<ArrayBuffer>() {
            let bytes = Uint8Array::new(&buffer).to_vec();
            app.borrow_mut().handle_frame(&bytes);
        }
    }));
    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
    onload.forget();
    let _ = reader.read_as_array_buffer(&blob);
}
