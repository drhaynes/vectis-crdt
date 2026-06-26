use app_core::{DemoApp, Direction, PacketStatus};
use web_sys::{HtmlButtonElement, HtmlElement};

use crate::dom::{element_by_id, set_text};

pub(crate) fn update_stats(app: &DemoApp) {
    let stats = app.stats();
    let network = app.network_state();

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

pub(crate) fn update_network_controls(app: &DemoApp) {
    let network = app.network_state();
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

pub(crate) fn render_log(app: &DemoApp) {
    let html = app
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

fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}
