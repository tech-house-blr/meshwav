//! The Meshwav window. Receive: pair with a sender, see whether this computer is playing or on
//! standby, set the volume. Send: start and stop the sender service, show a pairing code, manage
//! paired receivers. One mode runs at a time. The receiving engine is the library the command-line
//! receiver uses.
mod send;
mod ui;
#[cfg(windows)]
mod wintray;
use eframe::egui::{self, Color32, FontId, RichText, TextStyle};
use meshwav_receiver::sink::{list_outputs, open_default, Gain, Output, Volume};
use meshwav_receiver::store::{self, Paired};
use meshwav_receiver::{pair, run, Settings, Snapshot, Status};
use ui::{back_link, choice, device_row, finish_panes, is_wide, kv, pane, primary, sender_label, split};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Receive,
    Send,
}

const WINDOW_ICON: &[u8] = include_bytes!("../assets/icon-256.png");
const TRAY_ICON: &[u8] = include_bytes!("../assets/tray-64.png");

/// RGBA8 pixels of an embedded PNG.
#[cfg(unix)]
fn make_tray(tx: Sender<TrayCmd>, ctx: &egui::Context) -> Option<TrayHandle> {
    use ksni::blocking::TrayMethods;
    Tray { state: "Starting".into(), tx, ctx: ctx.clone() }
        .spawn()
        .map_err(|e| eprintln!("meshwav-receiver-gui: no tray icon ({e}); the window works without it"))
        .ok()
}
#[cfg(unix)]
fn shutdown_tray(t: TrayHandle) {
    t.shutdown().wait();
}
#[cfg(unix)]
fn set_tray_state(t: &TrayHandle, state: String) {
    t.update(|tray| tray.state = state);
}
#[cfg(windows)]
fn make_tray(tx: Sender<TrayCmd>, ctx: &egui::Context) -> Option<TrayHandle> {
    let (rgba, w, h) = decode_png(TRAY_ICON)?;
    wintray::WinTray::new(rgba, w, h, tx, ctx.clone())
        .map_err(|e| eprintln!("meshwav-receiver-gui: no tray icon ({e}); the window works without it"))
        .ok()
}
#[cfg(windows)]
fn shutdown_tray(t: TrayHandle) {
    drop(t);
}
#[cfg(windows)]
fn set_tray_state(t: &TrayHandle, state: String) {
    t.set_state(&state);
}

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut r = png::Decoder::new(bytes).read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((buf, info.width, info.height))
}

enum TrayCmd {
    Show,
    Quit,
}

#[cfg(unix)]
type TrayHandle = ksni::blocking::Handle<Tray>;
#[cfg(windows)]
type TrayHandle = wintray::WinTray;

#[cfg(unix)]
/// The notification-area icon (StatusNotifierItem): shows the state on hover, and offers Show window and Quit.
#[cfg(unix)]
struct Tray {
    state: String,
    tx: Sender<TrayCmd>,
    ctx: egui::Context,
}

#[cfg(unix)]
impl Tray {
    fn send(&self, c: TrayCmd) {
        let _ = self.tx.send(c);
        self.ctx.request_repaint();
    }
}

#[cfg(unix)]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "meshwav-receiver".into()
    }
    fn title(&self) -> String {
        "Meshwav".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let Some((mut data, w, h)) = decode_png(TRAY_ICON) else { return vec![] };
        for px in data.chunks_exact_mut(4) {
            px.rotate_right(1); // RGBA to the ARGB the protocol wants
        }
        vec![ksni::Icon { width: w as i32, height: h as i32, data }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "Meshwav".into(), description: self.state.clone(), ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCmd::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem { label: self.state.clone(), enabled: false, ..Default::default() }.into(),
            ksni::MenuItem::Separator,
            StandardItem { label: "Show window".into(), activate: Box::new(|t: &mut Self| t.send(TrayCmd::Show)), ..Default::default() }.into(),
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.send(TrayCmd::Quit)), ..Default::default() }.into(),
        ]
    }
}

