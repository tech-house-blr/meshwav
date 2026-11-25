//! Windows has no sender yet (it needs WASAPI loopback capture), so the Send tab only says so.
//! It keeps the same small interface as the Linux `send` module so the window code is shared.
use eframe::egui::{self, RichText};

pub struct SendPanel;

pub fn stop_service() -> Result<(), String> {
    Ok(())
}

impl SendPanel {
    pub fn new(_dir: &std::path::Path) -> SendPanel {
        SendPanel
    }
    pub fn summary(&self) -> String {
        "Sending is not available on Windows yet".into()
    }
    pub fn tick(&mut self) {}
    pub fn tick_now(&mut self) {}
    pub fn is_running(&self) -> bool {
        false
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.label(RichText::new("Sending from this computer is not available on Windows yet.").color(super::MUTED));
    }
}
