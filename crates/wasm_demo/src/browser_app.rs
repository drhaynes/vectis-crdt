use std::collections::BTreeMap;

use app_core::{AppEvent, DemoApp, Peer};
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, PointerEvent};

use crate::dom::{canvas_context, now, set_text};
use crate::{render, ui};

pub(crate) struct BrowserApp {
    app: DemoApp,
    alice_ctx: CanvasRenderingContext2d,
    bob_ctx: CanvasRenderingContext2d,
    packet_elements: BTreeMap<u32, HtmlElement>,
}

impl BrowserApp {
    pub(crate) fn new() -> Result<Self, JsValue> {
        Ok(Self {
            app: DemoApp::new(),
            alice_ctx: canvas_context("canvas-alice")?,
            bob_ctx: canvas_context("canvas-bob")?,
            packet_elements: BTreeMap::new(),
        })
    }

    pub(crate) fn frame(&mut self, now_ms: f64) {
        let events = self.app.tick(now_ms);
        self.handle_events(events);
        render::render_peer(&self.alice_ctx, &self.app, Peer::Alice);
        render::render_peer(&self.bob_ctx, &self.app, Peer::Bob);
        render::render_packets(&self.packet_elements, &self.app, now_ms);
        ui::update_stats(&self.app);
        ui::update_network_controls(&self.app);
        ui::render_log(&self.app);
    }

    pub(crate) fn pointer_down(
        &mut self,
        peer: Peer,
        canvas: &HtmlCanvasElement,
        event: &PointerEvent,
        color: u32,
    ) {
        self.app
            .begin_stroke(peer, render::pointer_pos(canvas, event), color);
    }

    pub(crate) fn pointer_move(
        &mut self,
        peer: Peer,
        canvas: &HtmlCanvasElement,
        event: &PointerEvent,
    ) {
        self.app
            .extend_stroke(peer, render::pointer_pos(canvas, event));
    }

    pub(crate) fn pointer_up(&mut self, peer: Peer) {
        let events = self.app.end_stroke(peer);
        self.handle_events(events);
    }

    pub(crate) fn pointer_cancel(&mut self, peer: Peer) {
        self.app.cancel_stroke(peer);
    }

    pub(crate) fn set_network_delay(&mut self, delay: u32) {
        self.app.set_network_delay(delay);
        let label = if delay == 0 {
            "0ms (instant)".to_string()
        } else {
            format!("{}ms", delay)
        };
        set_text("delay-label", &label);
    }

    pub(crate) fn toggle_disconnect(&mut self) {
        self.app.toggle_disconnect();
        ui::update_network_controls(&self.app);
    }

    pub(crate) fn reconnect_and_sync(&mut self) {
        self.app.reconnect_and_sync();
        ui::update_network_controls(&self.app);
        ui::render_log(&self.app);
    }

    pub(crate) fn undo(&mut self, peer: Peer) {
        let events = self.app.undo(peer);
        self.handle_events(events);
    }

    pub(crate) fn clear_all(&mut self) {
        let events = self.app.clear_all();
        self.handle_events(events);
        ui::render_log(&self.app);
    }

    fn handle_events(&mut self, events: Vec<AppEvent>) {
        for event in events {
            match event {
                AppEvent::PacketCreated {
                    id,
                    direction,
                    bytes,
                } => {
                    self.app.set_packet_start_time(id, now());
                    if let Ok(el) = render::make_packet_el(id, direction, bytes) {
                        self.packet_elements.insert(id, el);
                    }
                }
                AppEvent::PacketDelivered { id } => {
                    if let Some(el) = self.packet_elements.remove(&id) {
                        el.remove();
                    }
                }
                AppEvent::ClearPackets => {
                    for (_, el) in std::mem::take(&mut self.packet_elements) {
                        el.remove();
                    }
                }
            }
        }
    }
}