const BG: Color32 = Color32::from_rgb(0x14, 0x16, 0x18);
const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x21);
const HOVER: Color32 = Color32::from_rgb(0x17, 0x1a, 0x1c);
const LINE: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x33);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xea, 0xec);
const MUTED: Color32 = Color32::from_rgb(0x8f, 0x97, 0x9f);
/// Acid chartreuse. The one accent: the active device, the main button, "Playing".
const ACCENT: Color32 = Color32::from_rgb(0xc8, 0xff, 0x2e);
/// Text on a control filled with the accent.
const ACCENT_INK: Color32 = Color32::from_rgb(0x0f, 0x14, 0x00);
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
    /// Senders found by code alone, waiting for the person to compare fingerprints.
    ask: Receiver<Ask>,
    asking: Option<Ask>,
}

/// A sender found by code alone. Nothing is sent to it until the person says its fingerprint matches.
struct Ask {
    addr: std::net::SocketAddr,
    fingerprint: String,
    reply: Sender<bool>,
}

struct App {
    dir: PathBuf,
    name: String,
    paired: Option<Paired>,
    worker: Option<Worker>,
    volume: Arc<Volume>,
    slider: u32,
    outputs: Vec<Output>,
    device: String,
    /// Ask the sender for lossless PCM instead of Opus (applies live, remembered).
    lossless: bool,
    link: String,
    pairing: Option<Pairing>,
    message: Option<(String, Color32)>,
    confirm_forget: bool,
    tray: Option<TrayHandle>,
    tray_cmds: Receiver<TrayCmd>,
    /// Quit was chosen from the tray menu, so the close that follows must not be turned into a hide.
    quitting: bool,
    tray_state: String,
    mode: Mode,
    send: send::SendPanel,
    /// The sender's own name, as it last told us.
    agent_name: String,
    /// In a narrow window: showing the sender's details page instead of the main view.
    detail_open: bool,
    /// The Settings tab is showing (the mode underneath is unchanged).
    settings: bool,
    /// Closing the window hides it to the tray instead of quitting.
    background: bool,
}

fn device_path(dir: &std::path::Path) -> PathBuf {
    dir.join("output")
}

fn background_path(dir: &std::path::Path) -> PathBuf {
    dir.join("background")
}
fn mode_path(dir: &std::path::Path) -> PathBuf {
    dir.join("mode")
}

fn quality_path(dir: &std::path::Path) -> PathBuf {
    dir.join("quality")
}

