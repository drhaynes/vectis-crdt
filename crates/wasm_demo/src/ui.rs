use app_core::{ClientApp, Direction};
use web_sys::{HtmlButtonElement, HtmlElement};

use crate::dom::{element_by_id, set_text};

pub(crate) fn update_stats(app: &ClientApp) {
    let stats = app.stats();

    set_text("room-id", app.room());
    set_text(
        "actor-id",
        &stats
            .actor
            .map(|actor| actor.to_string())
            .unwrap_or_else(|| "pending".to_string()),
    );
    set_text("resume-token", &stats.resume_token);
    set_text("visible-strokes", &stats.visible_strokes.to_string());
    set_text("undo-depth", &stats.undo_depth.to_string());
    set_text("remote-cursors", &stats.remote_cursors.to_string());
    set_text("gc-generation", &stats.gc_generation.to_string());
    set_text("frames-sent", &stats.frames_sent.to_string());
    set_text("frames-received", &stats.frames_received.to_string());
    set_text("bytes-sent", &fmt_bytes(stats.bytes_sent));
    set_text("bytes-received", &fmt_bytes(stats.bytes_received));
    set_text("status-text", &stats.status);
    set_text(
        "badge-main",
        &format!(
            "{} stroke{}",
            stats.visible_strokes,
            if stats.visible_strokes == 1 { "" } else { "s" }
        ),
    );
}

pub(crate) fn update_controls(app: &ClientApp) {
    let state = app.connection_state();
    if let Ok(btn) = element_by_id::<HtmlButtonElement>("btn-undo") {
        btn.set_disabled(!state.loaded);
    }
    if let Ok(dot) = element_by_id::<HtmlElement>("status-dot") {
        dot.set_class_name(if state.connected && state.loaded {
            "status-dot ready"
        } else if state.connected {
            "status-dot connecting"
        } else {
            "status-dot off"
        });
    }
}

pub(crate) fn render_log(app: &ClientApp) {
    let html = app
        .wire_log()
        .iter()
        .map(|entry| {
            let dir_class = match entry.direction {
                Direction::Outbound => "out",
                Direction::Inbound => "in",
            };
            let dir_label = match entry.direction {
                Direction::Outbound => "out",
                Direction::Inbound => "in",
            };
            format!(
                "<div class=\"log-entry\"><span class=\"log-dir {dir_class}\">{dir_label}</span><span class=\"log-kind\">{}</span><span class=\"log-hex\">{} ...</span><span class=\"log-bytes\">{}B</span></div>",
                entry.kind, entry.hex, entry.bytes,
            )
        })
        .collect::<String>();

    if let Ok(el) = element_by_id::<HtmlElement>("wire-log-entries") {
        el.set_inner_html(&html);
    }
}

fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}
