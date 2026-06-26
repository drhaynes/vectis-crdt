use app_core::{AppPoint, ClientApp, StrokeView};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, PointerEvent};

pub(crate) fn render_app(ctx: &CanvasRenderingContext2d, app: &ClientApp) {
    if let Some(canvas) = ctx.canvas() {
        ctx.clear_rect(
            0.0,
            0.0,
            f64::from(canvas.width()),
            f64::from(canvas.height()),
        );
    }

    for stroke in app.strokes() {
        draw_stroke(ctx, &stroke);
    }

    if let Some(stroke) = app.live_stroke() {
        draw_stroke(ctx, &stroke);
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

fn u32_rgb(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 24) & 0xff) as u8,
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
    )
}
