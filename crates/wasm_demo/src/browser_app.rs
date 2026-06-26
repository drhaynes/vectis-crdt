use std::cell::RefCell;
use std::rc::Rc;

use app_core::{AppPoint, ClientApp, ClientEvent};
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    Blob, CanvasRenderingContext2d, Event, FileReader, HtmlCanvasElement, MessageEvent,
    PointerEvent, WebSocket,
};

use crate::dom::{
    canvas_context, resume_token_from_storage, room_from_url, store_resume_token, websocket_url,
};
use crate::{render, ui};

pub(crate) struct BrowserApp {
    app: ClientApp,
    ctx: CanvasRenderingContext2d,
    ws: Option<WebSocket>,
    latest_cursor: Option<AppPoint>,
    last_cursor_sent_ms: f64,
}

impl BrowserApp {
    pub(crate) fn new() -> Result<Self, JsValue> {
        let room = room_from_url();
        let resume_token = resume_token_from_storage(&room);
        let mut app = ClientApp::new(room);
        app.set_resume_token(resume_token);
        Ok(Self {
            app,
            ctx: canvas_context("canvas-main")?,
            ws: None,
            latest_cursor: None,
            last_cursor_sent_ms: 0.0,
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

    pub(crate) fn frame(&mut self, now_ms: f64) {
        if now_ms - self.last_cursor_sent_ms >= 80.0
            && let Some(point) = self.latest_cursor
        {
            self.last_cursor_sent_ms = now_ms;
            let events = self.app.awareness_frame(point, now_ms.max(0.0) as u64);
            self.send_events(events);
        }

        render::render_app(&self.ctx, &self.app);
        ui::update_stats(&self.app);
        ui::update_controls(&self.app);
        ui::render_log(&self.app);
    }

    pub(crate) fn pointer_down(&mut self, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        let point = render::pointer_pos(canvas, event);
        self.latest_cursor = Some(point);
        self.app.begin_stroke(point);
    }

    pub(crate) fn pointer_move(&mut self, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        let point = render::pointer_pos(canvas, event);
        self.latest_cursor = Some(point);
        self.app.extend_stroke(point);
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
        let events = self.app.receive_frame(bytes);
        store_resume_token(self.app.room(), self.app.resume_token());
        self.send_events(events);
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
