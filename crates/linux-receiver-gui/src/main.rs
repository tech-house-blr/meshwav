//! meshwav-receiver-gui: a small window for the Linux receiver. Pair with an agent, see whether this
//! computer is playing or on standby, and set the volume. The audio engine is the same library the
//! command-line receiver uses.
use eframe::egui::{self, Color32, FontId, RichText, TextStyle};
use meshwav_receiver::sink::{Gain, Pacat, Volume};
use meshwav_receiver::store::{self, Paired};
use meshwav_receiver::{pair, run, Settings, Snapshot, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const BG: Color32 = Color32::from_rgb(0x17, 0x19, 0x1c);
const PANEL: Color32 = Color32::from_rgb(0x1f, 0x22, 0x26);
const LINE: Color32 = Color32::from_rgb(0x34, 0x38, 0x3e);
const TEXT: Color32 = Color32::from_rgb(0xe3, 0xe6, 0xea);
const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xab);
const ACCENT: Color32 = Color32::from_rgb(0x7f, 0xc4, 0xb0);
const WARN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x5c);
const BAD: Color32 = Color32::from_rgb(0xe0, 0x7a, 0x70);

struct Worker {
    stop: Arc<AtomicBool>,
    status: Arc<Status>,
    handle: Option<JoinHandle<Result<(), String>>>,
}

impl Worker {
    fn stop_and_wait(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Pairing {
    stop: Arc<AtomicBool>,
    done: Receiver<Result<Paired, String>>,
}

struct App {
    dir: PathBuf,
    name: String,
    paired: Option<Paired>,
    worker: Option<Worker>,
    volume: Arc<Volume>,
    slider: u32,
    link: String,
    pairing: Option<Pairing>,
    message: Option<(String, Color32)>,
    confirm_forget: bool,
}

fn volume_path(dir: &std::path::Path) -> PathBuf {
    dir.join("volume")
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        style(&cc.egui_ctx);
        let dir = store::default_dir();
        let paired = store::load_paired(&dir).ok().flatten();
        let start_volume = std::fs::read_to_string(volume_path(&dir)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(100u32).min(100);
        let mut app = App {
            dir,
            name: store::host_name(),
            paired,
            worker: None,
            volume: Volume::new(start_volume),
            slider: start_volume,
            link: String::new(),
            pairing: None,
            message: None,
            confirm_forget: false,
        };
        if app.paired.is_some() {
            app.start();
        }
        app
    }

    fn start(&mut self) {
        let Some(p) = &self.paired else { return };
        let identity = match store::load_or_create_identity(&self.dir) {
            Ok(i) => i,
            Err(e) => return self.fail(format!("identity: {e}")),
        };
        let sink = match Pacat::start(40) {
            Ok(s) => s,
            Err(e) => return self.fail(e.to_string()),
        };
        let settings = Settings { agent: p.agent, agent_key: p.key, identity, name: self.name.clone() };
        let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
        let (st, sa) = (stop.clone(), status.clone());
        let sink = Gain::new(sink, self.volume.clone());
        let handle = std::thread::spawn(move || run(&settings, Box::new(sink), &st, &sa));
        self.message = None;
        self.worker = Some(Worker { stop, status, handle: Some(handle) });
    }

    fn fail(&mut self, e: String) {
        self.message = Some((e, BAD));
    }

    fn stop(&mut self) {
        if let Some(mut w) = self.worker.take() {
            w.stop_and_wait();
        }
    }

    fn begin_pairing(&mut self) {
        let link = match store::parse_pair_link(self.link.trim()) {
            Ok(l) => l,
            Err(e) => return self.fail(e),
        };
        let agent = match store::resolve(&link) {
            Ok(a) => a,
            Err(e) => return self.fail(e),
        };
        let identity = match store::load_or_create_identity(&self.dir) {
            Ok(i) => i,
            Err(e) => return self.fail(format!("identity: {e}")),
        };
        let name: String = self.name.trim().chars().take(32).collect();
        let settings = Settings { agent, agent_key: link.key, identity, name: if name.is_empty() { store::host_name() } else { name } };
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::spawn(move || {
            let r = pair(&settings, &link.code, &st).map(|_| Paired { agent, key: link.key });
            let _ = tx.send(r);
        });
        self.message = Some(("Pairing, this can take up to 10 seconds".into(), MUTED));
        self.pairing = Some(Pairing { stop, done: rx });
    }

    fn poll(&mut self) {
        if let Some(p) = &self.pairing {
            match p.done.try_recv() {
                Ok(Ok(paired)) => {
                    if let Err(e) = store::save_paired(&self.dir, &paired) {
                        self.fail(format!("saving the pairing: {e}"));
                    } else {
                        self.paired = Some(paired);
                        self.link.clear();
                        self.pairing = None;
                        self.start();
                    }
                    self.pairing = None;
                }
                Ok(Err(e)) => {
                    self.fail(e);
                    self.pairing = None;
                }
                Err(TryRecvError::Disconnected) => self.pairing = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(w) = &mut self.worker {
            if w.handle.as_ref().is_some_and(|h| h.is_finished()) {
                let r = w.handle.take().map(|h| h.join().unwrap_or_else(|_| Err("the receiver crashed".into())));
                if let Some(Err(e)) = r {
                    self.message = Some((e, BAD));
                }
                self.worker = None;
            }
        }
    }
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.override_text_color = Some(TEXT);
    v.extreme_bg_color = Color32::from_rgb(0x11, 0x13, 0x15);
    v.selection.bg_fill = Color32::from_rgb(0x2f, 0x5a, 0x4e);
    v.hyperlink_color = ACCENT;
    let r = egui::Rounding::same(2.0);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = r;
    }
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x2a, 0x2e, 0x33);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2a, 0x2e, 0x33);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x3b, 0x41);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x3b, 0x41);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, MUTED);
    v.widgets.active.bg_fill = Color32::from_rgb(0x3f, 0x45, 0x4c);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x3f, 0x45, 0x4c);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, TEXT);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.5_f32, TEXT);
    ctx.set_visuals(v);
    let mut s = (*ctx.style()).clone();
    s.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
    s.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
    s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
    s.text_styles.insert(TextStyle::Small, FontId::proportional(13.0));
    s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    s.spacing.item_spacing = egui::vec2(10.0, 8.0);
    s.spacing.button_padding = egui::vec2(14.0, 6.0);
    s.spacing.interact_size.y = 30.0;
    s.spacing.slider_width = 260.0;
    ctx.set_style(s);
}

