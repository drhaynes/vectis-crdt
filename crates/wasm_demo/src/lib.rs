use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use app_core::{
    ALICE_COLOR, AppEvent, AppPoint, BOB_COLOR, DemoApp, Direction, PacketStatus, Peer, StrokeView,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    CanvasRenderingContext2d, Document as DomDocument, Event, HtmlButtonElement, HtmlCanvasElement,
    HtmlElement, HtmlInputElement, PointerEvent, Window,
};

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

struct BrowserApp {
    app: DemoApp,
    alice_ctx: CanvasRenderingContext2d,
    bob_ctx: CanvasRenderingContext2d,
    packet_elements: BTreeMap<u32, HtmlElement>,
}

impl BrowserApp {
    fn new() -> Result<Self, JsValue> {
        Ok(Self {
            app: DemoApp::new(),
            alice_ctx: canvas_context("canvas-alice")?,
            bob_ctx: canvas_context("canvas-bob")?,
            packet_elements: BTreeMap::new(),
        })
    }

    fn frame(&mut self, now_ms: f64) {
        let events = self.app.tick(now_ms);
        self.handle_events(events);
        self.render_peer(Peer::Alice);
        self.render_peer(Peer::Bob);
        self.render_packets(now_ms);
        self.update_stats();
        self.update_network_controls();
        self.render_log();
    }

    fn pointer_down(
        &mut self,
        peer: Peer,
        canvas: &HtmlCanvasElement,
        event: &PointerEvent,
        color: u32,
    ) {
        self.app
            .begin_stroke(peer, pointer_pos(canvas, event), color);
    }

    fn pointer_move(&mut self, peer: Peer, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        self.app.extend_stroke(peer, pointer_pos(canvas, event));
    }

    fn pointer_up(&mut self, peer: Peer) {
        let events = self.app.end_stroke(peer);
        self.handle_events(events);
    }

    fn pointer_cancel(&mut self, peer: Peer) {
        self.app.cancel_stroke(peer);
    }

    fn set_network_delay(&mut self, delay: u32) {
        self.app.set_network_delay(delay);
        let label = if delay == 0 {
            "0ms (instant)".to_string()
        } else {
            format!("{}ms", delay)
        };
        set_text("delay-label", &label);
    }

    fn toggle_disconnect(&mut self) {
        self.app.toggle_disconnect();
        self.update_network_controls();
    }

    fn reconnect_and_sync(&mut self) {
        self.app.reconnect_and_sync();
        self.update_network_controls();
        self.render_log();
    }

    fn undo(&mut self, peer: Peer) {
        let events = self.app.undo(peer);
        self.handle_events(events);
    }

