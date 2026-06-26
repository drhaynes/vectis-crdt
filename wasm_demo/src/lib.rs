use std::cell::RefCell;
use std::rc::Rc;

use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::Document;
use vectis_crdt::encoding::{decode_update, encode_update};
use vectis_crdt::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Document as DomDocument, Event, HtmlButtonElement, HtmlCanvasElement,
    HtmlElement, HtmlInputElement, PointerEvent, Window,
};

const ALICE_ACTOR: u64 = 1;
const BOB_ACTOR: u64 = 2;
const ALICE_COLOR: u32 = 0xa78bfaff;
const BOB_COLOR: u32 = 0x60a5fbff;
const MAX_LOG: usize = 10;

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let app = Rc::new(RefCell::new(DemoApp::new()?));
    bind_canvas(&app, "canvas-alice", Peer::Alice, ALICE_COLOR)?;
    bind_canvas(&app, "canvas-bob", Peer::Bob, BOB_COLOR)?;
    bind_controls(&app)?;
    start_loop(app)?;

    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Peer {
    Alice,
    Bob,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    AliceToBob,
    BobToAlice,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PacketStatus {
    Inflight,
    Delivered,
    Queued,
}

struct PeerDoc {
    doc: Document,
    buffer: CausalBuffer,
}

impl PeerDoc {
    fn new(actor_id: u64) -> Self {
        Self {
            doc: Document::new(ActorId(actor_id)),
            buffer: CausalBuffer::new(),
        }
    }
}

struct LiveStroke {
    color: u32,
    points: Vec<StrokePoint>,
}

struct QueuedPacket {
    direction: Direction,
    payload: Vec<u8>,
}

struct InflightPacket {
    id: u32,
    start_time: f64,
    duration: f64,
    direction: Direction,
    payload: Vec<u8>,
    el: HtmlElement,
}

struct WireEntry {
    id: Option<u32>,
    direction: Direction,
    bytes: usize,
    hex: String,
    status: PacketStatus,
}

struct DemoApp {
    alice: PeerDoc,
    bob: PeerDoc,
    alice_ctx: CanvasRenderingContext2d,
    bob_ctx: CanvasRenderingContext2d,
    network_delay: u32,
    disconnected: bool,
    queued: Vec<QueuedPacket>,
    inflight: Vec<InflightPacket>,
    next_packet_id: u32,
    total_packets: u32,
    total_bytes: usize,
    alice_bytes: usize,
    bob_bytes: usize,
    wire_log: Vec<WireEntry>,
    live_alice: Option<LiveStroke>,
    live_bob: Option<LiveStroke>,
}

impl DemoApp {
    fn new() -> Result<Self, JsValue> {
        Ok(Self {
            alice: PeerDoc::new(ALICE_ACTOR),
            bob: PeerDoc::new(BOB_ACTOR),
            alice_ctx: canvas_context("canvas-alice")?,
            bob_ctx: canvas_context("canvas-bob")?,
            network_delay: 500,
            disconnected: false,
            queued: Vec::new(),
            inflight: Vec::new(),
            next_packet_id: 0,
            total_packets: 0,
            total_bytes: 0,
            alice_bytes: 0,
            bob_bytes: 0,
            wire_log: Vec::new(),
            live_alice: None,
            live_bob: None,
        })
    }

    fn frame(&mut self, now: f64) {
        self.render_peer(Peer::Alice);
        self.render_peer(Peer::Bob);
        self.tick_packets(now);
        self.update_stats();
    }

    fn reset_docs(&mut self) {
        self.alice = PeerDoc::new(ALICE_ACTOR);
        self.bob = PeerDoc::new(BOB_ACTOR);
    }

    fn pointer_down(
        &mut self,
        peer: Peer,
        canvas: &HtmlCanvasElement,
        event: &PointerEvent,
        color: u32,
    ) {
        let point = pointer_pos(canvas, event);
        let stroke = LiveStroke {
            color,
            points: vec![point],
        };
        match peer {
            Peer::Alice => self.live_alice = Some(stroke),
            Peer::Bob => self.live_bob = Some(stroke),
        }
    }

    fn pointer_move(&mut self, peer: Peer, canvas: &HtmlCanvasElement, event: &PointerEvent) {
        let point = pointer_pos(canvas, event);
        match peer {
            Peer::Alice => {
                if let Some(stroke) = &mut self.live_alice {
                    stroke.points.push(point);
                }
            }
            Peer::Bob => {
                if let Some(stroke) = &mut self.live_bob {
                    stroke.points.push(point);
                }
            }
        }
    }

    fn pointer_up(&mut self, peer: Peer) {
        let stroke = match peer {
            Peer::Alice => self.live_alice.take(),
            Peer::Bob => self.live_bob.take(),
        };

        if let Some(stroke) = stroke {
            if stroke.points.len() >= 2 {
                self.commit(peer, stroke);
            }
        }
    }

    fn pointer_cancel(&mut self, peer: Peer) {
        match peer {
            Peer::Alice => self.live_alice = None,
            Peer::Bob => self.live_bob = None,
        }
    }

    fn commit(&mut self, peer: Peer, stroke: LiveStroke) {
        let data = StrokeData::new(stroke.points.into_boxed_slice(), ToolKind::Pen);
        let props = StrokeProperties::new(stroke.color, 3.0, 1.0, OpId::ZERO);
        self.peer_mut(peer).doc.insert_stroke(data, props);
        self.flush(peer);
    }

    fn flush(&mut self, peer: Peer) {
        let ops = self.peer_mut(peer).doc.take_pending_ops();
        if ops.is_empty() {
            return;
        }

        let payload = encode_update(&ops);
        self.send_packet(peer, payload);
    }

    fn send_packet(&mut self, from_peer: Peer, payload: Vec<u8>) {
        let direction = match from_peer {
            Peer::Alice => Direction::AliceToBob,
            Peer::Bob => Direction::BobToAlice,
        };
        let hex = hex_prefix(&payload);
        let bytes = payload.len();

        self.total_bytes += bytes;
        match from_peer {
            Peer::Alice => self.alice_bytes += bytes,
            Peer::Bob => self.bob_bytes += bytes,
        }

        if self.disconnected {
            self.queued.push(QueuedPacket { direction, payload });
            self.log_entry(direction, bytes, hex, PacketStatus::Queued, None);
            return;
        }

        self.next_packet_id += 1;
        let id = self.next_packet_id;
        let duration = f64::from(self.network_delay.max(30));
        self.log_entry(direction, bytes, hex, PacketStatus::Inflight, Some(id));

        if let Ok(el) = make_packet_el(id, direction, bytes) {
            self.inflight.push(InflightPacket {
                id,
                start_time: now(),
                duration,
                direction,
                payload,
                el,
            });
        } else {
            self.apply_to_target(direction, &payload);
            self.total_packets += 1;
            self.mark_log(id, PacketStatus::Delivered);
        }
    }

    fn tick_packets(&mut self, now_ms: f64) {
        let width = element_width("packets-area").unwrap_or(150.0);
        let height = element_height("packets-area").unwrap_or(320.0);
        let cy = height / 2.0;
        let mut delivered = Vec::new();

        for (idx, packet) in self.inflight.iter_mut().enumerate() {
            let t = ((now_ms - packet.start_time) / packet.duration).clamp(0.0, 1.0);
            let x = match packet.direction {
                Direction::AliceToBob => t * (width - 34.0),
                Direction::BobToAlice => (1.0 - t) * (width - 34.0),
            };
            let opacity = if t < 0.15 {
                t / 0.15
            } else if t > 0.85 {
                (1.0 - t) / 0.15
            } else {
                1.0
            };

            let style = packet.el.style();
            let _ = style.set_property("left", &format!("{}px", x));
            let _ = style.set_property("top", &format!("{}px", cy));
            let _ = style.set_property("opacity", &opacity.to_string());

            if t >= 1.0 {
                delivered.push(idx);
            }
        }

        for idx in delivered.into_iter().rev() {
            let packet = self.inflight.remove(idx);
            packet.el.remove();
            self.apply_to_target(packet.direction, &packet.payload);
            self.total_packets += 1;
            self.mark_log(packet.id, PacketStatus::Delivered);
        }
    }

    fn apply_to_target(&mut self, direction: Direction, payload: &[u8]) {
        let peer = match direction {
            Direction::AliceToBob => &mut self.bob,
            Direction::BobToAlice => &mut self.alice,
        };

        if let Ok(ops) = decode_update(payload) {
            for op in ops {
                let _ = peer.doc.apply_remote_buffered(op, &mut peer.buffer);
            }
        }
    }

    fn sync_now(&mut self) {
        let queued = std::mem::take(&mut self.queued);
        for packet in queued {
            self.apply_to_target(packet.direction, &packet.payload);
            self.total_packets += 1;
        }

        for entry in &mut self.wire_log {
            if entry.status == PacketStatus::Queued {
                entry.status = PacketStatus::Delivered;
            }
        }
        self.render_log();
    }

    fn undo(&mut self, peer: Peer) {
        if self.peer_mut(peer).doc.undo_last_stroke().is_some() {
            self.flush(peer);
        }
    }

    fn clear_all(&mut self) {
        self.queued.clear();
        for packet in self.inflight.drain(..) {
            packet.el.remove();
        }
        self.total_packets = 0;
        self.total_bytes = 0;
        self.alice_bytes = 0;
        self.bob_bytes = 0;
        self.wire_log.clear();
        self.live_alice = None;
        self.live_bob = None;
        self.reset_docs();
        self.render_log();
    }

    fn set_network_delay(&mut self, delay: u32) {
        self.network_delay = delay;
        let label = if delay == 0 {
            "0ms (instant)".to_string()
        } else {
            format!("{}ms", delay)
        };
        set_text("delay-label", &label);
    }

    fn toggle_disconnect(&mut self) {
        self.disconnected = !self.disconnected;
        self.update_network_controls();
    }

    fn reconnect_and_sync(&mut self) {
        self.disconnected = false;
        self.sync_now();
        self.update_network_controls();
    }

    fn update_network_controls(&self) {
        if let Ok(btn) = element_by_id::<HtmlButtonElement>("btn-disconnect") {
            btn.set_text_content(Some(if self.disconnected {
                "Reconnect"
            } else {
                "Disconnect"
            }));
            btn.set_class_name(if self.disconnected {
                "btn active"
            } else {
                "btn"
            });
        }
        if let Ok(btn) = element_by_id::<HtmlButtonElement>("btn-sync") {
            btn.set_disabled(!self.disconnected);
        }
        if let Ok(dot) = element_by_id::<HtmlElement>("status-dot") {
            dot.set_class_name(if self.disconnected {
                "status-dot off"
            } else {
                "status-dot"
            });
        }
        set_text(
            "status-text",
            if self.disconnected {
                "disconnected"
            } else {
                "connected"
            },
        );
    }

    fn render_peer(&self, peer: Peer) {
        let (ctx, peer_doc, live) = match peer {
            Peer::Alice => (&self.alice_ctx, &self.alice, &self.live_alice),
            Peer::Bob => (&self.bob_ctx, &self.bob, &self.live_bob),
        };

        if let Some(canvas) = ctx.canvas() {
            ctx.clear_rect(
                0.0,
                0.0,
                f64::from(canvas.width()),
                f64::from(canvas.height()),
            );
        }

        for id in peer_doc.doc.visible_stroke_ids() {
            if let Some((data, props)) = peer_doc.doc.get_stroke(&id) {
                draw_stroke(
                    ctx,
                    &data.points,
                    props.color.value,
                    props.stroke_width.value,
                    props.opacity.value,
                );
            }
        }

        if let Some(stroke) = live {
            draw_stroke(ctx, &stroke.points, stroke.color, 3.0, 0.65);
        }
    }

    fn update_stats(&self) {
        let av = self.alice.doc.visible_stroke_ids().len();
        let bv = self.bob.doc.visible_stroke_ids().len();
        let aq = self
            .queued
            .iter()
            .filter(|p| p.direction == Direction::AliceToBob)
            .count();
        let bq = self
            .queued
            .iter()
            .filter(|p| p.direction == Direction::BobToAlice)
            .count();

        set_text("alice-visible", &av.to_string());
        set_text("alice-queue", &aq.to_string());
        set_text("alice-undo", &self.alice.doc.undo_depth().to_string());
        set_text("alice-bytes", &fmt_bytes(self.alice_bytes));
        set_text("bob-visible", &bv.to_string());
        set_text("bob-queue", &bq.to_string());
        set_text("bob-undo", &self.bob.doc.undo_depth().to_string());
        set_text("bob-bytes", &fmt_bytes(self.bob_bytes));
        set_text("total-packets", &self.total_packets.to_string());
        set_text("total-bytes", &fmt_bytes(self.total_bytes));
        set_text(
            "badge-alice",
            &format!("{} stroke{}", av, if av == 1 { "" } else { "s" }),
        );
        set_text(
            "badge-bob",
            &format!("{} stroke{}", bv, if bv == 1 { "" } else { "s" }),
        );
        let net_label = if self.network_delay == 0 {
            "instant".to_string()
        } else {
            format!("{}ms", self.network_delay)
        };
        set_text("delay-net-label", &net_label);
    }

    fn log_entry(
        &mut self,
        direction: Direction,
        bytes: usize,
        hex: String,
        status: PacketStatus,
        id: Option<u32>,
    ) {
        self.wire_log.insert(
            0,
            WireEntry {
                id,
                direction,
                bytes,
                hex,
                status,
            },
        );
        if self.wire_log.len() > MAX_LOG {
            self.wire_log.pop();
        }
        self.render_log();
    }

    fn mark_log(&mut self, id: u32, status: PacketStatus) {
        if let Some(entry) = self.wire_log.iter_mut().find(|entry| entry.id == Some(id)) {
            entry.status = status;
            self.render_log();
        }
    }

    fn render_log(&self) {
        let html = self.wire_log.iter().map(|entry| {
            let dir_class = match entry.direction { Direction::AliceToBob => "ab", Direction::BobToAlice => "ba" };
            let dir_label = match entry.direction { Direction::AliceToBob => "A-&gt;B", Direction::BobToAlice => "B-&gt;A" };
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
        }).collect::<String>();

        if let Ok(el) = element_by_id::<HtmlElement>("wire-log-entries") {
            el.set_inner_html(&html);
        }
    }

    fn peer_mut(&mut self, peer: Peer) -> &mut PeerDoc {
        match peer {
            Peer::Alice => &mut self.alice,
            Peer::Bob => &mut self.bob,
        }
    }
}

fn bind_canvas(
    app: &Rc<RefCell<DemoApp>>,
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

fn bind_controls(app: &Rc<RefCell<DemoApp>>) -> Result<(), JsValue> {
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

fn bind_button<F>(app: &Rc<RefCell<DemoApp>>, id: &str, mut f: F) -> Result<(), JsValue>
where
    F: 'static + FnMut(&mut DemoApp),
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

fn start_loop(app: Rc<RefCell<DemoApp>>) -> Result<(), JsValue> {
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

fn draw_stroke(
    ctx: &CanvasRenderingContext2d,
    points: &[StrokePoint],
    color: u32,
    width: f32,
    opacity: f32,
) {
    if points.len() < 2 {
        return;
    }

    let (r, g, b) = u32_rgb(color);
    ctx.begin_path();
    ctx.set_stroke_style_str(&format!("rgb({},{},{})", r, g, b));
    ctx.set_line_width(f64::from(width.max(1.0)));
    ctx.set_line_cap("round");
    ctx.set_line_join("round");
    ctx.set_global_alpha(f64::from(opacity));
    ctx.move_to(f64::from(points[0].x), f64::from(points[0].y));
    for point in &points[1..] {
        ctx.line_to(f64::from(point.x), f64::from(point.y));
    }
    ctx.stroke();
    ctx.set_global_alpha(1.0);
}

fn pointer_pos(canvas: &HtmlCanvasElement, event: &PointerEvent) -> StrokePoint {
    let rect = canvas.get_bounding_client_rect();
    let x =
        (f64::from(event.client_x()) - rect.left()) * (f64::from(canvas.width()) / rect.width());
    let y =
        (f64::from(event.client_y()) - rect.top()) * (f64::from(canvas.height()) / rect.height());
    StrokePoint::new(x as f32, y as f32, event.pressure().max(0.5))
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
    if let Ok(el) = element_by_id::<HtmlElement>(id) {
        if el.text_content().as_deref() != Some(text) {
            el.set_text_content(Some(text));
        }
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

fn hex_prefix(payload: &[u8]) -> String {
    payload
        .iter()
        .take(7)
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<_>>()
        .join(" ")
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