/// What to say in big letters, and the detail under it.
fn headline(running: bool, s: &Snapshot) -> (&'static str, Color32, String) {
    if !running {
        return ("Stopped", MUTED, "Not connected to the agent.".into());
    }
    if s.standby {
        return ("Standby", WARN, "The agent is sending audio to another device. Choose this one with `meshwav-agent use`.".into());
    }
    match s.state.as_str() {
        "Playing" => ("Playing", ACCENT, String::new()),
        "" | "Idle" => ("Connecting", MUTED, String::new()),
        other => ("Connecting", MUTED, other.to_string()),
    }
}

fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).color(MUTED));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(RichText::new(value).monospace());
    });
    ui.end_row();
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(22.0))).show(ctx, |ui| {
            ui.label(RichText::new("Meshwav").heading().strong());
            match self.paired.clone() {
                None => self.pairing_screen(ui),
                Some(p) => self.main_screen(ui, &p),
            }
            if let Some((m, c)) = &self.message {
                ui.add_space(6.0);
                ui.label(RichText::new(m).color(*c));
            }
        });
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.stop();
        let _ = std::fs::write(volume_path(&self.dir), self.volume.percent().to_string());
    }
}

impl App {
    fn pairing_screen(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Not paired with an agent yet.").color(MUTED));
        ui.add_space(10.0);
        ui.label("On the computer running the agent, run:");
        ui.label(RichText::new("meshwav-agent pair").monospace().color(ACCENT));
        ui.label("then paste the link it prints (it starts with meshwav://pair). The code in it works for 2 minutes.");
        ui.add_space(6.0);
        let busy = self.pairing.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            ui.label(RichText::new("Pair link").color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.link).desired_width(f32::INFINITY).hint_text("meshwav://pair?host=...&port=...&pk=...&code=...").font(TextStyle::Monospace));
            ui.label(RichText::new("Name shown on the agent").color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(240.0));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if busy {
                if ui.button("Cancel").clicked() {
                    if let Some(p) = self.pairing.take() {
                        p.stop.store(true, Ordering::Relaxed);
                    }
                    self.message = None;
                }
            } else if ui.add_enabled(!self.link.trim().is_empty(), egui::Button::new("Pair")).clicked() {
                self.begin_pairing();
            }
        });
    }

    fn main_screen(&mut self, ui: &mut egui::Ui, p: &Paired) {
        let running = self.worker.is_some();
        let snap = self.worker.as_ref().map(|w| w.status.snapshot()).unwrap_or_default();
        let (title, colour, detail) = headline(running, &snap);
        ui.label(RichText::new(format!("Agent {}", p.agent)).monospace().color(MUTED));
        ui.add_space(8.0);
        ui.label(RichText::new(title).size(34.0).color(colour));
        if !detail.is_empty() {
            ui.label(RichText::new(detail).color(MUTED));
        }
        if running && !snap.standby && snap.state == "Playing" {
            let codec = if snap.lossless { "Lossless PCM" } else { "Opus" };
            ui.label(RichText::new(format!("{codec}, {} ms frames", snap.frame_ms)).color(MUTED));
        }
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label(RichText::new("Volume").color(MUTED));
        ui.horizontal(|ui| {
            let r = ui.add(egui::Slider::new(&mut self.slider, 0..=100).show_value(false));
            if r.changed() {
                self.volume.set(self.slider);
            }
            ui.label(RichText::new(format!("{:>3}%", self.slider)).monospace());
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        egui::Grid::new("stats").num_columns(2).min_col_width(150.0).spacing([10.0, 6.0]).show(ui, |ui| {
            if running {
                row(ui, "Buffer", format!("{} ms of {} ms", snap.buffer_ms, snap.target_ms));
                row(ui, "Network jitter", format!("{:.1} ms", snap.jitter_ms));
                row(ui, "Packets received", snap.counters.received.to_string());
                row(ui, "Lost, concealed", snap.counters.lost.to_string());
                row(ui, "Reconnects", snap.reconnects.to_string());
            } else {
                row(ui, "Buffer", "-".into());
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if running {
                if ui.button("Stop").clicked() {
                    self.stop();
                }
            } else if ui.button("Start").clicked() {
                self.start();
            }
            if self.confirm_forget {
                ui.label(RichText::new("Forget this agent?").color(WARN));
                if ui.button("Forget").clicked() {
                    self.stop();
                    let _ = store::forget(&self.dir);
                    self.paired = None;
                    self.confirm_forget = false;
                    self.message = None;
                }
                if ui.button("Keep").clicked() {
                    self.confirm_forget = false;
                }
            } else if ui.button("Forget agent").clicked() {
                self.confirm_forget = true;
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([440.0, 560.0]).with_min_inner_size([400.0, 540.0]).with_title("Meshwav").with_app_id("meshwav-receiver"),
        ..Default::default()
    };
    eframe::run_native("Meshwav", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
