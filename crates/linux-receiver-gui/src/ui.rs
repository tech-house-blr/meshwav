//! Pieces both halves of the window share: the device row, the primary button, and the rule for
//! when the list and the details panel sit side by side or take turns as pages.
use super::{ACCENT, ACCENT_INK, HOVER, LINE, MUTED, PANEL, TEXT};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke};

/// Width of the details panel when it sits beside the list.
pub const DETAIL_W: f32 = 320.0;
/// From this window width the list and the details panel are shown together.
pub const WIDE_AT: f32 = 700.0;

pub fn is_wide(width: f32) -> bool {
    width >= WIDE_AT
}

/// The words the sender's `devices` reply uses, as a person would say them. The bool is true for
/// the device that is playing.
pub fn state_label(state: &str, chosen: bool) -> (String, bool) {
    let (text, active) = match state {
        "active" => ("Playing here".to_string(), true),
        "standby" => ("Standby".to_string(), false),
        "offline" => ("Offline".to_string(), false),
        other => {
            let mut c = other.chars();
            (c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default(), false)
        }
    };
    if chosen && !active {
        (format!("{text}, chosen"), active)
    } else {
        (text, active)
    }
}

/// A 16-digit key id as four groups of four: `a3f109c277de4b18` becomes `a3f1 09c2 77de 4b18`.
pub fn key_groups(key: &str) -> String {
    key.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// What to show as a sender's name: its own name once it has told us, otherwise a plain word
/// (never the address).
pub fn sender_label(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        "Your sender".to_string()
    } else {
        n.to_string()
    }
}

/// The primary action: filled with the accent, the only place it fills a control.
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(ACCENT_INK).strong()).fill(ACCENT).stroke(Stroke::NONE)
}

/// A full-width row with a name and a state. The active device gets a bar of the accent colour on
/// its left edge. Returns the click response.
pub fn device_row(ui: &mut egui::Ui, name: &str, state: &str, active: bool, selected: bool) -> egui::Response {
    let height = 58.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name}, {state}")));
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            Some(PANEL)
        } else if resp.hovered() {
            Some(HOVER)
        } else {
            None
        };
        let p = ui.painter();
        if let Some(f) = fill {
            p.rect_filled(rect, 0.0, f);
        }
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, LINE));
        if active {
            p.rect_filled(Rect::from_min_size(rect.min, egui::vec2(4.0, height)), 0.0, ACCENT);
        }
        let room = (rect.width() - 56.0).max(40.0);
        let one_line = |text: &str, size: f32, colour: Color32| {
            let mut job = LayoutJob::simple(text.to_string(), FontId::proportional(size), colour, room);
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            ui.fonts(|f| f.layout_job(job))
        };
        let name_g = one_line(name, 17.0, TEXT);
        let state_g = one_line(state, 13.0, if active { ACCENT } else { MUTED });
        p.galley(Pos2::new(rect.left() + 18.0, rect.top() + 10.0), name_g, TEXT);
        p.galley(Pos2::new(rect.left() + 18.0, rect.top() + 33.0), state_g, MUTED);
        // A chevron drawn as two lines, so it does not depend on a font having the glyph.
        let c = Pos2::new(rect.right() - 18.0, rect.center().y);
        let s = Stroke::new(1.6_f32, MUTED);
        p.line_segment([Pos2::new(c.x - 3.0, c.y - 6.0), Pos2::new(c.x + 3.0, c.y)], s);
        p.line_segment([Pos2::new(c.x + 3.0, c.y), Pos2::new(c.x - 3.0, c.y + 6.0)], s);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A selectable choice. The chosen one is filled with the accent, so its text must be the dark ink.
pub fn choice<T: PartialEq>(ui: &mut egui::Ui, current: &mut T, value: T, text: &str) -> egui::Response {
    let on = *current == value;
    let t = if on { RichText::new(text.to_string()).color(ACCENT_INK) } else { RichText::new(text.to_string()) };
    ui.selectable_value(current, value, t)
}

/// A quiet "back" link for the details page in a narrow window.
pub fn back_link(ui: &mut egui::Ui, text: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(format!("<  {text}")).color(MUTED)).frame(false)).clicked()
}

/// Two label and value lines, the value in monospace and wrapped when it is long.
pub fn kv(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(112.0, 18.0), Layout::left_to_right(egui::Align::Min), |ui| {
            ui.label(RichText::new(label).color(MUTED));
        });
        ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
            ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
        });
    });
}

/// Splits the free space into the list side and the details side of a wide window, and draws the
/// divider between them.
pub fn split(ui: &egui::Ui) -> (Rect, Rect) {
    let r = ui.available_rect_before_wrap();
    let x = r.right() - DETAIL_W;
    let list = Rect::from_min_max(r.min, Pos2::new(x - 18.0, r.bottom()));
    let detail = Rect::from_min_max(Pos2::new(x, r.top()), r.max);
    ui.painter().line_segment([Pos2::new(x - 9.0, r.top()), Pos2::new(x - 9.0, r.bottom().min(r.top() + 4000.0))], Stroke::new(1.0_f32, LINE));
    (list, detail)
}

/// A child area inside `rect`, laid out top down.
pub fn pane(ui: &mut egui::Ui, rect: Rect) -> egui::Ui {
    ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(egui::Align::Min)))
}

/// Tell the parent how much room two panes used.
pub fn finish_panes(ui: &mut egui::Ui, a: &egui::Ui, b: &egui::Ui) {
    ui.allocate_rect(a.min_rect().union(b.min_rect()), Sense::hover());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_read_as_words() {
        assert_eq!(state_label("active", false), ("Playing here".to_string(), true));
        assert_eq!(state_label("standby", false), ("Standby".to_string(), false));
        assert_eq!(state_label("offline", true), ("Offline, chosen".to_string(), false));
        assert_eq!(state_label("active", true), ("Playing here".to_string(), true));
        assert_eq!(state_label("connecting", false).0, "Connecting");
        assert_eq!(state_label("", false).0, "");
    }

    #[test]
    fn key_ids_are_grouped() {
        assert_eq!(key_groups("a3f109c277de4b18"), "a3f1 09c2 77de 4b18");
        assert_eq!(key_groups("ab"), "ab");
    }

    #[test]
    fn a_sender_without_a_name_is_never_shown_as_an_address() {
        assert_eq!(sender_label(""), "Your sender");
        assert_eq!(sender_label("  "), "Your sender");
        assert_eq!(sender_label("liberation"), "liberation");
    }

    #[test]
    fn the_panel_sits_beside_the_list_only_when_there_is_room() {
        assert!(!is_wide(440.0));
        assert!(!is_wide(699.0));
        assert!(is_wide(700.0));
        assert!(is_wide(1200.0));
    }
}