fn volume_path(dir: &std::path::Path) -> PathBuf {
    dir.join("volume")
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let tray = make_tray(tx, &cc.egui_ctx);
        let dir = store::default_dir();
        let base = dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.clone());
        let paired = store::load_paired(&dir).ok().flatten();
        let device = std::fs::read_to_string(device_path(&dir)).unwrap_or_default().trim().to_string();
        let start_volume = std::fs::read_to_string(volume_path(&dir)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(100u32).min(100);
        let lossless = std::fs::read_to_string(quality_path(&dir)).map(|s| s.trim() == "lossless").unwrap_or(false);
        let agent_name = store::load_agent_name(&dir).unwrap_or_default();
        let dir_for_bg = dir.clone();
        let mut app = App {
            dir,
            name: store::host_name(),
            paired,
            worker: None,
            volume: Volume::new(start_volume),
            slider: start_volume,
            outputs: list_outputs(),
            device,
            lossless,
            link: String::new(),
            pairing: None,
            message: None,
            confirm_forget: false,
            tray,
            tray_cmds: rx,
            quitting: false,
            tray_state: String::new(),
            mode: Mode::Receive,
            send: send::SendPanel::new(&base),
            agent_name,
            detail_open: false,
            settings: false,
            background: std::fs::read_to_string(background_path(&dir_for_bg)).is_ok_and(|t| t.trim() == "on"),
        };
        app.mode = std::fs::read_to_string(mode_path(&app.dir)).ok().and_then(|m| match m.trim() {
            "send" => Some(Mode::Send),
            "receive" => Some(Mode::Receive),
            _ => None,
        }).unwrap_or(if app.send.is_running() { Mode::Send } else { Mode::Receive });
        // A meshwav://pair link on the command line (the desktop file registers the scheme) fills in the field.
        if let Some(l) = std::env::args().skip(1).find(|a| a.contains("meshwav://pair?")) {
            app.link = l;
        }
        if app.paired.is_some() && app.mode == Mode::Receive {
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
        let sink = match open_default(40, Some(&self.device)) {
            Ok(s) => s,
            Err(e) => return self.fail(e.to_string()),
        };
        let settings = Settings { agent: p.agent, agent_key: p.key, identity, name: self.name.clone() };
        let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
        status.set_lossless(self.lossless);
        let (st, sa) = (stop.clone(), status.clone());
        let sink = Gain::new(sink, self.volume.clone());
        let handle = std::thread::spawn(move || run(&settings, Box::new(sink), &st, &sa));
        self.message = None;
        self.worker = Some(Worker { stop, status, handle: Some(handle) });
    }

    /// Only one mode runs at a time: leaving a mode stops it.
    fn switch_mode(&mut self, to: Mode) {
        if to == self.mode {
            return;
        }
        self.message = None;
        match to {
            Mode::Send => self.stop(),
            Mode::Receive => {
                if self.send.is_running() {
                    if let Err(e) = send::stop_service() {
                        return self.fail(e);
                    }
                    self.send.tick_now();
                }
            }
        }
        self.mode = to;
        let _ = std::fs::write(mode_path(&self.dir), if to == Mode::Send { "send" } else { "receive" });
        if to == Mode::Receive && self.paired.is_some() {
            self.start();
        }
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
        let text = self.link.trim().to_string();
        let identity = match store::load_or_create_identity(&self.dir) {
            Ok(i) => i,
            Err(e) => return self.fail(format!("identity: {e}")),
        };
        let name: String = self.name.trim().chars().take(32).collect();
        let name = if name.is_empty() { store::host_name() } else { name };
        let (tx, rx) = channel();
        let (ask_tx, ask_rx) = channel::<Ask>();
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        if text.contains("meshwav://") {
            // A full link (from the command line or pasted): the address and key are in it.
            let link = match store::parse_pair_link(&text) {
                Ok(l) => l,
                Err(e) => return self.fail(e),
            };
            let agent = match store::resolve(&link) {
                Ok(a) => a,
                Err(e) => return self.fail(e),
            };
            let settings = Settings { agent, agent_key: link.key, identity, name };
            std::thread::spawn(move || {
                let r = pair(&settings, &link.code, &st).map(|_| Paired { agent, key: link.key });
                let _ = tx.send(r);
            });
        } else {
            let code: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
                return self.fail("The pairing code is 6 digits".into());
            }
            let ask_tx = ask_tx.clone();
            std::thread::spawn(move || {
                let mut confirm = |f: &meshwav_receiver::discover::Found| {
                    let (reply, answer) = channel();
                    let fingerprint = f.fingerprint();
                    if ask_tx.send(Ask { addr: f.addr, fingerprint, reply }).is_err() {
                        return false;
                    }
                    loop {
                        match answer.recv_timeout(Duration::from_millis(200)) {
                            Ok(yes) => return yes,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !st.load(Ordering::Relaxed) => {}
                            Err(_) => return false,
                        }
                    }
                };
                let _ = tx.send(meshwav_receiver::discover::pair_with_code(&code, identity, name, &st.clone(), &mut confirm));
            });
        }
        self.message = Some(("Looking for the device and pairing, this can take up to 15 seconds".into(), MUTED));
        self.pairing = Some(Pairing { stop, done: rx, ask: ask_rx, asking: None });
    }

    fn poll(&mut self) {
        if let Some(w) = &self.worker {
            let n = w.status.snapshot().sender_name;
            store::remember_agent_name(&self.dir, &n);
            if !n.is_empty() && n != self.agent_name {
                self.agent_name = n;
            }
        }
        if let Some(p) = &mut self.pairing {
            if p.asking.is_none() {
                p.asking = p.ask.try_recv().ok();
            }
        }
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
    v.extreme_bg_color = Color32::from_rgb(0x0f, 0x11, 0x12);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT_INK);
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
        return ("Stopped", MUTED, "Not connected to the sender.".into());
    }
    if s.standby {
        let how = if s.can_play_here { "Tap Play here to move it to this device." } else { "Choose this one on the sender." };
        return ("Standby", WARN, format!("The sender is playing on another device. {how}"));
    }
    match s.state.as_str() {
        "Playing" => ("Playing", ACCENT, String::new()),
        "" | "Idle" => ("Connecting", MUTED, String::new()),
        other if other.starts_with("Connecting to ") => ("Connecting", MUTED, String::new()),
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
        self.send.tick();
        while let Ok(c) = self.tray_cmds.try_recv() {
            match c {
                TrayCmd::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayCmd::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        self.sync_tray();
        // With "keep running in the tray" on, the close button hides the window. Without a tray
        // there would be no way back, so the window closes as usual.
        if self.background && !self.quitting && self.tray.is_some() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(hide_command());
        }
        // A paste (Ctrl+V or the Paste button) on the pairing screen goes into the link field even if it is not focused.
        if self.mode == Mode::Receive && self.paired.is_none() && self.pairing.is_none() {
            let pasted = ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None }));
            if let Some(t) = pasted.filter(|t| t.contains("meshwav://pair?")) {
                self.link = t.split_whitespace().collect::<String>();
            }
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(22.0))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Meshwav").heading().strong());
                ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).weak().small());
                ui.add_space(14.0);
                let mut to = self.mode;
                for (m, label) in [(Mode::Receive, "Receive"), (Mode::Send, "Send")] {
                    let on = to == m && !self.settings;
                    let t = if on { RichText::new(label).color(ACCENT_INK) } else { RichText::new(label) };
                    if ui.selectable_label(on, t).clicked() {
                        self.settings = false;
                        to = m;
                    }
                }
                let t = if self.settings { RichText::new("Settings").color(ACCENT_INK) } else { RichText::new("Settings") };
                if ui.selectable_label(self.settings, t).clicked() {
                    self.settings = true;
                }
                self.switch_mode(to);
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match (self.mode, self.paired.clone()) {
                    _ if self.settings => self.settings_screen(ui),
                    (Mode::Send, _) => self.send.ui(ui),
                    (Mode::Receive, None) => self.pairing_screen(ui),
                    (Mode::Receive, Some(p)) => self.main_screen(ui, &p),
                }
                if let Some((m, c)) = &self.message {
                    ui.add_space(6.0);
                    ui.label(RichText::new(m).color(*c));
                }
            });
        });
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.stop();
        if let Some(t) = self.tray.take() {
            shutdown_tray(t);
        }
        let _ = std::fs::write(volume_path(&self.dir), self.volume.percent().to_string());
    }
}