    fn clear_all(&mut self) {
        let events = self.app.clear_all();
        self.handle_events(events);
        self.render_log();
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
                    if let Ok(el) = make_packet_el(id, direction, bytes) {
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

    fn render_peer(&self, peer: Peer) {
        let ctx = match peer {
            Peer::Alice => &self.alice_ctx,
            Peer::Bob => &self.bob_ctx,
        };

        if let Some(canvas) = ctx.canvas() {
            ctx.clear_rect(
                0.0,
                0.0,
                f64::from(canvas.width()),
                f64::from(canvas.height()),
            );
        }

        for stroke in self.app.strokes(peer) {
            draw_stroke(ctx, &stroke);
        }

        if let Some(stroke) = self.app.live_stroke(peer) {
            draw_stroke(ctx, &stroke);
        }
    }

    fn render_packets(&self, now_ms: f64) {
        let width = element_width("packets-area").unwrap_or(150.0);
        let height = element_height("packets-area").unwrap_or(320.0);
        let cy = height / 2.0;

        for packet in self.app.packet_views(now_ms) {
            let Some(el) = self.packet_elements.get(&packet.id) else {
                continue;
            };
            let x = match packet.direction {
                Direction::AliceToBob => packet.progress * (width - 34.0),
                Direction::BobToAlice => (1.0 - packet.progress) * (width - 34.0),
            };
            let style = el.style();
            let _ = style.set_property("left", &format!("{}px", x));
            let _ = style.set_property("top", &format!("{}px", cy));
            let _ = style.set_property("opacity", &packet.opacity.to_string());
        }
    }

    fn update_stats(&self) {
        let stats = self.app.stats();
        let network = self.app.network_state();

        set_text("alice-visible", &stats.alice_visible.to_string());
        set_text("alice-queue", &stats.alice_queued.to_string());
        set_text("alice-undo", &stats.alice_undo_depth.to_string());
        set_text("alice-bytes", &fmt_bytes(stats.alice_bytes));
        set_text("bob-visible", &stats.bob_visible.to_string());
        set_text("bob-queue", &stats.bob_queued.to_string());
        set_text("bob-undo", &stats.bob_undo_depth.to_string());
        set_text("bob-bytes", &fmt_bytes(stats.bob_bytes));
        set_text("total-packets", &stats.total_packets.to_string());
        set_text("total-bytes", &fmt_bytes(stats.total_bytes));
        set_text(
            "badge-alice",
            &format!(
                "{} stroke{}",
                stats.alice_visible,
                if stats.alice_visible == 1 { "" } else { "s" }
            ),
        );
        set_text(
            "badge-bob",
            &format!(
                "{} stroke{}",
                stats.bob_visible,
                if stats.bob_visible == 1 { "" } else { "s" }
            ),
        );
        let label = if network.delay_ms == 0 {
            "instant".to_string()
        } else {
            format!("{}ms", network.delay_ms)
        };
        set_text("delay-net-label", &label);
    }

    fn update_network_controls(&self) {
        let network = self.app.network_state();
        if let Ok(btn) = element_by_id::<HtmlButtonElement>("btn-disconnect") {
            btn.set_text_content(Some(if network.disconnected {
                "Reconnect"
            } else {
                "Disconnect"
            }));
            btn.set_class_name(if network.disconnected {
                "btn active"
            } else {
                "btn"
            });
        }
        if let Ok(btn) = element_by_id::<HtmlButtonElement>("btn-sync") {
            btn.set_disabled(!network.disconnected);
        }
        if let Ok(dot) = element_by_id::<HtmlElement>("status-dot") {
            dot.set_class_name(if network.disconnected {
                "status-dot off"
            } else {
                "status-dot"
            });
        }
        set_text(
            "status-text",
            if network.disconnected {
                "disconnected"
            } else {
                "connected"
            },
        );
    }

    fn render_log(&self) {
        let html = self
            .app
            .wire_log()
            .iter()
            .map(|entry| {
                let dir_class = match entry.direction {
                    Direction::AliceToBob => "ab",
                    Direction::BobToAlice => "ba",
                };
                let dir_label = match entry.direction {
                    Direction::AliceToBob => "A-&gt;B",
                    Direction::BobToAlice => "B-&gt;A",
                };
                let status_class = match entry.status {
                    PacketStatus::Inflight => "inflight",
                    PacketStatus::Delivered => "delivered",
                    PacketStatus::Queued => "queued",
                };
                let status_label = match entry.status {
                    PacketStatus::Inflight => "in-flight",
                    PacketStatus::Delivered => "delivered",
                    PacketStatus::Queued => "queued",
                };
                format!(
                    "<div class=\"log-entry\"><span class=\"log-dir {dir_class}\">{dir_label}</span><span class=\"log-hex\">{} ...</span><span class=\"log-bytes\">{}B</span><span class=\"log-tag {status_class}\">{status_label}</span></div>",
                    entry.hex, entry.bytes,
                )
            })
            .collect::<String>();

        if let Ok(el) = element_by_id::<HtmlElement>("wire-log-entries") {
            el.set_inner_html(&html);
        }
    }
}

fn bind_canvas(
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

fn bind_controls(app: &Rc<RefCell<BrowserApp>>) -> Result<(), JsValue> {
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

fn start_loop(app: Rc<RefCell<BrowserApp>>) -> Result<(), JsValue> {
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

fn draw_stroke(ctx: &CanvasRenderingContext2d, stroke: &StrokeView) {
    if stroke.points.len() < 2 {
        return;
    }

    let (r, g, b) = u32_rgb(stroke.color);
    ctx.begin_path();
    ctx.set_stroke_style_str(&format!("rgb({},{},{})", r, g, b));
    ctx.set_line_width(f64::from(stroke.width.max(1.0)));
    ctx.set_line_cap("round");
    ctx.set_line_join("round");
    ctx.set_global_alpha(f64::from(stroke.opacity));
    ctx.move_to(f64::from(stroke.points[0].x), f64::from(stroke.points[0].y));
    for point in &stroke.points[1..] {
        ctx.line_to(f64::from(point.x), f64::from(point.y));
    }
    ctx.stroke();
    ctx.set_global_alpha(1.0);
}

fn pointer_pos(canvas: &HtmlCanvasElement, event: &PointerEvent) -> AppPoint {
    let rect = canvas.get_bounding_client_rect();
    let x =
        (f64::from(event.client_x()) - rect.left()) * (f64::from(canvas.width()) / rect.width());
    let y =
        (f64::from(event.client_y()) - rect.top()) * (f64::from(canvas.height()) / rect.height());
    AppPoint::new(x as f32, y as f32, event.pressure().max(0.5))
}

fn make_packet_el(id: u32, direction: Direction, bytes: usize) -> Result<HtmlElement, JsValue> {
    let el = dom_document()?
        .create_element("div")?
        .dyn_into::<HtmlElement>()?;
    let class_name = match direction {
        Direction::AliceToBob => "packet from-alice",
        Direction::BobToAlice => "packet from-bob",
    };
    el.set_class_name(class_name);
    el.set_attribute("data-id", &id.to_string())?;
    el.set_attribute("title", &format!("{} B", bytes))?;
    el.set_text_content(Some(&format!("{}B", bytes)));
    element_by_id::<HtmlElement>("packets-area")?.append_child(&el)?;
    Ok(el)
}

fn canvas_context(id: &str) -> Result<CanvasRenderingContext2d, JsValue> {
    let canvas = element_by_id::<HtmlCanvasElement>(id)?;
    Ok(canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("2d canvas context unavailable"))?
        .dyn_into::<CanvasRenderingContext2d>()?)
}

fn element_by_id<T>(id: &str) -> Result<T, JsValue>
where
    T: JsCast,
{
    dom_document()?
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))?
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str(&format!("element #{id} has unexpected type")))
}

fn set_text(id: &str, text: &str) {
    if let Ok(el) = element_by_id::<HtmlElement>(id)
        && el.text_content().as_deref() != Some(text)
    {
        el.set_text_content(Some(text));
    }
}

fn element_width(id: &str) -> Option<f64> {
    element_by_id::<HtmlElement>(id)
        .ok()
        .map(|el| f64::from(el.offset_width()))
}

fn element_height(id: &str) -> Option<f64> {
    element_by_id::<HtmlElement>(id)
        .ok()
        .map(|el| f64::from(el.offset_height()))
}

fn request_animation_frame(callback: &Closure<dyn FnMut(f64)>) -> Result<i32, JsValue> {
    web_window()?.request_animation_frame(callback.as_ref().unchecked_ref())
}

fn web_window() -> Result<Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))
}

fn dom_document() -> Result<DomDocument, JsValue> {
    web_window()?
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))
}

fn now() -> f64 {
    web_window()
        .ok()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or(0.0)
}

fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}

fn u32_rgb(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 24) & 0xff) as u8,
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
    )
}
