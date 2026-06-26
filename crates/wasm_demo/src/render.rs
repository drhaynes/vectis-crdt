use std::collections::BTreeMap;

use app_core::{AppPoint, DemoApp, Direction, Peer, StrokeView};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, PointerEvent};

use crate::dom::{dom_document, element_by_id, element_height, element_width};

pub(crate) fn render_peer(ctx: &CanvasRenderingContext2d, app: &DemoApp, peer: Peer) {
    if let Some(canvas) = ctx.canvas() {
        ctx.clear_rect(
            0.0,
            0.0,
            f64::from(canvas.width()),
            f64::from(canvas.height()),
        );
    }

    for stroke in app.strokes(peer) {
        draw_stroke(ctx, &stroke);
    }

    if let Some(stroke) = app.live_stroke(peer) {
        draw_stroke(ctx, &stroke);
    }
}

pub(crate) fn render_packets(
    packet_elements: &BTreeMap<u32, HtmlElement>,
    app: &DemoApp,
    now_ms: f64,
) {
    let width = element_width("packets-area").unwrap_or(150.0);
    let height = element_height("packets-area").unwrap_or(320.0);
    let cy = height / 2.0;

    for packet in app.packet_views(now_ms) {
        let Some(el) = packet_elements.get(&packet.id) else {
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

pub(crate) fn draw_stroke(ctx: &CanvasRenderingContext2d, stroke: &StrokeView) {
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

pub(crate) fn pointer_pos(canvas: &HtmlCanvasElement, event: &PointerEvent) -> AppPoint {
    let rect = canvas.get_bounding_client_rect();
    let x =
        (f64::from(event.client_x()) - rect.left()) * (f64::from(canvas.width()) / rect.width());
    let y =
        (f64::from(event.client_y()) - rect.top()) * (f64::from(canvas.height()) / rect.height());
    AppPoint::new(x as f32, y as f32, event.pressure().max(0.5))
}

pub(crate) fn make_packet_el(
    id: u32,
    direction: Direction,
    bytes: usize,
) -> Result<HtmlElement, JsValue> {
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

fn u32_rgb(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 24) & 0xff) as u8,
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
    )
}