impl App {
    /// Keep the tray's hover text and menu in step with the window.
    fn sync_tray(&mut self) {
        let Some(t) = &self.tray else { return };
        let snap = self.worker.as_ref().map(|w| w.status.snapshot());
        let state = match (&self.paired, &snap) {
            _ if self.mode == Mode::Send => self.send.summary(),
            (None, _) => "Not paired".to_string(),
            (Some(_), None) => "Stopped".to_string(),
            (Some(_), Some(s)) => headline(true, s).0.to_string(),
        };
        if state != self.tray_state {
            self.tray_state = state.clone();
            set_tray_state(t, state);
        }
    }

    fn pairing_screen(&mut self, ui: &mut egui::Ui) {
        let busy = self.pairing.is_some();
        ui.add_space(4.0);
        ui.label(RichText::new("Connect a sender").size(34.0));
        ui.label(RichText::new("Pair once. After that this computer plays whatever the sender plays.").color(MUTED));
        ui.add_space(14.0);

        let step = |ui: &mut egui::Ui, n: &str, text: &str| {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(n).color(ACCENT).strong().size(20.0));
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(text)).wrap());
            });
            ui.add_space(6.0);
        };
        step(ui, "1", "On the sender, open Send and tap Show pairing code. On a computer, run `meshwav pair`. It shows a 6-digit code and a key fingerprint.");
        step(ui, "2", "Type the code here and press Pair.");
        step(ui, "3", "Check that the fingerprint shown here is the same as on the sender, then confirm.");
        ui.add_space(8.0);

        ui.add_enabled_ui(!busy, |ui| {
            ui.label(RichText::new("Pairing code").color(MUTED));
            let r = ui.add(egui::TextEdit::singleline(&mut self.link).desired_width(220.0).hint_text("123456").font(egui::FontId::monospace(30.0)));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.link.trim().is_empty() {
                self.begin_pairing();
            }
        });
        ui.add_space(8.0);
        let mut answer: Option<bool> = None;
        if let Some(a) = self.pairing.as_ref().and_then(|p| p.asking.as_ref()) {
            egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::same(14.0)).rounding(6.0).show(ui, |ui| {
                ui.label(RichText::new("Found a sender").strong());
                ui.label(RichText::new(format!("at {}", a.addr)).monospace().color(MUTED));
                ui.add_space(4.0);
                ui.label(RichText::new("Key fingerprint").color(MUTED));
                ui.label(RichText::new(&a.fingerprint).monospace().size(30.0).color(ACCENT));
                ui.add(egui::Label::new("Does it match the fingerprint on the sender next to the code? If not, someone else on your network may be answering. Choose No.").wrap());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(primary("Yes, it matches")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("No, not this one").clicked() {
                        answer = Some(false);
                    }
                });
            });
            ui.add_space(8.0);
        }
        if let (Some(yes), Some(p)) = (answer, self.pairing.as_mut()) {
            if let Some(a) = p.asking.take() {
                let _ = a.reply.send(yes);
            }
        }
        if busy {
            if self.pairing.as_ref().is_some_and(|p| p.asking.is_none()) {
                ui.label(RichText::new("Looking for the sender...").color(MUTED));
            }
            if ui.button("Cancel").clicked() {
                if let Some(p) = self.pairing.take() {
                    p.stop.store(true, Ordering::Relaxed);
                }
                self.message = None;
            }
        } else if ui.add_enabled(!self.link.trim().is_empty(), primary("Pair")).clicked() {
            self.begin_pairing();
        }
    }

    /// Everything that is a setting rather than something you do while listening or sending.
    fn settings_screen(&mut self, ui: &mut egui::Ui) {
        let running = self.worker.is_some();
        ui.add_space(4.0);
        ui.label(RichText::new("Settings").size(34.0));
        ui.add_space(10.0);

        ui.label(RichText::new("This app").color(MUTED));
        let before = self.background;
        ui.checkbox(&mut self.background, "Keep running in the tray when the window is closed");
        if self.background != before {
            let _ = std::fs::write(background_path(&self.dir), if self.background { "on" } else { "off" });
        }
        #[cfg(unix)]
        if self.background {
            ui.label(RichText::new("On Linux this takes effect the next time the app starts.").color(MUTED));
        }
        if self.background && self.tray.is_none() {
            ui.label(RichText::new("No tray icon is available here, so closing will still quit.").color(WARN));
        }
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.label(RichText::new("Receiving").size(20.0).strong());
        ui.add_space(4.0);
        ui.label(RichText::new("Output").color(MUTED));
        if self.outputs.is_empty() {
            ui.label(RichText::new("System default (no list available)").color(MUTED));
        } else {
            let label = |d: &str, o: &[Output]| if d.is_empty() { "System default".to_string() } else { o.iter().find(|x| x.name == d).map(|x| x.description.clone()).unwrap_or_else(|| d.to_string()) };
            let before = self.device.clone();
            egui::ComboBox::from_id_salt("output").width((ui.available_width() - 8.0).min(420.0)).selected_text(label(&self.device, &self.outputs)).show_ui(ui, |ui| {
                choice(ui, &mut self.device, String::new(), "System default");
                for o in &self.outputs {
                    choice(ui, &mut self.device, o.name.clone(), &o.description);
                }
            });
            if self.device != before {
                let _ = std::fs::write(device_path(&self.dir), &self.device);
                if running {
                    self.stop();
                    self.start();
                }
            }
            if ui.small_button("Refresh list").clicked() {
                self.outputs = list_outputs();
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Quality").color(MUTED));
        ui.horizontal(|ui| {
            let before = self.lossless;
            choice(ui, &mut self.lossless, false, "Opus");
            choice(ui, &mut self.lossless, true, "Lossless PCM");
            if self.lossless != before {
                let _ = std::fs::write(quality_path(&self.dir), if self.lossless { "lossless" } else { "opus" });
                if let Some(w) = &self.worker {
                    w.status.set_lossless(self.lossless);
                }
            }
        });
        let note = self.worker.as_ref().map(|w| w.status.snapshot().note).unwrap_or_default();
        if self.lossless && !note.is_empty() {
            ui.label(RichText::new(note).color(WARN));
        } else if self.lossless {
            ui.label(RichText::new("Sends the audio uncompressed, about 1.7 Mbit/s, in 5 ms packets.").color(MUTED));
        }
        ui.add_space(8.0);
        if self.paired.is_some() {
            ui.horizontal_wrapped(|ui| {
                if self.confirm_forget {
                    ui.label(RichText::new("Forget this sender?").color(WARN));
                    if ui.button("Forget").clicked() {
                        self.stop();
                        let _ = store::forget(&self.dir);
                        self.paired = None;
                        self.agent_name.clear();
                        self.detail_open = false;
                        self.confirm_forget = false;
                        self.message = None;
                    }
                    if ui.button("Keep").clicked() {
                        self.confirm_forget = false;
                    }
                } else if ui.button("Forget sender").clicked() {
                    self.confirm_forget = true;
                }
            });
        }
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.label(RichText::new("Sending").size(20.0).strong());
        ui.add_space(4.0);
        self.send.settings_body(ui);
    }

    fn main_screen(&mut self, ui: &mut egui::Ui, p: &Paired) {
        let snap = self.worker.as_ref().map(|w| w.status.snapshot()).unwrap_or_default();
        if is_wide(ui.available_width()) {
            let (left, right) = split(ui);
            let mut l = pane(ui, left);
            self.receive_main(&mut l, &snap, true);
            let mut r = pane(ui, right);
            self.receive_details(&mut r, p, &snap, false);
            finish_panes(ui, &l, &r);
        } else if self.detail_open {
            self.receive_details(ui, p, &snap, true);
        } else {
            self.receive_main(ui, &snap, false);
        }
    }

    /// What is happening, who it comes from, the volume and one button. Details are behind the sender's row.
    fn receive_main(&mut self, ui: &mut egui::Ui, snap: &Snapshot, wide: bool) {
        let running = self.worker.is_some();
        let (title, colour, detail) = headline(running, snap);
        let playing = running && !snap.standby && snap.state == "Playing";
        let name = sender_label(&self.agent_name);
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(44.0).color(colour));
        if playing {
            ui.label(RichText::new(format!("from {name}")).size(20.0));
            let codec = if snap.lossless { "Lossless PCM" } else { "Opus" };
            ui.label(RichText::new(format!("{codec}  |  {} ms frames  |  {} ms buffer", snap.frame_ms, snap.buffer_ms)).color(MUTED));
        } else if !detail.is_empty() {
            ui.add(egui::Label::new(RichText::new(detail).color(MUTED)).wrap());
        }
        ui.add_space(20.0);
        ui.label(RichText::new("Sender").color(MUTED));
        ui.add_space(2.0);
        if device_row(ui, &name, title, playing, wide).clicked() {
            self.detail_open = true;
        }
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Volume").color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{}%", self.slider)).monospace().size(18.0));
            });
        });
        ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(120.0);
        if ui.add(egui::Slider::new(&mut self.slider, 0..=100).show_value(false)).changed() {
            self.volume.set(self.slider);
        }
        ui.add_space(18.0);
        if running && snap.standby && snap.can_play_here {
            if ui.add(primary("Play here").min_size(egui::vec2(160.0, 36.0))).clicked() {
                if let Some(w) = &self.worker {
                    w.status.play_here();
                }
            }
            ui.add_space(8.0);
        }
        if running {
            if ui.add(egui::Button::new("Stop receiving").min_size(egui::vec2(160.0, 36.0))).clicked() {
                self.stop();
            }
        } else if ui.add(primary("Resume receiving").min_size(egui::vec2(160.0, 36.0))).clicked() {
            self.start();
        }
    }

    /// What used to sit on the main screen: where the sender is, the output, the quality, the numbers.
    fn receive_details(&mut self, ui: &mut egui::Ui, p: &Paired, snap: &Snapshot, page: bool) {
        let running = self.worker.is_some();
        if page && back_link(ui, "Back") {
            self.detail_open = false;
        }
        ui.add_space(4.0);
        ui.label(RichText::new(sender_label(&self.agent_name)).heading().strong());
        ui.add_space(6.0);
        kv(ui, "Address", &p.agent.to_string());
        kv(ui, "Key fingerprint", &meshwav_receiver::discover::Found { addr: p.agent, key: p.key }.fingerprint());
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        egui::Grid::new("stats").num_columns(2).min_col_width(130.0).spacing([10.0, 6.0]).show(ui, |ui| {
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
        ui.label(RichText::new("Output, quality and forgetting this sender are in Settings.").color(MUTED));
    }
}

/// Hiding a window is not possible on Wayland (the compositor decides), so there it is minimised.
fn hide_command() -> egui::ViewportCommand {
    #[cfg(unix)]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("WINIT_UNIX_BACKEND").is_none_or(|v| v != "x11") {
        return egui::ViewportCommand::Minimized(true);
    }
    egui::ViewportCommand::Visible(false)
}

fn window_icon() -> egui::IconData {
    let (rgba, width, height) = decode_png(WINDOW_ICON).unwrap_or((vec![0; 4], 1, 1));
    egui::IconData { rgba, width, height }
}

/// Open the window and run until it is closed.
pub fn run_window() -> eframe::Result {
    // A Wayland app cannot hide its window or bring a minimised one back, so with "keep running in
    // the tray" on, the window uses X11 (XWayland), where hiding and showing both work.
    #[cfg(unix)]
    if std::env::var_os("DISPLAY").is_some()
        && std::fs::read_to_string(background_path(&store::default_dir())).is_ok_and(|t| t.trim() == "on")
    {
        std::env::remove_var("WAYLAND_DISPLAY");
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_icon(window_icon()).with_inner_size([980.0, 700.0]).with_min_inner_size([360.0, 480.0]).with_title(concat!("Meshwav ", env!("CARGO_PKG_VERSION"))).with_app_id("meshwav"),
        ..Default::default()
    };
    eframe::run_native("Meshwav", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
